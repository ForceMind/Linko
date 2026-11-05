//! Actual desktop interactions. No example devices or simulated permissions.
use super::*;
impl App {
    fn writable(&self) -> bool {
        self.instance_lock.is_some() && self.config_store.is_some()
    }
    pub(super) fn idle_writable(&self) -> bool {
        self.writable() && self.child.is_none() && self.stopping.is_none()
    }
    pub(super) fn input_label(&self) -> &'static str {
        if self.recovery_blocked {
            return "输入恢复未确认";
        }
        if self.stopping.is_some() {
            return if self.active_mode == Some(SessionMode::Native) {
                "正在停止与恢复"
            } else {
                "正在停止通信（未开启输入）"
            };
        }
        match self.input_state {
            Some(WorkerInputState::Disabled) => "原生输入关闭 · 仅通信",
            Some(WorkerInputState::Local) => "本机控制（进程报告）",
            Some(WorkerInputState::ControllingRemote) => "正在控制对端",
            Some(WorkerInputState::ReceivingRemote) => "对端正在控制本机",
            Some(WorkerInputState::Recovering) => "正在恢复本机输入",
            Some(WorkerInputState::RecoveryUnconfirmed) => "输入恢复未确认",
            Some(WorkerInputState::Unavailable) => "输入归属尚未确认",
            None => "尚未开启键鼠会话",
        }
    }
    pub(super) fn heading(ui: &mut egui::Ui, title: &str, subtitle: &str) {
        ui.heading(RichText::new(title).size(27.0));
        ui.add_space(7.0);
        ui.weak(subtitle);
        ui.add_space(20.0);
    }
    fn unavailable(ui: &mut egui::Ui, title: &str, description: &str) {
        egui::Frame::group(ui.style())
            .inner_margin(18.0)
            .show(ui, |ui| {
                ui.strong(title);
                ui.add_space(7.0);
                ui.label(description);
                ui.add_enabled(false, egui::Button::new("尚未提供"));
            });
    }
    fn open_setup(&mut self) {
        self.wizard_name = self.config.peer_name.clone();
        self.wizard_address = self.config.address.clone();
        self.wizard_listen = self.config.listen;
        self.wizard_allow = false;
        self.use_connection_info = true;
        self.confirmed_fingerprint = None;
        self.wizard = Some(if self.local_fingerprint().is_some() {
            SetupStep::Find
        } else {
            SetupStep::Identity
        });
        self.page = Page::Devices;
    }
    fn connection_actions(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(
                    self.idle_writable() && self.config.fingerprint.is_some(),
                    egui::Button::new("连接设备 · 仅通信"),
                )
                .clicked()
            {
                let result = self.start(false);
                self.feedback(result, "正在建立通信连接，未开启原生键鼠。");
            }
            if ui
                .add_enabled(
                    self.idle_writable() && self.config.fingerprint.is_some(),
                    egui::Button::new("通信探针"),
                )
                .clicked()
            {
                let result = self.start(true);
                self.feedback(result, "正在运行通信探针，不捕获或注入输入。");
            }
            if ui
                .add_enabled(
                    cfg!(any(windows, target_os = "macos"))
                        && self.writable()
                        && self.config.fingerprint.is_some()
                        && self.stopping.is_none()
                        && !self.recovery_blocked
                        && (self.child.is_none()
                            || self.active_mode == Some(SessionMode::Communication)),
                    egui::Button::new("启用键鼠会话…"),
                )
                .clicked()
            {
                self.native_confirmation = false;
                self.dialog = Some(Dialog::NativeStart);
            }
        });
    }
    pub(super) fn home(&mut self, ui: &mut egui::Ui) {
        Self::heading(
            ui,
            "先从一件事开始",
            "一台电脑先配置本机鼠标；连接另一台电脑后，再分别选择协作方式。",
        );
        ui.columns(2, |columns| {
            egui::Frame::group(columns[0].style())
                .inner_margin(18.0)
                .show(&mut columns[0], |ui| {
                    ui.heading("本机鼠标");
                    ui.weak("全局功能开发中");
                    ui.add_space(10.0);
                    ui.label("画轨迹、选择动作、测试并保存规则，无需创建身份或连接设备。");
                    ui.add_space(15.0);
                    if ui.button("配置本机鼠标").clicked() {
                        self.page = Page::GestureLab;
                        self.mouse_tab = MouseTab::Rules;
                    }
                    if ui.button("先在窗口里试画").clicked() {
                        self.page = Page::GestureLab;
                        self.mouse_tab = MouseTab::Practice;
                    }
                    ui.add_space(10.0);
                    ui.weak("当前只支持离线配置与窗口内测试，不启动全局捕获或执行动作。");
                });
            egui::Frame::group(columns[1].style())
                .inner_margin(18.0)
                .show(&mut columns[1], |ui| {
                    ui.heading("多机协同");
                    ui.weak("当前支持一个可信对端");
                    ui.add_space(10.0);
                    ui.label("核验身份、建立连接，再把电脑摆到与桌面一致的方向。");
                    ui.add_space(15.0);
                    if ui
                        .add_enabled(self.idle_writable(), egui::Button::new("添加电脑"))
                        .clicked()
                    {
                        self.open_setup();
                    }
                    if ui.button("管理设备与屏幕").clicked() {
                        self.page = Page::Devices;
                    }
                    ui.add_space(10.0);
                    ui.weak("通信已连接不代表正在控制，剪贴板和文件尚未接通。");
                });
        });
        ui.add_space(22.0);
        ui.strong("下一步");
        ui.add_space(10.0);
        let identity = self.local_fingerprint().is_some();
        egui::Frame::group(ui.style())
            .inner_margin(16.0)
            .show(ui, |ui| {
                if !identity {
                    ui.label("1  创建本机身份（仅多机协同需要）");
                    if ui
                        .add_enabled(self.idle_writable(), egui::Button::new("开始添加电脑"))
                        .clicked()
                    {
                        self.open_setup();
                    }
                } else if self.config.fingerprint.is_none() {
                    ui.label("1  登记另一台电脑，独立核对证书指纹");
                    if ui
                        .add_enabled(self.idle_writable(), egui::Button::new("继续核验设备"))
                        .clicked()
                    {
                        self.open_setup();
                    }
                } else {
                    ui.label(format!("✓ 已登记：{}", self.config.peer_name));
                    self.connection_actions(ui);
                }
                ui.separator();
                ui.label("2  选择方向与本机接收权限");
                if ui.button("布局与设备权限").clicked() {
                    self.page = Page::Devices;
                    self.device_tab = DeviceTab::Layout;
                }
                ui.separator();
                ui.label("3  明确开启键鼠，或只进行通信诊断");
                ui.weak("Ctrl+Alt+Esc 紧急恢复；接收端物理输入可接管。实际兼容性仍须实机验收。");
            });
        self.recovery_panel(ui);
    }
    pub(super) fn devices(&mut self, ui: &mut egui::Ui) {
        Self::heading(
            ui,
            "设备与屏幕",
            "身份、通信和输入授权分别表达。当前为单对端与主显示器，配对码和多设备尚未实现。",
        );
        ui.horizontal_wrapped(|ui| {
            ui.selectable_value(&mut self.device_tab, DeviceTab::Device, "电脑与连接");
            ui.selectable_value(&mut self.device_tab, DeviceTab::Layout, "屏幕布局");
            ui.selectable_value(&mut self.device_tab, DeviceTab::Permissions, "本设备权限");
            if ui
                .add_enabled(self.idle_writable(), egui::Button::new("添加／重新核验…"))
                .clicked()
            {
                self.open_setup();
            }
        });
        ui.add_space(16.0);
        match self.device_tab {
            DeviceTab::Layout => self.displays(ui),
            DeviceTab::Permissions => self.device_permissions(ui),
            DeviceTab::Device => {
                egui::Frame::group(ui.style())
                    .inner_margin(18.0)
                    .show(ui, |ui| {
                        ui.heading(&self.config.peer_name);
                        ui.label(if self.config.fingerprint.is_some() {
                            "身份：已登记可信证书"
                        } else {
                            "身份：尚未核验"
                        });
                        ui.label(format!("通信：{}", self.status));
                        ui.label(format!("键鼠：{}", self.input_label()));
                        ui.weak("本机不能代替对端授予接收控制权限；控制请求由对端独立决定。");
                        if matches!(self.input_state, Some(WorkerInputState::ReceivingRemote | WorkerInputState::Recovering)) {
                            ui.label("本机首次物理动作只用于收回控制，不执行或回放；请先松开按键和鼠标按钮再继续操作。");
                        }
                        ui.add_space(12.0);
                        self.connection_actions(ui);
                        if self.config.fingerprint.is_some() && ui.button("撤销设备信任…").clicked()
                        {
                            self.dialog = Some(Dialog::Revoke);
                        }
                    });
                ui.add_space(16.0);
                egui::CollapsingHeader::new("发现与高级连接").default_open(self.config.fingerprint.is_none()).show(ui,|ui|{
                    let own=self.local_fingerprint();let publishing=self.child.is_some()&&self.stopping.is_none()&&self.config.listen;let idle=self.child.is_none();let writable=self.writable();
                    if let Some(address)=self.discovery.ui(ui,own,publishing,idle,writable){
                        let result=self.use_discovery_address(address);self.feedback(result,"仅选择了地址，尚未登记或授权。");
                        if self.notice.starts_with("仅选择"){self.open_setup();self.wizard=Some(SetupStep::Verify);}
                    }
                    ui.weak("手动地址与等待连接方式可在添加向导中设置；证书选择器尚未提供，暂用文件路径。");
                });
            }
        }
    }
    fn device_permissions(&mut self, ui: &mut egui::Ui) {
        ui.heading("本机这端的授权");
        ui.weak("允许此设备控制本机，不代表本机也可以控制对端。下面的偏好用于下一次键鼠会话。");
        ui.add_enabled_ui(
            self.idle_writable() && self.config.fingerprint.is_some(),
            |ui| {
                if ui
                    .checkbox(&mut self.config.allow, "允许已登记对端控制本机键鼠")
                    .changed()
                {
                    self.save();
                }
            },
        );
        // Correct the direction in plain language, not a network role.
        ui.label(
            "此项实际含义：允许对端把键鼠输入发送到本机。对端是否允许本机输入，须在对端设置。",
        );
        if self.child.is_some()
            && self.active_mode == Some(SessionMode::Native)
            && self.config.allow
            && ui.button("撤回本机接收许可并停止会话").clicked()
        {
            self.config.allow = false;
            self.stop();
            let result = self.save_config();
            self.feedback(result, "本机接收许可已撤回，正在停止旧会话。");
        }
        ui.add_space(16.0);
        Self::unavailable(
            ui,
            "文字剪贴板 · 尚未接通",
            "不会因键鼠授权而读取或同步系统剪贴板，未来按方向单独授权。",
        );
        ui.add_space(12.0);
        Self::unavailable(
            ui,
            "文件接收 · 尚未接通",
            "不会发送或保存文件，未来逐次询问接收位置和同意。",
        );
    }
    pub(super) fn displays(&mut self, ui: &mut egui::Ui) {
        ui.heading("排列主显示器");
        ui.weak("方向从下一次会话生效；请在另一台电脑设置相反方向。");
        ui.add_enabled_ui(self.idle_writable(), |ui| {
            let width = ui.available_width().min(700.0);
            let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 270.0), egui::Sense::hover());
            let center = rect.center();
            let size = Vec2::new(140.0, 85.0);
            let local = egui::Rect::from_center_size(center, size);
            let offset = match self.config.edge.as_str() {
                "left" => Vec2::new(-155.0, 0.0),
                "top" => Vec2::new(0.0, -95.0),
                "bottom" => Vec2::new(0.0, 95.0),
                _ => Vec2::new(155.0, 0.0),
            };
            let peer = egui::Rect::from_center_size(center + offset, size);
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
            for (r, text) in [
                (local, "这台电脑"),
                (displayed, self.config.peer_name.as_str()),
            ] {
                painter.rect_filled(r, 6.0, ui.visuals().faint_bg_color);
                painter.rect_stroke(
                    r,
                    6.0,
                    egui::Stroke::new(1.0, ui.visuals().text_color()),
                    egui::StrokeKind::Inside,
                );
                painter.text(
                    r.center(),
                    egui::Align2::CENTER_CENTER,
                    text,
                    egui::FontId::proportional(15.0),
                    ui.visuals().text_color(),
                );
            }
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
            ui.horizontal_wrapped(|ui| {
                ui.label("对端位于本机");
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
        ui.add_space(12.0);
        ui.weak("多显示器精细拓扑尚未实现；此区域只配置一个对端的相对方向，不表示真实屏幕尺寸。");
    }
    fn mouse(&mut self, ui: &mut egui::Ui) {
        if self.recording_mode {
            self.gesture_lab.ui(ui, self.child.is_some());
            self.gesture_rules
                .preview_ui(ui, self.gesture_lab.matched_id());
            self.use_recorded_gesture(ui);
            return;
        }
        Self::heading(
            ui,
            "本机鼠标",
            "录制轨迹、选择动作、测试并保存。当前为离线配置，不执行系统动作。",
        );
        egui::Frame::group(ui.style())
            .inner_margin(14.0)
            .show(ui, |ui| {
                let mut enabled = false;
                ui.add_enabled(
                    false,
                    egui::Checkbox::new(&mut enabled, "全局鼠标手势 · 开发与实机验收中"),
                );
                ui.weak(
                    "默认关闭。当前平台没有可在本GUI启用的全局手势，保存规则不授予捕获或动作权限。",
                );
            });
        ui.add_space(14.0);
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.mouse_tab, MouseTab::Rules, "规则与应用");
            ui.selectable_value(&mut self.mouse_tab, MouseTab::Practice, "录制与本窗口测试");
        });
        ui.add_space(14.0);
        let writable = self.idle_writable();
        match self.mouse_tab {
            MouseTab::Rules => self.gesture_rules.ui(ui, &self.dir, writable),
            MouseTab::Practice => {
                self.gesture_lab.ui(ui, self.child.is_some());
                self.gesture_rules
                    .preview_ui(ui, self.gesture_lab.matched_id());
                self.use_recorded_gesture(ui);
            }
        }
    }
    fn use_recorded_gesture(&mut self, ui: &mut egui::Ui) {
        if let Some(gesture) = self.gesture_lab.matched_id() {
            if ui
                .add_enabled(self.idle_writable(), egui::Button::new("用此轨迹创建规则…"))
                .clicked()
            {
                let result = self.gesture_rules.add_from_gesture(gesture);
                let success = result.is_ok();
                self.feedback(result, "已创建未保存规则，请选择动作和应用范围。");
                if success {
                    self.recording_mode = false;
                    self.mouse_tab = MouseTab::Rules;
                }
            }
        }
    }
    fn collaboration(&mut self, ui: &mut egui::Ui) {
        Self::heading(
            ui,
            "剪贴板与文件",
            "协作权限与键鼠控制独立。未接通的功能不会显示模拟设备、进度或成功状态。",
        );
        ui.horizontal(|ui| {
            ui.selectable_value(
                &mut self.collaboration_tab,
                CollaborationTab::Clipboard,
                "文字剪贴板",
            );
            ui.selectable_value(
                &mut self.collaboration_tab,
                CollaborationTab::Files,
                "文件收发",
            );
        });
        ui.add_space(18.0);
        match self.collaboration_tab {
            CollaborationTab::Clipboard => {
                Self::unavailable(ui,"文字同步尚未提供","目前不会读取、写入或共享系统剪贴板。后续按设备、方向和手动／自动方式分别授权。");
            }
            CollaborationTab::Files => {
                Self::unavailable(ui,"文件收发尚未提供","当前没有生产文件发送和接收界面。不会读取示例文件或保存到下载目录；原生跨屏拖拽也未实现。");
            }
        }
        ui.add_space(18.0);
        ui.weak("连接成功不包含上述权限。离线数据模块和设计原型都不是已可用的数据共享功能。");
    }
    pub(super) fn settings(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        Self::heading(
            ui,
            "设置",
            "管理系统权限、外观、配置与诊断。设备授权在对应设备页设置。",
        );
        if !self.writable() {
            ui.strong("设置当前只读，原文件已保留");
            ui.label("配置损坏、版本过新或另一窗口占用时不会覆盖。恢复会备份原设置，保留身份并关闭授权。");
            if self.instance_lock.is_some()
                && self.config_store.is_none()
                && ui.button("备份并恢复设置…").clicked()
            {
                self.dialog = Some(Dialog::Reset);
            }
            ui.separator();
        }
        ui.add_enabled_ui(self.writable(), |ui| {
            if ui.checkbox(&mut self.config.dark, "深色外观").changed() {
                self.theme(ctx);
                self.save();
            }
        });
        ui.add_enabled_ui(self.idle_writable(), |ui| {
            if ui
                .checkbox(&mut self.config.reconnect, "自动重连可信设备")
                .changed()
            {
                self.save();
            }
            ui.weak("重连后先回到本机控制，需重新发起切换，不复用旧控制状态。");
        });
        ui.add_space(16.0);
        ui.strong("本GUI进程的系统权限");
        ui.weak("只在点击时读取，不申请或修改权限。实际输入进程会独立复查。");
        if ui.button("检查当前系统权限").clicked() {
            self.permission_report = Some(peerglide_platform::context::permissions());
        }
        if let Some(report) = self.permission_report {
            use peerglide_platform::context::{PermissionStatus, SecureInput};
            let label = |p| match p {
                PermissionStatus::Granted => "已授予",
                PermissionStatus::Denied => "未授予",
                PermissionStatus::NotRequired => "此平台无需同名权限",
                PermissionStatus::Unknown => "未知",
                PermissionStatus::Unsupported => "尚不支持",
            };
            ui.label(format!(
                "辅助功能：{} · 输入监控：{} · 输入桌面：{}",
                label(report.accessibility),
                label(report.input_monitoring),
                label(report.input_desktop)
            ));
            ui.label(match report.secure_input {
                SecureInput::Active => "Secure Input正在保护输入，屏跃不会绕过。",
                SecureInput::Inactive => "当前未检测到Secure Input。",
                SecureInput::Unknown => "Secure Input状态未知。",
                SecureInput::Unsupported => "此平台无同名Secure Input检查。",
            });
        }
        ui.weak("macOS按需要在系统设置中处理辅助功能/输入监控，再重新检查。Windows UAC与安全桌面不绕过。GUI快照不等于真实输入可用。");
        let idle = self.child.is_none() && self.stopping.is_none();
        self.worker_permissions.ui(ui, idle);
        ui.add_space(16.0);
        ui.strong("后台运行");
        ui.weak("托盘/菜单栏尚未提供。关闭窗口会请求停止并等待清理，不暗示仍在后台运行。");
        egui::CollapsingHeader::new("会话诊断 · 不含按键内容").show(ui, |ui| {
            for line in &self.logs {
                ui.monospace(line);
            }
        });
    }
    fn recovery_panel(&mut self, ui: &mut egui::Ui) {
        if self.stopping.is_some() || self.recovery_blocked {
            ui.add_space(16.0);
            egui::Frame::group(ui.style()).inner_margin(16.0).show(ui,|ui|{ui.strong(self.input_label());ui.label("停止请求、进程退出或断开连接不能代替软件清理结果。未确认时不开始新的键鼠会话。");if ui.button("查看停止与恢复状态").clicked(){self.dialog=Some(Dialog::Recovery);}});
        }
    }
    pub(super) fn connection_exchange_ui(&mut self, ui: &mut egui::Ui) {
        if ui.button("复制本机公开连接信息").clicked() {
            match self.local_connection_info() {
                Ok(text) => {
                    ui.ctx().copy_text(text);
                    self.notice =
                        "已复制公开证书与连接地址，不含私钥或权限。请通过可信渠道发送给另一端。"
                            .into();
                }
                Err(error) => self.notice = error.to_string(),
            }
        }
        ui.weak("公开信息仍需在对端独立核对完整指纹；不要发送私钥。自动短码配对尚未提供。");
        ui.label("粘贴另一台电脑的连接信息");
        let changed = egui::ScrollArea::vertical()
            .id_salt("connection-info-text")
            .max_height(96.0)
            .show(ui, |ui| {
                ui.add(
                    egui::TextEdit::multiline(&mut self.connection_text)
                        .desired_width(ui.available_width())
                        .desired_rows(3)
                        .char_limit(connection_info::MAX_TEXT_BYTES)
                        .hint_text("PG-CONNECTION-1:…"),
                )
                .changed()
            })
            .inner;
        if changed {
            self.confirmed_fingerprint = None;
            self.connection_addresses.clear();
            self.loaded_connection_info = None;
        }
        if ui.button("读取对端连接信息").clicked() {
            let result = self.read_connection_info();
            self.feedback(
                result,
                "公开信息已读取，尚未登记或授权。请核对下面的完整指纹。",
            );
        }
    }
    fn setup_window(&mut self, ctx: &egui::Context) {
        let Some(step) = self.wizard else {
            return;
        };
        let mut open = true;
        egui::Window::new("添加电脑")
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_width(620.0)
            .default_height(560.0)
            .vscroll(true)
            .show(ctx, |ui| {
                ui.horizontal_wrapped(|ui| {
                    for (value, text) in [
                        (SetupStep::Identity, "1 本机身份"),
                        (SetupStep::Find, "2 选择连接"),
                        (SetupStep::Verify, "3 核验身份"),
                        (SetupStep::Permissions, "4 独立权限"),
                        (SetupStep::Layout, "5 屏幕布局"),
                    ] {
                        ui.label(if step == value {
                            RichText::new(text).strong()
                        } else {
                            RichText::new(text).weak()
                        });
                    }
                });
                ui.separator();
                if ui.button("紧急停止并恢复本机").clicked() {
                    self.stop();
                }
                ui.add_enabled_ui(self.idle_writable(), |ui| match step {
                    SetupStep::Identity => {
                        ui.heading("本机鼠标不需要身份，多机协同需要");
                        if self.local_fingerprint().is_some() {
                            ui.label("本机身份已存在，将继续使用，不覆盖。");
                            if ui.button("使用已有身份继续").clicked() {
                                self.wizard = Some(SetupStep::Find);
                            }
                            return;
                        }
                        ui.label("只创建本机身份，不登记或授权其他设备。私钥不导出。");
                        if ui.button("创建本机身份").clicked() {
                            let result = Identity::create(&self.identity_dir()).map(|_| ());
                            let success = result.is_ok();
                            self.feedback(result, "身份已创建，仅交换公钥证书。");
                            if success {
                                self.wizard = Some(SetupStep::Find);
                            }
                        }
                    }
                    SetupStep::Find => {
                        ui.heading("复制连接信息，不用搬证书文件");
                        ui.label("先在一端复制公开信息，粘贴到另一端；另一端再复制回传。两端各交换一次即可，之后保留登记。");
                        ui.horizontal_wrapped(|ui| {
                            if ui.selectable_value(&mut self.wizard_listen, true, "这台先等待连接").changed()
                                | ui.selectable_value(&mut self.wizard_listen, false, "这台连接另一台").changed() {
                                self.confirmed_fingerprint = None;
                            }
                        });
                        self.connection_exchange_ui(ui);
                        egui::CollapsingHeader::new("高级：设备发现或手动证书登记").show(ui, |ui| {
                            let own = self.local_fingerprint();
                            let writable = self.writable();
                            if let Some(address) = self.discovery.ui(ui, own, false, true, writable) {
                                self.wizard_address = address.to_string();
                                self.wizard_listen = false;
                                self.confirmed_fingerprint = None;
                            }
                            ui.label("对端实际 IP 与端口（请勿照抄示例）");
                            ui.add(egui::TextEdit::singleline(&mut self.wizard_address).hint_text("填写对端实际 IPv4:24870"));
                            if ui.button("使用证书文件登记").clicked() {
                                self.use_connection_info = false;
                                self.confirmed_fingerprint = None;
                                self.wizard = Some(SetupStep::Verify);
                            }
                        });
                    }
                    SetupStep::Verify => {
                        ui.heading("核验另一台电脑的身份");
                        ui.label("对端名称");
                        ui.text_edit_singleline(&mut self.wizard_name);
                        ui.horizontal_wrapped(|ui| {
                            if ui.selectable_value(&mut self.use_connection_info, true, "粘贴连接信息").changed()
                                | ui.selectable_value(&mut self.use_connection_info, false, "高级证书文件").changed() {
                                self.confirmed_fingerprint = None;
                            }
                        });
                        if self.use_connection_info {
                            self.connection_exchange_ui(ui);
                            if self.connection_addresses.len() > 1 {
                                ui.label("对端有多个网卡：选择与你同一局域网的地址");
                                egui::ComboBox::from_id_salt("peer-connection-address")
                                    .selected_text(if self.wizard_address.is_empty() { "请选择实际局域网地址" } else { &self.wizard_address })
                                    .show_ui(ui, |ui| {
                                        for address in &self.connection_addresses {
                                            let value = address.to_string();
                                            if ui.selectable_value(&mut self.wizard_address, value.clone(), value).changed() {
                                                self.confirmed_fingerprint = None;
                                            }
                                        }
                                    });
                            }
                        } else {
                            ui.label("对端公钥证书文件路径");
                            if ui.text_edit_singleline(&mut self.import_path).changed() {
                                self.confirmed_fingerprint = None;
                            }
                        }
                        ui.label(if self.wizard_listen {
                            "本机等待连接；对端需选择连接本机。".to_owned()
                        } else {
                            format!("本机将连接：{}", if self.wizard_address.is_empty() { "尚未选择地址" } else { &self.wizard_address })
                        });
                        let candidate = self
                            .candidate()
                            .ok()
                            .map(|bytes| transport::fingerprint(&bytes));
                        if let Some(fp) = &candidate {
                            ui.label("对端证书BLAKE3指纹");
                            ui.monospace(fp);
                        } else {
                            ui.weak("尚未读取有效的对端公开证书。连接信息不是短码配对，也不自动授信。");
                        }
                        let address_valid = self.wizard_listen || self.wizard_address.parse::<std::net::SocketAddr>().is_ok_and(|address| address.port() != 0 && !address.ip().is_unspecified());
                        let mut compared =
                            candidate.is_some() && candidate == self.confirmed_fingerprint;
                        if !compared {
                            self.confirmed_fingerprint = None;
                        }
                        if ui
                            .add_enabled(
                                candidate.is_some(),
                                egui::Checkbox::new(
                                    &mut compared,
                                    "我已在对端独立核对该指纹，完全一致",
                                ),
                            )
                            .changed()
                        {
                            self.confirmed_fingerprint = if compared { candidate } else { None };
                        }
                        if ui
                            .add_enabled(compared && address_valid, egui::Button::new("核验并登记设备"))
                            .clicked()
                        {
                            let old = self.config.clone();
                            self.config.peer_name = self.wizard_name.clone();
                            self.config.address = self.wizard_address.clone();
                            self.config.listen = self.wizard_listen;
                            let result = self.enroll();
                            let success = result.is_ok();
                            if !success {
                                self.config = old;
                            } else {
                                self.wizard_allow = self.config.allow;
                                self.wizard = Some(SetupStep::Permissions);
                            }
                            self.feedback(result, "设备已登记，键鼠与数据权限没有因此开放。");
                        }
                    }
                    SetupStep::Permissions => {
                        ui.heading("只选择你需要的权限");
                        ui.checkbox(&mut self.wizard_allow, "允许此对端把键鼠输入发送到本机");
                        ui.weak("不能代授对端接收本机控制。剪贴板/文件尚未提供，默认不开放。");
                        if ui.button("保存本机许可并继续").clicked() {
                            let old = self.config.allow;
                            self.config.allow = self.wizard_allow;
                            let result = self.save_config();
                            let success = result.is_ok();
                            if !success {
                                self.config.allow = old;
                            }
                            self.feedback(result, "已保存本机接收偏好，尚未启用键鼠。");
                            if success {
                                self.wizard = Some(SetupStep::Layout);
                            }
                        }
                    }
                    SetupStep::Layout => {
                        ui.heading("摆好屏幕，先测试通信");
                        ui.label(format!(
                            "已登记 {}。当前方向：{}",
                            self.config.peer_name,
                            edge_label(&self.config.edge)
                        ));
                        if ui.button("完成并查看屏幕布局").clicked() {
                            self.wizard = None;
                            self.device_tab = DeviceTab::Layout;
                        }
                        if ui.button("建立仅通信连接").clicked() {
                            let result = self.start(false);
                            let success = result.is_ok();
                            self.feedback(result, "通信工作进程正在启动，原生输入关闭。");
                            if success {
                                self.wizard = None;
                            }
                        }
                    }
                });
                ui.separator();
                ui.horizontal(|ui| {
                    if step != SetupStep::Identity
                        && ui
                            .add_enabled(self.child.is_none(), egui::Button::new("上一步"))
                            .clicked()
                    {
                        self.wizard = Some(match step {
                            SetupStep::Find => SetupStep::Identity,
                            SetupStep::Verify => SetupStep::Find,
                            SetupStep::Permissions => SetupStep::Verify,
                            SetupStep::Layout => SetupStep::Permissions,
                            SetupStep::Identity => SetupStep::Identity,
                        });
                    }
                    if ui.button("关闭向导（已登记配置保留）").clicked() {
                        self.wizard = None;
                    }
                });
                if !self.notice.is_empty() {
                    ui.label(&self.notice);
                }
                if let Some(fp) = self.local_fingerprint() {
                    egui::CollapsingHeader::new("本机公钥与指纹").show(ui, |ui| {
                        ui.monospace(
                            self.identity_dir()
                                .join("node.crt.der")
                                .display()
                                .to_string(),
                        );
                        ui.monospace(fp);
                    });
                }
            });
        if !open {
            self.wizard = None;
        }
    }
    fn dialog_window(&mut self, ctx: &egui::Context) {
        let Some(dialog) = self.dialog else {
            return;
        };
        let mut open = true;
        let title = match dialog {
            Dialog::NativeStart => "明确开启键鼠会话",
            Dialog::Revoke => "撤销设备信任",
            Dialog::Reset => "备份并恢复设置",
            Dialog::ForceStop => "强制停止工作进程",
            Dialog::Recovery => "停止与恢复状态",
        };
        egui::Window::new(title).open(&mut open).collapsible(false).default_width(580.0).show(ctx,|ui|{
            if ui.button("立即停止并恢复本机").clicked(){self.stop();}
            ui.separator();match dialog{
                Dialog::NativeStart=>{
                    ui.label("这会启动真实原生键鼠捕获与注入；实际工作进程将重新检查系统权限。连接/认证不代表对端允许本机控制。");
                    ui.label(if self.config.allow{"本机接收偏好：允许已登记对端控制本机。"}else{"本机接收偏好：不允许对端控制本机；可请求控制已授权本机的对端。"});
                    if self.child.is_some(){ui.weak("当前仅通信会话将先停止；清理成功后再以键鼠模式重连。失败不自动继续。");}
                    ui.label("Ctrl+Alt+Right切换；Ctrl+Alt+Esc紧急恢复。实机兼容性、DPI、输入法与权限变化仍未完成验收。");
                    ui.label("接收端首次物理动作仅用于收回控制，不执行或回放。切回本机后先松开物理按键／鼠标按钮，再继续操作。");
                    ui.checkbox(&mut self.native_confirmation,"我明确同意本次使用真实键鼠协同");
                    if ui.add_enabled(cfg!(any(windows,target_os="macos"))&&self.writable()&&self.native_confirmation&&self.stopping.is_none()&&!self.recovery_blocked,egui::Button::new("确认并启动键鼠会话")).clicked(){let result=self.request_native();let success=result.is_ok();self.feedback(result,"正在准备真实键鼠会话，输入归属等待进程报告。");if success{self.dialog=None;}}
                },
                Dialog::Revoke=>{ui.label("停止会话并清除对端信任与本机接收许可。保留本机身份；下次需重新核验。");if ui.button("撤销并停止会话").clicked(){let result=self.revoke();self.feedback(result,"设备信任已撤销，旧会话正在停止。");self.dialog=None;}},
                Dialog::Reset=>{ui.label("先备份原设置，再恢复默认并关闭授权。本机身份保持。无效或未来版本不会直接覆盖。");if ui.add_enabled(self.child.is_none()&&self.instance_lock.is_some(),egui::Button::new("确认备份并恢复设置")).clicked(){match config_store::Store::reset::<Config>(self.dir.join("settings.json")){Ok((store,config))=>{self.config_store=Some(store);self.config=config;self.theme(ctx);self.notice="已备份并恢复设置，授权关闭，本机身份保留。".into();self.dialog=None;},Err(error)=>self.notice=format!("恢复失败，原文件保留：{error:#}")}}},
                Dialog::ForceStop=>{ui.label("仅在正常停止无响应时使用。强制结束不能确认输入已释放，之后本窗口只允许通信诊断。");if ui.button("明确强制结束，保留恢复警告").clicked(){if let Some(child)=self.child.as_mut(){let result=child.kill().map_err(anyhow::Error::from);self.feedback(result,"已请求强制结束，输入恢复未确认。");}
                if self.active_mode==Some(SessionMode::Native){self.recovery_blocked=true;}self.pending_start=None;self.dialog=None;}},
                Dialog::Recovery=>{ui.strong(self.input_label());ui.label("松开物理按键/鼠标按钮，检查两端提示与系统权限。屏跃不会关闭安全防护，也不把退出当恢复成功。进程报告只表示软件路由与清理请求，不代表OS或应用效果已确认。");ui.label("恢复未确认时保留警告。可运行仅通信诊断；新的原生键鼠会话保持禁止。");ui.label("清理失败且原生捕获仍可用时：先松开全部物理按键／按钮，再重新按下并松开Ctrl+Alt+Esc，可请求强制返回。它仍保留未知警告，不证明系统按键已释放。");if self.child.is_some()&&ui.button("再次发送停止请求").clicked(){let result=self.resend_stop();self.feedback(result,"已再次发送停止请求，仍需等待清理结果。");}
                if self.child.is_some()&&ui.button("正常停止无响应，查看强制结束…").clicked(){self.dialog=Some(Dialog::ForceStop);}
                if self.close_requested&&self.child.is_none()&&ui.button("明确退出，已了解恢复未确认").clicked(){self.exit_confirmed=true;ctx.send_viewport_cmd(egui::ViewportCommand::Close);}},
            }
            if !self.notice.is_empty(){ui.label(&self.notice);}
        });
        if !open {
            self.dialog = None;
        }
    }
    pub(super) fn desktop(&mut self, root: &mut egui::Ui) {
        let ctx = root.ctx().clone();
        if ctx.input(|input| {
            input.key_pressed(egui::Key::Escape) && input.modifiers.ctrl && input.modifiers.alt
        }) {
            self.stop();
            self.gesture_lab.cancel();
        }
        egui::Panel::left("navigation")
            .exact_size(190.0)
            .show(root, |ui| {
                ui.add_space(24.0);
                ui.heading(RichText::new("屏跃").size(27.0));
                ui.weak("鼠标增强与多机协同");
                ui.add_space(28.0);
                for (page, label) in [
                    (Page::Home, "总览"),
                    (Page::Devices, "设备与屏幕"),
                    (Page::GestureLab, "本机鼠标"),
                    (Page::Transfers, "剪贴板与文件"),
                    (Page::Settings, "设置"),
                ] {
                    let selected = self.page == page
                        || (page == Page::Devices && self.page == Page::Displays)
                        || (page == Page::GestureLab && self.page == Page::GestureRules);
                    if ui
                        .selectable_label(selected, RichText::new(label).size(17.0))
                        .clicked()
                    {
                        self.page = page;
                    }
                    ui.add_space(12.0);
                }
                ui.separator();
                ui.weak(format!("{} · 实验性开发版", env!("CARGO_PKG_VERSION")));
                ui.weak("未完成实机验收的能力会明确标注");
            });
        egui::Panel::top("global-actions").show(root, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(format!("通信：{}", self.status));
                ui.separator();
                ui.strong(self.input_label());
                if self.child.is_some() && ui.button("停止协同并恢复本机").clicked() {
                    self.stop();
                }
                if (self.stopping.is_some() || self.recovery_blocked)
                    && ui.button("恢复状态…").clicked()
                {
                    self.dialog = Some(Dialog::Recovery);
                }
                if ui.button("紧急恢复本机控制").clicked() {
                    self.stop();
                    self.gesture_lab.cancel();
                }
            });
        });
        egui::Panel::bottom("status").show(root, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.weak("本机全局手势：未启用　·　剪贴板/文件：尚未接通");
                if !self.notice.is_empty() {
                    ui.separator();
                    ui.label(&self.notice);
                }
            });
        });
        if self.page != Page::GestureLab && self.page != Page::GestureRules
            || self.child.is_some()
            || self.mouse_tab != MouseTab::Practice
        {
            self.gesture_lab.cancel();
        }
        egui::CentralPanel::default().show(root, |ui| {
            ui.add_space(18.0);
            egui::ScrollArea::vertical().show(ui, |ui| match self.page {
                Page::Home => self.home(ui),
                Page::Devices => self.devices(ui),
                Page::Displays => {
                    self.device_tab = DeviceTab::Layout;
                    self.devices(ui);
                }
                Page::Settings => self.settings(ui, &ctx),
                Page::GestureLab | Page::GestureRules => self.mouse(ui),
                Page::Transfers => self.collaboration(ui),
            });
        });
        self.setup_window(&ctx);
        self.dialog_window(&ctx);
        if let Some((canvas, save)) = self.gesture_lab.inspection() {
            if let Some(path) = self.layout_report.take() {
                let rect = |r: egui::Rect| [r.min.x, r.min.y, r.width(), r.height()];
                let report = serde_json::json!({"version":1,"canvas":rect(canvas),"save":rect(save),"scope":"isolated_gui_widget_geometry"});
                let result = (|| -> Result<()> {
                    let mut file = std::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(path)?;
                    serde_json::to_writer(&mut file, &report)?;
                    Ok(())
                })();
                if let Err(error) = result {
                    self.notice = format!("UI几何报告未写入：{error:#}");
                }
            }
        }
    }
}
