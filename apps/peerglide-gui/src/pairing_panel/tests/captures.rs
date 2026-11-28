//! Draw the actual pairing panel over two disposable loopback profiles on the CI X server.
use super::{pump, worker, App, Identity, Mode, Panel, Phase, CODE_CELLS_ID};
use crate::visual_capture;
use anyhow::{Context, Result};
use eframe::egui;
use std::{
    path::PathBuf,
    sync::{mpsc, Arc},
    time::{Duration, Instant},
};

const SHOTS: [&str; 4] = [
    "peerglide-pairing-code-entry-compact.png",
    "peerglide-pairing-host-confirm-compact.png",
    "peerglide-pairing-guest-confirm-compact.png",
    "peerglide-pairing-host-waiting-compact.png",
];
type Screenshot = Arc<egui::ColorImage>;

struct Capture {
    host: App,
    guest: App,
    offer: Panel,
    join: Panel,
    binary: PathBuf,
    frames: Vec<Screenshot>,
    pending: Option<mpsc::Receiver<Screenshot>>,
    result: Option<mpsc::Sender<Result<Vec<Screenshot>>>>,
    settled: Instant,
    deadline: Instant,
}

impl Capture {
    fn start_loopback(&mut self, ctx: &egui::Context) -> Result<()> {
        self.offer.start(
            &self.host,
            ctx,
            Mode::Offer("127.0.0.1:0".parse()?),
            None,
            &self.binary,
        )?;
        pump(
            &mut self.host,
            &mut self.guest,
            &mut self.offer,
            &mut self.join,
            ctx,
            |_, _, offer, _| offer.supervisor.displayed_code().is_some(),
        )?;
        // Copy the live code directly into the real join worker. No rendered frame is
        // captured while either panel holds an unmasked code.
        let (code, address) = self
            .offer
            .supervisor
            .displayed_code()
            .context("loopback offer did not produce a pairing code")?;
        let mode = Mode::connect(address, code.to_owned())?;
        self.join
            .start(&self.guest, ctx, mode, Some(address), &self.binary)?;
        pump(
            &mut self.host,
            &mut self.guest,
            &mut self.offer,
            &mut self.join,
            ctx,
            |_, _, offer, join| {
                offer.supervisor.phase() == Phase::Verified
                    && join.supervisor.phase() == Phase::Verified
            },
        )
    }

    fn advance(&mut self, ctx: &egui::Context) -> Result<()> {
        match self.frames.len() {
            1 => self.start_loopback(ctx)?,
            3 => {
                self.offer.approve(&self.host)?;
                anyhow::ensure!(self.offer.supervisor.phase() == Phase::WaitingPeer);
                anyhow::ensure!(self.join.supervisor.phase() == Phase::Verified);
            }
            _ => {}
        }
        for endpoint in [&self.host, &self.guest] {
            anyhow::ensure!(
                endpoint.config.fingerprint.is_none()
                    && !endpoint.config.native
                    && !endpoint.config.allow
                    && !endpoint.config.reconnect
                    && endpoint.child.is_none()
                    && !endpoint.dir.join("trusted-peer.crt.der").exists(),
                "a single confirmation must not save trust or start input"
            );
        }
        self.settled = Instant::now();
        Ok(())
    }

    fn finish(&mut self, ctx: &egui::Context, result: Result<Vec<Screenshot>>) {
        if let Some(sender) = self.result.take() {
            let _ = sender.send(result);
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
}

impl eframe::App for Capture {
    fn ui(&mut self, ui: &mut egui::Ui, _: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if self.result.is_none() {
            return;
        }
        if Instant::now() >= self.deadline {
            self.finish(&ctx, Err(anyhow::anyhow!("pairing capture timed out")));
            return;
        }
        if let Some(frame) = self.pending.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.pending = None;
            self.frames.push(frame);
            if let Err(error) = self.advance(&ctx) {
                self.finish(&ctx, Err(error));
                return;
            }
            if self.frames.len() == SHOTS.len() {
                let frames = std::mem::take(&mut self.frames);
                self.finish(&ctx, Ok(frames));
                return;
            }
        }
        let (app, panel, phase) = match self.frames.len() {
            0 => (&mut self.guest, &mut self.join, Phase::Idle),
            1 => (&mut self.host, &mut self.offer, Phase::Verified),
            2 => (&mut self.guest, &mut self.join, Phase::Verified),
            _ => (&mut self.host, &mut self.offer, Phase::WaitingPeer),
        };
        if panel.supervisor.phase() != phase || !panel.code.is_empty() {
            self.finish(
                &ctx,
                Err(anyhow::anyhow!("unexpected pairing capture state")),
            );
            return;
        }
        std::mem::swap(&mut app.pairing_panel, panel);
        app.desktop(ui);
        std::mem::swap(&mut app.pairing_panel, panel);
        if self.pending.is_none() && self.settled.elapsed() >= Duration::from_millis(320) {
            let cells_visible = ctx
                .read_response(egui::Id::unique(CODE_CELLS_ID))
                .is_some_and(|response| response.interact_rect == response.rect);
            if phase == Phase::Idle && !cells_visible {
                self.finish(&ctx, Err(anyhow::anyhow!("the code cells need scrolling")));
                return;
            }
            let (tx, rx) = mpsc::sync_channel(1);
            ctx.request_screenshot(move |image| {
                let _ = tx.send(image);
            });
            self.pending = Some(rx);
        }
        ctx.request_repaint_after(Duration::from_millis(50));
    }
}

#[test]
#[ignore = "requires exact release CLI and the isolated CI X server"]
fn capture_real_pairing_views() -> Result<()> {
    let output = visual_capture::output_dir()?;
    let fixture = crate::tests::Fixture::new()?;
    let host = App::load(fixture.path.join("host"));
    let guest = App::load(fixture.path.join("guest"));
    Identity::create(&host.identity_dir())?;
    Identity::create(&guest.identity_dir())?;
    let mut offer = Panel::default();
    offer.open("测试电脑 B", "");
    let mut join = Panel::default();
    join.open("测试电脑 A", "127.0.0.1");
    join.select_entry(true);
    let binary = worker()?;
    let (tx, rx) = mpsc::channel();
    eframe::run_native(
        "Linko / 屏跃",
        visual_capture::options(),
        Box::new(move |cc| {
            host.theme(&cc.egui_ctx);
            Ok(Box::new(Capture {
                host,
                guest,
                offer,
                join,
                binary,
                frames: Vec::new(),
                pending: None,
                result: Some(tx),
                settled: Instant::now(),
                deadline: Instant::now() + Duration::from_secs(90),
            }))
        }),
    )
    .map_err(|error| anyhow::anyhow!("native pairing capture: {error}"))?;
    let frames = rx.try_recv().context("pairing capture did not finish")??;
    anyhow::ensure!(frames.len() == SHOTS.len());
    for (file, image) in SHOTS.into_iter().zip(frames) {
        visual_capture::write_shot(&output.join(file), &image)?;
    }
    fixture.close()?;
    Ok(())
}
