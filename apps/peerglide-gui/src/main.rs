#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
mod config_store;
mod discovery_panel;
mod gesture_lab;
mod gesture_rules;
use anyhow::{Context, Result};
use clap::Parser;
use eframe::egui::{self, Color32, RichText, Vec2};
use peerglide_protocol::worker::{WorkerEvent, WorkerState};
use peerglide_transport::{self as transport, Identity};
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicU16, Ordering},
        mpsc, Arc,
    },
    time::{Duration, Instant},
};

#[derive(Parser)]
#[command(name = "peerglide-gui", version, about = "PeerGlide desktop control panel")]
struct Options {
    /// Use this configuration/identity directory instead of the user profile.
    /// Does not import, migrate or authorize anything from another directory.
    #[arg(long, value_name = "DIRECTORY")]
    data_dir: Option<PathBuf>,
    /// Open the offline, window-local gesture recording page.
    #[arg(long)]
    gesture_lab: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Config {
    peer_name: String,
    address: String,
    listen: bool,
    allow: bool,
    native: bool,
    reconnect: bool,
    edge: String,
    dark: bool,
    fingerprint: Option<String>,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            peer_name: "My other computer".into(),
            address: "192.168.1.10:24870".into(),
            listen: true,
            allow: false,
            native: false,
            reconnect: false,
            edge: "right".into(),
            dark: true,
            fingerprint: None,
        }
    }
}
#[derive(Clone, Copy, PartialEq)]
enum Page {
    Home,
    Devices,
    Displays,
    Transfers,
    Settings,
    GestureLab,
    GestureRules,
}
/// Coalesced lifecycle state cannot be dropped by a full diagnostic queue.
#[derive(Default)]
struct WorkerSnapshot(AtomicU16);
impl WorkerSnapshot {
    fn publish(&self, state: WorkerState) {
        let code = match state {
            WorkerState::Listening => 1,
            WorkerState::Reconnecting => 2,
            WorkerState::Disconnected => 3,
            WorkerState::Authenticated => 4,
            WorkerState::ProbePassed => 5,
            WorkerState::Stopped => 6,
            WorkerState::Failed => 7,
        };
        let mut old = self.0.load(Ordering::Relaxed);
        loop {
            let next = (old & 0x100)
                | code
                | if state == WorkerState::ProbePassed {
                    0x100
                } else {
                    0
                };
            match self
                .0
                .compare_exchange_weak(old, next, Ordering::Release, Ordering::Relaxed)
            {
                Ok(_) => break,
                Err(current) => old = current,
            }
        }
    }
    fn take(&self) -> (Option<WorkerState>, bool) {
        let value = self.0.swap(0, Ordering::AcqRel);
        let state = match value & 0xff {
            1 => Some(WorkerState::Listening),
            2 => Some(WorkerState::Reconnecting),
            3 => Some(WorkerState::Disconnected),
            4 => Some(WorkerState::Authenticated),
            5 => Some(WorkerState::ProbePassed),
            6 => Some(WorkerState::Stopped),
            7 => Some(WorkerState::Failed),
            _ => None,
        };
        (state, value & 0x100 != 0)
    }
}
struct App {
    discovery: discovery_panel::Panel,
    gesture_lab: gesture_lab::GestureLab,
    gesture_rules: gesture_rules::RuleEditor,
    dir: PathBuf,
    config: Config,
    config_store: Option<config_store::Store>,
    instance_lock: Option<config_store::Lock>,
    page: Page,
    import_path: String,
    confirmed_fingerprint: Option<String>,
    notice: String,
    child: Option<Child>,
    events: Option<mpsc::Receiver<String>>,
    worker_state: Option<Arc<WorkerSnapshot>>,
    status: String,
    connected: bool,
    logs: VecDeque<String>,
    stopping: Option<Instant>,
}
impl App {
    fn new(cc: &eframe::CreationContext<'_>, dir: PathBuf) -> Self {
        let app = Self::load(dir);
        app.theme(&cc.egui_ctx);
        app
    }
    fn load(dir: PathBuf) -> Self {
        let mut notice = String::new();
        let instance_lock = std::fs::create_dir_all(&dir)
            .map_err(anyhow::Error::from)
            .and_then(|_| config_store::lock(&dir.join("gui-instance.lock")))
            .map_err(|e| {
                notice = format!("Read-only instance: {e:#}");
                e
            })
            .ok();
        let (config_store, config) =
            match config_store::Store::open::<Config>(dir.join("settings.json")) {
                Ok((store, config)) => (Some(store), config),
                Err(e) => {
                    notice = format!("Settings preserved; recovery required: {e:#}");
                    (None, Config::default())
                }
            };
        let mut app = Self {
            discovery: discovery_panel::Panel::default(),
            gesture_lab: gesture_lab::GestureLab::new(&dir),
            gesture_rules: gesture_rules::RuleEditor::new(&dir),
            dir,
            config,
            config_store,
            instance_lock,
            page: Page::Home,
            import_path: String::new(),
            confirmed_fingerprint: None,
            notice,
            child: None,
            events: None,
            worker_state: None,
            status: "Offline".into(),
            connected: false,
            logs: VecDeque::new(),
            stopping: None,
        };
        if let Err(e) = std::fs::create_dir_all(&app.dir) {
            app.notice = e.to_string();
        }
        app
    }
    fn theme(&self, ctx: &egui::Context) {
        ctx.set_visuals(if self.config.dark {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        });
    }
    fn save(&mut self) {
        if let Err(e) = self.save_config() {
            self.notice = e.to_string();
        }
    }
    fn save_config(&mut self) -> Result<()> {
        anyhow::ensure!(
            self.instance_lock.is_some(),
            "another GUI instance owns this data folder"
        );
        self.config_store
            .as_mut()
            .context("settings recovery required; original file preserved")?
            .save(&self.config)
    }
    fn identity_dir(&self) -> PathBuf {
        self.dir.join("identity")
    }
    fn local_fingerprint(&self) -> Option<String> {
        Identity::load(&self.identity_dir())
            .ok()
            .map(|i| i.fingerprint())
    }
    fn use_discovery_address(&mut self, address: std::net::SocketAddr) -> Result<()> {
        anyhow::ensure!(
            self.child.is_none() && self.instance_lock.is_some(),
            "stop the session before changing its address"
        );
        self.config.address = address.to_string();
        self.config.listen = false;
        self.save_config()
    }
    fn candidate(&self) -> Result<Vec<u8>> {
        transport::read_certificate(std::path::Path::new(self.import_path.trim()))
    }
    fn enroll(&mut self) -> Result<()> {
        anyhow::ensure!(
            self.instance_lock.is_some() && self.config_store.is_some(),
            "writable settings required before enrollment"
        );
        anyhow::ensure!(
            self.child.is_none(),
            "stop the device session before enrollment"
        );
        let confirmed = self
            .confirmed_fingerprint
            .as_deref()
            .context("compare the fingerprint on both computers first")?;
        let bytes = self.candidate()?;
        anyhow::ensure!(
            transport::fingerprint(&bytes) == confirmed,
            "certificate changed; compare the fingerprint again"
        );
        if let Ok(local) = Identity::load(&self.identity_dir()) {
            anyhow::ensure!(
                bytes != local.certificate,
                "choose the other computer's certificate"
            );
        }
        std::fs::write(self.dir.join("trusted-peer.crt.der"), &bytes)?;
        let fingerprint = transport::fingerprint(&bytes);
        if self.config.fingerprint.as_deref() != Some(fingerprint.as_str()) {
            self.config.allow = false;
        }
        self.config.fingerprint = Some(fingerprint);
        self.save_config()?;
        self.confirmed_fingerprint = None;
        Ok(())
    }
    fn start(&mut self, probe: bool) -> Result<()> {
        anyhow::ensure!(
            self.instance_lock.is_some(),
            "another GUI instance owns this data folder"
        );
        anyhow::ensure!(self.child.is_none(), "a session is already running");
        Identity::load(&self.identity_dir())
            .context("create this computer's identity in Devices first")?;
        let cert = self.dir.join("trusted-peer.crt.der");
        let bytes = transport::read_certificate(&cert).context("enroll a peer in Devices first")?;
        anyhow::ensure!(
            self.config.fingerprint.as_deref() == Some(transport::fingerprint(&bytes).as_str()),
            "enrolled certificate changed; explicitly re-enroll it"
        );
        if !self.config.listen || probe {
            self.config
                .address
                .parse::<std::net::SocketAddr>()
                .context("enter peer IP:port")?;
        }
        self.save_config()?;
        let executable = std::env::current_exe()?.with_file_name(if cfg!(windows) {
            "peerglide.exe"
        } else {
            "peerglide"
        });
        anyhow::ensure!(
            executable.is_file(),
            "peerglide CLI must be next to peerglide-gui; build the whole workspace"
        );
        let mut cmd = Command::new(executable);
        if self.config.listen && !probe {
            cmd.arg("listen");
        } else {
            cmd.arg("connect")
                .arg(&self.config.address)
                .args(["--bind", "0.0.0.0:0"]);
        }
        cmd.arg("--identity")
            .arg(self.identity_dir())
            .arg("--peer-cert")
            .arg(cert)
            .arg("--edge")
            .arg(&self.config.edge)
            .arg("--managed");
        if self.config.reconnect && !probe {
            cmd.arg("--reconnect");
        }
        if probe {
            cmd.arg("--probe");
        } else {
            if self.config.allow {
                cmd.arg("--allow-control");
            }
            if self.config.native {
                cmd.arg("--native");
            }
        }
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x08000000);
        }
        let mut child = cmd.spawn()?;
        let (tx, rx) = mpsc::sync_channel(128);
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let output_tx = tx.clone();
        let snapshot = Arc::new(WorkerSnapshot::default());
        let output_state = snapshot.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Some(event) = WorkerEvent::parse(&line) {
                    output_state.publish(event.state);
                } else {
                    let _ = output_tx.try_send(line.chars().take(600).collect());
                }
            }
        });
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                let _ = tx.try_send(line.chars().take(600).collect());
            }
        });
        self.child = Some(child);
        self.events = Some(rx);
        self.worker_state = Some(snapshot);
        self.logs.clear();
        self.connected = false;
        self.stopping = None;
        self.status = "Starting".into();
        Ok(())
    }
    fn stop(&mut self) {
        if let Some(child) = &mut self.child {
            if let Some(stdin) = &mut child.stdin {
                let _ = stdin.write_all(b"stop\n");
                let _ = stdin.flush();
            }
            self.connected = false;
            self.stopping = Some(Instant::now());
            self.status = "Restoring local control".into();
        }
    }
    fn revoke(&mut self) -> Result<()> {
        anyhow::ensure!(
            self.instance_lock.is_some(),
            "another GUI instance owns this data folder"
        );
        self.stop();
        self.confirmed_fingerprint = None;
        self.connected = false;
        self.config.fingerprint = None;
        self.config.allow = false;
        match std::fs::remove_file(self.dir.join("trusted-peer.crt.der")) {
            Ok(()) => (),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(e).context("peer disabled but removing certificate failed"),
        }
        self.save_config().context("persist revoked peer trust")
    }
    fn poll(&mut self) {
        self.discovery
            .poll(self.child.is_some() && self.stopping.is_none() && self.config.listen);
        if let Some(rx) = &self.events {
            while let Ok(line) = rx.try_recv() {
                self.logs.push_back(line);
                if self.logs.len() > 100 {
                    self.logs.pop_front();
                }
            }
        }
        if let Some(snapshot) = &self.worker_state {
            let (state, probe_passed) = snapshot.take();
            if probe_passed {
                self.notice = "Connection probe passed. No input was injected.".into();
            }
            if self.child.is_some() && self.stopping.is_none() {
                match state {
                    Some(WorkerState::Listening) => {
                        self.connected = false;
                        self.status = "Waiting for peer".into();
                    }
                    Some(WorkerState::ProbePassed) => {
                        self.connected = false;
                        self.status = "Probe passed".into();
                    }
                    Some(WorkerState::Reconnecting) => {
                        self.connected = false;
                        self.status = "Reconnecting trusted peer".into();
                    }
                    Some(WorkerState::Disconnected) => {
                        self.connected = false;
                        self.status = "Disconnected".into();
                    }
                    Some(WorkerState::Authenticated) => {
                        self.connected = true;
                        self.status = "Authenticated".into();
                    }
                    Some(WorkerState::Stopped | WorkerState::Failed) => {
                        self.connected = false;
                        self.status = "Worker stopped".into();
                    }
                    _ => (),
                }
            }
        }
        if let Some(child) = &mut self.child {
            if self
                .stopping
                .is_some_and(|s| s.elapsed() > Duration::from_secs(3))
            {
                let _ = child.kill();
                self.notice = "Worker required forced stop; remote input lease will expire.".into();
            }
            match child.try_wait() {
                Ok(Some(exit)) => {
                    self.connected = false;
                    self.status = if exit.success() {
                        "Offline".into()
                    } else {
                        format!("Worker exited: {exit}")
                    };
                    self.child = None;
                    self.stopping = None;
                }
                Err(e) => {
                    self.notice = e.to_string();
                }
                _ => (),
            }
        }
    }
    fn feedback(&mut self, result: Result<()>, success: &str) {
        self.notice = match result {
            Ok(()) => success.into(),
            Err(e) => format!("{e:#}"),
        };
    }
    fn heading(ui: &mut egui::Ui, title: &str, subtitle: &str) {
        ui.heading(RichText::new(title).size(30.0));
        ui.add_space(8.0);
        ui.label(subtitle);
        ui.add_space(24.0);
    }
    fn home(&mut self, ui: &mut egui::Ui) {
        Self::heading(
            ui,
            "Your workspace, connected.",
            "Multiple Computers. One Seamless Experience.",
        );
        egui::Frame::group(ui.style())
            .inner_margin(20.0)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.strong("THIS COMPUTER");
                        ui.add_space(8.0);
                        ui.label(std::env::consts::OS);
                        ui.label(if self.local_fingerprint().is_some() {
                            "Identity ready"
                        } else {
                            "Create an identity to get started"
                        });
                    });
                    ui.add_space(50.0);
                    ui.vertical(|ui| {
                        ui.strong(&self.config.peer_name);
                        ui.add_space(8.0);
                        ui.colored_label(
                            if self.connected {
                                Color32::from_rgb(94, 211, 161)
                            } else {
                                ui.visuals().text_color()
                            },
                            &self.status,
                        );
                        ui.label(if self.config.fingerprint.is_some() {
                            "Explicitly enrolled peer"
                        } else {
                            "No enrolled peer"
                        });
                    });
                });
            });
        ui.add_space(22.0);
        ui.add_enabled_ui(self.child.is_none() && self.instance_lock.is_some(), |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.config.listen, true, "Listen on this computer");
                ui.selectable_value(&mut self.config.listen, false, "Connect to peer");
            });
            if !self.config.listen {
                ui.horizontal(|ui| {
                    ui.label("Peer IP:port");
                    ui.text_edit_singleline(&mut self.config.address);
                });
            }
            ui.add_space(12.0);
            if ui
                .button(RichText::new("Start session").size(18.0))
                .clicked()
            {
                let r = self.start(false);
                self.feedback(r, "Session worker started.");
            }
            if ui.button("Run connection probe").clicked() {
                let r = self.start(true);
                self.feedback(
                    r,
                    "Probing the peer address. Its listener must already be running.",
                );
            }
        });
        if self.child.is_some() && ui.button("Disconnect & restore local control").clicked() {
            self.stop();
        }
        ui.add_space(18.0);
        ui.label(if self.config.native {
            "Native input enabled. Ctrl+Alt+Esc restores local control."
        } else {
            "Diagnostic mode: native capture/injection is off. Enable it explicitly in Settings."
        });
        egui::CollapsingHeader::new("Session diagnostics — no keystroke payloads").show(ui, |ui| {
            egui::ScrollArea::vertical()
                .max_height(180.0)
                .show(ui, |ui| {
                    for line in &self.logs {
                        ui.monospace(line);
                    }
                });
        });
    }
    fn devices(&mut self, ui: &mut egui::Ui) {
        Self::heading(
            ui,
            "Trusted devices",
            "Trust is explicit. Discovery and LAN access never authorize control.",
        );
        let own = self.local_fingerprint();
        if let Some(address) = self.discovery.ui(
            ui,
            own,
            self.child.is_some() && self.stopping.is_none() && self.config.listen,
            self.child.is_none(),
            self.instance_lock.is_some(),
        ) {
            let result = self.use_discovery_address(address);
            self.feedback(
                result,
                "Address selected only; certificate enrollment and input permission are unchanged",
            );
        }
        ui.strong("This computer");
        if let Some(fp) = self.local_fingerprint() {
            ui.label("Public certificate");
            ui.monospace(
                self.identity_dir()
                    .join("node.crt.der")
                    .display()
                    .to_string(),
            );
            ui.label("BLAKE3 fingerprint");
            ui.monospace(fp);
        } else if ui
            .add_enabled(
                self.instance_lock.is_some(),
                egui::Button::new("Create local identity"),
            )
            .clicked()
        {
            let r = Identity::create(&self.identity_dir()).map(|_| ());
            self.feedback(
                r,
                "Identity created. Exchange only node.crt.der; keep the private key here.",
            );
        }
        ui.add_space(18.0);
        ui.separator();
        ui.add_space(18.0);
        ui.add_enabled_ui(self.child.is_none() && self.instance_lock.is_some(), |ui| {
            ui.horizontal(|ui| {
                ui.label("Peer name");
                ui.text_edit_singleline(&mut self.config.peer_name);
            });
            ui.label("Other computer's public certificate file");
            if ui.text_edit_singleline(&mut self.import_path).changed() {
                self.confirmed_fingerprint = None;
            }
            let candidate_fingerprint = self
                .candidate()
                .ok()
                .map(|bytes| transport::fingerprint(&bytes));
            if let Some(fp) = &candidate_fingerprint {
                ui.monospace(fp);
            }
            let mut compared = candidate_fingerprint.is_some()
                && self.confirmed_fingerprint == candidate_fingerprint;
            if !compared {
                self.confirmed_fingerprint = None;
            }
            if ui
                .add_enabled(
                    candidate_fingerprint.is_some(),
                    egui::Checkbox::new(
                        &mut compared,
                        "I independently compared this fingerprint on the other computer",
                    ),
                )
                .changed()
            {
                self.confirmed_fingerprint = if compared {
                    candidate_fingerprint
                } else {
                    None
                };
            }
            if ui
                .add_enabled(compared, egui::Button::new("Enroll trusted peer"))
                .clicked()
            {
                let r = self.enroll();
                self.feedback(
                    r,
                    "Peer enrolled. Input permission remains a separate setting.",
                );
            }
            if let Some(fp) = &self.config.fingerprint {
                ui.add_space(14.0);
                ui.label("Enrolled fingerprint");
                ui.monospace(fp);
            }
        });
        if self.config.fingerprint.is_some()
            && ui.button("Disconnect and revoke peer trust").clicked()
        {
            let result = self.revoke();
            self.feedback(result, "Peer trust revoked; active worker is stopping.");
        }
        ui.add_space(18.0);
        ui.weak("Automatic discovery and pairing codes are planned. Enrollment currently uses public certificate files.");
    }
    fn displays(&mut self, ui: &mut egui::Ui) {
        Self::heading(
            ui,
            "Arrange your displays",
            "Drag the peer around this computer to choose the switching edge.",
        );
        ui.add_enabled_ui(self.child.is_none() && self.instance_lock.is_some(), |ui| {
            let (rect, _) = ui.allocate_exact_size(
                Vec2::new(ui.available_width().min(700.0), 340.0),
                egui::Sense::hover(),
            );
            let center = rect.center();
            let local = egui::Rect::from_center_size(center, Vec2::new(170.0, 100.0));
            let offset = match self.config.edge.as_str() {
                "left" => Vec2::new(-190.0, 0.0),
                "top" => Vec2::new(0.0, -110.0),
                "bottom" => Vec2::new(0.0, 110.0),
                _ => Vec2::new(190.0, 0.0),
            };
            let peer = egui::Rect::from_center_size(center + offset, Vec2::new(170.0, 100.0));
            let response = ui.interact(
                peer,
                ui.scope_id().with("peer-display"),
                egui::Sense::drag(),
            );
            let displayed = if response.dragged() {
                peer.translate(response.drag_delta())
            } else {
                peer
            };
            let painter = ui.painter();
            painter.rect_filled(local, 10.0, Color32::from_rgb(49, 65, 91));
            painter.rect_filled(displayed, 10.0, Color32::from_rgb(41, 103, 130));
            painter.text(
                local.center(),
                egui::Align2::CENTER_CENTER,
                "This computer",
                egui::FontId::proportional(16.0),
                Color32::WHITE,
            );
            painter.text(
                displayed.center(),
                egui::Align2::CENTER_CENTER,
                &self.config.peer_name,
                egui::FontId::proportional(16.0),
                Color32::WHITE,
            );
            if response.drag_stopped() {
                if let Some(p) = response.interact_pointer_pos() {
                    let d = p - center;
                    self.config.edge = if d.x.abs() >= d.y.abs() {
                        if d.x < 0.0 {
                            "left"
                        } else {
                            "right"
                        }
                    } else if d.y < 0.0 {
                        "top"
                    } else {
                        "bottom"
                    }
                    .into();
                    self.save();
                }
            }
            ui.horizontal(|ui| {
                ui.label("Switching edge");
                for side in ["left", "right", "top", "bottom"] {
                    if ui
                        .selectable_value(&mut self.config.edge, side.into(), side)
                        .changed()
                    {
                        self.save();
                    }
                }
            });
        });
        ui.add_space(18.0);
        ui.label("Primary displays only in this prototype. The layout applies to the next session. Configure the opposite edge on your other computer.");
    }
    fn settings(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        Self::heading(ui, "Make it yours", "Input permissions and appearance.");
        if self.config_store.is_none() && self.instance_lock.is_some() {
            ui.label("The settings file was preserved. Reset makes a backup and disables grants; identities are kept.");
            if ui.button("Back up and reset settings").clicked() {
                match config_store::Store::reset::<Config>(self.dir.join("settings.json")) {
                    Ok((store, config)) => {
                        self.config_store = Some(store);
                        self.config = config;
                        self.notice = "Settings reset; previous bytes backed up".into();
                    }
                    Err(e) => self.notice = format!("Reset failed: {e:#}"),
                }
            }
        }
        if self.instance_lock.is_none() || self.config_store.is_none() {
            ui.disable();
        }
        if ui
            .checkbox(&mut self.config.dark, "Dark appearance")
            .changed()
        {
            self.theme(ctx);
            self.save();
        }
        ui.add_enabled_ui(self.child.is_none() && self.instance_lock.is_some(), |ui| {
            if ui
                .checkbox(
                    &mut self.config.reconnect,
                    "Reconnect trusted peer automatically",
                )
                .changed()
            {
                self.save();
            }
            ui.weak("Reconnect is optional. Control stays local until you switch again.");
        });
        ui.add_space(18.0);
        ui.add_enabled_ui(self.child.is_none() && self.instance_lock.is_some(), |ui| {
            if ui
                .checkbox(
                    &mut self.config.allow,
                    "Allow this enrolled peer to control this computer",
                )
                .changed()
            {
                self.save();
            }
            ui.add_enabled_ui(cfg!(any(windows, target_os = "macos")), |ui| {
                if ui
                    .checkbox(
                        &mut self.config.native,
                        "Enable native keyboard and mouse capture / injection",
                    )
                    .changed()
                {
                    self.save();
                }
            });
        });
        ui.add_space(18.0);
        ui.label("Ctrl+Alt+Right, then release: toggle target\nCtrl+Alt+Esc: emergency local recovery\nPhysical input at receiver: reclaim control");
        ui.add_space(14.0);
        ui.label("macOS: Accessibility and Input Monitoring permissions are required. Windows: elevated and secure desktops are outside this prototype's control.");
        ui.add_space(14.0);
        ui.label("Clipboard and file permissions are separate future features. No telemetry or cloud account.");
        ui.add_space(14.0);
        ui.weak(format!("Settings: {}", self.dir.display()));
    }
}
impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        self.poll();
        if ctx.input(|i| !i.focused || i.viewport().minimized.unwrap_or(false)) {
            self.gesture_lab.cancel();
        }
        ctx.request_repaint_after(Duration::from_millis(100));
    }
    fn ui(&mut self, root: &mut egui::Ui, _: &mut eframe::Frame) {
        let ctx = root.ctx().clone();
        egui::Panel::left("navigation")
            .exact_size(180.0)
            .show(root, |ui| {
                ui.add_space(24.0);
                ui.heading(RichText::new("PeerGlide").size(25.0));
                ui.weak("SCREEN TO SCREEN");
                ui.add_space(34.0);
                for (page, label) in [
                    (Page::Home, "Home"),
                    (Page::Devices, "Devices"),
                    (Page::Displays, "Displays"),
                    (Page::Transfers, "Transfers"),
                    (Page::Settings, "Settings"),
                    (Page::GestureLab, "Gesture lab"),
                    (Page::GestureRules, "Gesture rules"),
                ] {
                    if ui
                        .selectable_label(self.page == page, RichText::new(label).size(18.0))
                        .clicked()
                    {
                        self.page = page;
                    }
                    ui.add_space(12.0);
                }
                ui.separator();
                ui.weak("0.1.0-dev\nExperimental prototype");
            });
        egui::Panel::bottom("status").show(root, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(&self.status);
                if !self.notice.is_empty() {
                    ui.separator();
                    ui.label(&self.notice);
                }
            });
        });
        if self.page != Page::GestureLab || self.child.is_some() {
            self.gesture_lab.cancel();
        }
        egui::CentralPanel::default().show(root,|ui|{ui.add_space(20.0);egui::ScrollArea::vertical().show(ui,|ui|match self.page {
            Page::Home=>self.home(ui),Page::Devices=>self.devices(ui),Page::Displays=>self.displays(ui),Page::Settings=>self.settings(ui,&ctx),
            Page::GestureLab=>{self.gesture_lab.ui(ui,self.child.is_some());self.gesture_rules.preview_ui(ui,self.gesture_lab.matched_id());},
            Page::GestureRules=>self.gesture_rules.ui(ui,&self.dir,self.instance_lock.is_some() && self.child.is_none()),
            Page::Transfers=>{Self::heading(ui,"File transfers","Coming in M4");ui.label("File sending, resumable chunks, clipboard sharing and native cross-screen drag are not available in this prototype.");},
        });});
    }
}
impl Drop for App {
    fn drop(&mut self) {
        self.stop();
        if let Some(child) = &mut self.child {
            for _ in 0..40 {
                if child.try_wait().ok().flatten().is_some() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
fn main() -> eframe::Result<()> {
    let options = Options::parse();
    let dir = options.data_dir.unwrap_or_else(default_data_dir);
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1000.0, 720.0])
            .with_min_inner_size([860.0, 640.0]),
        ..Default::default()
    };
    eframe::run_native(
        "PeerGlide",
        native_options,
        Box::new(move |cc| {
            let mut app = App::new(cc, dir);
            if options.gesture_lab {
                app.page = Page::GestureLab;
            }
            Ok(Box::new(app))
        }),
    )
}

fn default_data_dir() -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(std::env::var_os("LOCALAPPDATA").expect("LOCALAPPDATA unavailable"))
            .join("PeerGlide")
    } else if cfg!(target_os = "macos") {
        PathBuf::from(std::env::var_os("HOME").expect("user directory unavailable"))
            .join("Library/Application Support/PeerGlide")
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(std::env::var_os("HOME").expect("user directory unavailable"))
                    .join(".local/share")
            })
            .join("peerglide")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_data_directory_keeps_profiles_and_permissions_separate() {
        let fixture = Fixture::new().unwrap();
        let original = fixture.path.join("original");
        std::fs::create_dir(&original).unwrap();
        let sentinel = original.join("settings.json");
        let preserved = br#"{"store_version":999,"data":{"native":true}}"#;
        std::fs::write(&sentinel, preserved).unwrap();
        let selected = fixture.path.join("isolated profile 屏跃");
        let options = Options::try_parse_from([
            std::ffi::OsString::from("peerglide-gui"),
            std::ffi::OsString::from("--data-dir"),
            selected.clone().into_os_string(),
            std::ffi::OsString::from("--gesture-lab"),
        ])
        .unwrap();
        assert!(options.gesture_lab);
        let mut app = App::load(options.data_dir.unwrap());
        assert_eq!(app.dir, selected);
        assert!(!app.config.native && !app.config.allow && !app.config.reconnect);
        assert!(app.config.fingerprint.is_none());
        assert!(app.local_fingerprint().is_none());
        app.save_config().unwrap();
        assert!(selected.join("settings.json").is_file());
        assert!(!selected.join("identity").exists());
        assert_eq!(std::fs::read(sentinel).unwrap(), preserved);
    }

    #[test]
    fn malformed_gui_options_fail_before_creating_a_profile() {
        assert!(Options::try_parse_from(["peerglide-gui", "--data-dir"]).is_err());
        assert!(Options::try_parse_from(["peerglide-gui", "--unknown-option"]).is_err());
        let options = Options::try_parse_from(["peerglide-gui"]).unwrap();
        assert!(options.data_dir.is_none());
        assert!(!options.gesture_lab);
    }
    pub(super) struct Fixture {
        pub(super) path: PathBuf,
        live: bool,
    }
    impl Fixture {
        pub(super) fn new() -> Result<Self> {
            static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            for _ in 0..100 {
                let path = std::env::temp_dir().join(format!(
                    "peerglide-ui-{}-{}-{}",
                    std::process::id(),
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)?
                        .as_nanos(),
                    NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                ));
                match std::fs::create_dir(&path) {
                    Ok(()) => return Ok(Self { path, live: true }),
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(e) => {
                        return Err(e)
                            .with_context(|| format!("create GUI fixture {}", path.display()))
                    }
                }
            }
            anyhow::bail!("unable to reserve a unique GUI test directory")
        }
        pub(super) fn close(mut self) -> Result<()> {
            std::fs::remove_dir_all(&self.path)
                .with_context(|| format!("cleanup GUI fixture {}", self.path.display()))?;
            self.live = false;
            Ok(())
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            if self.live {
                if let Err(e) = std::fs::remove_dir_all(&self.path) {
                    eprintln!("GUI fixture cleanup failed at {}: {e}", self.path.display());
                }
            }
        }
    }
    #[test]
    fn parallel_fixtures_have_exclusive_ownership_and_cleanup() -> Result<()> {
        let fixtures: Vec<_> = (0..32).map(|_| std::thread::spawn(Fixture::new)).collect();
        let fixtures: Vec<_> = fixtures
            .into_iter()
            .map(|t| t.join().expect("fixture thread panicked"))
            .collect::<Result<_>>()?;
        let paths: std::collections::BTreeSet<_> =
            fixtures.iter().map(|f| f.path.clone()).collect();
        assert_eq!(paths.len(), 32);
        for fixture in &fixtures {
            assert!(fixture.path.is_dir());
        }
        for fixture in fixtures {
            fixture.close()?;
        }
        for path in paths {
            assert!(!path.exists());
        }
        let path = {
            let fixture = Fixture::new()?;
            fixture.path.clone()
        };
        assert!(!path.exists(), "RAII cleanup on early return");
        Ok(())
    }
    #[test]
    fn enrollment_requires_confirmation_and_detects_changed_certificate() -> Result<()> {
        let fixture = Fixture::new()?;
        let dir = fixture.path.clone();
        {
            let mut app = App::load(dir.clone());
            let local = Identity::create(&app.identity_dir())?;
            let peer = Identity::generate()?;
            let candidate = dir.join("candidate.der");
            std::fs::write(&candidate, &peer.certificate)
                .context("write peer candidate fixture")?;
            app.import_path = candidate.display().to_string();
            assert!(app.enroll().is_err());
            app.confirmed_fingerprint = Some(peer.fingerprint());
            app.enroll()?;
            assert!(!app.config.allow);
            assert!(!app.config.native);
            let loaded = App::load(dir.clone());
            assert_eq!(loaded.config.fingerprint, Some(peer.fingerprint()));
            drop(loaded);
            std::fs::write(dir.join("trusted-peer.crt.der"), &local.certificate)
                .context("replace pinned certificate fixture")?;
            assert!(app
                .start(false)
                .unwrap_err()
                .to_string()
                .contains("changed"));
            app.import_path = app
                .identity_dir()
                .join("node.crt.der")
                .display()
                .to_string();
            app.confirmed_fingerprint = Some(local.fingerprint());
            assert!(app.enroll().is_err());
        }
        fixture.close()?;
        Ok(())
    }
    #[test]
    fn enrollment_confirmation_cannot_follow_replaced_file_contents() -> Result<()> {
        let fixture = Fixture::new()?;
        let mut app = App::load(fixture.path.clone());
        Identity::create(&app.identity_dir())?;
        let first = Identity::generate()?;
        let replacement = Identity::generate()?;
        let candidate = fixture.path.join("candidate.der");
        std::fs::write(&candidate, &first.certificate)?;
        app.import_path = candidate.display().to_string();
        app.confirmed_fingerprint = Some(first.fingerprint());
        std::fs::write(&candidate, &replacement.certificate)?;
        assert!(
            app.enroll().is_err(),
            "confirmation followed a different certificate"
        );
        assert!(app.config.fingerprint.is_none());
        assert!(!fixture.path.join("trusted-peer.crt.der").exists());
        app.confirmed_fingerprint = Some(replacement.fingerprint());
        app.enroll()?;
        assert_eq!(app.config.fingerprint, Some(replacement.fingerprint()));
        let enrolled = std::fs::read(fixture.path.join("trusted-peer.crt.der"))?;
        assert!(enrolled == replacement.certificate);
        std::fs::write(&candidate, vec![0; transport::MAX_CERTIFICATE_BYTES + 1])?;
        app.confirmed_fingerprint = Some(replacement.fingerprint());
        assert!(app.enroll().is_err());
        assert_eq!(app.config.fingerprint, Some(replacement.fingerprint()));
        assert!(std::fs::read(fixture.path.join("trusted-peer.crt.der"))? == enrolled);
        std::fs::write(&candidate, &replacement.certificate)?;
        app.confirmed_fingerprint = Some(replacement.fingerprint());
        app.revoke()?;
        assert!(app.confirmed_fingerprint.is_none());
        assert!(
            app.enroll().is_err(),
            "revocation must invalidate pending confirmation"
        );
        Ok(())
    }
    #[test]
    fn replacing_a_peer_never_inherits_its_control_grant_and_second_gui_cannot_write() -> Result<()>
    {
        let fixture = Fixture::new()?;
        let mut app = App::load(fixture.path.clone());
        Identity::create(&app.identity_dir())?;
        let candidate = fixture.path.join("candidate.der");
        for _ in 0..2 {
            let peer = Identity::generate()?;
            std::fs::write(&candidate, &peer.certificate)?;
            app.import_path = candidate.display().to_string();
            app.confirmed_fingerprint = Some(peer.fingerprint());
            app.config.allow = true;
            app.enroll()?;
            assert!(!app.config.allow);
        }
        let mut second = App::load(fixture.path.clone());
        assert!(second.instance_lock.is_none());
        assert!(second.save_config().is_err());
        assert!(second.start(true).is_err());
        drop(second);
        drop(app);
        let replacement = App::load(fixture.path.clone());
        assert!(replacement.instance_lock.is_some());
        Ok(())
    }
    #[test]
    fn all_configuration_pages_render_in_both_themes_without_a_display() -> Result<()> {
        let fixture = Fixture::new()?;
        let dir = fixture.path.clone();
        {
            let mut app = App::load(dir.clone());
            let ctx = egui::Context::default();
            for dark in [true, false] {
                app.config.dark = dark;
                app.theme(&ctx);
                for page in [
                    Page::Home,
                    Page::Devices,
                    Page::Displays,
                    Page::Settings,
                    Page::GestureLab,
                    Page::GestureRules,
                ] {
                    let output = ctx.run_ui(
                        egui::RawInput {
                            screen_rect: Some(egui::Rect::from_min_size(
                                egui::Pos2::ZERO,
                                Vec2::new(1000.0, 720.0),
                            )),
                            ..Default::default()
                        },
                        |root| {
                            let ctx = root.ctx().clone();
                            egui::CentralPanel::default().show(root, |ui| match page {
                                Page::Home => app.home(ui),
                                Page::Devices => app.devices(ui),
                                Page::Displays => app.displays(ui),
                                Page::Settings => app.settings(ui, &ctx),
                                Page::GestureLab => {
                                    app.gesture_lab.ui(ui, false);
                                }
                                Page::GestureRules => app.gesture_rules.ui(ui, &app.dir, true),
                                _ => (),
                            });
                        },
                    );
                    assert!(!output.shapes.is_empty());
                    output.drop_without_applying_deltas();
                }
            }
        }
        fixture.close()?;
        Ok(())
    }
    #[test]
    fn discovery_selection_changes_only_address_never_peer_authority() -> Result<()> {
        let fixture = Fixture::new()?;
        let mut app = App::load(fixture.path.clone());
        app.use_discovery_address("192.168.1.2:24870".parse()?)?;
        assert!(!app.config.listen && !app.config.allow && app.config.fingerprint.is_none());
        app.config.fingerprint = Some("existing pinned peer".into());
        app.config.allow = true;
        app.use_discovery_address("192.168.1.3:24870".parse()?)?;
        assert_eq!(
            app.config.fingerprint.as_deref(),
            Some("existing pinned peer")
        );
        assert!(app.config.allow);
        Ok(())
    }
}

