//! Read-only observations from the actual sibling worker, never input authority.
use eframe::egui;
use peerglide_platform::context::{PermissionReport, PermissionStatus, SecureInput};
use serde::Deserialize;
use std::{
    io::Read,
    path::Path,
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    time::{Duration, Instant},
};

const MAX_REPORT: usize = 8192;
const FRESHNESS: Duration = Duration::from_secs(30);
type Outcome = Result<PermissionReport, &'static str>;

/// Short-lived presentation only; not consent or proof that injection works.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Readiness {
    Unchecked,
    Checking,
    Ready,
    Accessibility,
    InputMonitoring,
    InputDesktop,
    SecureInput,
    Unknown,
    Unsupported,
    Failed,
}
impl Readiness {
    pub(super) fn detail(self) -> &'static str {
        match self {
            Self::Unchecked => "先检查实际输入程序的本机权限；不会联网、捕获输入或更改系统设置。",
            Self::Checking => "正在读取实际输入程序的权限，检查不会开启键鼠。",
            Self::Ready => "本机权限刚刚检查通过；仍需明确确认，且另一台电脑必须独立允许接收。",
            Self::Accessibility => "请在macOS系统设置的辅助功能中允许本次运行的屏跃，再返回重新检查。",
            Self::InputMonitoring => "请在macOS系统设置的输入监控中允许本次运行的屏跃，再返回重新检查。",
            Self::InputDesktop => "当前输入桌面不可访问。请返回普通桌面再检查；屏跃不会绕过UAC或安全桌面。",
            Self::SecureInput => "系统正在保护输入。请结束当前受保护的输入后重新检查，屏跃不会关闭Secure Input。",
            Self::Unknown => "实际输入程序未能确认权限，请重新检查；未知状态不能视为已允许。",
            Self::Unsupported => "当前环境未提供所需的输入权限检查，暂不能从这个窗口开启键鼠。",
            Self::Failed => "实际输入程序的权限检查未完成，请重新检查；可在设置查看原因。",
        }
    }
}
fn readiness(report: PermissionReport) -> Readiness {
    if report.secure_input == SecureInput::Active {
        return Readiness::SecureInput;
    }
    for (status, denied) in [
        (report.accessibility, Readiness::Accessibility),
        (report.input_monitoring, Readiness::InputMonitoring),
        (report.input_desktop, Readiness::InputDesktop),
    ] {
        match status {
            PermissionStatus::Denied => return denied,
            PermissionStatus::Unknown => return Readiness::Unknown,
            PermissionStatus::Unsupported => return Readiness::Unsupported,
            PermissionStatus::Granted | PermissionStatus::NotRequired => (),
        }
    }
    if report.secure_input == SecureInput::Unknown {
        Readiness::Unknown
    } else {
        Readiness::Ready
    }
}
#[derive(Clone, Copy)]
struct Observation {
    outcome: Outcome,
    finished: Instant,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum WirePermission {
    Granted,
    Denied,
    NotRequired,
    Unknown,
    Unsupported,
}
impl From<WirePermission> for PermissionStatus {
    fn from(value: WirePermission) -> Self {
        match value {
            WirePermission::Granted => Self::Granted,
            WirePermission::Denied => Self::Denied,
            WirePermission::NotRequired => Self::NotRequired,
            WirePermission::Unknown => Self::Unknown,
            WirePermission::Unsupported => Self::Unsupported,
        }
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum WireSecure {
    Active,
    Inactive,
    Unknown,
    Unsupported,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Permissions {
    accessibility: WirePermission,
    input_monitoring: WirePermission,
    input_desktop: WirePermission,
    secure_input: WireSecure,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Report {
    schema_version: u16,
    platform: String,
    native_capture_started: bool,
    action_execution_supported: bool,
    observation_scope: String,
    permissions: Permissions,
    #[serde(rename = "foreground_status")]
    _foreground_status: String,
    #[serde(rename = "focus_status")]
    _focus_status: String,
    #[serde(rename = "recheck_status")]
    _recheck_status: String,
}
fn parse(bytes: &[u8]) -> Outcome {
    if bytes.len() > MAX_REPORT {
        return Err("工作进程诊断超过大小限制");
    }
    let report: Report = serde_json::from_slice(bytes).map_err(|_| "工作进程诊断格式无效")?;
    if report.schema_version != 1
        || report.platform != std::env::consts::OS
        || report.native_capture_started
        || report.action_execution_supported
        || report.observation_scope != "sampled_no_notifications"
    {
        return Err("工作进程诊断版本或模式不匹配");
    }
    Ok(PermissionReport {
        accessibility: report.permissions.accessibility.into(),
        input_monitoring: report.permissions.input_monitoring.into(),
        input_desktop: report.permissions.input_desktop.into(),
        secure_input: match report.permissions.secure_input {
            WireSecure::Active => SecureInput::Active,
            WireSecure::Inactive => SecureInput::Inactive,
            WireSecure::Unknown => SecureInput::Unknown,
            WireSecure::Unsupported => SecureInput::Unsupported,
        },
    })
}
fn diagnostic_command(path: &Path) -> Command {
    let mut command = Command::new(path);
    command
        .arg("diagnose-context")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    command
}
struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}
fn run(path: &Path, cancel: &AtomicBool) -> Outcome {
    run_with_timeout(path, cancel, Duration::from_secs(3))
}
fn run_with_timeout(path: &Path, cancel: &AtomicBool, timeout: Duration) -> Outcome {
    if cancel.load(Ordering::Acquire) {
        return Err("工作进程诊断已取消");
    }
    let mut child = OwnedChild(
        diagnostic_command(path)
            .spawn()
            .map_err(|_| "无法启动同包工作进程诊断")?,
    );
    let stdout = child.0.stdout.take().ok_or("无法读取工作进程诊断")?;
    let (tx, rx) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = stdout
            .take(MAX_REPORT as u64 + 1)
            .read_to_end(&mut bytes)
            .map(|_| bytes)
            .map_err(|_| "无法读取工作进程诊断");
        let _ = tx.send(result);
    });
    let deadline = Instant::now() + timeout;
    let mut bytes = None;
    loop {
        if cancel.load(Ordering::Acquire) {
            return Err("工作进程诊断已取消");
        }
        if Instant::now() >= deadline {
            return Err("工作进程诊断超时");
        }
        if let Ok(result) = rx.try_recv() {
            let value = result?;
            if value.len() > MAX_REPORT {
                return Err("工作进程诊断超过大小限制");
            }
            bytes = Some(value);
        }
        if let Some(status) = child.0.try_wait().map_err(|_| "无法确认工作进程诊断退出")?
        {
            if !status.success() {
                return Err("工作进程诊断失败");
            }
            if let Some(bytes) = &bytes {
                return parse(bytes);
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[derive(Default)]
pub struct Checker {
    receiver: Option<mpsc::Receiver<Observation>>,
    cancel: Option<Arc<AtomicBool>>,
    task: Option<std::thread::JoinHandle<()>>,
    result: Option<Observation>,
}
impl Checker {
    pub(super) fn readiness(&self) -> Readiness {
        self.readiness_at(Instant::now())
    }
    fn readiness_at(&self, now: Instant) -> Readiness {
        if self.receiver.is_some() {
            return Readiness::Checking;
        }
        let Some(observed) = self.result else {
            return Readiness::Unchecked;
        };
        if !now.checked_duration_since(observed.finished).is_some_and(|age| age < FRESHNESS) {
            return Readiness::Unchecked;
        }
        match observed.outcome {
            Ok(report) => readiness(report),
            Err(_) => Readiness::Failed,
        }
    }
    pub(super) fn expires_in(&self) -> Option<Duration> {
        let age = Instant::now().checked_duration_since(self.result?.finished)?;
        FRESHNESS.checked_sub(age).filter(|remaining| !remaining.is_zero())
    }
    pub fn needs_poll(&self) -> bool {
        self.receiver.is_some()
    }
    pub fn poll(&mut self) {
        let Some(receiver) = &self.receiver else {
            return;
        };
        let result = match receiver.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => Observation {
                outcome: Err("工作进程诊断未完成"),
                finished: Instant::now(),
            },
        };
        self.result = Some(result);
        self.receiver = None;
        self.cancel = None;
        if let Some(task) = self.task.take() {
            let _ = task.join();
        }
    }
    pub(super) fn start(&mut self) {
        if self.receiver.is_some() {
            return;
        }
        self.result = None;
        let Ok(path) = std::env::current_exe() else {
            self.result = Some(Observation { outcome: Err("无法定位同包工作进程"), finished: Instant::now() });
            return;
        };
        let path = path.with_file_name(if cfg!(windows) {
            "peerglide.exe"
        } else {
            "peerglide"
        });
        self.start_path(path);
    }
    fn start_path(&mut self, path: std::path::PathBuf) {
        let cancel = Arc::new(AtomicBool::new(false));
        let task_cancel = cancel.clone();
        let (tx, rx) = mpsc::sync_channel(1);
        self.task = Some(std::thread::spawn(move || {
            let outcome = run(&path, &task_cancel);
            // Stamp before queueing: a paused GUI must not refresh an old report.
            let _ = tx.send(Observation { outcome, finished: Instant::now() });
        }));
        self.receiver = Some(rx);
        self.cancel = Some(cancel);
    }
    pub fn ui(&mut self, ui: &mut egui::Ui, idle: bool) {
        ui.add_space(12.0);
        ui.strong("键鼠权限");
        ui.weak("由实际输入程序检查，不联网、不捕获输入，也不申请权限。这里显示上次检查结果，开启键鼠时会再次确认。");
        if ui
            .add_enabled(
                idle && self.receiver.is_none(),
                egui::Button::new("检查键鼠权限"),
            )
            .clicked()
        {
            self.start();
        }
        if self.receiver.is_some() {
            ui.label("正在检查键鼠权限…");
        }
        match self.result.map(|observation| observation.outcome) {
            Some(Ok(report)) => {
                let label = |value| match value {
                    PermissionStatus::Granted => "已授予",
                    PermissionStatus::Denied => "未授予",
                    PermissionStatus::NotRequired => "无需同名权限",
                    PermissionStatus::Unknown => "未知",
                    PermissionStatus::Unsupported => "尚不支持",
                };
                ui.label(format!(
                    "上次检查 · 辅助功能：{} · 输入监控：{} · 输入桌面：{}",
                    label(report.accessibility),
                    label(report.input_monitoring),
                    label(report.input_desktop)
                ));
                ui.label(match report.secure_input {
                    SecureInput::Active => "Secure Input正在保护输入，屏跃不会绕过。",
                    SecureInput::Inactive => "工作进程未检测到Secure Input。",
                    SecureInput::Unknown => "工作进程的Secure Input状态未知。",
                    SecureInput::Unsupported => "此平台无同名Secure Input检查。",
                });
            }
            Some(Err(error)) => {
                ui.label(error);
            }
            None if self.receiver.is_none() => {
                ui.weak("点击检查，再按系统提示处理。界面自身的许可不能代替实际输入程序的许可。");
            }
            None => (),
        }
        if self.result.is_some() && self.readiness() == Readiness::Unchecked {
            ui.weak("上次结果已过期，开启前请重新检查。检查通过也不代表对端许可或实际输入效果。");
        }
    }
}
impl Drop for Checker {
    fn drop(&mut self) {
        if let Some(cancel) = &self.cancel {
            cancel.store(true, Ordering::Release);
        }
        // The application must not exit before the diagnostic child is reaped.
        if let Some(task) = self.task.take() {
            let _ = task.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn report() -> serde_json::Value {
        serde_json::json!({"schema_version":1,"platform":std::env::consts::OS,"native_capture_started":false,"action_execution_supported":false,"observation_scope":"sampled_no_notifications","permissions":{"accessibility":"granted","input_monitoring":"denied","input_desktop":"not_required","secure_input":"inactive"},"foreground_status":"available","focus_status":"permission_denied","recheck_status":"permission_denied"})
    }
    fn ready() -> PermissionReport {
        PermissionReport { accessibility: PermissionStatus::Granted,
            input_monitoring: PermissionStatus::Granted, input_desktop: PermissionStatus::NotRequired,
            secure_input: SecureInput::Inactive }
    }
    #[test]
    fn readiness_preserves_denials_unknown_and_system_protection() {
        let original = ready();
        assert_eq!(readiness(original), Readiness::Ready);
        for (field, expected) in [(0, Readiness::Accessibility), (1, Readiness::InputMonitoring), (2, Readiness::InputDesktop)] {
            for (status, expected) in [(PermissionStatus::Denied, expected), (PermissionStatus::Unknown, Readiness::Unknown), (PermissionStatus::Unsupported, Readiness::Unsupported)] {
                let mut report = original;
                match field { 0 => report.accessibility = status, 1 => report.input_monitoring = status, _ => report.input_desktop = status }
                assert_eq!(readiness(report), expected);
            }
        }
        for (secure_input, expected) in [(SecureInput::Active, Readiness::SecureInput), (SecureInput::Unknown, Readiness::Unknown), (SecureInput::Unsupported, Readiness::Ready)] {
            assert_eq!(readiness(PermissionReport { secure_input, ..original }), expected);
        }
    }
    #[test]
    fn delayed_gui_poll_does_not_refresh_old_observation_or_persist_authority() {
        let finished = Instant::now() - FRESHNESS;
        let (tx, rx) = mpsc::sync_channel(1);
        // Synthetic permission metadata only; no native worker or grant exists.
        assert!(tx.send(Observation { outcome: Ok(ready()), finished }).is_ok());
        let mut checker = Checker { receiver: Some(rx), ..Default::default() };
        assert_eq!(checker.readiness(), Readiness::Checking);
        checker.poll();
        assert_eq!(checker.readiness_at(finished + FRESHNESS - Duration::from_nanos(1)), Readiness::Ready);
        assert_eq!(checker.readiness_at(finished + FRESHNESS), Readiness::Unchecked);
        assert_eq!(checker.readiness_at(finished - Duration::from_nanos(1)), Readiness::Unchecked);
        assert_eq!(checker.readiness(), Readiness::Unchecked);
        assert!(checker.expires_in().is_none() && !checker.needs_poll());
        checker.result = Some(Observation { outcome: Err("工作进程诊断失败"), finished: Instant::now() });
        assert_eq!(checker.readiness(), Readiness::Failed);
    }
    #[test]
    fn worker_snapshot_preserves_denial_and_does_not_infer_authority() {
        let parsed = parse(&serde_json::to_vec(&report()).unwrap()).unwrap();
        assert_eq!(parsed.accessibility, PermissionStatus::Granted);
        assert_eq!(parsed.input_monitoring, PermissionStatus::Denied);
        assert_eq!(parsed.secure_input, SecureInput::Inactive);
    }
    #[test]
    fn incompatible_or_unbounded_reports_are_rejected_without_echoing_contents() {
        for (key, value) in [
            ("schema_version", serde_json::json!(2)),
            ("platform", serde_json::json!("other")),
            ("native_capture_started", serde_json::json!(true)),
            ("action_execution_supported", serde_json::json!(true)),
            (
                "observation_scope",
                serde_json::json!("sampled_with_observer_attempt"),
            ),
            ("application_id", serde_json::json!("private-app-id")),
        ] {
            let mut altered = report();
            altered[key] = value;
            assert!(parse(&serde_json::to_vec(&altered).unwrap()).is_err());
        }
        let mut altered = report();
        altered["permissions"]["accessibility"] = serde_json::json!("private-unrecognized-value");
        let error = parse(&serde_json::to_vec(&altered).unwrap()).unwrap_err();
        assert!(!error.contains("private"));
        assert!(parse(&vec![b' '; MAX_REPORT + 1]).is_err());
    }
    #[test]
    fn diagnostic_command_never_enables_observation_network_or_input() {
        let command = diagnostic_command(Path::new("worker-fixture"));
        assert_eq!(
            command
                .get_args()
                .map(|arg| arg.to_str().unwrap())
                .collect::<Vec<_>>(),
            ["diagnose-context"]
        );
        assert_eq!(
            run(Path::new("never-started"), &AtomicBool::new(true)).unwrap_err(),
            "工作进程诊断已取消"
        );
    }
    #[cfg(unix)]
    #[test]
    fn real_diagnostic_child_is_bounded_and_reaped_on_timeout() {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let marker = peerglide_transport::Identity::generate()
            .unwrap()
            .fingerprint();
        let directory = std::env::temp_dir().join(format!("peerglide-worker-check-{marker}"));
        std::fs::create_dir(&directory).unwrap();
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(directory.clone());
        let script = directory.join("worker");
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o700)
            .open(&script)
            .unwrap();
        file.write_all(b"#!/bin/sh\nexec /bin/sleep 5\n").unwrap();
        drop(file);
        let started = Instant::now();
        assert_eq!(
            run_with_timeout(&script, &AtomicBool::new(false), Duration::from_millis(60))
                .unwrap_err(),
            "工作进程诊断超时"
        );
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[cfg(unix)]
    #[test]
    fn dropping_checker_waits_for_its_real_diagnostic_child_to_exit() {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let marker = peerglide_transport::Identity::generate()
            .unwrap()
            .fingerprint();
        let directory = std::env::temp_dir().join(format!("peerglide-check-close-{marker}"));
        std::fs::create_dir(&directory).unwrap();
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(directory.clone());
        let script = directory.join("worker");
        let pid_file = directory.join("worker.pid");
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o700)
            .open(&script)
            .unwrap();
        let body = b"#!/bin/sh\nprintf '%s' \"$$\" > \"$0.pid\"\nexec /bin/sleep 5\n";
        file.write_all(body).unwrap();
        drop(file);
        let mut checker = Checker::default();
        checker.start_path(script);
        let deadline = Instant::now() + Duration::from_secs(2);
        let pid = loop {
            match std::fs::read_to_string(&pid_file) {
                Ok(value) if value.trim().parse::<u32>().is_ok() => break value,
                _ => (),
            }
            assert!(Instant::now() < deadline, "diagnostic PID was not ready");
            std::thread::sleep(Duration::from_millis(5));
        };
        drop(checker);
        let exists = Command::new("/bin/kill")
            .args(["-0", pid.trim()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(
            !exists.success(),
            "diagnostic child remained after GUI teardown"
        );
    }
}
