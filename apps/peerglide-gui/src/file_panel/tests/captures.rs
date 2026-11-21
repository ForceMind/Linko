//! Native consent and terminal screens from real file workers with disposable peers.
use super::{finish_sender, next_event, Approval, Event, Panel, ReceiveBind, Request};
use crate::{visual_capture, App, Dialog, Page};
use anyhow::{Context, Result};
use eframe::egui;
use peerglide_transport::{file_worker, Identity};
use std::{
    fs,
    net::SocketAddr,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const FILE: &str = "测试样张.bin";
const CONTENTS: &[u8] = b"isolated Linko file receipt";
const SHOTS: &[&str] = &[
    "peerglide-file-awaiting-consent-compact.png",
    "peerglide-file-paused-compact.png",
    "peerglide-file-completed-compact.png",
    "peerglide-file-declined-compact.png",
    "peerglide-settings-dark-compact.png",
    "peerglide-recovery-unconfirmed-compact.png",
    "peerglide-sharing-clipboard-linux-compact.png",
    "peerglide-sharing-audio-linux-compact.png",
];

struct Peer {
    app: App,
    files: Panel,
}

impl Peer {
    fn new(dir: PathBuf) -> Result<(Self, Identity)> {
        let mut app = App::load(dir);
        let identity = Identity::create(&app.identity_dir())?;
        app.navigate_to(Page::Transfers);
        Ok((
            Self {
                app,
                files: Panel::default(),
            },
            identity,
        ))
    }

    fn remember(&mut self, other: &Identity) -> Result<()> {
        fs::write(
            self.app.dir.join("trusted-peer.crt.der"),
            &other.certificate,
        )?;
        self.app.config.fingerprint = Some(other.fingerprint());
        Ok(())
    }

    fn listen(&mut self, address: SocketAddr) -> Result<SocketAddr> {
        self.files.start_job(
            &mut self.app,
            Request::Receive {
                bind: ReceiveBind::Explicit(address),
                budget: file_worker::MAX_FILE_BYTES,
            },
            None,
        )?;
        let active = self.files.active.as_mut().context("receiver not created")?;
        let Event::Listening(bound) = next_event(&mut active.job)? else {
            anyhow::bail!("file receiver did not start listening");
        };
        Ok(bound)
    }

    fn send(&mut self, source: &Path, target: SocketAddr) -> Result<()> {
        self.files.start_job(
            &mut self.app,
            Request::Send {
                target,
                source: source.into(),
                resume_id: None,
                prepare_handoff: false,
            },
            None,
        )
    }

    fn offer(&mut self, ctx: &egui::Context) -> Result<[u8; 16]> {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            self.files.tick(&mut self.app, ctx);
            if let Some(offer) = self.files.active.as_ref().and_then(|job| job.offer.as_ref()) {
                return Ok(offer.id);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        anyhow::bail!("file offer did not arrive")
    }

    fn finish(&mut self, ctx: &egui::Context) -> Result<()> {
        finish_sender(&mut self.files, &mut self.app, ctx)
    }

    fn draw(&mut self, ui: &mut egui::Ui) {
        std::mem::swap(&mut self.app.file_panel, &mut self.files);
        self.app.desktop(ui);
        std::mem::swap(&mut self.app.file_panel, &mut self.files);
    }
}

#[test]
#[ignore = "requires the isolated CI X server"]
fn capture_file_settings_and_recovery_review() -> Result<()> {
    let fixture = crate::tests::Fixture::new()?;
    let (mut sender, local) = Peer::new(fixture.path.join("sender"))?;
    let (mut receiver, remote) = Peer::new(fixture.path.join("receiver"))?;
    sender.remember(&remote)?;
    receiver.remember(&local)?;
    let source = fixture.path.join(FILE);
    let destination = fixture.path.join("received");
    fs::write(&source, CONTENTS)?;
    fs::create_dir(&destination)?;
    receiver.files.destination = destination.to_str().context("fixture path")?.into();
    let target = receiver.listen("127.0.0.1:0".parse()?)?;
    sender.send(&source, target)?;
    let mut last = None;
    let mut first_offer = None;
    visual_capture::capture_views(SHOTS, move |index, ui| {
        let ctx = ui.ctx().clone();
        if last != Some(index) {
            match index {
                0 => {
                    first_offer = Some(receiver.offer(&ctx)?);
                    sender.files.tick(&mut sender.app, &ctx);
                    sender.app.theme(&ctx);
                }
                1 => {
                    sender.files.pause(&mut sender.app);
                    sender.finish(&ctx)?;
                    receiver.finish(&ctx)?;
                    anyhow::ensure!(sender.files.paused.is_some());
                    anyhow::ensure!(!destination.join(FILE).exists());
                }
                2 => {
                    anyhow::ensure!(receiver.listen(target)? == target);
                    sender.files.resume_paused(&mut sender.app)?;
                    anyhow::ensure!(Some(receiver.offer(&ctx)?) == first_offer);
                    receiver.files.decide(&receiver.app, Approval::Receive)?;
                    sender.finish(&ctx)?;
                    receiver.finish(&ctx)?;
                    anyhow::ensure!(sender.files.paused.is_none());
                    anyhow::ensure!(fs::read(destination.join(FILE))? == CONTENTS);
                }
                3 => {
                    anyhow::ensure!(receiver.listen(target)? == target);
                    sender.send(&source, target)?;
                    receiver.offer(&ctx)?;
                    receiver.files.decide(&receiver.app, Approval::Decline)?;
                    receiver.finish(&ctx)?;
                    sender.finish(&ctx)?;
                    anyhow::ensure!(fs::read(destination.join(FILE))? == CONTENTS);
                }
                4 => {
                    sender.app.navigate_to(Page::Settings);
                    sender.app.config.dark = true;
                    sender.app.desktop_preferences.reduce_motion = true;
                    sender.app.theme(&ctx);
                }
                5 => {
                    sender.app.config.dark = false;
                    sender.app.theme(&ctx);
                    sender.app.navigate_to(Page::Displays);
                    sender.app.recovery_blocked = true;
                    sender.app.dialog = Some(Dialog::Recovery);
                }
                6 => {
                    sender.app.dialog = None;
                    sender.app.recovery_blocked = false;
                    sender.app.navigate_to(Page::Clipboard);
                }
                _ => sender.app.navigate_to(Page::Audio),
            }
            last = Some(index);
        }
        if matches!(index, 0 | 3) {
            receiver.draw(ui);
        } else {
            sender.draw(ui);
        }
        Ok(())
    })?;
    fixture.close()
}
