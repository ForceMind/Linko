//! Read-only observations from the actual sibling worker, never input authority.
use super::desktop_ui::{icon_row, safety_point, SafetyPoint};
use super::icons::{self, Icon};
use super::{desktop_style, motion};
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

/// W3 header status once the last observation is older than `FRESHNESS`.
const EXPIRED_SUMMARY: &str =
    "上次结果已过期，开启前请重新检查。检查通过不代表另一台已许可，也不代表实际输入有效。";
/// W3 header: what the check itself never does.
pub(super) const HEADER_NOTE: SafetyPoint = SafetyPoint {
    icon: Icon::Lock,
    text: "检查不联网、不捕获输入，也不申请系统权限。",
};
/// W3 details before the last report: who checks and what the result is.
pub(super) const DETAIL_SOURCE: [SafetyPoint; 3] = [
    SafetyPoint {
        icon: Icon::Device,
        text: "由实际输入程序检查。这里显示上次检查结果。",
    },
    SafetyPoint {
        icon: Icon::Recover,
        text: "开启键鼠时会再次确认。",
    },
    SafetyPoint {
        icon: Icon::Lock,
        text: "界面自身的许可不能代替实际输入程序的许可。",
    },
];
/// W3 details after the last report. The number matches `FRESHNESS` (pinned by a test).
pub(super) const DETAIL_FRESHNESS: [SafetyPoint; 2] = [
    SafetyPoint {
        icon: Icon::Recover,
        text: "权限观察仅30秒有效，过期后需重新检查。",
    },
    SafetyPoint {
        icon: Icon::Warning,
        text: "检查通过不代表另一台已许可，也不代表实际输入有效。",
    },
];

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
            Self::Unchecked => "先检查屏跃所需的系统权限；这一步不会开启键鼠或更改系统设置。",
            Self::Checking => "正在检查这台电脑的系统权限，不会开启键鼠。",
            Self::Ready => "本机权限已就绪。开启键鼠仍需你确认，并由另一台电脑单独允许接收。",
            Self::Accessibility => {
                "请在macOS系统设置的辅助功能中允许本次运行的屏跃，再返回重新检查。"
            }
            Self::InputMonitoring => {
                "请在macOS系统设置的输入监控中允许本次运行的屏跃，再返回重新检查。"
            }
            Self::InputDesktop => {
                "当前输入桌面不可访问。请返回普通桌面再检查；屏跃不会绕过UAC或安全桌面。"
            }
            Self::SecureInput => {
                "系统正在保护输入。请结束当前受保护的输入后重新检查，屏跃不会关闭Secure Input。"
            }
            Self::Unknown => "本次权限检查结果不明确，请重新检查；未知状态不能视为已允许。",
            Self::Unsupported => "当前环境未提供所需的输入权限检查，暂不能从这个窗口开启键鼠。",
            Self::Failed => "权限检查未完成，请重试或到设置查看原因。",
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
        if !now
            .checked_duration_since(observed.finished)
            .is_some_and(|age| age < FRESHNESS)
        {
            return Readiness::Unchecked;
        }
        match observed.outcome {
            Ok(report) => readiness(report),
            Err(_) => Readiness::Failed,
        }
    }
    pub(super) fn expires_in(&self) -> Option<Duration> {
        let age = Instant::now().checked_duration_since(self.result?.finished)?;
        FRESHNESS
            .checked_sub(age)
            .filter(|remaining| !remaining.is_zero())
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
            self.result = Some(Observation {
                outcome: Err("无法定位同包工作进程"),
                finished: Instant::now(),
            });
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
            let _ = tx.send(Observation {
                outcome,
                finished: Instant::now(),
            });
        }));
        self.receiver = Some(rx);
        self.cancel = Some(cancel);
    }
    fn check_button(&mut self, ui: &mut egui::Ui, idle: bool) {
        if ui
            .add_enabled(
                idle && self.receiver.is_none(),
                egui::Button::new("检查键鼠权限"),
            )
            .clicked()
        {
            self.start();
        }
    }

    fn report_ui(&self, ui: &mut egui::Ui) {
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
    }

    /// A fixed-size presentation for the confirmation window, not an input grant.
    pub(super) fn native_start_header(&mut self, ui: &mut egui::Ui, idle: bool) {
        ui.spacing_mut().item_spacing.y = 4.0;
        ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width(), 38.0),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                let width = (ui.available_width() - 152.0).max(1.0);
                ui.add_sized(
                    [width, 38.0],
                    egui::Label::new(super::desktop_style::semibold("键鼠权限", 16.0))
                        .halign(egui::Align::LEFT),
                );
                self.check_button(ui, idle);
            },
        );
        let state = self.readiness();
        let expired = self.result.is_some() && state == Readiness::Unchecked;
        let summary = if expired {
            EXPIRED_SUMMARY
        } else {
            state.detail()
        };
        let neutral = matches!(state, Readiness::Checking | Readiness::Unchecked) && !expired;
        let tone = if state == Readiness::Ready {
            desktop_style::Tone::Good
        } else if neutral {
            desktop_style::Tone::Neutral
        } else {
            desktop_style::Tone::Warning
        };
        let dark = ui.visuals().dark_mode;
        let ink = desktop_style::status_colors(dark, tone).1;
        // M9: the check mark draws once when a fresh report arrives, then stops repainting.
        let drawn = motion::animate_flag(
            ui.ctx(),
            egui::Id::unique("native-start-ready-check"),
            state == Readiness::Ready,
            desktop_style::tokens::STROKE_DRAW_SECS,
        );
        let now = ui.input(|input| input.time);
        ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width(), 64.0),
            egui::Layout::top_down(egui::Align::LEFT),
            |ui| {
                ui.set_min_height(64.0);
                let summary = desktop_style::status_text(summary, dark, tone).size(13.0);
                icon_row(ui, summary, |painter, rect| match state {
                    Readiness::Ready => icons::paint_check_progress(painter, rect, ink, drawn),
                    // Steps only on the existing permission poll beat; it never schedules one.
                    Readiness::Checking => {
                        let head = motion::spinner_head_at(now);
                        icons::paint_spinner(painter, rect, ink, head);
                    }
                    _ if neutral => Icon::Lock.paint(painter, rect, ink),
                    _ => Icon::Warning.paint(painter, rect, ink),
                });
                let note = egui::RichText::new(HEADER_NOTE.text).size(12.0).weak();
                let muted = desktop_style::tokens::muted_color(dark);
                icon_row(ui, note, |painter, rect| {
                    HEADER_NOTE.icon.paint(painter, rect, muted);
                });
            },
        );
    }

    /// Historical observations stay in the scroll body when freshness expires.
    pub(super) fn native_start_details(&self, ui: &mut egui::Ui) {
        ui.label(desktop_style::semibold("检查详情", 15.0));
        for point in DETAIL_SOURCE {
            safety_point(ui, point);
        }
        self.report_ui(ui);
        for point in DETAIL_FRESHNESS {
            safety_point(ui, point);
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, idle: bool) {
        ui.add_space(12.0);
        ui.strong("键鼠权限");
        ui.weak("由实际输入程序检查，不联网、不捕获输入，也不申请权限。这里显示上次检查结果，开启键鼠时会再次确认。");
        self.check_button(ui, idle);
        if self.receiver.is_some() {
            ui.label("正在检查键鼠权限…");
        }
        self.report_ui(ui);
        if self.result.is_some() && self.readiness() == Readiness::Unchecked {
            ui.weak(EXPIRED_SUMMARY);
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
        PermissionReport {
            accessibility: PermissionStatus::Granted,
            input_monitoring: PermissionStatus::Granted,
            input_desktop: PermissionStatus::NotRequired,
            secure_input: SecureInput::Inactive,
        }
    }
    fn native_dialog_frame(
        app: &mut crate::App,
        ctx: &egui::Context,
        size: egui::Vec2,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                focused: true,
                events,
                ..Default::default()
            },
            |root| app.desktop(root),
        )
    }

    fn text_bounds(shape: &egui::epaint::Shape, label: &str) -> Option<egui::Rect> {
        match shape {
            egui::epaint::Shape::Text(text) if text.galley.job.text == label => {
                Some(text.visual_bounding_rect())
            }
            egui::epaint::Shape::Vec(shapes) => {
                shapes.iter().find_map(|shape| text_bounds(shape, label))
            }
            _ => None,
        }
    }

    fn native_dialog_layout(
        output: egui::FullOutput,
        size: egui::Vec2,
    ) -> anyhow::Result<([egui::Rect; 4], Option<egui::Rect>)> {
        use anyhow::Context as _;
        let labels = [
            "检查键鼠权限",
            "我明确同意本次使用真实键鼠协同",
            "确认并开启键鼠",
            "立即停止并恢复本机",
        ];
        let controls: Vec<_> = labels
            .iter()
            .map(|label| {
                output.shapes.iter().find_map(|shape| {
                    text_bounds(&shape.shape, label).map(|rect| (shape.clip_rect, rect))
                })
            })
            .collect();
        let explanation = output
            .shapes
            .iter()
            .find_map(|shape| text_bounds(&shape.shape, "检查详情"));
        output.drop_without_applying_deltas();
        let mut result = [egui::Rect::NOTHING; 4];
        for (index, (label, bounds)) in labels.iter().zip(controls).enumerate() {
            let (clip, rect) = bounds.with_context(|| format!("{label} absent"))?;
            assert!(clip.contains_rect(rect), "{label} clipped at {size:?}");
            assert!(egui::Rect::from_min_size(egui::Pos2::ZERO, size).contains_rect(rect));
            result[index] = rect;
        }
        Ok((result, explanation))
    }

    fn pointer_button(position: egui::Pos2, pressed: bool) -> Vec<egui::Event> {
        vec![
            egui::Event::PointerMoved(position),
            egui::Event::PointerButton {
                pos: position,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            },
        ]
    }

    #[test]
    fn native_dialog_controls_stay_fixed_as_permission_reports_change() -> anyhow::Result<()> {
        let fixture = crate::tests::Fixture::new()?;
        let mut app = crate::App::load(fixture.path.clone());
        let before = app.config.clone();
        // Synthetic observations exercise presentation only; no worker or input.
        for size in [egui::vec2(1280.0, 800.0), egui::vec2(920.0, 700.0)] {
            let ctx = crate::tests::ui_context();
            app.theme(&ctx);
            app.dialog = Some(crate::Dialog::NativeStart);
            app.worker_permissions.result = Some(Observation {
                outcome: Ok(ready()),
                finished: Instant::now(),
            });
            app.native_confirmation = true;
            native_dialog_frame(&mut app, &ctx, size, vec![]).drop_without_applying_deltas();
            let (baseline, _) =
                native_dialog_layout(native_dialog_frame(&mut app, &ctx, size, vec![]), size)?;
            assert!(app.native_confirmation);
            for outcome in [
                None,
                Some(Err("synthetic diagnostic failure")),
                Some(Ok(PermissionReport {
                    accessibility: PermissionStatus::Denied,
                    ..ready()
                })),
                Some(Ok(PermissionReport {
                    input_monitoring: PermissionStatus::Unknown,
                    ..ready()
                })),
            ] {
                app.worker_permissions.result = outcome.map(|outcome| Observation {
                    outcome,
                    finished: Instant::now(),
                });
                app.native_confirmation = true;
                let (actual, _) =
                    native_dialog_layout(native_dialog_frame(&mut app, &ctx, size, vec![]), size)?;
                assert_eq!(actual, baseline);
                assert!(!app.native_confirmation);
            }
            app.worker_permissions.result = Some(Observation {
                outcome: Ok(ready()),
                finished: Instant::now() - FRESHNESS,
            });
            app.native_confirmation = true;
            let (expired, _) =
                native_dialog_layout(native_dialog_frame(&mut app, &ctx, size, vec![]), size)?;
            assert_eq!(expired, baseline);
            assert!(!app.native_confirmation);
            let (_sender, receiver) = mpsc::channel();
            app.worker_permissions.receiver = Some(receiver);
            app.worker_permissions.result = None;
            app.native_confirmation = true;
            let (checking, _) =
                native_dialog_layout(native_dialog_frame(&mut app, &ctx, size, vec![]), size)?;
            assert_eq!(checking, baseline);
            assert!(!app.native_confirmation);
            app.worker_permissions.receiver = None;
            assert!(app.config == before && app.child.is_none());
            assert!(!app.identity_dir().exists() && !app.dir.join("settings.json").exists());
        }
        drop(app);
        fixture.close()?;
        Ok(())
    }

    #[test]
    fn native_dialog_scroll_and_recheck_never_restore_uncommitted_consent() -> anyhow::Result<()> {
        let fixture = crate::tests::Fixture::new()?;
        let mut app = crate::App::load(fixture.path.clone());
        let before = app.config.clone();
        app.notice = "布局回归说明，不包含实际输入内容。".repeat(80);
        for size in [egui::vec2(1280.0, 800.0), egui::vec2(920.0, 700.0)] {
            let ctx = crate::tests::ui_context();
            app.theme(&ctx);
            app.dialog = Some(crate::Dialog::NativeStart);
            app.worker_permissions.result = Some(Observation {
                outcome: Ok(ready()),
                finished: Instant::now(),
            });
            app.native_confirmation = true;
            native_dialog_frame(&mut app, &ctx, size, vec![]).drop_without_applying_deltas();
            let (baseline, anchor) =
                native_dialog_layout(native_dialog_frame(&mut app, &ctx, size, vec![]), size)?;
            let point = egui::pos2(
                baseline[0].center().x,
                (baseline[0].max.y + baseline[1].min.y) / 2.0,
            );
            native_dialog_frame(
                &mut app,
                &ctx,
                size,
                vec![
                    egui::Event::PointerMoved(point),
                    egui::Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Point,
                        delta: egui::vec2(0.0, -500.0),
                        phase: egui::TouchPhase::Move,
                        source: egui::MouseWheelSource::Wheel,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            )
            .drop_without_applying_deltas();
            let (scrolled, after_scroll) =
                native_dialog_layout(native_dialog_frame(&mut app, &ctx, size, vec![]), size)?;
            assert_eq!(scrolled, baseline);
            assert_ne!(
                after_scroll, anchor,
                "the middle explanation did not actually scroll"
            );
            app.worker_permissions.result = Some(Observation {
                outcome: Ok(ready()),
                finished: Instant::now() - FRESHNESS,
            });
            let (expired, _) =
                native_dialog_layout(native_dialog_frame(&mut app, &ctx, size, vec![]), size)?;
            assert_eq!(expired, baseline);
            assert!(!app.native_confirmation);
            let notice = app.notice.clone();
            for pressed in [true, false] {
                native_dialog_frame(
                    &mut app,
                    &ctx,
                    size,
                    pointer_button(expired[2].center(), pressed),
                )
                .drop_without_applying_deltas();
            }
            assert_eq!(
                app.notice, notice,
                "expired confirmation button accepted a click"
            );
            app.worker_permissions.result = Some(Observation {
                outcome: Ok(ready()),
                finished: Instant::now(),
            });
            let (fresh, _) =
                native_dialog_layout(native_dialog_frame(&mut app, &ctx, size, vec![]), size)?;
            assert_eq!(fresh, baseline);
            assert!(
                !app.native_confirmation,
                "a new report must not restore old consent"
            );
            for pressed in [true, false] {
                native_dialog_frame(
                    &mut app,
                    &ctx,
                    size,
                    pointer_button(fresh[1].center(), pressed),
                )
                .drop_without_applying_deltas();
            }
            assert!(
                app.native_confirmation,
                "fresh consent requires the explicit checkbox click"
            );
            assert!(app.config == before && app.child.is_none());
            assert!(!app.worker_permissions.needs_poll());
            assert!(!app.identity_dir().exists() && !app.dir.join("settings.json").exists());
        }
        drop(app);
        fixture.close()?;
        Ok(())
    }

    fn checker(checking: bool, observation: Option<(Outcome, Instant)>) -> Checker {
        let result = observation.map(|(outcome, finished)| Observation { outcome, finished });
        Checker {
            receiver: checking.then(|| mpsc::channel().1),
            cancel: None,
            task: None,
            result,
        }
    }

    /// Unchecked, checking, ready, denied, failed and expired, without any worker.
    fn header_states() -> Vec<Checker> {
        let now = Instant::now();
        let denied = PermissionReport {
            accessibility: PermissionStatus::Denied,
            ..ready()
        };
        vec![
            checker(false, None),
            checker(true, None),
            checker(false, Some((Ok(ready()), now))),
            checker(false, Some((Ok(denied), now))),
            checker(false, Some((Err("synthetic diagnostic failure"), now))),
            checker(false, Some((Ok(ready()), now - FRESHNESS))),
        ]
    }

    /// The status sentence the header shows for `checker`.
    fn header_summary(checker: &Checker) -> &'static str {
        let state = checker.readiness();
        if checker.result.is_some() && state == Readiness::Unchecked {
            EXPIRED_SUMMARY
        } else {
            state.detail()
        }
    }

    #[test]
    fn native_header_and_details_pair_every_sentence_with_an_icon() {
        use crate::desktop_ui::safety_dialog_tests::{assert_short_sentences, texts_of, Painted};
        assert_short_sentences(&[HEADER_NOTE]);
        assert_short_sentences(&DETAIL_SOURCE);
        assert_short_sentences(&DETAIL_FRESHNESS);
        let seconds = format!("权限观察仅{}秒有效", FRESHNESS.as_secs());
        assert!(DETAIL_FRESHNESS[0].text.starts_with(&seconds));
        for mut checker in header_states() {
            // A fresh context per state: the ready check mark starts fully drawn.
            let ctx = crate::tests::ui_context();
            let mut labels = vec![header_summary(&checker), HEADER_NOTE.text];
            labels.extend(texts_of(&DETAIL_SOURCE));
            labels.extend(texts_of(&DETAIL_FRESHNESS));
            let mut frame = || {
                let size = egui::vec2(580.0, 1200.0);
                let input = egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                    ..Default::default()
                };
                ctx.run_ui(input, |ui| {
                    checker.native_start_header(ui, true);
                    checker.native_start_details(ui);
                })
            };
            frame().drop_without_applying_deltas();
            Painted::of(frame()).assert_points(&labels);
        }
    }

    #[test]
    fn native_dialog_points_stay_inside_the_fixed_window() -> anyhow::Result<()> {
        use crate::desktop_ui::{native_start_points, safety_dialog_tests::texts_of};
        use anyhow::Context as _;
        let fixture = crate::tests::Fixture::new()?;
        let mut app = crate::App::load(fixture.path.clone());
        let before = app.config.clone();
        let waits = !app.config.listen && !app.config.reconnect;
        let mut labels = texts_of(&native_start_points(app.config.allow, waits, false));
        labels.extend(texts_of(&DETAIL_SOURCE));
        labels.extend(texts_of(&DETAIL_FRESHNESS));
        for size in [egui::vec2(1280.0, 800.0), egui::vec2(920.0, 700.0)] {
            let viewport = egui::Rect::from_min_size(egui::Pos2::ZERO, size);
            for state in header_states() {
                let ctx = crate::tests::ui_context();
                app.theme(&ctx);
                app.dialog = Some(crate::Dialog::NativeStart);
                app.worker_permissions = state;
                let summary = header_summary(&app.worker_permissions);
                native_dialog_frame(&mut app, &ctx, size, vec![]).drop_without_applying_deltas();
                let output = native_dialog_frame(&mut app, &ctx, size, vec![]);
                let painted = crate::desktop_ui::safety_dialog_tests::Painted::of(output);
                let consent = painted.text("我明确同意本次使用真实键鼠协同");
                let (consent, _) = consent.context("consent absent")?;
                for label in [summary, HEADER_NOTE.text] {
                    let (bounds, clip) = painted.text(label).context("header text absent")?;
                    assert!(clip.contains_rect(bounds), "{label} clipped at {size:?}");
                    assert!(viewport.contains_rect(bounds), "{label} left the window");
                    assert!(bounds.max.y < consent.min.y, "{label} reaches the footer");
                }
                let mut shown = 0;
                for label in &labels {
                    let Some((bounds, clip)) = painted.text(label) else {
                        continue;
                    };
                    shown += 1;
                    let inside = bounds.min.x >= clip.min.x && bounds.max.x <= clip.max.x;
                    assert!(inside, "{label} overflows sideways at {size:?}");
                    assert!(bounds.min.x >= viewport.min.x && bounds.max.x <= viewport.max.x);
                }
                assert!(shown >= 3, "only {shown} points visible at {size:?}");
            }
        }
        assert!(app.config == before && app.child.is_none());
        assert!(!app.identity_dir().exists() && !app.dir.join("settings.json").exists());
        drop(app);
        fixture.close()?;
        Ok(())
    }

    #[test]
    fn native_and_recovery_dialog_motion_settles_after_open_and_close() -> anyhow::Result<()> {
        use std::sync::atomic::AtomicUsize;
        let fixture = crate::tests::Fixture::new()?;
        let mut app = crate::App::load(fixture.path.clone());
        let before = app.config.clone();
        let ctx = crate::tests::ui_context();
        app.theme(&ctx);
        let mut time = 0.0;
        let mut frames = |app: &mut crate::App, count: usize| {
            for _ in 0..count {
                let input = egui::RawInput {
                    time: Some(time),
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1280.0, 800.0),
                    )),
                    focused: true,
                    ..Default::default()
                };
                ctx.run_ui(input, |root| app.desktop(root))
                    .drop_without_applying_deltas();
                time += 1.0 / 60.0;
            }
        };
        frames(&mut app, 20);
        let requests = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&requests);
        ctx.set_request_repaint_callback(move |request| {
            if request.delay < Duration::from_secs(1) {
                observed.fetch_add(1, Ordering::SeqCst);
            }
        });
        let count = || requests.load(Ordering::SeqCst);
        frames(&mut app, 60);
        assert_eq!(count(), 0, "the idle window asked for frames");
        app.dialog = Some(crate::Dialog::NativeStart);
        frames(&mut app, 60);
        // A fresh report while the dialog is open draws the check mark once (M9).
        let drawn = count();
        app.worker_permissions.result = Some(Observation {
            outcome: Ok(ready()),
            finished: Instant::now(),
        });
        frames(&mut app, 30);
        assert!(count() > drawn, "the check mark never animated");
        for next in [None, Some(crate::Dialog::Recovery), None] {
            frames(&mut app, 40);
            let settled = count();
            frames(&mut app, 120);
            assert_eq!(count(), settled, "{:?} kept repainting", app.dialog);
            app.dialog = next;
        }
        frames(&mut app, 40);
        let settled = count();
        frames(&mut app, 120);
        assert_eq!(count(), settled, "the closed dialog kept repainting");
        assert!(app.config == before && app.child.is_none());
        assert!(!app.identity_dir().exists() && !app.dir.join("settings.json").exists());
        drop(app);
        fixture.close()?;
        Ok(())
    }

    #[test]
    fn readiness_preserves_denials_unknown_and_system_protection() {
        let original = ready();
        assert_eq!(readiness(original), Readiness::Ready);
        for (field, expected) in [
            (0, Readiness::Accessibility),
            (1, Readiness::InputMonitoring),
            (2, Readiness::InputDesktop),
        ] {
            for (status, expected) in [
                (PermissionStatus::Denied, expected),
                (PermissionStatus::Unknown, Readiness::Unknown),
                (PermissionStatus::Unsupported, Readiness::Unsupported),
            ] {
                let mut report = original;
                match field {
                    0 => report.accessibility = status,
                    1 => report.input_monitoring = status,
                    _ => report.input_desktop = status,
                }
                assert_eq!(readiness(report), expected);
            }
        }
        for (secure_input, expected) in [
            (SecureInput::Active, Readiness::SecureInput),
            (SecureInput::Unknown, Readiness::Unknown),
            (SecureInput::Unsupported, Readiness::Ready),
        ] {
            assert_eq!(
                readiness(PermissionReport {
                    secure_input,
                    ..original
                }),
                expected
            );
        }
    }
    #[test]
    fn delayed_gui_poll_does_not_refresh_old_observation_or_persist_authority() {
        let finished = Instant::now() - FRESHNESS;
        let (tx, rx) = mpsc::sync_channel(1);
        // Synthetic permission metadata only; no native worker or grant exists.
        assert!(tx
            .send(Observation {
                outcome: Ok(ready()),
                finished
            })
            .is_ok());
        let mut checker = Checker {
            receiver: Some(rx),
            cancel: None,
            task: None,
            result: None,
        };
        assert_eq!(checker.readiness(), Readiness::Checking);
        checker.poll();
        // Instant arithmetic need not represent a one-nanosecond delta on every OS.
        // Establish real ordering before testing either side of the deadline.
        let before_expiry = finished + FRESHNESS - Duration::from_millis(1);
        let before_observation = finished - Duration::from_millis(1);
        assert!(before_observation < finished && before_expiry < finished + FRESHNESS);
        assert_eq!(checker.readiness_at(before_expiry), Readiness::Ready);
        assert_eq!(
            checker.readiness_at(finished + FRESHNESS),
            Readiness::Unchecked
        );
        assert_eq!(
            checker.readiness_at(before_observation),
            Readiness::Unchecked
        );
        assert_eq!(checker.readiness(), Readiness::Unchecked);
        assert!(checker.expires_in().is_none() && !checker.needs_poll());
        checker.result = Some(Observation {
            outcome: Err("工作进程诊断失败"),
            finished: Instant::now(),
        });
        assert_eq!(checker.readiness(), Readiness::Failed);
    }
    #[test]
    #[ignore = "requires exact-built CLI; mandatory post-release CI step"]
    fn real_worker_readiness_remains_readonly_and_requires_separate_consent() {
        let binary = std::path::PathBuf::from(
            std::env::var_os("PEERGLIDE_READINESS_TEST_WORKER")
                .expect("matching release CLI required"),
        );
        assert!(binary.is_file());
        let mut checker = Checker::default();
        checker.start_path(binary);
        let deadline = Instant::now() + Duration::from_secs(10);
        while checker.needs_poll() && Instant::now() < deadline {
            checker.poll();
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!checker.needs_poll() && checker.task.is_none());
        let report = checker
            .result
            .and_then(|observed| observed.outcome.ok())
            .expect("actual worker did not return a bounded read-only permission report");
        assert_eq!(checker.readiness(), readiness(report));
        assert!(checker.expires_in().is_some());
        let fixture = crate::tests::Fixture::new().unwrap();
        let mut app = crate::App::load(fixture.path.join("profile"));
        let original = app.config.clone();
        app.worker_permissions = checker;
        // Even a real Granted observation is not the user's native consent.
        assert!(app.request_native().is_err());
        assert!(app.config == original && app.child.is_none() && app.pending_start.is_none());
        assert!(!app.native_confirmation && !app.dir.join("settings.json").exists());
        assert!(!app.identity_dir().exists());
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
