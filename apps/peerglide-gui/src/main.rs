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
#[command(name = "peerglide-gui", version, about = "屏跃桌面控制面板")]
struct Options {
    /// 使用指定的配置与身份目录，不导入或迁移其他目录的数据与授权。
    #[arg(long, value_name = "DIRECTORY")]
    data_dir: Option<PathBuf>,
    /// 打开离线手势实验室，仅记录窗口内轨迹。
    #[arg(long)]
    gesture_lab: bool,
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
            peer_name: "另一台电脑".into(),
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
    permission_report: Option<peerglide_platform::context::PermissionReport>,
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
            permission_report: None,
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
        transport::read_certificate(std::path::Path::new(self.import_path.trim()))
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
        self.status = "正在启动".into();
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
            self.status = "正在恢复本机控制".into();
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
        if let Some(child) = &mut self.child {
            if self
                .stopping
                .is_some_and(|s| s.elapsed() > Duration::from_secs(3))
            {
                let _ = child.kill();
                self.notice = "工作进程已被强制停止，对端输入租约将自动到期。".into();
            }
            match child.try_wait() {
                Ok(Some(exit)) => {
                    self.connected = false;
                    self.status = if exit.success() {
                        "离线".into()
                    } else {
                        format!("工作进程已退出：{exit}")
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
            Err(e) => format!("操作未完成：{e:#}"),
        };
    }
    fn heading(ui: &mut egui::Ui, title: &str, subtitle: &str) {
        ui.heading(RichText::new(title).size(30.0));
        ui.add_space(8.0);
        ui.label(subtitle);
        ui.add_space(24.0);
    }
    fn home(&mut self, ui: &mut egui::Ui) {
        Self::heading(ui, "多台电脑，一种体验。", "连接设备，让键鼠协作更连贯。");
        egui::Frame::group(ui.style())
            .inner_margin(20.0)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.strong("本机");
                        ui.add_space(8.0);
                        ui.label(std::env::consts::OS);
                        ui.label(if self.local_fingerprint().is_some() {
                            "身份已就绪"
                        } else {
                            "请先创建本机身份"
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
                            "已明确登记的可信设备"
                        } else {
                            "尚未登记对端"
                        });
                    });
                });
            });
        ui.add_space(22.0);
        ui.add_enabled_ui(self.child.is_none() && self.instance_lock.is_some(), |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.config.listen, true, "在本机等待连接");
                ui.selectable_value(&mut self.config.listen, false, "连接另一台电脑");
            });
            if !self.config.listen {
                ui.horizontal(|ui| {
                    ui.label("对端地址与端口");
                    ui.text_edit_singleline(&mut self.config.address);
                });
            }
            ui.add_space(12.0);
            if ui.button(RichText::new("启动会话").size(18.0)).clicked() {
                let r = self.start(false);
                self.feedback(r, "会话工作进程已启动。");
            }
            if ui.button("运行通信探针").clicked() {
                let r = self.start(true);
                self.feedback(r, "正在探测对端地址，请先在对端启动监听。");
            }
        });
        if self.child.is_some() && ui.button("断开连接并恢复本机控制").clicked() {
            self.stop();
        }
        ui.add_space(18.0);
        ui.label(if self.config.native {
            "原生输入已启用，按 Ctrl+Alt+Esc 恢复本机控制。"
        } else {
            "当前为诊断模式：原生捕获与注入已关闭，可在设置中明确启用。"
        });
        egui::CollapsingHeader::new("会话诊断（不含按键内容，原始日志保留原文）").show(ui, |ui| {
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
            "可信设备",
            "信任需要明确确认。发现设备或处于同一局域网，不代表允许控制。",
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
            self.feedback(result, "仅已选择地址，证书登记和输入授权未改变");
        }
        ui.strong("本机");
        if let Some(fp) = self.local_fingerprint() {
            ui.label("公钥证书");
            ui.monospace(
                self.identity_dir()
                    .join("node.crt.der")
                    .display()
                    .to_string(),
            );
            ui.label("BLAKE3 指纹");
            ui.monospace(fp);
        } else if ui
            .add_enabled(
                self.instance_lock.is_some(),
                egui::Button::new("创建本机身份"),
            )
            .clicked()
        {
            let r = Identity::create(&self.identity_dir()).map(|_| ());
            self.feedback(r, "身份已创建。仅交换 node.crt.der，私钥请保留在本机。");
        }
        ui.add_space(18.0);
        ui.separator();
        ui.add_space(18.0);
        ui.add_enabled_ui(self.child.is_none() && self.instance_lock.is_some(), |ui| {
            ui.horizontal(|ui| {
                ui.label("设备名称");
                ui.text_edit_singleline(&mut self.config.peer_name);
            });
            ui.label("另一台电脑的公钥证书文件");
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
                    egui::Checkbox::new(&mut compared, "我已在另一台电脑上独立核对该指纹"),
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
                .add_enabled(compared, egui::Button::new("登记可信设备"))
                .clicked()
            {
                let r = self.enroll();
                self.feedback(r, "设备已登记，输入权限仍需单独设置。");
            }
            if let Some(fp) = &self.config.fingerprint {
                ui.add_space(14.0);
                ui.label("已登记的指纹");
                ui.monospace(fp);
            }
        });
        if self.config.fingerprint.is_some() && ui.button("断开连接并撤销设备信任").clicked()
        {
            let result = self.revoke();
            self.feedback(result, "设备信任已撤销，正在停止活动工作进程。");
        }
        ui.add_space(18.0);
        ui.weak("可手动查找局域网设备；发现结果不可信。当前使用公钥证书登记，配对码尚未实现。");
    }
    fn displays(&mut self, ui: &mut egui::Ui) {
        Self::heading(ui, "排列屏幕", "拖动对端屏幕，选择从本机哪一侧切换。");
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
                "本机",
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
                ui.label("切换方向");
                for side in ["left", "right", "top", "bottom"] {
                    if ui
                        .selectable_value(&mut self.config.edge, side.into(), edge_label(side))
                        .changed()
                    {
                        self.save();
                    }
                }
            });
        });
        ui.add_space(18.0);
        ui.label("当前原型仅支持主显示器。布局从下一次会话生效；请在另一台电脑设置相反方向。");
    }
    fn settings(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        Self::heading(ui, "设置", "管理输入权限与外观。");
        if self.config_store.is_none() && self.instance_lock.is_some() {
            ui.label("原设置文件已保留。重置会先备份并关闭授权，本机身份不变。");
            if ui.button("备份并重置设置").clicked() {
                match config_store::Store::reset::<Config>(self.dir.join("settings.json")) {
                    Ok((store, config)) => {
                        self.config_store = Some(store);
                        self.config = config;
                        self.notice = "设置已重置，原文件已备份".into();
                    }
                    Err(e) => self.notice = format!("重置失败：{e:#}"),
                }
            }
        }
        if self.instance_lock.is_none() || self.config_store.is_none() {
            ui.disable();
        }
        if ui.checkbox(&mut self.config.dark, "深色外观").changed() {
            self.theme(ctx);
            self.save();
        }
        ui.add_enabled_ui(self.child.is_none() && self.instance_lock.is_some(), |ui| {
            if ui
                .checkbox(&mut self.config.reconnect, "自动重连可信设备")
                .changed()
            {
                self.save();
            }
            ui.weak("自动重连可选开启；重连后仍由本机控制，直到你再次切换。");
        });
        ui.add_space(18.0);
        ui.add_enabled_ui(self.child.is_none() && self.instance_lock.is_some(), |ui| {
            if ui
                .checkbox(&mut self.config.allow, "允许已登记的对端控制本机")
                .changed()
            {
                self.save();
            }
            ui.add_enabled_ui(cfg!(any(windows, target_os = "macos")), |ui| {
                if ui
                    .checkbox(&mut self.config.native, "启用原生键鼠捕获与注入")
                    .changed()
                {
                    self.save();
                }
            });
        });
        ui.add_space(18.0);
        ui.label("Ctrl+Alt+Right，松开后切换控制目标\nCtrl+Alt+Esc：紧急恢复本机控制\n接收端的物理输入：收回控制");
        ui.add_space(14.0);
        ui.label("macOS 需要辅助功能和输入监控权限。Windows 提权窗口与安全桌面不在当前原型的控制范围内。");
        ui.separator();
        ui.strong("本 GUI 进程的系统权限");
        ui.weak("仅在点击时读取，不申请或修改权限。此快照不能代替工作进程检查或真实输入验收，也不会启用手势。");
        if ui.button("检查当前系统权限").clicked() {
            self.permission_report = Some(peerglide_platform::context::permissions());
        }
        if let Some(report) = self.permission_report {
            use peerglide_platform::context::{PermissionStatus, SecureInput};
            let label = |status| match status {
                PermissionStatus::Granted => "系统查询通过，不代表用户已授权控制",
                PermissionStatus::Denied => "未授予或访问受限",
                PermissionStatus::NotRequired => "此平台无需同名权限",
                PermissionStatus::Unknown => "未知，需要实机确认",
                PermissionStatus::Unsupported => "暂不支持此项检测",
            };
            ui.label(format!("辅助功能：{}", label(report.accessibility)));
            ui.label(format!("输入监控：{}", label(report.input_monitoring)));
            ui.label(format!("输入桌面查询：{}", label(report.input_desktop)));
            ui.label(format!("安全输入：{}", match report.secure_input {
                SecureInput::Active => "已开启，保留系统保护",
                SecureInput::Inactive => "检查时未检测到",
                SecureInput::Unknown => "未知，需要实机确认",
                SecureInput::Unsupported => "暂不支持此项检测，不能据此判断 UAC 状态",
            }));
            ui.weak("权限或系统状态变化后请重新检查；GUI 与通信工作进程可能有不同的系统授权。");
        }
        ui.add_space(14.0);
        ui.label("剪贴板和文件需独立授权，目前尚未接通。无需云账号，不收集遥测。");
        ui.add_space(14.0);
        ui.weak(format!("设置目录：{}", self.dir.display()));
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
                ui.heading(RichText::new("屏跃").size(25.0));
                ui.weak("屏幕相连，协作无间");
                ui.add_space(34.0);
                for (page, label) in [
                    (Page::Home, "主页"),
                    (Page::Devices, "设备"),
                    (Page::Displays, "显示器"),
                    (Page::Transfers, "传输"),
                    (Page::Settings, "设置"),
                    (Page::GestureLab, "手势实验室"),
                    (Page::GestureRules, "手势规则"),
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
                ui.weak("0.1.0-dev\n实验性原型");
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
        egui::CentralPanel::default().show(root, |ui| {
            ui.add_space(20.0);
            egui::ScrollArea::vertical().show(ui, |ui| match self.page {
                Page::Home => self.home(ui),
                Page::Devices => self.devices(ui),
                Page::Displays => self.displays(ui),
                Page::Settings => self.settings(ui, &ctx),
                Page::GestureLab => {
                    self.gesture_lab.ui(ui, self.child.is_some());
                    self.gesture_rules
                        .preview_ui(ui, self.gesture_lab.matched_id());
                }
                Page::GestureRules => self.gesture_rules.ui(
                    ui,
                    &self.dir,
                    self.instance_lock.is_some() && self.child.is_none(),
                ),
                Page::Transfers => {
                    Self::heading(ui, "文件传输", "后续 M4 阶段提供");
                    ui.label("当前原型尚不支持文件发送、断点续传、剪贴板共享或原生跨屏拖拽。");
                }
            });
        });
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
        "屏跃 · PeerGlide",
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
