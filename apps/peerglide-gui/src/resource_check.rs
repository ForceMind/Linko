//! Explicit CI-only loopback communication. Never enables input or user grants.
use super::{enrollment_store, App, Config, Identity, SessionMode, WorkerInputState};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{BufRead, Read, Write},
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU8, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Spec {
    schema_version: u16,
    address: SocketAddr,
    peer_certificate: PathBuf,
    report: PathBuf,
    seconds: u64,
    #[serde(default)]
    tray_hidden: bool,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Prepared,
    Connecting,
    Hiding,
    Sampling,
    Restoring,
    Stopping,
    FailedStopping,
    Done,
}
#[derive(Serialize)]
struct Record {
    schema_version: u16,
    scope: &'static str,
    state: &'static str,
    worker_pid: Option<u32>,
    worker_reaped: bool,
    elapsed_sec: f64,
    native: bool,
    window_state: &'static str,
    tray_show_received: bool,
    tray_stop_received: bool,
}
pub(super) struct Check {
    file: File,
    peer: Vec<u8>,
    local: Vec<u8>,
    address: SocketAddr,
    seconds: Duration,
    started: Instant,
    connected: Option<Instant>,
    worker: Option<u32>,
    control: Arc<AtomicU8>,
    phase: Phase,
    tray_hidden: bool,
    transition: Option<Instant>,
    window_state: &'static str,
    tray_show_received: bool,
    tray_stop_received: bool,
}
fn regular(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    anyhow::ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink(),
        "检查材料必须是普通文件"
    );
    Ok(())
}
impl Check {
    pub(super) fn prepare(dir: &Path, spec_path: &Path, ci: bool) -> Result<Self> {
        anyhow::ensure!(ci, "隔离资源检查仅供CI使用");
        regular(spec_path)?;
        let mut bytes = Vec::new();
        File::open(spec_path)?.take(8193).read_to_end(&mut bytes)?;
        anyhow::ensure!(bytes.len() <= 8192, "资源检查说明过大");
        let spec: Spec = serde_json::from_slice(&bytes).context("资源检查说明无效")?;
        anyhow::ensure!(
            !spec.tray_hidden || cfg!(target_os = "macos"),
            "真实菜单栏隐藏检查仅支持macOS"
        );
        anyhow::ensure!(
            spec.schema_version == 1
                && spec.address.is_ipv4()
                && spec.address.ip().is_loopback()
                && spec.address.port() != 0
                && (17..=600).contains(&spec.seconds),
            "资源检查只接受有界回环通信"
        );
        let metadata = fs::symlink_metadata(dir)?;
        anyhow::ensure!(
            metadata.is_dir() && !metadata.file_type().is_symlink(),
            "资源目录必须独立"
        );
        let entries: Vec<_> = fs::read_dir(dir)?.collect::<std::io::Result<Vec<_>>>()?;
        anyhow::ensure!(
            entries.len() == 1 && entries[0].file_name() == "identity",
            "资源检查不覆盖已有设置或用户文件"
        );
        let identity_dir = dir.join("identity");
        let identity_meta = fs::symlink_metadata(&identity_dir)?;
        anyhow::ensure!(
            identity_meta.is_dir() && !identity_meta.file_type().is_symlink(),
            "身份目录必须独立"
        );
        let identities: Vec<_> =
            fs::read_dir(&identity_dir)?.collect::<std::io::Result<Vec<_>>>()?;
        anyhow::ensure!(identities.len() == 2, "检查身份目录含额外文件");
        for name in ["node.crt.der", "node.key.der"] {
            regular(&identity_dir.join(name))?;
        }
        let local = Identity::load(&identity_dir)?.certificate;
        regular(&spec.peer_certificate)?;
        let peer = peerglide_transport::read_certificate(&spec.peer_certificate)?;
        anyhow::ensure!(peer != local, "检查对端必须是独立身份");
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(&spec.report).context("资源报告必须是新文件")?;
        Ok(Self {
            file,
            peer,
            local,
            address: spec.address,
            seconds: Duration::from_secs(spec.seconds),
            started: Instant::now(),
            connected: None,
            worker: None,
            control: Arc::new(AtomicU8::new(0)),
            phase: Phase::Prepared,
            tray_hidden: spec.tray_hidden,
            transition: None,
            window_state: "normal_window_requested",
            tray_show_received: false,
            tray_stop_received: false,
        })
    }
    pub(super) fn setup(&mut self, app: &mut App) -> Result<()> {
        anyhow::ensure!(
            app.instance_lock.is_some()
                && app.config_store.is_some()
                && app.config == Config::default()
                && !app.dir.join("settings.json").exists()
                && !app.dir.join("trusted-peer.crt.der").exists(),
            "检查只接受全新设置"
        );
        anyhow::ensure!(
            Identity::load(&app.identity_dir())?.certificate == self.local,
            "检查身份已变化"
        );
        let next = Config {
            fingerprint: Some(peerglide_transport::fingerprint(&self.peer)),
            peer_name: "隔离通信对端".into(),
            address: self.address.to_string(),
            listen: false,
            allow: false,
            native: false,
            reconnect: false,
            ..Config::default()
        };
        enrollment_store::commit(app.config_store.as_mut().unwrap(), &next, Some(&self.peer))?;
        app.config = next;
        app.resource_check_mode = true;
        if self.tray_hidden {
            // This fresh diagnostic's in-memory choice only. Never save it to
            // user preferences or assume the real status item was created.
            app.desktop_preferences.tray_enabled = true;
            app.desktop_preferences.close_to_tray = true;
        }
        app.notice = "CI隔离资源检查 · 仅回环通信，不开启键鼠".into();
        self.record("prepared", false)
    }
    pub(super) fn listen_stdin(&self, wake: Arc<dyn Fn() + Send + Sync>) {
        let control = self.control.clone();
        std::thread::spawn(move || {
            let mut line = Vec::new();
            let valid = std::io::stdin()
                .lock()
                .take(65)
                .read_until(b'\n', &mut line)
                .is_ok()
                && line == b"stop\n";
            control.store(if valid { 1 } else { 2 }, Ordering::Release);
            wake();
        });
    }
    fn record(&mut self, state: &'static str, worker_reaped: bool) -> Result<()> {
        let line = serde_json::to_vec(&Record {
            schema_version: 2,
            scope: "ci_owned_gui_communication_resources",
            state,
            worker_pid: self.worker,
            worker_reaped,
            elapsed_sec: self.started.elapsed().as_secs_f64(),
            native: false,
            window_state: self.window_state,
            tray_show_received: self.tray_show_received,
            tray_stop_received: self.tray_stop_received,
        })?;
        self.file.write_all(&line)?;
        self.file.write_all(b"\n")?;
        self.file.flush()?;
        Ok(())
    }
    fn fail(&mut self, app: &mut App) {
        let _ = self.record("failed", app.child.is_none());
        self.phase = Phase::FailedStopping;
        app.close_requested = true;
        app.stop();
    }
    pub(super) fn observe_tray(&mut self, pending: peerglide_platform::tray::Pending) {
        self.tray_show_received |= pending.show;
        self.tray_stop_received |= pending.stop;
    }
    fn visible(&self, app: &App) -> Result<bool> {
        #[cfg(target_os = "macos")]
        {
            app.application_menu
                .as_ref()
                .context("缺少退出保护")?
                .window_visible_for_check("Linko / 屏跃")
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = app;
            anyhow::bail!("平台未实现本次窗口核验")
        }
    }
    fn request_close(&self, app: &App) -> Result<()> {
        #[cfg(target_os = "macos")]
        {
            app.application_menu
                .as_ref()
                .context("缺少退出保护")?
                .request_window_close_for_check("Linko / 屏跃")
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = app;
            anyhow::bail!("平台未实现本次窗口核验")
        }
    }
    fn request_tray(&self, app: &App, command: peerglide_platform::tray::Command) -> Result<()> {
        #[cfg(target_os = "macos")]
        {
            app.tray
                .as_ref()
                .context("缺少菜单栏恢复入口")?
                .request_command_for_check(command)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (app, command);
            anyhow::bail!("平台未实现本次菜单栏核验")
        }
    }
    fn begin_sampling(&mut self) -> Result<()> {
        self.connected = Some(Instant::now());
        self.phase = Phase::Sampling;
        self.record("connected", false)
    }
    pub(super) fn tick(&mut self, app: &mut App) {
        if self.phase == Phase::Done {
            return;
        }
        if self.phase == Phase::FailedStopping {
            if app.child.is_none() {
                self.phase = Phase::Done;
            }
            return;
        }
        if self.control.load(Ordering::Acquire) == 2 {
            self.fail(app);
            return;
        }
        if !app.resource_check_mode
            || app.config.native
            || app.config.allow
            || app.config.reconnect
            || app.config.listen
        {
            self.fail(app);
            return;
        }
        if matches!(
            self.phase,
            Phase::Hiding | Phase::Sampling | Phase::Restoring
        ) && (app.close_requested
            || !app.connected
            || app.child.as_ref().map(|c| c.id()) != self.worker
            || app.active_mode != Some(SessionMode::Communication)
            || app.input_state != Some(WorkerInputState::Disabled)
            || app.input_terminal)
        {
            self.fail(app);
            return;
        }
        if self.tray_hidden && !app.tray.as_ref().is_some_and(|tray| tray.is_available()) {
            self.fail(app);
            return;
        }
        if self.phase == Phase::Prepared {
            if app.start_mode(SessionMode::Communication).is_err() {
                self.fail(app);
                return;
            }
            self.worker = app.child.as_ref().map(|child| child.id());
            self.phase = Phase::Connecting;
        }
        if self.phase == Phase::Connecting {
            if app.close_requested
                || self.started.elapsed() > Duration::from_secs(20)
                || app.child.is_none()
            {
                self.fail(app);
                return;
            }
            if app.connected
                && app.active_mode == Some(SessionMode::Communication)
                && app.input_state == Some(WorkerInputState::Disabled)
                && !app.input_terminal
            {
                if self.tray_hidden {
                    if !matches!(self.visible(app), Ok(true)) {
                        // Window construction can finish after the first logic
                        // callback; the same 20-second connection cap applies.
                        return;
                    }
                    self.window_state = "appkit_visible";
                    if self.record("visible", false).is_err() || self.request_close(app).is_err() {
                        self.fail(app);
                        return;
                    }
                    self.phase = Phase::Hiding;
                    self.transition = Some(Instant::now());
                } else if self.begin_sampling().is_err() {
                    self.fail(app);
                    return;
                }
            }
        }
        if matches!(self.phase, Phase::Hiding | Phase::Restoring) {
            if self
                .transition
                .is_none_or(|at| at.elapsed() > Duration::from_secs(5))
            {
                self.fail(app);
                return;
            }
            if self.phase == Phase::Hiding {
                if app.window_hidden && matches!(self.visible(app), Ok(false)) {
                    self.window_state = "appkit_hidden";
                    if self.begin_sampling().is_err() {
                        self.fail(app);
                        return;
                    }
                }
            } else if !app.window_hidden
                && self.tray_show_received
                && matches!(self.visible(app), Ok(true))
            {
                self.window_state = "appkit_visible";
                if self.record("shown", false).is_err()
                    || self
                        .request_tray(app, peerglide_platform::tray::Command::Stop)
                        .is_err()
                {
                    self.fail(app);
                    return;
                }
                self.phase = Phase::Stopping;
                if self.record("stopping", false).is_err() {
                    self.fail(app);
                    return;
                }
                app.close_requested = true;
            }
        }
        if self.phase == Phase::Sampling {
            if self.tray_hidden && (!app.window_hidden || !matches!(self.visible(app), Ok(false))) {
                self.fail(app);
                return;
            }
            if app.close_requested
                || !app.connected
                || app.child.as_ref().map(|child| child.id()) != self.worker
                || app.active_mode != Some(SessionMode::Communication)
                || app.input_state != Some(WorkerInputState::Disabled)
            {
                self.fail(app);
                return;
            }
            let elapsed = self.connected.unwrap().elapsed();
            let control = self.control.load(Ordering::Acquire);
            if control == 2 || elapsed > self.seconds + Duration::from_secs(15) {
                self.fail(app);
                return;
            }
            if control == 1 {
                if elapsed < self.seconds {
                    self.fail(app);
                    return;
                }
                if self.tray_hidden {
                    if self
                        .request_tray(app, peerglide_platform::tray::Command::Show)
                        .is_err()
                    {
                        self.fail(app);
                        return;
                    }
                    self.phase = Phase::Restoring;
                    self.transition = Some(Instant::now());
                } else {
                    self.phase = Phase::Stopping;
                    if self.record("stopping", false).is_err() {
                        self.fail(app);
                        return;
                    }
                    app.close_requested = true;
                    app.stop();
                }
            }
        }
        if self.phase == Phase::Stopping && app.child.is_none() {
            let clean = !app.recovery_blocked
                && (!self.tray_hidden
                    || (self.tray_show_received
                        && self.tray_stop_received
                        && self.window_state == "appkit_visible"))
                && app.stopping.is_none()
                && app.events.is_none()
                && app.input_terminal
                && app.input_state == Some(WorkerInputState::Disabled);
            if !clean || self.record("completed", true).is_err() {
                self.fail(app);
                return;
            }
            self.phase = Phase::Done;
        }
    }
    pub(super) fn finished(&self) -> bool {
        self.phase == Phase::Done
    }
    pub(super) fn next_tick(&self) -> Option<Duration> {
        if self.tray_hidden && self.phase == Phase::Connecting {
            return Some(Duration::from_millis(50));
        }
        let remaining = match self.phase {
            Phase::Prepared | Phase::Connecting => {
                Duration::from_secs(20).saturating_sub(self.started.elapsed())
            }
            Phase::Sampling => {
                (self.seconds + Duration::from_secs(15)).saturating_sub(self.connected?.elapsed())
            }
            Phase::Hiding | Phase::Restoring => return Some(Duration::from_millis(50)),
            _ => return None,
        };
        Some(remaining.max(Duration::from_millis(1)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(root: &Path) -> Result<(PathBuf, PathBuf, PathBuf)> {
        let dir = root.join("profile");
        fs::create_dir(&dir)?;
        Identity::create(&dir.join("identity"))?;
        let peer = root.join("peer.der");
        fs::write(&peer, Identity::generate()?.certificate)?;
        let report = root.join("new-report.jsonl");
        let spec = root.join("spec.json");
        fs::write(
            &spec,
            serde_json::to_vec(
                &serde_json::json!({"schema_version":1,"address":"127.0.0.1:24870",
            "peer_certificate":peer,"report":report,"seconds":17}),
            )?,
        )?;
        Ok((dir, spec, report))
    }
    #[test]
    fn resource_deadlines_do_not_require_a_periodic_paint_clock() -> Result<()> {
        let fixture = crate::tests::Fixture::new()?;
        let (dir, spec, _) = self::fixture(&fixture.path)?;
        let mut check = Check::prepare(&dir, &spec, true)?;
        assert!(check.next_tick().is_some_and(
            |delay| delay > Duration::from_secs(10) && delay <= Duration::from_secs(20)
        ));
        check.phase = Phase::Sampling;
        check.connected = Some(Instant::now());
        assert!(check.next_tick().is_some_and(
            |delay| delay > Duration::from_secs(20) && delay <= Duration::from_secs(32)
        ));
        check.connected = Some(Instant::now() - Duration::from_secs(33));
        assert_eq!(check.next_tick(), Some(Duration::from_millis(1)));
        check.phase = Phase::Stopping;
        assert_eq!(check.next_tick(), None);
        drop(check);
        fixture.close()?;
        Ok(())
    }
    #[test]
    fn resource_setup_is_fresh_loopback_and_never_opens_native_or_mutates_existing_data(
    ) -> Result<()> {
        let fixture = crate::tests::Fixture::new()?;
        let (dir, spec, report) = self::fixture(&fixture.path)?;
        assert!(Check::prepare(&dir, &spec, false).is_err() && !report.exists());
        let key = fs::read(dir.join("identity/node.key.der"))?;
        let mut check = Check::prepare(&dir, &spec, true)?;
        let mut app = App::load(dir.clone());
        check.setup(&mut app)?;
        let receipt: serde_json::Value = serde_json::from_slice(&fs::read(&report)?)?;
        assert_eq!(receipt["schema_version"], 2);
        assert_eq!(receipt["window_state"], "normal_window_requested");
        assert_eq!(receipt["tray_show_received"], false);
        assert_eq!(receipt["tray_stop_received"], false);
        let settings = fs::read(dir.join("settings.json"))?;
        assert!(
            app.resource_check_mode
                && !app.config.native
                && !app.config.allow
                && !app.config.reconnect
        );
        assert!(
            app.start_mode(SessionMode::Native).is_err()
                && app.start_mode(SessionMode::Probe).is_err()
        );
        app.native_confirmation = true;
        assert!(app.request_native().is_err() && !app.native_confirmation);
        assert!(
            app.save_config().is_err() && app.revoke().is_err() && app.reset_settings().is_err()
        );
        assert!(app.child.is_none() && fs::read(dir.join("settings.json"))? == settings);
        assert!(fs::read(dir.join("identity/node.key.der"))? == key);
        drop(app);
        assert!(Check::prepare(&dir, &spec, true).is_err());
        Ok(())
    }
    #[test]
    fn extra_or_future_profile_and_existing_report_are_preserved() -> Result<()> {
        let fixture = crate::tests::Fixture::new()?;
        let (dir, spec, report) = self::fixture(&fixture.path)?;
        fs::write(&report, b"user report")?;
        assert!(Check::prepare(&dir, &spec, true).is_err());
        assert!(fs::read(&report)? == b"user report");
        fs::write(dir.join("settings.json"), b"future settings")?;
        assert!(Check::prepare(&dir, &spec, true).is_err());
        assert!(fs::read(dir.join("settings.json"))? == b"future settings");
        Ok(())
    }
    #[test]
    fn remote_address_and_unbounded_window_are_rejected_before_reserving_report() -> Result<()> {
        let fixture = crate::tests::Fixture::new()?;
        let (dir, spec, report) = self::fixture(&fixture.path)?;
        let original: serde_json::Value = serde_json::from_slice(&fs::read(&spec)?)?;
        if !cfg!(target_os = "macos") {
            let mut hidden = original.clone();
            hidden["tray_hidden"] = serde_json::json!(true);
            fs::write(&spec, serde_json::to_vec(&hidden)?)?;
            assert!(Check::prepare(&dir, &spec, true).is_err() && !report.exists());
        }
        for (field, value) in [
            ("address", serde_json::json!("192.168.1.2:24870")),
            ("address", serde_json::json!("127.0.0.1:0")),
            ("seconds", serde_json::json!(601)),
            ("native", serde_json::json!(true)),
            ("schema_version", serde_json::json!(2)),
        ] {
            let mut changed = original.clone();
            changed[field] = value;
            fs::write(&spec, serde_json::to_vec(&changed)?)?;
            assert!(Check::prepare(&dir, &spec, true).is_err() && !report.exists());
        }
        Ok(())
    }
}
