#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
mod config_store;
mod connection_info;
mod desktop_state;
mod desktop_style;
mod desktop_ui;
mod discovery_panel;
mod enrollment_store;
mod gesture_lab;
mod gesture_rules;
mod quit_check;
mod worker_permissions;
use anyhow::{Context, Result};
use clap::Parser;
use eframe::egui::{self, RichText, Vec2};
use peerglide_protocol::worker::{WorkerEvent, WorkerInputEvent, WorkerInputState, WorkerState};
use peerglide_transport::{self as transport, Identity};
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicU16, Ordering},
        mpsc, Arc,
    },
    time::{Duration, Instant},
};

#[derive(Parser)]
#[command(name = "peerglide-gui", version, about = "屏跃桌面控制面板")]
struct Options {
    /// 使用指定的配置与身份目录，不导入或迁移其他目录的数据与授权。
    #[arg(long, value_name = "DIRECTORY")]
    data_dir: Option<PathBuf>,
    /// 打开离线手势实验室，仅记录窗口内轨迹。
    #[arg(long)]
    gesture_lab: bool,
    /// Isolated GUI smoke geometry only; create a new report, never overwrite.
    #[arg(long, hide = true, requires = "gesture_lab")]
    ui_layout_report: Option<PathBuf>,
    /// Isolated AppKit terminate integration check, never native input.
    #[arg(
        long,
        hide = true,
        requires = "data_dir",
        conflicts_with = "gesture_lab"
    )]
    macos_quit_check_report: Option<PathBuf>,
}

fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let name = "noto-sans-sc".to_owned();
    let mut data = egui::FontData::from_static(include_bytes!("../assets/fonts/NotoSansSC.ttf"));
    // The upstream variable face defaults to Thin (100); use Regular for UI text.
    data.tweak.coords.push(b"wght", 400.0);
    fonts.font_data.insert(name.clone(), Arc::new(data));
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts.families.entry(family).or_default().push(name.clone());
    }
    ctx.set_fonts(fonts);
}

fn edge_label(edge: &str) -> &'static str {
    match edge {
        "left" => "左侧",
        "right" => "右侧",
        "top" => "上侧",
        "bottom" => "下侧",
        _ => "未知方向",
    }
}

#[derive(Clone, Serialize, Deserialize)]
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
            peer_name: "另一台电脑".into(),
            address: String::new(),
            listen: true,
            allow: false,
            native: false,
            reconnect: false,
            edge: "right".into(),
            dark: false,
            fingerprint: None,
        }
    }
}