#[cfg(test)]
mod lifecycle_tests {
    fn set_state(app: &mut App, state: WorkerState) {
        app.worker_state
            .get_or_insert_with(|| Arc::new(WorkerSnapshot::default()))
            .publish(state);
    }
    #[test]
    fn coalescing_keeps_latest_state_and_completed_probe() {
        let snapshot = WorkerSnapshot::default();
        snapshot.publish(WorkerState::Authenticated);
        for _ in 0..10000 {
            snapshot.publish(WorkerState::Listening);
            snapshot.publish(WorkerState::Reconnecting);
        }
        snapshot.publish(WorkerState::Disconnected);
        assert_eq!(snapshot.take(), (Some(WorkerState::Disconnected), false));
        snapshot.publish(WorkerState::ProbePassed);
        snapshot.publish(WorkerState::Stopped);
        assert_eq!(snapshot.take(), (Some(WorkerState::Stopped), true));
        assert_eq!(snapshot.take(), (None, false));
    }

    use super::*;
    #[test]
    fn revoke_clears_grants_and_stale_diagnostics_cannot_authenticate() -> Result<()> {
        let fixture = super::tests::Fixture::new()?;
        let mut app = App::load(fixture.path.clone());
        app.config.fingerprint = Some("test".into());
        app.config.allow = true;
        app.revoke()?;
        assert!(app.config.fingerprint.is_none());
        assert!(!app.config.allow);
        let loaded = App::load(fixture.path.clone());
        assert!(loaded.config.fingerprint.is_none());
        assert!(!loaded.config.allow);
        drop(loaded);
        let (tx, rx) = mpsc::sync_channel(128);
        app.events = Some(rx);
        tx.send("Authenticated peer connected".into())?;
        app.poll();
        assert!(!app.connected);
        drop(app);
        fixture.close()?;
        Ok(())
    }
    #[test]
    fn revocation_persistence_failure_is_reported() -> Result<()> {
        let fixture = super::tests::Fixture::new()?;
        let mut app = App::load(fixture.path.clone());
        app.config.fingerprint = Some("test".into());
        app.config.allow = true;
        std::fs::create_dir(fixture.path.join("settings.json"))?;
        assert!(app
            .revoke()
            .unwrap_err()
            .to_string()
            .contains("persist revoked"));
        assert!(!app.config.allow);
        assert!(app.config.fingerprint.is_none());
        drop(app);
        fixture.close()?;
        Ok(())
    }
    #[test]
    fn managed_test_child() -> Result<()> {
        if std::env::var_os("PEERGLIDE_GUI_LIFECYCLE_HELPER").is_some() {
            let mut command = String::new();
            std::io::stdin().read_line(&mut command)?;
        }
        Ok(())
    }
    #[test]
    fn retrying_child_cannot_remain_connected_or_override_stop() -> Result<()> {
        let fixture = super::tests::Fixture::new()?;
        let mut app = App::load(fixture.path.clone());
        app.child = Some(
            Command::new(std::env::current_exe()?)
                .args([
                    "--exact",
                    "lifecycle_tests::managed_test_child",
                    "--nocapture",
                ])
                .env("PEERGLIDE_GUI_LIFECYCLE_HELPER", "1")
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()?,
        );
        let (tx, rx) = mpsc::sync_channel(1);
        tx.send("queued diagnostic".into())?;
        assert!(tx.try_send("dropped diagnostic".into()).is_err());
        app.events = Some(rx);
        app.connected = true;
        set_state(&mut app, WorkerState::Authenticated);
        set_state(&mut app, WorkerState::Disconnected);
        set_state(&mut app, WorkerState::Reconnecting);
        set_state(&mut app, WorkerState::Listening);
        app.poll();
        assert!(!app.connected);
        assert_eq!(app.status, "Waiting for peer");
        set_state(&mut app, WorkerState::Reconnecting);
        app.poll();
        assert_eq!(app.status, "Reconnecting trusted peer");
        app.stop();
        set_state(&mut app, WorkerState::Authenticated);
        app.poll();
        assert!(!app.connected);
        drop(app);
        fixture.close()?;
        Ok(())
    }
    #[test]
    fn old_configuration_never_enables_reconnect_implicitly() -> Result<()> {
        let config: Config = serde_json::from_str(r#"{"allow":true,"native":true}"#)?;
        assert!(!config.reconnect);
        Ok(())
    }
    #[test]
    fn delayed_worker_output_cannot_report_a_dead_process_as_connected() -> Result<()> {
        let fixture = super::tests::Fixture::new()?;
        let dir = fixture.path.clone();
        {
            let mut app = App::load(dir.clone());
            set_state(&mut app, WorkerState::Authenticated);
            app.poll();
            assert!(!app.connected);
            assert_eq!(app.status, "Offline");
        }
        fixture.close()?;
        Ok(())
    }
}
