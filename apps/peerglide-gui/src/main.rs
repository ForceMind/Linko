#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
mod config_store;
mod connection_info;
mod desktop_ui;
mod discovery_panel;
mod gesture_lab;
mod gesture_rules;
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
#[derive(Clone, Copy, PartialEq, Eq)]
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
    discovery: discovery_panel::Panel,
    gesture_lab: gesture_lab::GestureLab,
    gesture_rules: gesture_rules::RuleEditor,
    dir: PathBuf,
    config: Config,
    config_store: Option<config_store::Store>,
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
    logs: VecDeque<String>,
    stopping: Option<Instant>,
}
impl App {
    fn new(cc: &eframe::CreationContext<'_>, dir: PathBuf) -> Self {
        install_fonts(&cc.egui_ctx);
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
                notice = format!("当前为只读窗口：{e:#}");
                e
            })
            .ok();
        let (config_store, config) =
            match config_store::Store::open::<Config>(dir.join("settings.json")) {
                Ok((store, config)) => (Some(store), config),
                Err(e) => {
                    notice = format!("设置已保留，需要恢复：{e:#}");
                    (None, Config::default())
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
            discovery: discovery_panel::Panel::default(),
            gesture_lab: gesture_lab::GestureLab::new(&dir),
            gesture_rules: gesture_rules::RuleEditor::new(&dir),
            dir,
            config,
            config_store,
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
        let confirmed = self
            .confirmed_fingerprint
            .as_deref()
            .context("请先在两台电脑上独立核对指纹")?;
        let bytes = self.candidate()?;
        anyhow::ensure!(
            transport::fingerprint(&bytes) == confirmed,
            "证书已变化，请重新核对指纹"
        );
        if let Ok(local) = Identity::load(&self.identity_dir()) {
            anyhow::ensure!(bytes != local.certificate, "请选择另一台电脑的证书");
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
        self.start_mode(if probe {
            SessionMode::Probe
        } else {
            SessionMode::Communication
        })
    }
    fn start_mode(&mut self, mode: SessionMode) -> Result<()> {
        let probe = mode == SessionMode::Probe;
        anyhow::ensure!(
            mode != SessionMode::Native || cfg!(any(windows, target_os = "macos")),
            "此平台只支持通信诊断，原生输入尚未实现"
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
        anyhow::ensure!(self.native_confirmation, "请明确确认启动真实键鼠");
        anyhow::ensure!(
            !self.recovery_blocked && self.stopping.is_none(),
            "恢复未确认，不能开始新的键鼠会话"
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
        match std::fs::remove_file(self.dir.join("trusted-peer.crt.der")) {
            Ok(()) => (),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(e).context("对端已禁用，但移除证书失败"),
        }
        self.save_config().context("保存设备信任撤销结果")
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
                            }.into();
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
    fn logic_tick(&mut self, ctx: &egui::Context) {
        self.poll();
        if ctx.input(|i| {
            !i.focused
                || i.viewport().minimized.unwrap_or(false)
                || i.viewport().occluded.unwrap_or(false)
        }) {
            self.gesture_lab.cancel();
        }
        if ctx.input(|i| i.viewport().close_requested()) && !self.exit_confirmed {
            if self.child.is_some() || self.recovery_blocked {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.close_requested = true;
                self.stop();
                self.dialog = Some(Dialog::Recovery);
            } else {
                self.exit_confirmed = true;
            }
        }
        if self.close_requested && self.child.is_none() && !self.recovery_blocked {
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
        input.viewports.entry(egui::ViewportId::ROOT).or_default().occluded = Some(occluded);
        for _ in 0..4 {
            let _ = ctx.run_logic(&input, |ctx| app.logic_tick(ctx));
        }
    }
    fn managed_child() -> Result<Child> {
        Ok(Command::new(std::env::current_exe()?)
            .args(["--exact", "lifecycle_tests::managed_test_child", "--nocapture"])
            .env("PEERGLIDE_GUI_LIFECYCLE_HELPER", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?)
    }
    #[test]
    fn idle_logic_stops_periodic_paints_but_hidden_worker_and_stop_keep_ticking() -> Result<()> {
        let fixture = super::tests::Fixture::new()?;
        let mut app = App::load(fixture.path.clone());
        let ctx = egui::Context::default();
        let ticks = count_activity_ticks(&ctx);
        logic_frames(&mut app, &ctx, false);
        assert_eq!(ticks.load(Ordering::SeqCst), 0, "idle GUI retained its 10Hz clock");
        app.child = Some(managed_child()?);
        app.active_mode = Some(SessionMode::Communication);
        logic_frames(&mut app, &ctx, true);
        assert!(ticks.load(Ordering::SeqCst) > 0, "hidden worker status polling stopped");
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
        assert_eq!(ticks.load(Ordering::SeqCst), settled, "finished work retained periodic polling");
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
        assert!(output.viewport_commands.get(&egui::ViewportId::ROOT).is_some_and(|commands| commands.contains(&egui::ViewportCommand::CancelClose)));
        assert!(app.close_requested && app.stopping.is_some());
        assert!(!app.exit_confirmed);
        input.viewports.get_mut(&egui::ViewportId::ROOT).unwrap().events.clear();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut close_sent = false;
        while app.child.is_some() && Instant::now() < deadline {
            let output = ctx.run_logic(&input, |ctx| app.logic_tick(ctx));
            close_sent |= output.viewport_commands.get(&egui::ViewportId::ROOT).is_some_and(|commands| commands.contains(&egui::ViewportCommand::Close));
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
        assert!(output.viewport_commands.get(&egui::ViewportId::ROOT).is_some_and(|commands| commands.contains(&egui::ViewportCommand::CancelClose)));
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
        assert_eq!(app.logs.back().map(String::as_str), Some("final diagnostic fixture"));
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
        assert!(app.child.is_some(), "exited process discarded pending stdout state");
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