// Separate file: older input GUIs keep reading settings.json unchanged.
#[derive(Clone, Copy, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct DesktopPreferences {
    tray_enabled: bool,
    close_to_tray: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SessionMode {
    Communication,
    Native,
    Probe,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum DeviceTab {
    Device,
    Layout,
    Permissions,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum MouseTab {
    Rules,
    Practice,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum CollaborationTab {
    Clipboard,
    Files,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Dialog {
    NativeStart,
    Revoke,
    Reset,
    ForceStop,
    Recovery,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum SetupStep {
    Identity,
    Find,
    Verify,
    Permissions,
    Layout,
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
struct WorkerSnapshot(AtomicU16, AtomicU16, AtomicBool, AtomicBool);
impl WorkerSnapshot {
    fn publish_input(&self, state: WorkerInputState, terminal: bool) {
        let code = match state {
            WorkerInputState::Disabled => 1,
            WorkerInputState::Local => 2,
            WorkerInputState::ControllingRemote => 3,
            WorkerInputState::ReceivingRemote => 4,
            WorkerInputState::Recovering => 5,
            WorkerInputState::RecoveryUnconfirmed => 6,
            WorkerInputState::Unavailable => 7,
        };
        self.1
            .store(code | if terminal { 0x100 } else { 0 }, Ordering::Release);
    }
    fn take_input(&self) -> Option<(WorkerInputState, bool)> {
        let code = self.1.swap(0, Ordering::AcqRel);
        let state = match code & 0xff {
            1 => WorkerInputState::Disabled,
            2 => WorkerInputState::Local,
            3 => WorkerInputState::ControllingRemote,
            4 => WorkerInputState::ReceivingRemote,
            5 => WorkerInputState::Recovering,
            6 => WorkerInputState::RecoveryUnconfirmed,
            7 => WorkerInputState::Unavailable,
            _ => return None,
        };
        Some((state, code & 0x100 != 0))
    }

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
    recording_mode: bool,
    layout_report: Option<PathBuf>,
    device_tab: DeviceTab,
    mouse_tab: MouseTab,
    collaboration_tab: CollaborationTab,
    wizard: Option<SetupStep>,
    wizard_name: String,
    wizard_address: String,
    wizard_listen: bool,
    wizard_allow: bool,
    dialog: Option<Dialog>,
    native_confirmation: bool,
    active_mode: Option<SessionMode>,
    pending_start: Option<SessionMode>,
    input_state: Option<WorkerInputState>,
    input_terminal: bool,
    recovery_blocked: bool,
    close_requested: bool,
    exit_confirmed: bool,
    permission_report: Option<peerglide_platform::context::PermissionReport>,
    worker_permissions: worker_permissions::Checker,
    desktop_preferences: DesktopPreferences,
    desktop_store: Option<config_store::Store>,
    tray: Option<peerglide_platform::tray::Tray>,
    tray_mailbox: Arc<peerglide_platform::tray::Mailbox>,
    application_menu: Option<peerglide_platform::application_menu::Owner>,
    application_menu_error: Option<String>,
    quit_check: Option<quit_check::Check>,
    quit_check_mode: bool,
    window_hidden: bool,
    screen_drag: Option<(egui::Vec2, egui::Pos2, egui::Rect)>,
    discovery: discovery_panel::Panel,
    gesture_lab: gesture_lab::GestureLab,
    gesture_rules: gesture_rules::RuleEditor,
    dir: PathBuf,
    config: Config,
    config_store: Option<config_store::Store>,
    enrollment_blocked: bool,
    instance_lock: Option<config_store::Lock>,
    page: Page,
    import_path: String,
    use_connection_info: bool,
    connection_text: String,
    loaded_connection_info: Option<blake3::Hash>,
    connection_addresses: Vec<std::net::SocketAddr>,
    confirmed_fingerprint: Option<String>,
    notice: String,
    child: Option<Child>,
    events: Option<mpsc::Receiver<String>>,
    worker_state: Option<Arc<WorkerSnapshot>>,
    status: String,
    connected: bool,
    last_failure: bool,
    logs: VecDeque<String>,
    stopping: Option<Instant>,
}
impl App {
    fn new(cc: &eframe::CreationContext<'_>, dir: PathBuf) -> Self {
        install_fonts(&cc.egui_ctx);
        let app = Self::load(dir);
        app.theme(&cc.egui_ctx);
        let mut app = app;
        let wake_context = cc.egui_ctx.clone();
        app.tray_mailbox = peerglide_platform::tray::Mailbox::new(Arc::new(move || {
            wake_context.request_repaint();
        }));
        match peerglide_platform::application_menu::install(app.tray_mailbox.clone()) {
            Ok(owner) => app.application_menu = Some(owner),
            Err(error) => {
                app.application_menu_error = Some(format!("{error:#}"));
                app.notice = "退出恢复入口未就绪，本窗口只能使用通信诊断，不能开启键鼠。请重新打开本版检查。".into();
            }
        }
        app.sync_tray(&cc.egui_ctx);
        app
    }
    fn load(dir: PathBuf) -> Self {
        let mut notice = String::new();
        let instance_lock = std::fs::create_dir_all(&dir)
            .map_err(anyhow::Error::from)
            .and_then(|_| config_store::lock(&dir.join("gui-instance.lock")))
            .map_err(|e| {
                notice = format!("当前为只读窗口：{e:#}");
                e
            })
            .ok();
        if instance_lock.is_some() {
            match enrollment_store::recover(&dir) {
                Ok(true) => notice = "已完成上次设备登记收尾，键鼠与接收权限保持关闭。".into(),
                Ok(false) => (),
                Err(error) => notice = format!("设备登记恢复未完成，原文件已保留：{error:#}"),
            }
        }
        let enrollment_blocked = enrollment_store::ensure_idle(&dir.join("settings.json")).is_err();
        let (config_store, config) =
            match config_store::Store::open::<Config>(dir.join("settings.json")) {
                Ok((store, config)) => (Some(store), config),
                Err(e) => {
                    notice.push_str(&format!(" 设置已保留，需要恢复：{e:#}"));
                    (None, Config::default())
                }
            };
        let (desktop_store, desktop_preferences) =
            match config_store::Store::open::<DesktopPreferences>(
                dir.join("desktop-preferences.json"),
            ) {
                Ok((store, preferences)) => (Some(store), preferences),
                Err(error) => {
                    notice.push_str(&format!(" 桌面选项已保留，当前不可编辑：{error:#}"));
                    (None, DesktopPreferences::default())
                }
            };
        let mut app = Self {
            recording_mode: false,
            layout_report: None,
            device_tab: DeviceTab::Device,
            mouse_tab: MouseTab::Rules,
            collaboration_tab: CollaborationTab::Clipboard,
            wizard: None,
            wizard_name: config.peer_name.clone(),
            wizard_address: config.address.clone(),
            wizard_listen: config.listen,
            wizard_allow: false,
            dialog: None,
            native_confirmation: false,
            active_mode: None,
            pending_start: None,
            input_state: None,
            input_terminal: false,
            recovery_blocked: false,
            close_requested: false,
            exit_confirmed: false,
            permission_report: None,
            worker_permissions: worker_permissions::Checker::default(),
            desktop_preferences,
            desktop_store,
            tray: None,
            tray_mailbox: peerglide_platform::tray::Mailbox::new(Arc::new(|| {})),
            application_menu: None,
            application_menu_error: None,
            quit_check: None,
            quit_check_mode: false,
            window_hidden: false,
            screen_drag: None,
            discovery: discovery_panel::Panel::default(),
            gesture_lab: gesture_lab::GestureLab::new(&dir),
            gesture_rules: gesture_rules::RuleEditor::new(&dir),
            dir,
            config,
            config_store,
            enrollment_blocked,
            instance_lock,
            page: Page::Home,
            import_path: String::new(),
            use_connection_info: false,
            connection_text: String::new(),
            loaded_connection_info: None,
            connection_addresses: Vec::new(),
            confirmed_fingerprint: None,
            notice,
            child: None,
            events: None,
            worker_state: None,
            status: "离线".into(),
            connected: false,
            last_failure: false,
            logs: VecDeque::new(),
            stopping: None,
        };
        if let Err(e) = std::fs::create_dir_all(&app.dir) {
            app.notice = e.to_string();
        }
        app
    }
    fn theme(&self, ctx: &egui::Context) {
        desktop_style::install(ctx, self.config.dark);
    }
    fn save(&mut self) {
        if let Err(e) = self.save_config() {
            self.notice = e.to_string();
        }
    }
    fn save_config(&mut self) -> Result<()> {
        anyhow::ensure!(!self.quit_check_mode, "隔离退出检查不会保存设备设置");
        anyhow::ensure!(self.instance_lock.is_some(), "另一窗口正在使用此数据目录");
        self.config_store
            .as_mut()
            .context("设置需要恢复，原文件已保留")?
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
            "更改地址前请先停止会话"
        );
        self.config.address = address.to_string();
        self.config.listen = false;
        self.save_config()
    }
    fn candidate(&self) -> Result<Vec<u8>> {
        if self.use_connection_info {
            anyhow::ensure!(
                self.loaded_connection_info == Some(blake3::hash(self.connection_text.as_bytes())),
                "请先读取当前连接信息，修改后需要重新核验"
            );
            Ok(connection_info::decode(&self.connection_text)?.certificate)
        } else {
            transport::read_certificate(std::path::Path::new(self.import_path.trim()))
        }
    }
    fn read_connection_info(&mut self) -> Result<()> {
        anyhow::ensure!(self.idle_writable(), "请先停止会话并恢复设置写入");
        self.confirmed_fingerprint = None;
        self.loaded_connection_info = None;
        self.connection_addresses.clear();
        let info = connection_info::decode(&self.connection_text)?;
        let local = Identity::load(&self.identity_dir()).context("请先创建本机身份")?;
        anyhow::ensure!(
            info.certificate != local.certificate,
            "这是本机信息，请粘贴另一台电脑的信息"
        );
        self.use_connection_info = true;
        self.connection_addresses = info.addresses;
        self.loaded_connection_info = Some(blake3::hash(self.connection_text.as_bytes()));
        if !self.connection_addresses.is_empty() {
            self.wizard_listen = false;
            self.wizard_address = if self.connection_addresses.len() == 1 {
                self.connection_addresses[0].to_string()
            } else {
                String::new()
            };
        } else {
            self.wizard_listen = true;
            self.wizard_address.clear();
        }
        // This is draft data only: no certificate/config writes or permissions.
        self.wizard = Some(SetupStep::Verify);
        Ok(())
    }
    fn local_connection_info(&self) -> Result<String> {
        let identity = Identity::load(&self.identity_dir()).context("请先创建本机身份")?;
        let mut addresses = Vec::new();
        if self.wizard_listen {
            for interface in
                if_addrs::get_if_addrs().context("无法读取本机网卡地址，请使用高级登记")?
            {
                if let std::net::IpAddr::V4(ip) = interface.ip() {
                    if !ip.is_loopback()
                        && !ip.is_unspecified()
                        && !ip.is_multicast()
                        && !ip.is_broadcast()
                        && (ip.is_private() || ip.is_link_local())
                    {
                        addresses.push(std::net::SocketAddr::from((ip, 24870)));
                    }
                }
            }
            addresses.sort_unstable();
            addresses.dedup();
            anyhow::ensure!(
                !addresses.is_empty(),
                "未找到局域网 IPv4，请检查网络或使用高级登记"
            );
        }
        connection_info::encode(&identity.certificate, &addresses)
    }
    fn enroll(&mut self) -> Result<()> {
        anyhow::ensure!(
            self.instance_lock.is_some() && self.config_store.is_some(),
            "登记设备前需要可写入的设置"
        );
        anyhow::ensure!(self.child.is_none(), "登记设备前请先停止当前会话");
        self.native_confirmation = false;
        self.pending_start = None;
        self.screen_drag = None;
        let confirmed = self
            .confirmed_fingerprint
            .take()
            .context("请先在两台电脑上独立核对指纹")?;
        let bytes = self.candidate()?;
        anyhow::ensure!(
            transport::fingerprint(&bytes) == confirmed,
            "证书已变化，请重新核对指纹"
        );
        let local = Identity::load(&self.identity_dir()).context("请先创建可用的本机身份")?;
        anyhow::ensure!(bytes != local.certificate, "请选择另一台电脑的证书");
        let mut next = self.config.clone();
        next.fingerprint = Some(transport::fingerprint(&bytes));
        next.allow = false;
        next.native = false;
        let result =
            enrollment_store::commit(self.config_store.as_mut().unwrap(), &next, Some(&bytes));
        if let Err(error) = result {
            self.enrollment_blocked =
                enrollment_store::ensure_idle(&self.dir.join("settings.json")).is_err();
            self.config_store = None;
            return Err(error).context("设备登记未完成，请重新打开屏跃检查收尾；不会开启键鼠");
        }
        self.config = next;
        Ok(())
    }
    fn start(&mut self, probe: bool) -> Result<()> {
        self.start_mode(if probe {
            SessionMode::Probe
        } else {
            SessionMode::Communication
        })
    }
    fn start_mode(&mut self, mode: SessionMode) -> Result<()> {
        anyhow::ensure!(!self.quit_check_mode, "隔离退出检查不会启动设备会话");
        let probe = mode == SessionMode::Probe;
        anyhow::ensure!(
            mode != SessionMode::Native || cfg!(any(windows, target_os = "macos")),
            "此平台只支持通信诊断，原生输入尚未实现"
        );
        anyhow::ensure!(
            mode != SessionMode::Native || self.native_exit_protected(),
            "退出恢复入口未就绪，不能开启新的键鼠会话；仍可使用通信诊断"
        );
        anyhow::ensure!(self.stopping.is_none(), "停止和恢复尚未完成");
        anyhow::ensure!(
            mode != SessionMode::Native || !self.recovery_blocked,
            "上次输入恢复未确认，本窗口只允许通信诊断"
        );
        anyhow::ensure!(self.instance_lock.is_some(), "另一窗口正在使用此数据目录");
        anyhow::ensure!(self.child.is_none(), "已有会话正在运行");
        Identity::load(&self.identity_dir()).context("请先在“设备”页创建本机身份")?;
        let cert = self.dir.join("trusted-peer.crt.der");
        let bytes = transport::read_certificate(&cert).context("请先在“设备”页登记可信设备")?;
        anyhow::ensure!(
            self.config.fingerprint.as_deref() == Some(transport::fingerprint(&bytes).as_str()),
            "已登记的证书发生变化，请重新明确登记"
        );
        if !self.config.listen || probe {
            self.config
                .address
                .parse::<std::net::SocketAddr>()
                .context("请输入对端 IP 地址和端口")?;
        }
        self.save_config()?;
        let executable = std::env::current_exe()?.with_file_name(if cfg!(windows) {
            "peerglide.exe"
        } else {
            "peerglide"
        });
        anyhow::ensure!(
            executable.is_file(),
            "缺少同目录的 peerglide 通信程序，请使用完整开发包"
        );
        let mut cmd = self.worker_command(executable, cert, mode);
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
            for result in BufReader::new(stdout).lines() {
                let line = match result {
                    Ok(line) => line,
                    Err(_) => {
                        output_state.3.store(true, Ordering::Release);
                        break;
                    }
                };
                if let Some(event) = WorkerEvent::parse(&line) {
                    output_state.publish(event.state);
                } else if let Some(event) = WorkerInputEvent::parse(&line) {
                    output_state.publish_input(event.state, event.terminal);
                } else {
                    let _ = output_tx.try_send(line.chars().take(600).collect());
                }
            }
            output_state.2.store(true, Ordering::Release);
        });
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                let _ = tx.try_send(line.chars().take(600).collect());
            }
        });
        self.child = Some(child);
        self.active_mode = Some(mode);
        self.input_terminal = false;
        self.input_state = Some(if mode == SessionMode::Native {
            WorkerInputState::Unavailable
        } else {
            WorkerInputState::Disabled
        });
        self.events = Some(rx);
        self.worker_state = Some(snapshot);
        self.logs.clear();
        self.connected = false;
        self.stopping = None;
        self.last_failure = false;
        self.status = "正在启动".into();
        Ok(())
    }
    fn native_cleanup_reported(&self) -> bool {
        self.worker_state.as_ref().is_some_and(|snapshot| {
            snapshot.2.load(Ordering::Acquire) && !snapshot.3.load(Ordering::Acquire)
        }) && self.input_terminal
            && matches!(
                self.input_state,
                Some(WorkerInputState::Local | WorkerInputState::Disabled)
            )
            && !self.recovery_blocked
    }
    fn worker_command(&self, executable: PathBuf, cert: PathBuf, mode: SessionMode) -> Command {
        let probe = mode == SessionMode::Probe;
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
        } else if mode == SessionMode::Native {
            if self.config.allow {
                cmd.arg("--allow-control");
            }
            cmd.arg("--native");
        }
        cmd
    }
    fn resend_stop(&mut self) -> Result<()> {
        self.pending_start = None;
        let child = self.child.as_mut().context("没有活动工作进程")?;
        let stdin = child.stdin.as_mut().context("工作进程的停止通道不可用")?;
        stdin.write_all(b"stop\n").context("再次发送停止请求失败")?;
        stdin.flush()?;
        if self.stopping.is_none() {
            self.stopping = Some(Instant::now());
        }
        self.connected = false;
        Ok(())
    }
    fn stop(&mut self) {
        self.pending_start = None;
        self.native_confirmation = false;
        self.screen_drag = None;
        if let Some(child) = &mut self.child {
            if self.input_terminal {
                self.connected = false;
                self.status = "清理已报告，正在等待工作进程退出".into();
                return;
            }
            if self.stopping.is_none() {
                let sent = child
                    .stdin
                    .as_mut()
                    .map(|stdin| stdin.write_all(b"stop\n").and_then(|_| stdin.flush()));
                if !matches!(sent, Some(Ok(()))) {
                    if self.active_mode == Some(SessionMode::Native) {
                        self.recovery_blocked = true;
                    }
                    self.notice = "未能发送停止请求，请检查工作进程状态".into();
                }
                self.stopping = Some(Instant::now());
            }
            self.connected = false;
            self.input_state = Some(WorkerInputState::Recovering);
            self.status = "正在停止并恢复本机控制".into();
        }
    }
    fn mark_stop_timeout(&mut self) {
        self.pending_start = None;
        if self.input_terminal {
            self.notice = "已收到最终清理状态，仍在等待工作进程与输出通道结束。".into();
            return;
        }
        if self.active_mode == Some(SessionMode::Native) {
            self.recovery_blocked = true;
            self.input_state = Some(WorkerInputState::RecoveryUnconfirmed);
            self.notice = "停止尚未完成。请查看恢复状态；不会自动启动新键鼠会话。".into();
        } else {
            self.notice =
                "通信进程停止尚未完成，未开启原生输入。等待退出或明确结束后再开始新会话。".into();
        }
    }
    fn request_native(&mut self) -> Result<()> {
        anyhow::ensure!(
            std::mem::take(&mut self.native_confirmation),
            "请明确确认启动真实键鼠"
        );
        anyhow::ensure!(!self.quit_check_mode, "隔离退出检查不会开启键鼠");
        anyhow::ensure!(
            !self.recovery_blocked && self.stopping.is_none(),
            "恢复未确认，不能开始新的键鼠会话"
        );
        anyhow::ensure!(
            self.native_exit_protected(),
            "退出恢复入口未就绪，不能开启键鼠"
        );
        self.config.native = true;
        self.save_config()?;
        if self.child.is_some() {
            anyhow::ensure!(
                self.active_mode == Some(SessionMode::Communication),
                "请先停止当前会话"
            );
            self.stop();
            self.pending_start = Some(SessionMode::Native);
            Ok(())
        } else {
            self.start_mode(SessionMode::Native)
        }
    }
    fn revoke(&mut self) -> Result<()> {
        anyhow::ensure!(self.instance_lock.is_some(), "另一窗口正在使用此数据目录");
        self.stop();
        self.confirmed_fingerprint = None;
        self.connected = false;
        self.config.fingerprint = None;
        self.config.allow = false;
        self.config.native = false;
        let result = self
            .config_store
            .as_mut()
            .context("设置只读，设备信任撤销尚未持久保存")
            .and_then(|store| enrollment_store::commit(store, &self.config, None));
        if result.is_err() {
            self.enrollment_blocked =
                enrollment_store::ensure_idle(&self.dir.join("settings.json")).is_err();
            self.config_store = None;
        }
        result.context("保存设备信任撤销结果；未确认持久完成，请重新打开屏跃检查")
    }
    fn poll(&mut self) {
        self.worker_permissions.poll();
        self.discovery
            .poll(self.child.is_some() && self.stopping.is_none() && self.config.listen);
        let diagnostics_complete = if let Some(rx) = &self.events {
            loop {
                match rx.try_recv() {
                    Ok(line) => {
                        self.logs.push_back(line);
                        if self.logs.len() > 100 {
                            self.logs.pop_front();
                        }
                    }
                    Err(mpsc::TryRecvError::Empty) => break false,
                    Err(mpsc::TryRecvError::Disconnected) => break true,
                }
            }
        } else {
            false
        };
        if diagnostics_complete {
            self.events = None;
        }
        if let Some(snapshot) = &self.worker_state {
            let (state, probe_passed) = snapshot.take();
            if let Some((input, terminal)) = snapshot.take_input() {
                if self.child.is_some() {
                    self.input_terminal = terminal;
                    if input == WorkerInputState::RecoveryUnconfirmed {
                        self.recovery_blocked = true;
                        self.pending_start = None;
                    }
                    if self.stopping.is_none()
                        || matches!(
                            input,
                            WorkerInputState::Local
                                | WorkerInputState::Disabled
                                | WorkerInputState::RecoveryUnconfirmed
                        )
                    {
                        self.input_state = Some(input);
                    }
                }
            }
            if probe_passed {
                self.notice = "通信探针通过，未注入任何输入。".into();
            }
            if self.child.is_some() && self.stopping.is_none() {
                match state {
                    Some(WorkerState::Listening) => {
                        self.connected = false;
                        self.status = "等待对端连接".into();
                    }
                    Some(WorkerState::ProbePassed) => {
                        self.connected = false;
                        self.status = "探针通过".into();
                    }
                    Some(WorkerState::Reconnecting) => {
                        self.connected = false;
                        self.status = "正在重连可信设备".into();
                    }
                    Some(WorkerState::Disconnected) => {
                        self.connected = false;
                        self.status = "连接已断开".into();
                    }
                    Some(WorkerState::Authenticated) => {
                        self.connected = true;
                        self.status = "已通过身份验证".into();
                    }
                    Some(WorkerState::Stopped | WorkerState::Failed) => {
                        self.connected = false;
                        self.last_failure |= state == Some(WorkerState::Failed);
                        self.status = "工作进程已停止".into();
                    }
                    _ => (),
                }
            }
        }
        if self
            .stopping
            .is_some_and(|started| started.elapsed() > Duration::from_secs(3))
        {
            self.mark_stop_timeout();
        }
        if let Some(child) = &mut self.child {
            match child.try_wait() {
                Ok(Some(exit)) => {
                    self.connected = false;
                    self.last_failure |= !exit.success();
                    self.status = if exit.success() {
                        "离线".into()
                    } else {
                        format!("工作进程已退出：{exit}")
                    };
                    let native = self.active_mode == Some(SessionMode::Native);
                    if let Some(snapshot) = self.worker_state.as_ref() {
                        if !snapshot.2.load(Ordering::Acquire) {
                            self.status = if native {
                                "进程已退出，正在读取最终输入状态"
                            } else {
                                "进程已退出，正在读取最终通信状态"
                            }
                            .into();
                            return;
                        }
                        // The reader can publish its final probe result between
                        // the earlier snapshot read and this EOF observation.
                        let (_, probe_passed) = snapshot.take();
                        if probe_passed {
                            self.notice = "通信探针通过，未注入任何输入。".into();
                        }
                        if let Some((input, terminal)) = snapshot.take_input() {
                            self.input_terminal = terminal;
                            if input == WorkerInputState::RecoveryUnconfirmed {
                                self.recovery_blocked = true;
                            }
                            self.input_state = Some(input);
                        }
                    }
                    if self.events.is_some() {
                        self.status = "进程已退出，正在读取最终诊断".into();
                        return;
                    }
                    let clean = if native {
                        self.native_cleanup_reported()
                    } else {
                        exit.success()
                    };
                    if !clean && native {
                        self.recovery_blocked = true;
                        self.input_state = Some(WorkerInputState::RecoveryUnconfirmed);
                        self.notice = "工作进程已退出，输入恢复未确认。".into();
                    }
                    self.child = None;
                    self.active_mode = None;
                    self.stopping = None;
                    if clean && !self.recovery_blocked {
                        if let Some(mode) = self.pending_start.take() {
                            let result = self.start_mode(mode);
                            self.feedback(result, "键鼠会话正在启动，控制归属以工作进程报告为准");
                        }
                    } else {
                        self.pending_start = None;
                    }
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
            Err(e) => format!("操作未完成：{e:#}"),
        };
    }
}
impl App {
    fn native_exit_protected(&self) -> bool {
        !cfg!(target_os = "macos")
            || (self.application_menu_error.is_none()
                && self
                    .application_menu
                    .as_ref()
                    .is_some_and(|owner| owner.is_available()))
    }
    fn check_exit_protection(&mut self, ctx: &egui::Context) {
        if cfg!(target_os = "macos")
            && self.application_menu.is_some()
            && self.application_menu_error.is_none()
            && !self.native_exit_protected()
        {
            self.exit_protection_lost(ctx);
        }
    }
    fn exit_protection_lost(&mut self, ctx: &egui::Context) {
        self.application_menu_error = Some("应用退出入口发生变化".into());
        self.native_confirmation = false;
        self.pending_start = None;
        if self.active_mode == Some(SessionMode::Native) {
            self.stop();
        }
        self.show_window(ctx);
        self.notice = "退出恢复入口发生变化，已请求停止键鼠。请确认恢复状态并重新打开屏跃。".into();
    }
    fn request_activity_tick(&self, ctx: &egui::Context) {
        // Native capture/watchdogs stay in the worker. GUI polling is needed
        // only while local asynchronous work or canvas deadlines are pending.
        if self.child.is_some()
            || self.events.is_some()
            || self.stopping.is_some()
            || self.pending_start.is_some()
            || self.discovery.needs_poll()
            || self.worker_permissions.needs_poll()
            || self.gesture_lab.needs_tick()
            || self.layout_report.is_some()
        {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }
    fn show_window(&mut self, ctx: &egui::Context) {
        self.window_hidden = false;
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    }
    fn sync_tray(&mut self, ctx: &egui::Context) {
        if !self.desktop_preferences.tray_enabled || self.instance_lock.is_none() {
            if self.window_hidden {
                self.show_window(ctx);
            }
            self.tray = None;
            self.process_tray_commands(ctx);
            return;
        }
        if self.tray.as_ref().is_some_and(|tray| tray.is_available()) {
            return;
        }
        self.tray = None;
        self.process_tray_commands(ctx);
        if self.close_requested || self.exit_confirmed {
            return;
        }
        if self.window_hidden {
            self.show_window(ctx);
        }
        match peerglide_platform::tray::create(self.tray_mailbox.clone()) {
            Ok(tray) => {
                self.tray = Some(tray);
            }
            Err(error) => {
                self.notice = format!("无法显示菜单栏／托盘：{error:#}。关闭窗口仍会停止并退出。");
            }
        }
    }
    fn set_desktop_preferences(
        &mut self,
        ctx: &egui::Context,
        preferences: DesktopPreferences,
    ) -> Result<()> {
        anyhow::ensure!(self.instance_lock.is_some(), "另一窗口正在使用此资料");
        self.desktop_store
            .as_mut()
            .context("桌面选项只读，原文件已保留")?
            .save(&preferences)?;
        self.desktop_preferences = preferences;
        self.sync_tray(ctx);
        Ok(())
    }
    fn process_tray_commands(&mut self, ctx: &egui::Context) {
        let pending = self.tray_mailbox.take();
        if pending.quit {
            if let Some(check) = self.quit_check.as_mut() {
                check.received_quit();
            }
            self.close_requested = true;
            self.stop();
            if self.child.is_some() || self.recovery_blocked {
                self.show_window(ctx);
                self.dialog = Some(Dialog::Recovery);
            }
        } else {
            if pending.stop {
                self.stop();
            }
            if pending.show && !self.close_requested && !self.exit_confirmed {
                self.show_window(ctx);
            }
        }
    }
    fn process_window_close(&mut self, ctx: &egui::Context, tray_available: bool) {
        if tray_available
            && self.desktop_preferences.tray_enabled
            && self.desktop_preferences.close_to_tray
            && !self.close_requested
            && !self.recovery_blocked
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            self.window_hidden = true;
            self.native_confirmation = false;
            self.screen_drag = None;
            self.gesture_lab.cancel();
        } else if self.child.is_some() || self.recovery_blocked {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.close_requested = true;
            self.stop();
            self.dialog = Some(Dialog::Recovery);
            self.show_window(ctx);
        } else {
            self.exit_confirmed = true;
        }
    }
    fn logic_tick(&mut self, ctx: &egui::Context) {
        if let Some(check) = self.quit_check.as_mut() {
            let unsafe_context = self.child.is_some()
                || self.config.fingerprint.is_some()
                || self.config.native
                || self.config.allow
                || self.pending_start.is_some();
            let abort = check.tick(self.application_menu.as_ref(), unsafe_context);
            ctx.request_repaint_after(Duration::from_millis(50));
            if abort {
                self.close_requested = true;
                self.stop();
            }
        }
        self.check_exit_protection(ctx);
        // Stop/Quit must cancel a queued upgrade before poll can start a successor.
        self.process_tray_commands(ctx);
        self.poll();
        if self.window_hidden && self.recovery_blocked {
            self.show_window(ctx);
            self.dialog = Some(Dialog::Recovery);
        }
        if ctx.input(|i| {
            !i.focused
                || i.events
                    .iter()
                    .any(|event| matches!(event, egui::Event::WindowFocused(false)))
                || i.viewport().minimized.unwrap_or(false)
                || i.viewport().occluded.unwrap_or(false)
        }) {
            self.gesture_lab.cancel();
            self.screen_drag = None;
            self.native_confirmation = false;
        }
        if ctx.input(|i| i.viewport().close_requested()) && !self.exit_confirmed {
            self.process_window_close(
                ctx,
                self.tray.as_ref().is_some_and(|tray| tray.is_available()),
            );
        }
        if self.close_requested && self.child.is_none() && !self.recovery_blocked {
            if let Some(check) = self.quit_check.take() {
                let no_input = self.active_mode.is_none()
                    && self.events.is_none()
                    && !self.config.native
                    && !self.config.allow
                    && self.config.fingerprint.is_none();
                if let Err(error) = check.finish(no_input) {
                    self.notice = format!("退出检查回执写入失败：{error:#}");
                }
            }
            self.exit_confirmed = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        self.request_activity_tick(ctx);
    }
}
impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        self.logic_tick(ctx);
    }
    fn ui(&mut self, root: &mut egui::Ui, _: &mut eframe::Frame) {
        self.desktop(root);
        // UI actions may start work after logic ran for this frame. Schedule
        // its first poll even if the app was completely idle before the click.
        self.request_activity_tick(root.ctx());
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
    let quit_check = options
        .macos_quit_check_report
        .as_ref()
        .map(|report| {
            quit_check::Check::prepare(
                options
                    .data_dir
                    .as_deref()
                    .expect("clap requires an isolated directory"),
                report,
            )
        })
        .transpose()
        .unwrap_or_else(|error| {
            eprintln!("退出检查无法开始：{error:#}");
            std::process::exit(2);
        });
    let dir = options.data_dir.unwrap_or_else(default_data_dir);
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1180.0, 820.0])
            .with_min_inner_size([920.0, 700.0]),
        ..Default::default()
    };
    eframe::run_native(
        "屏跃 · PeerGlide",
        native_options,
        Box::new(move |cc| {
            let mut app = App::new(cc, dir);
            app.quit_check_mode = quit_check.is_some();
            app.quit_check = quit_check;
            if options.gesture_lab {
                app.page = Page::GestureLab;
                app.mouse_tab = MouseTab::Practice;
                app.recording_mode = true;
                app.gesture_lab.set_export_open(true);
            }
            app.layout_report = options.ui_layout_report;
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
    fn communication_and_probe_commands_never_inherit_saved_native_or_receive_grants() -> Result<()>
    {
        let fixture = Fixture::new()?;
        let mut app = App::load(fixture.path.clone());
        app.config.native = true;
        app.config.allow = true;
        app.config.listen = false;
        for mode in [SessionMode::Communication, SessionMode::Probe] {
            let command = app.worker_command(
                PathBuf::from("not-executed-worker"),
                PathBuf::from("public-test.crt"),
                mode,
            );
            let args: Vec<_> = command
                .get_args()
                .map(|s| s.to_string_lossy().to_string())
                .collect();
            assert!(!args
                .iter()
                .any(|s| s == "--native" || s == "--allow-control"));
            assert_eq!(
                args.iter().any(|s| s == "--probe"),
                mode == SessionMode::Probe
            );
        }
        let native = app.worker_command(
            PathBuf::from("not-executed-worker"),
            PathBuf::from("public-test.crt"),
            SessionMode::Native,
        );
        let args: Vec<_> = native
            .get_args()
            .map(|s| s.to_string_lossy().to_string())
            .collect();
        assert!(
            args.iter().any(|s| s == "--native") && args.iter().any(|s| s == "--allow-control")
        );
        Ok(())
    }
    #[test]
    fn resend_stop_really_writes_again_without_resetting_recovery_deadline() -> Result<()> {
        use std::io::Read;
        let fixture = Fixture::new()?;
        let mut app = App::load(fixture.path.clone());
        let python = if cfg!(windows) { "python" } else { "python3" };
        let mut child = Command::new(python)
            .args([
                "-c",
                "import sys; print(len([sys.stdin.readline(), sys.stdin.readline()]), flush=True)",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        let mut out = child.stdout.take().unwrap();
        app.child = Some(child);
        app.stop();
        let deadline = app.stopping;
        app.recovery_blocked = true;
        app.resend_stop()?;
        let mut output = String::new();
        out.read_to_string(&mut output)?;
        assert_eq!(output.trim(), "2");
        assert_eq!(app.stopping, deadline);
        assert!(app.recovery_blocked);
        app.child.take().unwrap().wait()?;
        Ok(())
    }
    #[test]
    fn stop_after_terminal_receipt_preserves_cleanup_while_process_is_exiting() -> Result<()> {
        for state in [WorkerInputState::Local, WorkerInputState::Disabled] {
            let fixture = Fixture::new()?;
            let mut app = App::load(fixture.path.clone());
            let python = if cfg!(windows) { "python" } else { "python3" };
            app.child = Some(
                Command::new(python)
                    .args(["-c", "import sys; sys.stdin.readline()"])
                    .stdin(Stdio::piped())
                    .spawn()?,
            );
            app.active_mode = Some(SessionMode::Native);
            app.input_state = Some(state);
            app.input_terminal = true;
            let drained = Arc::new(WorkerSnapshot::default());
            drained.2.store(true, Ordering::Release);
            app.worker_state = Some(drained);
            app.stopping = Some(Instant::now() - Duration::from_secs(4));
            app.poll();
            app.stop();
            assert_eq!(app.input_state, Some(state));
            assert!(app.native_cleanup_reported());
            assert!(!app.recovery_blocked);
            let child = app.child.as_mut().unwrap();
            child.stdin.as_mut().unwrap().write_all(b"exit\n")?;
            assert!(child.wait()?.success());
            app.poll();
            assert!(app.child.is_none());
            assert!(!app.recovery_blocked);
        }
        Ok(())
    }
    #[test]
    fn communication_stop_timeout_cancels_upgrade_without_inventing_input_failure() -> Result<()> {
        let fixture = Fixture::new()?;
        let mut app = App::load(fixture.path.clone());
        app.active_mode = Some(SessionMode::Communication);
        app.pending_start = Some(SessionMode::Native);
        app.mark_stop_timeout();
        assert!(!app.recovery_blocked);
        assert!(app.pending_start.is_none());
        app.active_mode = Some(SessionMode::Native);
        app.mark_stop_timeout();
        assert!(app.recovery_blocked);
        Ok(())
    }
    #[test]
    fn old_routing_state_and_process_eof_are_not_a_terminal_cleanup_receipt() -> Result<()> {
        let fixture = Fixture::new()?;
        let mut app = App::load(fixture.path.clone());
        for input in [WorkerInputState::Local, WorkerInputState::Disabled] {
            app.input_state = Some(input);
            app.input_terminal = false;
            assert!(!app.native_cleanup_reported());
        }
        let drained = Arc::new(WorkerSnapshot::default());
        drained.2.store(true, Ordering::Release);
        app.worker_state = Some(drained.clone());
        app.input_terminal = true;
        assert!(app.native_cleanup_reported());
        drained.3.store(true, Ordering::Release);
        assert!(!app.native_cleanup_reported());
        drained.3.store(false, Ordering::Release);
        app.recovery_blocked = true;
        assert!(!app.native_cleanup_reported());
        let snapshot = WorkerSnapshot::default();
        snapshot.publish_input(WorkerInputState::Local, false);
        assert_eq!(
            snapshot.take_input(),
            Some((WorkerInputState::Local, false))
        );
        snapshot.publish_input(WorkerInputState::Local, true);
        assert_eq!(snapshot.take_input(), Some((WorkerInputState::Local, true)));
        Ok(())
    }
    #[test]
    fn authentication_and_saved_preferences_do_not_invent_input_ownership() -> Result<()> {
        let fixture = Fixture::new()?;
        let mut app = App::load(fixture.path.clone());
        app.config.native = true;
        app.connected = true;
        app.status = "已通过身份验证".into();
        assert_eq!(app.input_label(), "尚未开启键鼠会话");
        app.input_state = Some(WorkerInputState::ReceivingRemote);
        assert_eq!(app.input_label(), "对端正在控制本机");
        app.recovery_blocked = true;
        assert_eq!(app.input_label(), "输入恢复未确认");
        app.native_confirmation = true;
        assert!(app.request_native().is_err());
        assert!(app.child.is_none());
        Ok(())
    }
    #[test]
    fn workspace_start_only_opens_fresh_consent_and_readonly_recovery_blocks_it() -> Result<()> {
        let fixture = Fixture::new()?;
        let mut app = App::load(fixture.path.clone());
        // Presentation fixture only: no identity, certificate or worker is created.
        app.config.fingerprint = Some("presentation-only".into());
        app.native_confirmation = true;
        app.workspace_action(desktop_state::Action::StartInput);
        if cfg!(any(windows, target_os = "macos")) {
            assert_eq!(app.dialog, Some(Dialog::NativeStart));
            assert!(!app.native_confirmation);
        }
        assert!(app.child.is_none());
        assert!(!app.config.native && !app.config.allow);
        assert!(!app.identity_dir().exists());
        app.native_confirmation = true;
        app.stop();
        assert!(!app.native_confirmation);
        app.native_confirmation = true;
        app.workspace_action(desktop_state::Action::AddDevice);
        assert!(!app.native_confirmation && app.dialog.is_none());
        assert!(app.wizard.is_some());
        app.wizard = None;
        app.dialog = None;
        app.recovery_blocked = true;
        app.workspace_action(desktop_state::Action::StartInput);
        assert!(app.dialog.is_none());
        app.workspace_action(desktop_state::Action::Recovery);
        assert_eq!(app.dialog, Some(Dialog::Recovery));
        let mut readonly = App::load(fixture.path.clone());
        readonly.config.fingerprint = app.config.fingerprint.clone();
        for action in [
            desktop_state::Action::StartInput,
            desktop_state::Action::Connect,
            desktop_state::Action::AddDevice,
        ] {
            readonly.workspace_action(action);
            assert!(
                readonly.child.is_none() && readonly.dialog.is_none() && readonly.wizard.is_none()
            );
        }
        Ok(())
    }
    #[test]
    fn workspace_primary_action_stays_visible_in_supported_windows() -> Result<()> {
        fn text_rect(shape: &egui::epaint::Shape, label: &str) -> Option<egui::Rect> {
            match shape {
                egui::epaint::Shape::Text(text) if text.galley.job.text == label => {
                    Some(text.visual_bounding_rect())
                }
                egui::epaint::Shape::Vec(shapes) => {
                    shapes.iter().find_map(|shape| text_rect(shape, label))
                }
                _ => None,
            }
        }
        let fixture = Fixture::new()?;
        let mut app = App::load(fixture.path.clone());
        // Rendering metadata only. No peer certificate, identity or worker is created.
        app.config.fingerprint = Some("workspace-presentation-only".into());
        let ctx = egui::Context::default();
        install_fonts(&ctx);
        let label = app.workspace_view().action.label();
        for dark in [false, true] {
            app.config.dark = dark;
            app.theme(&ctx);
            for size in [Vec2::new(920.0, 700.0), Vec2::new(1180.0, 820.0)] {
                for page in [Page::Home, Page::Devices] {
                    app.page = page;
                    let output = ctx.run_ui(
                        egui::RawInput {
                            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                            ..Default::default()
                        },
                        |ui| app.desktop(ui),
                    );
                    let (clip, bounds) = output
                        .shapes
                        .iter()
                        .find_map(|paint| {
                            text_rect(&paint.shape, label).map(|bounds| (paint.clip_rect, bounds))
                        })
                        .context("primary action was not painted in the initial viewport")?;
                    assert!(
                        clip.contains_rect(bounds),
                        "primary action clipped at {size:?}: {bounds:?} outside {clip:?}"
                    );
                    assert!(app.child.is_none());
                    output.drop_without_applying_deltas();
                }
            }
        }
        Ok(())
    }
    #[test]
    fn workspace_pointer_drag_and_direction_buttons_commit_only_editable_layout() -> Result<()> {
        fn text_center(shape: &egui::epaint::Shape, label: &str) -> Option<egui::Pos2> {
            match shape {
                egui::epaint::Shape::Text(text) if text.galley.job.text == label => {
                    Some(text.visual_bounding_rect().center())
                }
                egui::epaint::Shape::Vec(shapes) => {
                    shapes.iter().find_map(|shape| text_center(shape, label))
                }
                _ => None,
            }
        }
        fn frame(
            app: &mut App,
            ctx: &egui::Context,
            time: f64,
            events: Vec<egui::Event>,
        ) -> egui::FullOutput {
            ctx.run_ui(
                egui::RawInput {
                    time: Some(time),
                    events,
                    focused: true,
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        Vec2::new(920.0, 700.0),
                    )),
                    ..Default::default()
                },
                |ui| app.desktop(ui),
            )
        }
        fn step(app: &mut App, ctx: &egui::Context, time: f64, events: Vec<egui::Event>) {
            frame(app, ctx, time, events).drop_without_applying_deltas();
        }
        fn button(pos: egui::Pos2, pressed: bool) -> egui::Event {
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: Default::default(),
            }
        }
        fn press(pos: egui::Pos2) -> Vec<egui::Event> {
            vec![egui::Event::PointerMoved(pos), button(pos, true)]
        }
        let fixture = Fixture::new()?;
        let mut app = App::load(fixture.path.clone());
        // Presentation fixture only: this test never authenticates or starts a worker.
        app.config.fingerprint = Some("layout-presentation-only".into());
        app.config.peer_name = "B".into();
        app.page = Page::Devices;
        app.device_tab = DeviceTab::Layout;
        let ctx = egui::Context::default();
        install_fonts(&ctx);
        app.theme(&ctx);
        step(&mut app, &ctx, 0.0, vec![]);
        let output = frame(&mut app, &ctx, 0.1, vec![]);
        let start = output
            .shapes
            .iter()
            .find_map(|paint| text_center(&paint.shape, "B"))
            .context("peer screen label not painted")?;
        output.drop_without_applying_deltas();
        let end = start - Vec2::new(260.0, 0.0);
        step(&mut app, &ctx, 0.2, press(start));
        step(&mut app, &ctx, 0.3, vec![egui::Event::PointerMoved(end)]);
        assert!(
            app.screen_drag.is_some(),
            "drag did not acquire the peer screen"
        );
        step(&mut app, &ctx, 0.4, vec![button(end, false)]);
        assert!(
            app.screen_drag.is_none(),
            "released drag left the layout locked"
        );
        assert_eq!(app.config.edge, "left");
        let stored: serde_json::Value =
            serde_json::from_slice(&std::fs::read(app.dir.join("settings.json"))?)?;
        assert_eq!(stored["data"]["edge"], "left");
        let output = frame(&mut app, &ctx, 0.5, vec![]);
        let right = output
            .shapes
            .iter()
            .find_map(|paint| text_center(&paint.shape, "右侧"))
            .context("direction button not painted")?;
        output.drop_without_applying_deltas();
        step(&mut app, &ctx, 0.6, press(right));
        step(&mut app, &ctx, 0.7, vec![button(right, false)]);
        assert_eq!(app.config.edge, "right");
        // Paint the new position before pressing it: egui hit testing uses painted geometry.
        step(&mut app, &ctx, 0.75, vec![]);
        step(
            &mut app,
            &ctx,
            0.8,
            vec![
                egui::Event::PointerMoved(start),
                button(start, true),
                egui::Event::PointerMoved(end),
                button(end, false),
            ],
        );
        assert!(app.screen_drag.is_none());
        assert_eq!(app.config.edge, "left", "coalesced drag was lost");
        step(&mut app, &ctx, 0.81, press(right));
        step(&mut app, &ctx, 0.82, vec![button(right, false)]);
        assert_eq!(app.config.edge, "right");
        let before = std::fs::read(app.dir.join("settings.json"))?;
        drop(app.instance_lock.take());
        step(&mut app, &ctx, 0.85, vec![]);
        step(&mut app, &ctx, 0.9, press(start));
        step(&mut app, &ctx, 1.0, vec![egui::Event::PointerMoved(end)]);
        step(&mut app, &ctx, 1.1, vec![button(end, false)]);
        assert!(app.screen_drag.is_none());
        assert_eq!(app.config.edge, "right");
        assert_eq!(std::fs::read(app.dir.join("settings.json"))?, before);
        assert!(app.child.is_none());
        Ok(())
    }
    #[test]
    fn desktop_preferences_preserve_input_schema_and_future_desktop_file() -> Result<()> {
        let fixture = Fixture::new()?;
        let mut app = App::load(fixture.path.clone());
        app.save_config()?;
        let original = std::fs::read(app.dir.join("settings.json"))?;
        let preferences = DesktopPreferences {
            tray_enabled: true,
            close_to_tray: true,
        };
        app.desktop_store.as_mut().unwrap().save(&preferences)?;
        assert_eq!(std::fs::read(app.dir.join("settings.json"))?, original);
        drop(app);
        let app = App::load(fixture.path.clone());
        assert!(app.desktop_preferences.tray_enabled && app.desktop_preferences.close_to_tray);
        assert!(app.tray.is_none() && app.child.is_none());
        drop(app);
        let future = br#"{"store_version":999,"data":{"tray_enabled":true}}"#;
        std::fs::write(fixture.path.join("desktop-preferences.json"), future)?;
        let app = App::load(fixture.path.clone());
        assert!(app.desktop_store.is_none());
        assert!(!app.desktop_preferences.tray_enabled && !app.desktop_preferences.close_to_tray);
        assert_eq!(
            std::fs::read(app.dir.join("desktop-preferences.json"))?,
            future
        );
        assert_eq!(std::fs::read(app.dir.join("settings.json"))?, original);
        Ok(())
    }
    #[test]
    fn full_desktop_and_dialogs_render_without_starting_workers_or_exporting_samples() -> Result<()>
    {
        let fixture = Fixture::new()?;
        let mut app = App::load(fixture.path.clone());
        let ctx = egui::Context::default();
        install_fonts(&ctx);
        for page in [
            Page::Home,
            Page::Devices,
            Page::GestureLab,
            Page::Transfers,
            Page::Settings,
        ] {
            app.page = page;
            for dialog in [
                None,
                Some(Dialog::NativeStart),
                Some(Dialog::Recovery),
                Some(Dialog::Reset),
            ] {
                app.dialog = dialog;
                let output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            Vec2::new(1180.0, 820.0),
                        )),
                        ..Default::default()
                    },
                    |ui| app.desktop(ui),
                );
                assert!(!output.shapes.is_empty());
                output.drop_without_applying_deltas();
                assert!(app.child.is_none());
            }
        }
        assert!(!app.identity_dir().exists());
        assert!(!app.dir.join("gesture-samples").exists());
        Ok(())
    }
    #[test]
    fn bundled_font_covers_chinese_ui_without_system_fonts() {
        let ctx = egui::Context::default();
        install_fonts(&ctx);
        let output = ctx.run_ui(egui::RawInput::default(), |ui| {
            for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                let font = egui::FontId::new(16.0, family);
                assert!(ui.fonts_mut(|fonts| fonts.has_glyphs(
                    &font,
                    "屏跃主页设备显示器传输设置手势实验室规则离线可信权限恢复"
                )));
            }
            ui.label("屏跃：多台电脑，一种体验。");
        });
        assert!(!output.shapes.is_empty());
        output.drop_without_applying_deltas();
    }

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
    fn pasted_connection_material_keeps_following_controls_within_a_small_window() -> Result<()> {
        let fixture = Fixture::new()?;
        let mut app = App::load(fixture.path.clone());
        app.connection_text = connection_info::encode(&Identity::generate()?.certificate, &[])?;
        let ctx = egui::Context::default();
        install_fonts(&ctx);
        for width in [300.0, 600.0] {
            let output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        Vec2::new(width, 600.0),
                    )),
                    ..Default::default()
                },
                |root| {
                    egui::CentralPanel::default().show(root, |ui| {
                        let top = ui.cursor().min.y;
                        app.connection_exchange_ui(ui);
                        assert!(ui.cursor().min.y - top < 400.0, "pasted certificate pushed read/confirmation controls below a small window");
                    });
                },
            );
            output.drop_without_applying_deltas();
        }
        Ok(())
    }
    #[test]
    fn public_connection_exchange_enrolls_both_peers_without_candidate_files() -> Result<()> {
        let waiting = Fixture::new()?;
        let connecting = Fixture::new()?;
        let mut a = App::load(waiting.path.clone());
        let mut b = App::load(connecting.path.clone());
        let identity_a = Identity::create(&a.identity_dir())?;
        let identity_b = Identity::create(&b.identity_dir())?;
        assert!(
            a.config.address.is_empty(),
            "no example IP may look like a real peer"
        );
        let address = "192.168.20.8:24870".parse()?;
        b.connection_text = connection_info::encode(&identity_a.certificate, &[address])?;
        b.read_connection_info()?;
        assert!(!b.wizard_listen);
        assert_eq!(b.wizard_address, address.to_string());
        assert!(b.config.fingerprint.is_none());
        assert!(!connecting.path.join("trusted-peer.crt.der").exists());
        assert!(
            b.enroll().is_err(),
            "reading public data must not confer trust"
        );
        b.confirmed_fingerprint = Some(identity_a.fingerprint());
        b.config.address = b.wizard_address.clone();
        b.config.listen = b.wizard_listen;
        b.enroll()?;
        assert!(!b.config.allow && !b.config.native);
        // The connector's reply needs no address, and selects the waiting role.
        a.wizard_listen = false;
        a.wizard_address = "192.168.20.99:24870".into();
        a.connection_text = connection_info::encode(&identity_b.certificate, &[])?;
        a.read_connection_info()?;
        assert!(a.wizard_listen);
        assert!(a.wizard_address.is_empty());
        a.confirmed_fingerprint = Some(identity_b.fingerprint());
        a.enroll()?;
        assert!(!a.config.allow && !a.config.native);
        assert_eq!(
            transport::read_certificate(&waiting.path.join("trusted-peer.crt.der"))?,
            identity_b.certificate
        );
        assert_eq!(
            transport::read_certificate(&connecting.path.join("trusted-peer.crt.der"))?,
            identity_a.certificate
        );
        assert_eq!(
            Identity::load(&a.identity_dir())?.certificate,
            identity_a.certificate
        );
        assert_eq!(
            Identity::load(&b.identity_dir())?.certificate,
            identity_b.certificate
        );
        Ok(())
    }
    #[test]
    fn edited_unread_self_and_invalid_connection_info_cannot_reuse_confirmation() -> Result<()> {
        let fixture = Fixture::new()?;
        let mut app = App::load(fixture.path.clone());
        let local = Identity::create(&app.identity_dir())?;
        let peer = Identity::generate()?;
        app.connection_text =
            connection_info::encode(&peer.certificate, &["192.168.20.8:24870".parse()?])?;
        app.read_connection_info()?;
        app.confirmed_fingerprint = Some(peer.fingerprint());
        // Even editing just the address requires a fresh read and confirmation.
        app.connection_text =
            connection_info::encode(&peer.certificate, &["192.168.20.9:24870".parse()?])?;
        assert!(app.enroll().is_err());
        assert!(!fixture.path.join("trusted-peer.crt.der").exists());
        app.read_connection_info()?;
        assert!(app.confirmed_fingerprint.is_none());
        app.confirmed_fingerprint = Some(peer.fingerprint());
        app.connection_text = "invalid public connection data".into();
        assert!(app.read_connection_info().is_err());
        assert!(app.loaded_connection_info.is_none());
        assert!(app.confirmed_fingerprint.is_none());
        assert!(app.enroll().is_err());
        app.connection_text = connection_info::encode(&local.certificate, &[])?;
        assert!(app.read_connection_info().is_err());
        assert!(app.loaded_connection_info.is_none());
        assert!(app.config.fingerprint.is_none());
        assert!(!app.config.allow && !app.config.native);
        Ok(())
    }
    #[test]
    fn multihomed_connection_info_requires_explicit_address_selection() -> Result<()> {
        let fixture = Fixture::new()?;
        let mut app = App::load(fixture.path.clone());
        Identity::create(&app.identity_dir())?;
        let peer = Identity::generate()?;
        let addresses = ["192.168.20.8:24870".parse()?, "10.0.0.8:24870".parse()?];
        app.connection_text = connection_info::encode(&peer.certificate, &addresses)?;
        app.read_connection_info()?;
        assert_eq!(app.connection_addresses, addresses);
        assert!(app.wizard_address.is_empty());
        assert!(!app.wizard_listen);
        // A second GUI cannot import into a profile locked by the first.
        let mut second = App::load(fixture.path.clone());
        second.connection_text = app.connection_text.clone();
        assert!(second.read_connection_info().is_err());
        assert!(second.config.fingerprint.is_none());
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
            assert!(app.start(false).unwrap_err().to_string().contains("变化"));
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
    fn settings_conflict_during_enrollment_preserves_existing_peer_and_consumes_confirmation(
    ) -> Result<()> {
        let fixture = Fixture::new()?;
        let mut app = App::load(fixture.path.clone());
        Identity::create(&app.identity_dir())?;
        let original = Identity::generate()?;
        let candidate = fixture.path.join("candidate.der");
        std::fs::write(&candidate, &original.certificate)?;
        app.import_path = candidate.display().to_string();
        app.confirmed_fingerprint = Some(original.fingerprint());
        app.enroll()?;
        let other = Identity::generate()?;
        std::fs::write(&candidate, &other.certificate)?;
        app.confirmed_fingerprint = Some(other.fingerprint());
        app.native_confirmation = true;
        app.pending_start = Some(SessionMode::Native);
        let future = br#"{"store_version":999,"data":{}}"#;
        std::fs::write(fixture.path.join("settings.json"), future)?;
        assert!(app.enroll().is_err());
        assert_eq!(app.config.fingerprint, Some(original.fingerprint()));
        assert!(app.confirmed_fingerprint.is_none() && app.config_store.is_none());
        assert!(!app.native_confirmation && app.pending_start.is_none() && app.child.is_none());
        assert_eq!(std::fs::read(fixture.path.join("settings.json"))?, future);
        assert_eq!(
            std::fs::read(fixture.path.join("trusted-peer.crt.der"))?,
            original.certificate
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
            install_fonts(&ctx);
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
    fn count_activity_ticks(ctx: &egui::Context) -> Arc<std::sync::atomic::AtomicUsize> {
        let ticks = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed = ticks.clone();
        ctx.set_request_repaint_callback(move |request| {
            if request.delay == Duration::from_millis(100) {
                observed.fetch_add(1, Ordering::SeqCst);
            }
        });
        ticks
    }
    fn logic_frames(app: &mut App, ctx: &egui::Context, occluded: bool) {
        let mut input = egui::RawInput::default();
        input
            .viewports
            .entry(egui::ViewportId::ROOT)
            .or_default()
            .occluded = Some(occluded);
        for _ in 0..4 {
            let _ = ctx.run_logic(&input, |ctx| app.logic_tick(ctx));
        }
    }
    fn managed_child() -> Result<Child> {
        Ok(Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "lifecycle_tests::managed_test_child",
                "--nocapture",
            ])
            .env("PEERGLIDE_GUI_LIFECYCLE_HELPER", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?)
    }
    #[test]
    fn tray_close_requires_user_choice_and_a_live_recovery_entry() -> Result<()> {
        let fixture = super::tests::Fixture::new()?;
        let mut app = App::load(fixture.path.clone());
        app.child = Some(managed_child()?);
        app.active_mode = Some(SessionMode::Communication);
        app.desktop_preferences.tray_enabled = true;
        app.desktop_preferences.close_to_tray = true;
        let ctx = egui::Context::default();
        let _ = ctx.run_logic(&egui::RawInput::default(), |ctx| {
            app.process_window_close(ctx, true);
        });
        assert!(app.window_hidden && app.stopping.is_none() && !app.close_requested);
        assert!(app.child.is_some());
        app.tray_mailbox
            .post(peerglide_platform::tray::Command::Show);
        logic_frames(&mut app, &ctx, false);
        assert!(!app.window_hidden && app.stopping.is_none());
        let _ = ctx.run_logic(&egui::RawInput::default(), |ctx| {
            app.process_window_close(ctx, false);
        });
        assert!(app.close_requested && !app.window_hidden);
        assert!(app.stopping.is_some() || app.child.is_none());
        Ok(())
    }
    #[test]
    fn tray_quit_cancels_upgrade_before_poll_and_unknown_cleanup_reveals_window() -> Result<()> {
        let fixture = super::tests::Fixture::new()?;
        let mut app = App::load(fixture.path.clone());
        app.child = Some(managed_child()?);
        app.active_mode = Some(SessionMode::Communication);
        app.pending_start = Some(SessionMode::Native);
        app.window_hidden = true;
        app.tray_mailbox
            .post(peerglide_platform::tray::Command::Show);
        app.tray_mailbox
            .post(peerglide_platform::tray::Command::Quit);
        let ctx = egui::Context::default();
        logic_frames(&mut app, &ctx, true);
        assert!(app.close_requested && !app.window_hidden);
        assert!(app.pending_start.is_none());
        assert!(app.stopping.is_some() || app.child.is_none());
        assert_ne!(app.active_mode, Some(SessionMode::Native));
        app.recovery_blocked = true;
        app.window_hidden = true;
        app.exit_confirmed = false;
        logic_frames(&mut app, &ctx, true);
        assert!(!app.window_hidden && !app.exit_confirmed);
        assert_eq!(app.dialog, Some(Dialog::Recovery));
        Ok(())
    }
    #[test]
    fn removing_or_rebuilding_tray_consumes_late_stop_and_quit_intents() -> Result<()> {
        for enabled in [false, true] {
            let fixture = super::tests::Fixture::new()?;
            let mut app = App::load(fixture.path.clone());
            app.child = Some(managed_child()?);
            app.active_mode = Some(SessionMode::Communication);
            app.pending_start = Some(SessionMode::Native);
            app.desktop_preferences.tray_enabled = enabled;
            app.tray_mailbox
                .post(peerglide_platform::tray::Command::Stop);
            app.tray_mailbox
                .post(peerglide_platform::tray::Command::Quit);
            let ctx = egui::Context::default();
            let _ = ctx.run_logic(&egui::RawInput::default(), |ctx| app.sync_tray(ctx));
            assert!(app.close_requested && app.pending_start.is_none());
            assert!(app.stopping.is_some());
            assert!(app.tray.is_none());
            assert!(!app.tray_mailbox.take().quit);
        }
        Ok(())
    }
    #[test]
    fn application_menu_handle_survives_tray_disable_and_quit_preempts_upgrade() -> Result<()> {
        let fixture = super::tests::Fixture::new()?;
        let mut app = App::load(fixture.path.clone());
        let handle = app.tray_mailbox.clone();
        let ctx = egui::Context::default();
        app.desktop_preferences.tray_enabled = false;
        let _ = ctx.run_logic(&egui::RawInput::default(), |ctx| app.sync_tray(ctx));
        assert!(Arc::ptr_eq(&handle, &app.tray_mailbox));
        app.window_hidden = true;
        handle.post(peerglide_platform::tray::Command::Show);
        logic_frames(&mut app, &ctx, false);
        assert!(!app.window_hidden);
        app.child = Some(managed_child()?);
        app.active_mode = Some(SessionMode::Communication);
        app.pending_start = Some(SessionMode::Native);
        app.desktop_preferences.close_to_tray = true;
        handle.post(peerglide_platform::tray::Command::Quit);
        logic_frames(&mut app, &ctx, true);
        assert!(app.close_requested && app.pending_start.is_none());
        assert!(app.stopping.is_some() || app.child.is_none());
        assert_ne!(app.active_mode, Some(SessionMode::Native));
        assert!(Arc::ptr_eq(&handle, &app.tray_mailbox));
        Ok(())
    }
    #[test]
    fn lost_exit_protection_stops_owned_helper_and_never_fakes_recovery() -> Result<()> {
        let fixture = super::tests::Fixture::new()?;
        let mut app = App::load(fixture.path.clone());
        // UI-state fixture only: helper reads Stop; no capture/injection exists.
        app.child = Some(managed_child()?);
        app.active_mode = Some(SessionMode::Native);
        app.pending_start = Some(SessionMode::Native);
        app.native_confirmation = true;
        app.window_hidden = true;
        let ctx = egui::Context::default();
        let _ = ctx.run_logic(&egui::RawInput::default(), |ctx| {
            app.exit_protection_lost(ctx)
        });
        assert!(!app.window_hidden && !app.native_confirmation && app.pending_start.is_none());
        assert!(app.application_menu_error.is_some() && app.stopping.is_some());
        app.recovery_blocked = true;
        app.tray_mailbox
            .post(peerglide_platform::tray::Command::Quit);
        logic_frames(&mut app, &ctx, true);
        assert!(!app.exit_confirmed && !app.window_hidden);
        assert_eq!(app.dialog, Some(Dialog::Recovery));
        Ok(())
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn missing_exit_owner_blocks_native_before_writing_preferences_or_spawning() -> Result<()> {
        let fixture = super::tests::Fixture::new()?;
        let mut app = App::load(fixture.path.clone());
        app.native_confirmation = true;
        assert!(app
            .request_native()
            .unwrap_err()
            .to_string()
            .contains("退出恢复入口"));
        assert!(!app.native_confirmation && !app.config.native && app.child.is_none());
        assert!(!fixture.path.join("settings.json").exists());
        let error = app.start_mode(SessionMode::Native).unwrap_err();
        assert!(error.to_string().contains("退出恢复入口"));
        assert!(!fixture.path.join("identity").exists());
        Ok(())
    }
    #[test]
    fn isolated_quit_check_cannot_save_or_start_any_device_session() -> Result<()> {
        let fixture = super::tests::Fixture::new()?;
        let mut app = App::load(fixture.path.clone());
        app.quit_check_mode = true;
        app.native_confirmation = true;
        assert!(app
            .request_native()
            .unwrap_err()
            .to_string()
            .contains("隔离退出检查"));
        for mode in [
            SessionMode::Communication,
            SessionMode::Native,
            SessionMode::Probe,
        ] {
            assert!(app
                .start_mode(mode)
                .unwrap_err()
                .to_string()
                .contains("隔离退出检查"));
        }
        assert!(app.save_config().is_err());
        assert!(!app.native_confirmation && !app.config.native && app.child.is_none());
        assert!(
            !fixture.path.join("settings.json").exists() && !fixture.path.join("identity").exists()
        );
        assert!(Options::try_parse_from([
            "peerglide-gui",
            "--macos-quit-check-report",
            "unused.json"
        ])
        .is_err());
        Ok(())
    }
    #[test]
    fn hidden_window_cancels_layout_and_uncommitted_consent_without_saving() -> Result<()> {
        let fixture = super::tests::Fixture::new()?;
        let mut app = App::load(fixture.path.clone());
        let original = app.config.edge.clone();
        app.native_confirmation = true;
        app.screen_drag = Some((Vec2::ZERO, egui::pos2(10.0, 10.0), egui::Rect::EVERYTHING));
        let ctx = egui::Context::default();
        logic_frames(&mut app, &ctx, true);
        assert!(app.screen_drag.is_none());
        assert!(!app.native_confirmation);
        assert_eq!(app.config.edge, original);
        assert!(!app.dir.join("settings.json").exists());
        assert!(app.child.is_none());
        app.native_confirmation = true;
        let input = egui::RawInput {
            focused: true,
            events: vec![
                egui::Event::WindowFocused(false),
                egui::Event::WindowFocused(true),
            ],
            ..Default::default()
        };
        let _ = ctx.run_logic(&input, |ctx| app.logic_tick(ctx));
        assert!(!app.native_confirmation);
        Ok(())
    }
    #[test]
    fn idle_logic_stops_periodic_paints_but_hidden_worker_and_stop_keep_ticking() -> Result<()> {
        let fixture = super::tests::Fixture::new()?;
        let mut app = App::load(fixture.path.clone());
        let ctx = egui::Context::default();
        let ticks = count_activity_ticks(&ctx);
        logic_frames(&mut app, &ctx, false);
        assert_eq!(
            ticks.load(Ordering::SeqCst),
            0,
            "idle GUI retained its 10Hz clock"
        );
        app.child = Some(managed_child()?);
        app.active_mode = Some(SessionMode::Communication);
        logic_frames(&mut app, &ctx, true);
        assert!(
            ticks.load(Ordering::SeqCst) > 0,
            "hidden worker status polling stopped"
        );
        app.stop();
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.child.is_some() && Instant::now() < deadline {
            logic_frames(&mut app, &ctx, true);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(app.child.is_none(), "cooperative stop was starved");
        assert!(!app.recovery_blocked);
        let settled = ticks.load(Ordering::SeqCst);
        logic_frames(&mut app, &ctx, true);
        assert_eq!(
            ticks.load(Ordering::SeqCst),
            settled,
            "finished work retained periodic polling"
        );
        drop(app);
        fixture.close()?;
        Ok(())
    }
    #[test]
    fn hidden_close_cancels_window_exit_until_cooperative_worker_stop() -> Result<()> {
        let fixture = super::tests::Fixture::new()?;
        let mut app = App::load(fixture.path.clone());
        app.child = Some(managed_child()?);
        app.active_mode = Some(SessionMode::Communication);
        let ctx = egui::Context::default();
        let mut input = egui::RawInput::default();
        let viewport = input.viewports.entry(egui::ViewportId::ROOT).or_default();
        viewport.occluded = Some(true);
        viewport.events.push(egui::ViewportEvent::Close);
        let output = ctx.run_logic(&input, |ctx| app.logic_tick(ctx));
        assert!(output
            .viewport_commands
            .get(&egui::ViewportId::ROOT)
            .is_some_and(|commands| commands.contains(&egui::ViewportCommand::CancelClose)));
        assert!(app.close_requested && app.stopping.is_some());
        assert!(!app.exit_confirmed);
        input
            .viewports
            .get_mut(&egui::ViewportId::ROOT)
            .unwrap()
            .events
            .clear();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut close_sent = false;
        while app.child.is_some() && Instant::now() < deadline {
            let output = ctx.run_logic(&input, |ctx| app.logic_tick(ctx));
            close_sent |= output
                .viewport_commands
                .get(&egui::ViewportId::ROOT)
                .is_some_and(|commands| commands.contains(&egui::ViewportCommand::Close));
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(app.child.is_none() && app.exit_confirmed && close_sent);
        assert!(!app.recovery_blocked);
        drop(app);
        fixture.close()?;
        Ok(())
    }
    #[test]
    fn hidden_close_preserves_unconfirmed_recovery_without_claiming_exit() -> Result<()> {
        let fixture = super::tests::Fixture::new()?;
        let mut app = App::load(fixture.path.clone());
        app.recovery_blocked = true;
        app.input_state = Some(WorkerInputState::RecoveryUnconfirmed);
        let ctx = egui::Context::default();
        let mut input = egui::RawInput::default();
        let viewport = input.viewports.entry(egui::ViewportId::ROOT).or_default();
        viewport.occluded = Some(true);
        viewport.events.push(egui::ViewportEvent::Close);
        let output = ctx.run_logic(&input, |ctx| app.logic_tick(ctx));
        assert!(output
            .viewport_commands
            .get(&egui::ViewportId::ROOT)
            .is_some_and(|commands| commands.contains(&egui::ViewportCommand::CancelClose)));
        assert!(app.close_requested && !app.exit_confirmed && app.recovery_blocked);
        assert!(app.dialog == Some(Dialog::Recovery));
        drop(app);
        fixture.close()?;
        Ok(())
    }
    #[test]
    fn late_diagnostics_finish_before_exit_and_idle_timer_settles() -> Result<()> {
        let fixture = super::tests::Fixture::new()?;
        let mut app = App::load(fixture.path.clone());
        let mut child = managed_child()?;
        child.stdin.as_mut().unwrap().write_all(b"stop\n")?;
        assert!(child.wait()?.success());
        app.child = Some(child);
        app.active_mode = Some(SessionMode::Communication);
        let (tx, rx) = mpsc::sync_channel(2);
        app.events = Some(rx);
        let ctx = egui::Context::default();
        let ticks = count_activity_ticks(&ctx);
        logic_frames(&mut app, &ctx, true);
        assert!(app.child.is_some() && ticks.load(Ordering::SeqCst) > 0);
        tx.send("final diagnostic fixture".into())?;
        drop(tx);
        logic_frames(&mut app, &ctx, true);
        assert!(app.child.is_none() && app.events.is_none());
        assert_eq!(
            app.logs.back().map(String::as_str),
            Some("final diagnostic fixture")
        );
        let settled = ticks.load(Ordering::SeqCst);
        logic_frames(&mut app, &ctx, true);
        assert_eq!(ticks.load(Ordering::SeqCst), settled);
        drop(app);
        fixture.close()?;
        Ok(())
    }
    #[test]
    fn communication_exit_waits_for_its_final_probe_report_before_becoming_idle() -> Result<()> {
        let fixture = super::tests::Fixture::new()?;
        let mut app = App::load(fixture.path.clone());
        let mut child = managed_child()?;
        child.stdin.as_mut().unwrap().write_all(b"stop\n")?;
        assert!(child.wait()?.success());
        app.child = Some(child);
        app.active_mode = Some(SessionMode::Probe);
        let snapshot = Arc::new(WorkerSnapshot::default());
        app.worker_state = Some(snapshot.clone());
        app.poll();
        assert!(
            app.child.is_some(),
            "exited process discarded pending stdout state"
        );
        snapshot.publish(WorkerState::ProbePassed);
        snapshot.publish_input(WorkerInputState::Disabled, true);
        snapshot.2.store(true, Ordering::Release);
        app.poll();
        assert!(app.child.is_none());
        assert!(app.notice.contains("通信探针通过"));
        assert_eq!(app.input_state, Some(WorkerInputState::Disabled));
        drop(app);
        fixture.close()?;
        Ok(())
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
        drop(tx);
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
        assert!(app.revoke().unwrap_err().to_string().contains("信任撤销"));
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
        // Enrolled presentation fixture only; this child never authenticates a peer.
        app.config.fingerprint = Some("lifecycle-presentation-only".into());
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
        drop(tx);
        app.events = Some(rx);
        app.connected = true;
        set_state(&mut app, WorkerState::Authenticated);
        set_state(&mut app, WorkerState::Disconnected);
        set_state(&mut app, WorkerState::Reconnecting);
        set_state(&mut app, WorkerState::Listening);
        app.poll();
        assert!(!app.connected);
        assert_eq!(app.status, "等待对端连接");
        set_state(&mut app, WorkerState::Reconnecting);
        app.poll();
        assert_eq!(app.status, "正在重连可信设备");
        set_state(&mut app, WorkerState::Failed);
        app.poll();
        assert!(app.last_failure);
        set_state(&mut app, WorkerState::Stopped);
        app.poll();
        assert!(
            app.last_failure,
            "a final stop must not hide the earlier failure"
        );
        assert_eq!(app.workspace_view().title, "连接未完成，正在结束");
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
            assert_eq!(app.status, "离线");
        }
        fixture.close()?;
        Ok(())
    }
}
