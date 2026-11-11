//! Actual desktop interactions. No example devices or simulated permissions.
use super::*;
use desktop_style::tokens;
use icons::Icon;
impl App {
    pub(super) fn writable(&self) -> bool {
        !self.quit_check_mode
            && !self.resource_check_mode
            && self.instance_lock.is_some()
            && self.config_store.is_some()
            && !self.pairing_open
    }
    pub(super) fn idle_writable(&self) -> bool {
        self.writable() && self.child.is_none() && self.stopping.is_none() && !self.pairing_open
    }
    pub(super) fn layout_editable(&self) -> bool {
        self.writable()
            && self.stopping.is_none()
            && self.layout_request.is_none()
            && (self.child.is_none()
                || (self.connected
                    && matches!(
                        self.input_state,
                        Some(WorkerInputState::Local | WorkerInputState::Disabled)
                    )))
            && self.config.fingerprint.is_some()
            && !self.recovery_blocked
            && !self.pairing_pending()
            && self.wizard.is_none()
            && self.dialog.is_none()
    }
    pub(super) fn try_set_edge(&mut self, edge: &str) -> bool {
        if !self.layout_editable() || !matches!(edge, "left" | "right" | "top" | "bottom") {
            self.screen_drag = None;
            return false;
        }
        if self.displayed_edge() == edge {
            return true;
        }
        if self.child.is_some() {
            return self.request_edge(edge);
        }
        let previous = self.config.edge.clone();
        self.config.edge = edge.into();
        if let Err(error) = self.save_config() {
            self.config.edge = previous;
            self.notice = format!("屏幕位置未保存：{error:#}");
            return false;
        }
        true
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
            Some(WorkerInputState::Disabled) => "键鼠未开启",
            Some(WorkerInputState::Local) => "本机控制（进程报告）",
            Some(WorkerInputState::ControllingRemote) => "已获准向对端发送输入",
            Some(WorkerInputState::ReceivingRemote) => "对端正在控制本机",
            Some(WorkerInputState::Recovering) => "正在恢复本机输入",
            Some(WorkerInputState::RecoveryUnconfirmed) => "输入恢复未确认",
            Some(WorkerInputState::Unavailable) => "输入归属尚未确认",
            None => "尚未开启键鼠会话",
        }
    }
    fn unavailable(ui: &mut egui::Ui, title: &str, sentences: &[&str]) {
        egui::Frame::group(ui.style())
            .inner_margin(18.0)
            .show(ui, |ui| {
                ui.strong(title);
                ui.add_space(7.0);
                for sentence in sentences {
                    ui.label(*sentence);
                }
                ui.weak("功能准备好后，会在这里提供独立的授权与操作入口。");
            });
    }
    pub(super) fn open_setup(&mut self) {
        self.peer_wait_expired = false;
        self.native_confirmation = false;
        self.pending_start = None;
        self.screen_drag = None;
        self.confirmed_fingerprint = None;
        self.wizard = None;
        self.dialog = None;
        self.pairing_panel
            .open(&self.config.peer_name, &self.config.address);
        self.pairing_open = true;
    }
    pub(super) fn open_advanced_setup(&mut self) {
        self.peer_wait_expired = false;
        self.screen_drag = None;
        self.native_confirmation = false;
        if self.dialog == Some(Dialog::NativeStart) {
            self.dialog = None;
        }
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
    pub(super) fn workspace_view(&self) -> desktop_state::View {
        desktop_state::present(desktop_state::Snapshot {
            registered: self.config.fingerprint.is_some()
                && !self.pairing_pending()
                && self.config_store.is_some(),
            writable: !self.quit_check_mode
                && !self.resource_check_mode
                && self.instance_lock.is_some()
                && self.config_store.is_some(),
            running: self.child.is_some(),
            connected: self.connected,
            failed: self.last_failure,
            stopping: self.stopping.is_some(),
            recovery_blocked: self.recovery_blocked,
            terminal: self.input_terminal,
            mode: self.active_mode,
            input: self.input_state,
            native_supported: cfg!(any(windows, target_os = "macos")),
            pairing: self.pairing_open,
            pairing_pending: self.pairing_pending(),
            readiness: self.worker_permissions.readiness(),
            switch_state: self.switch_state,
            waiting_for_peer: self.waiting_for_peer,
            peer_wait_expired: self.peer_wait_expired,
        })
    }
    pub(super) fn native_start_available(&self) -> bool {
        cfg!(any(windows, target_os = "macos"))
            && self.application_menu_error.is_none()
            && self.writable()
            && self.config.fingerprint.is_some()
            && self.stopping.is_none()
            && !self.recovery_blocked
            && self.wizard.is_none()
            && self.dialog.is_none()
            && (self.child.is_none() || self.active_mode == Some(SessionMode::Communication))
    }
    pub(super) fn workspace_action(&mut self, action: desktop_state::Action) {
        match action {
            desktop_state::Action::CheckPermissions if self.native_start_available() => {
                self.native_confirmation = false;
                self.worker_permissions.start();
            }
            desktop_state::Action::AddDevice if self.idle_writable() => self.open_setup(),
            desktop_state::Action::StartInput if self.native_start_available() => {
                self.native_confirmation = false;
                self.dialog = Some(Dialog::NativeStart);
            }
            desktop_state::Action::Connect if self.idle_writable() => {
                let result = self.start(false);
                self.feedback(result, "正在连接电脑，键鼠尚未开启。");
            }
            desktop_state::Action::Recovery => self.dialog = Some(Dialog::Recovery),
            _ => (),
        }
    }
    /// Secondary connection actions with short labels; the hover keeps the full sentence.
    pub(super) fn connection_actions(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(
                    self.idle_writable() && self.config.fingerprint.is_some(),
                    egui::Button::new("仅通信"),
                )
                .on_hover_text("只连接，不开启键鼠")
                .clicked()
            {
                let result = self.start(false);
                self.feedback(result, "正在建立连接，键鼠尚未开启。");
            }
            if ui
                .add_enabled(
                    self.idle_writable() && self.config.fingerprint.is_some(),
                    egui::Button::new("连通检查"),
                )
                .on_hover_text("检查两台电脑的连接，不读取或控制键鼠")
                .clicked()
            {
                let result = self.start(true);
                self.feedback(result, "正在检查连接，不读取或控制键鼠。");
            }
        });
    }
    fn work_controls(&mut self, ui: &mut egui::Ui) {
        let view = self.workspace_view();
        ui.horizontal_wrapped(|ui| {
            if view.action != desktop_state::Action::None
                && ui
                    .add_enabled(
                        (self.wizard.is_none() && self.dialog.is_none())
                            || view.action == desktop_state::Action::Recovery,
                        desktop_style::primary(view.action.label()),
                    )
                    .clicked()
            {
                self.workspace_action(view.action);
            }
            if self.child.is_some() && ui.button("停止协同").clicked() {
                self.stop();
            }
            if ui.button("检查本机权限").clicked() {
                self.page = Page::Settings;
                self.settings_section = SettingsSection::Permissions;
            }
        });
        ui.add_space(6.0);
        ui.weak(view.detail);
    }
    pub(super) fn home(&mut self, ui: &mut egui::Ui) {
        self.subpage_header(ui, "协作总览", Page::Shortcuts);
        desktop_style::card(ui).show(ui, |ui| {
            let view = self.workspace_view();
            ui.heading(view.title);
            ui.add_space(10.0);
            self.work_controls(ui);
            if ui.button("打开设备工作区 →").clicked() {
                self.navigate_to(Page::Devices);
            }
        });
        ui.add_space(20.0);
        desktop_style::card(ui).show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.strong("鼠标手势");
                desktop_style::pill(ui, "全局功能开发中", desktop_style::Tone::Neutral);
            });
            ui.label("在窗口里试画轨迹，或为常用应用保存规则。");
            ui.weak("现在可离线配置和预览；不会捕获全局鼠标或执行动作。");
            ui.horizontal_wrapped(|ui| {
                if ui.button("试画一条手势").clicked() {
                    self.page = Page::GestureLab;
                    self.mouse_tab = MouseTab::Practice;
                }
                if ui.button("管理应用规则").clicked() {
                    self.page = Page::GestureLab;
                    self.mouse_tab = MouseTab::Rules;
                }
            });
        });
    }
    /// Home view. The connection and identity details live in the context column's drawer.
    pub(super) fn devices(&mut self, ui: &mut egui::Ui) {
        self.recovery_panel(ui);
        self.device_workspace(ui);
    }
    /// Body of the details drawer: the former connection and identity card, discovery included.
    pub(super) fn connection_details(&mut self, ui: &mut egui::Ui) {
        ui.strong("连接与身份详情");
        ui.label(format!("连接详情：{}", self.status));
        ui.label(if self.config.listen {
            "连接方式：等待另一台电脑"
        } else {
            "连接方式：连接另一台电脑"
        });
        if !self.config.address.is_empty() {
            ui.monospace(&self.config.address);
        }
        self.connection_actions(ui);
        if ui
            .add_enabled(
                self.idle_writable(),
                egui::Button::new("手动设置地址与核验…"),
            )
            .clicked()
        {
            self.open_advanced_setup();
        }
        if self.config.fingerprint.is_some() && ui.button("移除这台电脑的信任…").clicked()
        {
            self.dialog = Some(Dialog::Revoke);
        }
        let own = self.local_fingerprint();
        let publishing = self.child.is_some() && self.stopping.is_none() && self.config.listen;
        let idle = self.child.is_none();
        let writable = self.writable();
        if let Some(address) = self.discovery.ui(ui, own, publishing, idle, writable) {
            let result = self.use_discovery_address(address);
            self.feedback(result, "仅选择了地址，尚未登记或授权。");
            if self.notice.starts_with("仅选择") {
                self.open_setup();
                self.wizard = Some(SetupStep::Verify);
            }
        }
        if let Some(fp) = &self.config.fingerprint {
            ui.label("已登记的完整身份指纹");
            ui.monospace(fp);
        }
        ui.weak("短码核验支持两台电脑；多设备和精细多屏拓扑尚未提供。");
    }
    pub(super) fn device_permissions(&mut self, ui: &mut egui::Ui) {
        ui.heading("让另一台电脑控制我");
        ui.label("只影响这台电脑接收键鼠；对方是否接受你的控制，需要它自己同意。");
        ui.add_enabled_ui(
            self.idle_writable() && self.config.fingerprint.is_some() && !self.pairing_pending(),
            |ui| {
                if ui
                    .checkbox(&mut self.config.allow, "允许已核验的电脑控制本机键鼠")
                    .changed()
                {
                    self.native_confirmation = false;
                    self.save();
                }
            },
        );
        ui.weak("修改用于下一次键鼠会话，不会自动开启控制。");
        if self.child.is_some()
            && self.active_mode == Some(SessionMode::Native)
            && self.config.allow
            && ui.button("撤回接收许可并停止协同").clicked()
        {
            self.config.allow = false;
            self.stop();
            let result = self.save_config();
            self.feedback(result, "接收许可已撤回，正在结束原有会话。");
        }
        ui.add_space(16.0);
        ui.separator();
        ui.strong("更多协作方式");
        ui.label("文字、图片和文件尚未接通，当前不会读取或同步这些内容。");
        ui.weak("后续逐项开放，并分别询问你的同意，不包含在键鼠许可里。");
    }
    fn mouse(&mut self, ui: &mut egui::Ui) {
        if self.recording_mode {
            self.gesture_lab.ui(ui, self.child.is_some());
            self.gesture_rules
                .preview_ui(ui, self.gesture_lab.matched_id());
            self.use_recorded_gesture(ui);
            return;
        }
        self.subpage_header(ui, "本机鼠标", Page::Shortcuts);
        ui.weak("离线配置：录制、测试并保存规则，不执行系统动作。");
        ui.add_space(14.0);
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
    fn shortcuts(&mut self, ui: &mut egui::Ui) {
        self.subpage_header(ui, "快捷键", Page::Displays);
        desktop_style::card(ui).show(ui, |ui| {
            ui.set_min_width(ui.available_width().min(680.0));
            ui.strong("键鼠协同");
            ui.add_space(10.0);
            for shortcut in &SHORTCUTS {
                shortcut_row(ui, shortcut);
            }
            ui.add_space(6.0);
            ui.weak("仅在已明确开启的键鼠会话中生效。");
            ui.weak("切回后先松开物理按键与鼠标按钮。");
        });
        ui.add_space(14.0);
        desktop_style::card(ui).show(ui, |ui| {
            ui.set_min_width(ui.available_width().min(680.0));
            ui.strong("本机鼠标");
            ui.label("录制轨迹、预览识别、管理应用规则。");
            ui.weak("全局手势尚未接通；离线配置不会捕获全局鼠标或执行系统动作。");
            ui.horizontal_wrapped(|ui| {
                if ui.button("手势实验室").clicked() {
                    self.navigate_to(Page::GestureLab);
                    self.mouse_tab = MouseTab::Practice;
                }
                if ui.button("规则与应用").clicked() {
                    self.navigate_to(Page::GestureRules);
                    self.mouse_tab = MouseTab::Rules;
                }
            });
        });
        ui.add_space(14.0);
        if ui.button("查看协作总览").clicked() {
            self.navigate_to(Page::Home);
        }
    }
    fn collaboration(&mut self, ui: &mut egui::Ui) {
        self.collaboration_tab = if self.page == Page::Clipboard {
            CollaborationTab::Clipboard
        } else {
            CollaborationTab::Files
        };
        self.subpage_header(ui, "文件与剪贴板", Page::Displays);
        ui.horizontal(|ui| {
            for (page, label) in [(Page::Transfers, "文件传输"), (Page::Clipboard, "剪贴板")]
            {
                if ui.selectable_label(self.page == page, label).clicked() {
                    self.navigate_to(page);
                }
            }
        });
        let muted = tokens::muted_color(ui.visuals().dark_mode);
        icon_row(
            ui,
            "内容共享与键鼠控制分别授权。",
            |painter, rect| {
                Icon::Lock.paint(painter, rect, muted);
            },
        );
        ui.add_space(14.0);
        desktop_style::card(ui).show(ui, |ui| {
            desktop_style::pill(ui, "尚未接通", desktop_style::Tone::Neutral);
            ui.add_space(12.0);
            match self.collaboration_tab {
                CollaborationTab::Clipboard => Self::unavailable(
                    ui,
                    "系统剪贴板尚未接通",
                    &[
                        "目前不会读取、写入或共享系统剪贴板。",
                        "后续按设备、方向和手动／自动方式分别授权。",
                    ],
                ),
                CollaborationTab::Files => Self::unavailable(
                    ui,
                    "文件发送与接收尚未接通",
                    &[
                        "当前不会读取或传送文件，也未提供原生跨屏拖拽。",
                        "不会展示模拟传输进度。",
                    ],
                ),
            }
        });
        ui.add_space(18.0);
        ui.weak("连接成功不包含内容共享许可。");
        ui.weak("离线数据模块不是已可用的系统共享功能。");
    }
    pub(super) fn settings(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        self.subpage_header(ui, "设置", Page::Displays);
        if self.application_menu_error.is_some() {
            desktop_style::card(ui).show(ui, |ui| {
                ui.set_min_width(ui.available_width().min(680.0));
                desktop_style::pill(ui, "退出恢复入口未就绪", desktop_style::Tone::Warning);
                ui.label("本窗口不能开启新的键鼠会话，仍可使用通信诊断。");
                ui.label("请先确认恢复状态，再重新打开本版屏跃。");
            });
            ui.add_space(14.0);
        }
        if !self.writable() {
            desktop_style::card(ui).show(ui, |ui| {
                ui.set_min_width(ui.available_width().min(680.0));
                desktop_style::pill(ui, "设置只读，原资料已保留", desktop_style::Tone::Warning);
                if self.enrollment_blocked {
                    ui.label("设备变更尚未完成，原资料已保留，当前不能重置设备设置。");
                    ui.weak("重新打开仍无法收尾时，请保留整个资料目录供检查。");
                    ui.weak("不会开启键鼠或覆盖冲突文件。");
                } else {
                    ui.label("可能是另一窗口正在使用资料，或设置需要恢复。");
                    ui.label("恢复前会备份，不改变本机身份。");
                }
                if !self.enrollment_blocked
                    && self.instance_lock.is_some()
                    && self.config_store.is_none()
                    && ui.button("备份并恢复设置…").clicked()
                {
                    self.dialog = Some(Dialog::Reset);
                }
            });
            ui.add_space(14.0);
        }
        segmented(ui, &mut self.settings_section, &SETTINGS_SECTIONS);
        ui.add_space(18.0);
        match self.settings_section {
            SettingsSection::General => {
                desktop_style::card(ui).show(ui, |ui| {
                    ui.set_min_width(ui.available_width().min(680.0));
                    ui.strong("外观与连接");
                    ui.add_enabled_ui(self.writable(), |ui| {
                        let mut dark = self.config.dark;
                        ui.horizontal(|ui| {
                            ui.label("外观");
                            segmented(ui, &mut dark, &APPEARANCES);
                        });
                        if dark != self.config.dark {
                            self.config.dark = dark;
                            self.theme(ctx);
                            self.save();
                        }
                        ui.horizontal_wrapped(|ui| {
                            ui.label("界面语言");
                            ui.weak("简体中文");
                        });
                    });
                    ui.add_enabled_ui(self.idle_writable() && !self.pairing_pending(), |ui| {
                        if ui
                            .checkbox(&mut self.config.reconnect, "断开后自动重连已记住的电脑")
                            .changed()
                        {
                            self.save();
                        }
                    });
                    ui.weak("重连会回到本机控制，不恢复上次的按键或控制状态。");
                });
                ui.add_space(14.0);
                desktop_style::card(ui).show(ui, |ui| {
                    ui.set_min_width(ui.available_width().min(680.0));
                    ui.strong(if cfg!(target_os = "macos") {
                        "菜单栏与关闭窗口"
                    } else {
                        "托盘与关闭窗口"
                    });
                    let supported = cfg!(any(windows, target_os = "macos"));
                    let editable =
                        self.instance_lock.is_some() && self.desktop_store.is_some() && supported;
                    ui.add_enabled_ui(editable, |ui| {
                        let mut preferences = self.desktop_preferences;
                        if ui
                            .checkbox(&mut preferences.tray_enabled, "显示屏跃的菜单栏／托盘入口")
                            .changed()
                        {
                            if !preferences.tray_enabled {
                                preferences.close_to_tray = false;
                            }
                            let result = self.set_desktop_preferences(ctx, preferences);
                            if let Err(error) = result {
                                self.notice = format!("桌面选项未保存：{error:#}");
                            }
                        }
                        let mut close_to_tray = self.desktop_preferences.close_to_tray;
                        if ui
                            .add_enabled(
                                self.tray.as_ref().is_some_and(|tray| tray.is_available()),
                                egui::Checkbox::new(&mut close_to_tray, "关闭窗口后继续在后台协同"),
                            )
                            .changed()
                        {
                            let preferences = DesktopPreferences {
                                close_to_tray,
                                ..self.desktop_preferences
                            };
                            let result = self.set_desktop_preferences(ctx, preferences);
                            if let Err(error) = result {
                                self.notice = format!("关闭方式未保存：{error:#}");
                            }
                        }
                    });
                    if self.tray.as_ref().is_some_and(|tray| tray.is_available()) {
                        ui.label("入口已显示：可打开窗口、停止协同或退出屏跃。");
                        if self.desktop_preferences.close_to_tray {
                            ui.weak("关闭窗口不会停止协同。");
                            ui.weak("停止或退出请使用菜单栏／托盘；退出仍等待输入清理。");
                        } else {
                            ui.weak("关闭窗口会停止协同、清理输入，然后退出。");
                        }
                    } else {
                        ui.label("关闭窗口会停止协同、清理输入，然后退出。");
                        if !supported {
                            ui.weak("此平台的托盘适配尚未提供。");
                        } else if self.desktop_preferences.tray_enabled
                            && editable
                            && ui.button("重试显示入口").clicked()
                        {
                            self.sync_tray(ctx);
                        }
                    }
                    if self.desktop_store.is_none() {
                        ui.weak("桌面选项只读，原文件已保留；不会启用后台保留。");
                    }
                    ui.weak("默认不启用后台保留；不会注册开机启动。");
                });
                ui.add_space(16.0);
            }
            SettingsSection::Permissions => {
                desktop_style::card(ui).show(ui, |ui| {
                    ui.set_min_width(ui.available_width().min(680.0));
                    let idle = self.child.is_none() && self.stopping.is_none();
                    self.worker_permissions.ui(ui, idle);
                });
                ui.add_space(14.0);
            }
            SettingsSection::Advanced => {
                egui::CollapsingHeader::new("高级诊断").show(ui, |ui| {
                    self.diagnostics(ui);
                });
            }
        }
    }
    /// Advanced diagnostics, collapsed by default. The summary keeps its source: the newest
    /// log line that `input_diagnostics::is_summary` accepts. Only the layout lives here.
    fn diagnostics(&mut self, ui: &mut egui::Ui) {
        use peerglide_platform::input_diagnostics::is_summary;
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
        ui.weak("macOS按需要在系统设置中处理辅助功能/输入监控，再重新检查。");
        ui.weak("Windows UAC与安全桌面不绕过。GUI快照不等于真实输入可用。");
        ui.separator();
        ui.strong("会话记录 · 不含按键内容");
        let summary = self.logs.iter().rev().find(|line| is_summary(line));
        if let Some(summary) = summary {
            if ui.button("复制本机输入诊断摘要").clicked() {
                ui.ctx().copy_text(summary.clone());
            }
        }
        for line in &self.logs {
            ui.monospace(line);
        }
    }
    fn recovery_panel(&mut self, ui: &mut egui::Ui) {
        if self.stopping.is_some() || self.recovery_blocked {
            ui.add_space(16.0);
            egui::Frame::group(ui.style()).inner_margin(16.0).show(ui,|ui|{ui.strong(self.input_label());ui.label("停止请求、进程退出或断开连接不能代替软件清理结果。未确认时不开始新的键鼠会话。");if ui.button("查看停止与恢复状态").clicked(){self.dialog=Some(Dialog::Recovery);}});
        }
    }
    pub(super) fn connection_exchange_ui(&mut self, ui: &mut egui::Ui) {
        if ui.button("复制这台电脑的连接卡").clicked() {
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
        ui.weak("高级公开信息需独立核对完整指纹；不要发送私钥。也可返回短码核验流程。");
        ui.label("粘贴另一台电脑的连接卡");
        let changed = egui::ScrollArea::vertical()
            .id_salt("connection-info-text")
            .max_height(96.0)
            .show(ui, |ui| {
                ui.add(
                    egui::TextEdit::multiline(&mut self.connection_text)
                        .desired_width(ui.available_width())
                        .desired_rows(3)
                        .char_limit(connection_info::MAX_TEXT_BYTES)
                        .hint_text("粘贴另一台电脑发来的完整连接卡"),
                )
                .changed()
            })
            .inner;
        if changed {
            self.confirmed_fingerprint = None;
            self.connection_addresses.clear();
            self.loaded_connection_info = None;
        }
        if ui.button("读取连接卡").clicked() {
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
                        (SetupStep::Identity, "1 准备本机"),
                        (SetupStep::Find, "2 找到电脑"),
                        (SetupStep::Verify, "3 确认是它"),
                        (SetupStep::Permissions, "4 选择用途"),
                        (SetupStep::Layout, "5 摆放屏幕"),
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
                        ui.heading("先为这台电脑做准备");
                        if self.local_fingerprint().is_some() {
                            ui.label("本机身份已存在，将继续使用，不覆盖。");
                            if ui.button("使用已有身份继续").clicked() {
                                self.wizard = Some(SetupStep::Find);
                            }
                            return;
                        }
                        ui.label("为这台电脑创建独立身份，用于确认连接对象。不会因此允许任何电脑控制你。");
                        if ui.button("准备这台电脑").clicked() {
                            let result = Identity::create(&self.identity_dir()).map(|_| ());
                            let success = result.is_ok();
                            self.feedback(result, "身份已创建，仅交换公钥证书。");
                            if success {
                                self.wizard = Some(SetupStep::Find);
                            }
                        }
                    }
                    SetupStep::Find => {
                        ui.heading("交换连接卡，让两台电脑认识彼此");
                        ui.label("这里是高级人工核验：两端各交换一次公开连接卡，再独立核对完整指纹。普通添加可返回使用短码。");
                        self.connection_exchange_ui(ui);
                        egui::CollapsingHeader::new("高级：设备发现或手动证书登记").show(ui, |ui| {
                        ui.horizontal_wrapped(|ui| {
                            if ui.selectable_value(&mut self.wizard_listen, true, "这台先等待连接").changed()
                                | ui.selectable_value(&mut self.wizard_listen, false, "这台连接另一台").changed() {
                                self.confirmed_fingerprint = None;
                            }
                        });
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
                        ui.heading("确认连接的是你的电脑");
                        ui.label("给这台电脑取个名字");
                        ui.text_edit_singleline(&mut self.wizard_name);
                        ui.horizontal_wrapped(|ui| {
                            if ui.selectable_value(&mut self.use_connection_info, true, "使用连接卡").changed()
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
                            .add_enabled(compared && address_valid, egui::Button::new("确认并记住这台电脑"))
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
                        ui.heading("选择你需要的协作方式");
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
                        ui.heading("已添加，摆好屏幕就可以继续");
                        ui.label(format!(
                            "已登记 {}。当前方向：{}",
                            self.config.peer_name,
                            edge_label(self.displayed_edge())
                        ));
                        if ui.button("完成并查看屏幕布局").clicked() {
                            self.wizard = None;
                            self.device_tab = DeviceTab::Layout;
                        }
                        if ui.button("先检查连接（不开启键鼠）").clicked() {
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
                    let mut summary: String = self.notice.chars().take(64).collect();
                    if self.notice.chars().count() > 64 { summary.push('…'); }
                    ui.small(summary).on_hover_text(&self.notice);
                    if ui.small_button("详情").clicked() { self.notice_details = true; }
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
    fn native_start_window(&mut self, ctx: &egui::Context) {
        let mut open = true;
        let viewport = ctx.viewport_rect();
        let size = Vec2::new(
            580.0f32.min((viewport.width() - 48.0).max(1.0)),
            540.0f32.min((viewport.height() - 48.0).max(1.0)),
        );
        egui::Window::new("开启键鼠协同")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .fixed_size(size)
            .vscroll(false)
            .show(ctx, |ui| self.native_start_contents(ui));
        if !open {
            self.native_confirmation = false;
            self.dialog = None;
        }
    }

    fn native_start_contents(&mut self, ui: &mut egui::Ui) {
        const HEADER_HEIGHT: f32 = 112.0;
        const FOOTER_HEIGHT: f32 = 104.0;
        let permission_idle =
            self.child.is_none() || self.active_mode == Some(SessionMode::Communication);
        ui.allocate_ui_with_layout(
            Vec2::new(ui.available_width(), HEADER_HEIGHT),
            egui::Layout::top_down(egui::Align::LEFT),
            |ui| {
                ui.set_min_height(HEADER_HEIGHT);
                self.worker_permissions
                    .native_start_header(ui, permission_idle && self.stopping.is_none());
            },
        );
        // Keep all changing explanations inside this one bounded scroll body.
        // Neither an expired observation nor scrolling may move the controls.
        let body_height =
            (ui.available_height() - FOOTER_HEIGHT - ui.spacing().item_spacing.y).max(1.0);
        egui::ScrollArea::vertical()
            .id_salt("native-start-explanation")
            .auto_shrink([false, false])
            .min_scrolled_height(body_height)
            .max_height(body_height)
            .show(ui, |ui| {
                self.worker_permissions.native_start_details(ui);
                ui.separator();
                let points = native_start_points(
                    self.config.allow,
                    !self.config.listen && !self.config.reconnect,
                    self.child.is_some(),
                );
                for point in points {
                    safety_point(ui, point);
                }
                if !self.notice.is_empty() {
                    ui.separator();
                    ui.label(&self.notice);
                }
            });
        let permissions_fresh =
            self.worker_permissions.readiness() == worker_permissions::Readiness::Ready;
        if !permissions_fresh {
            self.native_confirmation = false;
        }
        ui.allocate_ui_with_layout(
            Vec2::new(ui.available_width(), FOOTER_HEIGHT),
            egui::Layout::top_down(egui::Align::LEFT),
            |ui| {
                ui.set_min_height(FOOTER_HEIGHT);
                ui.separator();
                ui.add_enabled(
                    permissions_fresh,
                    egui::Checkbox::new(
                        &mut self.native_confirmation,
                        "我明确同意本次使用真实键鼠协同",
                    ),
                );
                ui.allocate_ui_with_layout(
                    Vec2::new(ui.available_width(), 38.0),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        if ui.button("立即停止并恢复本机").clicked() {
                            self.stop();
                        }
                        let ready = cfg!(any(windows, target_os = "macos"))
                            && self.writable()
                            && self.worker_permissions.readiness()
                                == worker_permissions::Readiness::Ready
                            && self.native_confirmation
                            && self.stopping.is_none()
                            && !self.recovery_blocked;
                        let start = ui
                            .add_enabled(ready, egui::Button::new("确认并开启键鼠"))
                            .clicked();
                        let diagnose = ui
                            .add_enabled(
                                ready && self.child.is_none(),
                                egui::Button::new("开启并诊断"),
                            )
                            .on_hover_text("统计本机输入链路 120 秒，可复制不含输入内容的摘要")
                            .clicked();
                        if start || diagnose {
                            let result = if diagnose {
                                self.request_native_with_diagnostics(true)
                            } else {
                                self.request_native()
                            };
                            let success = result.is_ok();
                            self.feedback(result, "正在准备真实键鼠会话，输入归属等待进程报告。");
                            if success {
                                self.dialog = None;
                            }
                        }
                    },
                );
            },
        );
    }

    fn dialog_window(&mut self, ctx: &egui::Context) {
        let Some(dialog) = self.dialog else {
            return;
        };
        if dialog == Dialog::NativeStart {
            self.native_start_window(ctx);
            return;
        }
        let mut open = true;
        let title = match dialog {
            Dialog::NativeStart => "开启键鼠协同",
            Dialog::Revoke => "撤销设备信任",
            Dialog::Reset => "备份并恢复设置",
            Dialog::ForceStop => "强制停止工作进程",
            Dialog::Recovery => "停止与恢复状态",
        };
        egui::Window::new(title)
            .open(&mut open)
            .collapsible(false)
            .default_width(580.0)
            .default_height(540.0)
            .vscroll(true)
            .show(ctx, |ui| self.dialog_contents(ui, dialog));
        if !open {
            self.dialog = None;
        }
    }

    /// W4: the stop control first, then icon points, then the one explicit action.
    fn dialog_contents(&mut self, ui: &mut egui::Ui, dialog: Dialog) {
        if ui.button("立即停止并恢复本机").clicked() {
            self.stop();
        }
        ui.separator();
        if dialog == Dialog::Recovery {
            ui.strong(self.input_label());
        }
        for point in dialog_points(dialog) {
            safety_point(ui, *point);
        }
        match dialog {
            Dialog::NativeStart => {}
            Dialog::Revoke => self.revoke_action(ui),
            Dialog::Reset => self.reset_action(ui),
            Dialog::ForceStop => self.force_stop_action(ui),
            Dialog::Recovery => self.recovery_actions(ui),
        }
        if !self.notice.is_empty() {
            ui.label(&self.notice);
        }
    }

    fn revoke_action(&mut self, ui: &mut egui::Ui) {
        if ui.button("撤销并停止会话").clicked() {
            let result = self.revoke();
            self.feedback(result, "设备信任已撤销，旧会话正在停止。");
            self.dialog = None;
        }
    }

    fn reset_action(&mut self, ui: &mut egui::Ui) {
        let confirm = egui::Button::new("确认备份并恢复设置");
        if !ui.add_enabled(self.idle_writable(), confirm).clicked() {
            return;
        }
        match self.reset_settings() {
            Ok(()) => {
                self.theme(ui.ctx());
                self.notice = "已备份并恢复默认，设备信任和授权关闭，本机身份保留。".into();
                self.dialog = None;
            }
            Err(error) => self.notice = format!("恢复失败，原文件保留：{error:#}"),
        }
    }

    fn force_stop_action(&mut self, ui: &mut egui::Ui) {
        if !ui.button("明确强制结束，保留恢复警告").clicked() {
            return;
        }
        if let Some(child) = self.child.as_mut() {
            let result = child.kill().map_err(anyhow::Error::from);
            self.feedback(result, "已请求强制结束，输入恢复未确认。");
        }
        if self.active_mode == Some(SessionMode::Native) {
            self.recovery_blocked = true;
        }
        self.pending_start = None;
        self.dialog = None;
    }

    fn recovery_actions(&mut self, ui: &mut egui::Ui) {
        if self.child.is_some() && ui.button("再次发送停止请求").clicked() {
            let result = self.resend_stop();
            self.feedback(result, "已再次发送停止请求，仍需等待清理结果。");
        }
        let force = "正常停止无响应，查看强制结束…";
        if self.child.is_some() && ui.button(force).clicked() {
            self.dialog = Some(Dialog::ForceStop);
        }
        let exit = "明确退出，已了解恢复未确认";
        if self.close_requested && self.child.is_none() && ui.button(exit).clicked() {
            self.exit_confirmed = true;
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
    pub(super) fn record_layout_control(
        &mut self,
        key: &str,
        response: &egui::Response,
        clip: egui::Rect,
    ) {
        let rect = response.rect;
        if self.layout_report.is_some()
            && response.enabled()
            && clip.contains_rect(rect)
            && rect.is_positive()
            && [rect.min.x, rect.min.y, rect.max.x, rect.max.y]
                .iter()
                .all(|v| v.is_finite())
        {
            self.layout_geometry.insert(
                key.into(),
                [rect.min.x, rect.min.y, rect.width(), rect.height()],
            );
        }
    }
    pub(super) fn navigate_to(&mut self, page: Page) {
        self.page = page;
        self.screen_drag = None;
        self.native_confirmation = false;
        if self.dialog == Some(Dialog::NativeStart) {
            self.dialog = None;
        }
    }
    /// Laid out right to left: emergency recovery stays in the top-right corner.
    pub(super) fn safety_controls(&mut self, ui: &mut egui::Ui) {
        if shell_ui::icon_button(ui, Icon::Recover, "紧急恢复本机", true)
            .on_hover_text("Ctrl+Alt+Esc；立即请求停止，不等待二次确认")
            .clicked()
        {
            self.stop();
            self.gesture_lab.cancel();
        }
        let stop = ui.add_enabled(self.child.is_some(), |ui: &mut egui::Ui| {
            shell_ui::icon_button(ui, Icon::Stop, "停止协同", true)
        });
        if stop.clicked() {
            self.stop();
        }
        // The capsule carries the recovery state; this entry opens the full guidance.
        if (self.stopping.is_some() || self.recovery_blocked)
            && shell_ui::icon_button(ui, Icon::Warning, "查看恢复状态", true).clicked()
        {
            self.dialog = Some(Dialog::Recovery);
        }
    }
    pub(super) fn desktop(&mut self, root: &mut egui::Ui) {
        let ctx = root.ctx().clone();
        if self.layout_report.is_some() {
            self.layout_geometry.clear();
        }
        if ctx.input(|input| {
            input.key_pressed(egui::Key::Escape) && input.modifiers.ctrl && input.modifiers.alt
        }) {
            self.stop();
            self.gesture_lab.cancel();
        }
        let window_width = ctx.viewport_rect().width();
        // The title bar spans the window, so the safety controls never move with the sidebar.
        egui::Panel::top("global-actions")
            .exact_size(52.0)
            .resizable(false)
            .show_separator_line(false)
            .frame(
                egui::Frame::NONE
                    .fill(root.visuals().panel_fill)
                    .inner_margin(egui::Margin::symmetric(16, 6)),
            )
            .show(root, |ui| self.title_bar(ui, window_width));
        egui::Panel::bottom("status").show(root, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.small("Linko / 屏跃");
                if !self.notice.is_empty() {
                    ui.separator();
                    let mut summary: String = self.notice.chars().take(64).collect();
                    if self.notice.chars().count() > 64 {
                        summary.push('…');
                    }
                    ui.small(summary).on_hover_text(&self.notice);
                    if ui.small_button("详情").clicked() {
                        self.notice_details = true;
                    }
                }
            });
        });
        let expanded = self.sidebar_expanded(window_width);
        let sidebar_mix = motion::sidebar_mix(&ctx, egui::Id::unique("linko-sidebar"), expanded);
        egui::Panel::left("navigation")
            .exact_size(motion::sidebar_width_for(sidebar_mix))
            .resizable(false)
            .frame(
                egui::Frame::NONE
                    .fill(root.visuals().panel_fill)
                    .inner_margin(egui::Margin::symmetric(shell_ui::SIDEBAR_MARGIN, 8)),
            )
            .show(root, |ui| self.sidebar(ui, sidebar_mix));
        if self.page != Page::GestureLab && self.page != Page::GestureRules
            || self.child.is_some()
            || self.mouse_tab != MouseTab::Practice
        {
            self.gesture_lab.cancel();
        }
        // M4: the arriving view slides in from the right and fades in. Margins move in
        // opposite directions, so the content width and its wrapping stay constant.
        let now = ctx.input(|input| input.time);
        let view = shell_ui::view_key(self.page);
        self.page_motion
            .observe(view, now, desktop_style::tokens::VIEW_SWITCH_SECS);
        let progress = self.page_motion.tick(&ctx);
        let slide = motion::slide_offset(progress, desktop_style::tokens::VIEW_SLIDE_PX);
        let slide = slide.round() as i8;
        let mut margin = egui::Margin::symmetric(24, 16);
        margin.left += slide;
        margin.right -= slide;
        egui::CentralPanel::default()
            .frame(
                egui::Frame::default()
                    .inner_margin(margin)
                    .fill(root.visuals().panel_fill),
            )
            .show(root, |ui| {
                ui.multiply_opacity(shell_ui::view_alpha(progress));
                egui::ScrollArea::vertical().show(ui, |ui| match self.page {
                    Page::Home => self.home(ui),
                    Page::Devices => self.devices(ui),
                    Page::Displays => self.devices(ui),
                    Page::Settings => self.settings(ui, &ctx),
                    Page::GestureLab | Page::GestureRules => self.mouse(ui),
                    Page::Transfers | Page::Clipboard => self.collaboration(ui),
                    Page::Shortcuts => self.shortcuts(ui),
                });
            });
        if self.notice_details {
            egui::Window::new("操作详情")
                .open(&mut self.notice_details)
                .default_width(480.0)
                .show(&ctx, |ui| {
                    ui.label(&self.notice);
                });
        }
        self.setup_window(&ctx);
        self.dialog_window(&ctx);
        let mut pairing = std::mem::take(&mut self.pairing_panel);
        pairing.ui(self, &ctx);
        self.pairing_open = pairing.blocks_actions();
        self.pairing_panel = pairing;
        // Report ordinary geometry only while the sidebar rests, so scripted clicks land on
        // controls that keep their place.
        let report = if self.layout_report.is_none() {
            None
        } else if self.recording_mode {
            self.gesture_lab.inspection().map(|(canvas, save)| {
                let rect = |r: egui::Rect| [r.min.x, r.min.y, r.width(), r.height()];
                serde_json::json!({"version":1,"canvas":rect(canvas),"save":rect(save),"scope":"isolated_gui_widget_geometry"})
            })
        } else if (sidebar_mix <= 0.0 || sidebar_mix >= 1.0)
            && self.layout_geometry.contains_key("nav_layout")
            && self.layout_geometry.contains_key("add_device")
            && self.layout_geometry.contains_key("primary_action")
        {
            Some(
                serde_json::json!({"version":2,"controls":self.layout_geometry,"scope":"isolated_gui_widget_geometry"}),
            )
        } else {
            None
        };
        if let Some(report) = report {
            if let Some(path) = self.layout_report.take() {
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
                self.layout_geometry.clear();
            }
        }
    }
}

/// Reserve a stable slot while leaving text in an ordinary left-to-right Ui.
/// add_sized uses centered_and_justified and must not carry these text labels.
pub(super) fn left_text_slot(
    ui: &mut egui::Ui,
    width: f32,
    height: f32,
    text: egui::RichText,
) -> egui::Response {
    ui.allocate_ui_with_layout(
        Vec2::new(width, height),
        egui::Layout::left_to_right(egui::Align::Center),
        |slot| {
            slot.set_min_width(width);
            slot.set_min_height(height);
            slot.add(egui::Label::new(text).truncate())
        },
    )
    .inner
}

/// Icon side of a safety dialog point, in points.
const POINT_ICON: f32 = desktop_style::tokens::ICON_SMALL;

/// One sentence of a safety dialog with its icon. The text is the content; the icon only
/// helps scanning, so a point can never be painted as an icon alone.
#[derive(Clone, Copy)]
pub(super) struct SafetyPoint {
    pub(super) icon: Icon,
    pub(super) text: &'static str,
}

impl SafetyPoint {
    /// Warnings carry the warning color; every other point uses the accent color.
    fn ink(self, dark: bool) -> egui::Color32 {
        let tone = if self.icon == Icon::Warning {
            desktop_style::Tone::Warning
        } else {
            desktop_style::Tone::Accent
        };
        desktop_style::status_colors(dark, tone).1
    }
}

/// Paint `point` as an icon followed by its wrapped sentence.
pub(super) fn safety_point(ui: &mut egui::Ui, point: SafetyPoint) -> egui::Response {
    let ink = point.ink(ui.visuals().dark_mode);
    icon_row(ui, point.text, |painter, rect| {
        point.icon.paint(painter, rect, ink);
    })
}

/// A row with a fixed icon cell and text that wraps in the remaining width. `paint` draws
/// the icon; it is skipped while the cell is scrolled out of view.
pub(super) fn icon_row(
    ui: &mut egui::Ui,
    text: impl Into<egui::WidgetText>,
    paint: impl FnOnce(&egui::Painter, egui::Rect),
) -> egui::Response {
    let width = ui.available_width();
    let line = ui.text_style_height(&egui::TextStyle::Body).max(POINT_ICON);
    ui.horizontal_top(|ui| {
        let cell = Vec2::new(POINT_ICON, line);
        let (rect, _) = ui.allocate_exact_size(cell, egui::Sense::hover());
        if ui.is_rect_visible(rect) {
            let icon = egui::Rect::from_center_size(rect.center(), Vec2::splat(POINT_ICON));
            paint(ui.painter(), icon);
        }
        let text_width = (width - POINT_ICON - ui.spacing().item_spacing.x).max(1.0);
        ui.vertical(|ui| {
            ui.set_width(text_width);
            ui.label(text)
        })
        .inner
    })
    .inner
}

/// Height of one segment of a segmented control, in points.
const SEGMENT_HEIGHT: f32 = 32.0;
/// Width around a segment label: both side paddings, the icon and the icon-to-text gap.
const SEGMENT_PADDING: f32 = tokens::ICON_SMALL + tokens::SPACE_SM + 2.0 * tokens::SPACE_MD;

/// Settings sections with their icons, in reading order.
const SETTINGS_SECTIONS: [(SettingsSection, Icon, &str); 3] = [
    (SettingsSection::General, Icon::Sliders, "常规"),
    (SettingsSection::Permissions, Icon::Lock, "系统权限"),
    (SettingsSection::Advanced, Icon::Log, "诊断与资料"),
];
/// Appearance choices; the value is the stored dark mode flag.
const APPEARANCES: [(bool, Icon, &str); 2] =
    [(false, Icon::Sun, "浅色"), (true, Icon::Moon, "深色")];

/// Segmented control. Every segment shows its icon and its label, and the label is its
/// accessible name, so an icon never stands alone. Returns true when a click changed
/// `current`. Hover and selection fade once (M1, M2) and then ask for no more frames.
fn segmented<T: Copy + PartialEq>(
    ui: &mut egui::Ui,
    current: &mut T,
    options: &[(T, Icon, &str)],
) -> bool {
    let mut changed = false;
    let track = egui::Frame::NONE
        .fill(ui.visuals().faint_bg_color)
        .corner_radius(egui::CornerRadius::same(tokens::RADIUS_CONTROL))
        .inner_margin(egui::Margin::same(3));
    track.show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = tokens::SPACE_XS;
            for &(value, icon, label) in options {
                let selected = *current == value;
                if segment(ui, icon, label, selected).clicked() && !selected {
                    *current = value;
                    changed = true;
                }
            }
        });
    });
    changed
}

fn segment(ui: &mut egui::Ui, icon: Icon, label: &str, selected: bool) -> egui::Response {
    let ink = if selected {
        desktop_style::selection_colors(ui).1
    } else {
        ui.visuals().text_color()
    };
    let font = egui::TextStyle::Button.resolve(ui.style());
    let galley = ui.painter().layout_no_wrap(label.to_owned(), font, ink);
    let size = Vec2::new(galley.size().x + SEGMENT_PADDING, SEGMENT_HEIGHT);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    let enabled = ui.is_enabled();
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::Role::RadioButton, enabled, selected, label)
    });
    if ui.is_rect_visible(rect) {
        let ctx = ui.ctx();
        let hovered = response.hovered() && enabled;
        let hover = motion::hover_mix(ctx, response.id.with("hover"), hovered);
        let select = motion::select_mix(ctx, response.id.with("select"), selected);
        let radius = f32::from(tokens::RADIUS_CONTROL) - 2.0;
        let painter = ui.painter();
        let hover_fill = ui.visuals().widgets.hovered.weak_bg_fill;
        painter.rect_filled(rect, radius, tokens::faded(hover_fill, hover));
        let select_fill = desktop_style::selection_colors(ui).0;
        painter.rect_filled(rect, radius, tokens::faded(select_fill, select));
        // Selection also draws an outline, so it never differs from hover by color alone.
        let accent = tokens::accent_color(ui.visuals().dark_mode);
        let ring = if response.has_focus() {
            egui::Stroke::new(1.5, accent)
        } else {
            egui::Stroke::new(1.0, tokens::faded(accent, select))
        };
        painter.rect_stroke(rect.shrink(0.5), radius, ring, egui::StrokeKind::Inside);
        let icon_center = egui::pos2(
            rect.left() + tokens::SPACE_MD + tokens::ICON_SMALL / 2.0,
            rect.center().y,
        );
        let icon_rect = egui::Rect::from_center_size(icon_center, Vec2::splat(tokens::ICON_SMALL));
        icon.paint(painter, icon_rect, ink);
        let text = egui::pos2(
            icon_rect.right() + tokens::SPACE_SM,
            rect.center().y - galley.size().y / 2.0,
        );
        painter.galley(text, galley, ink);
    }
    response
}

/// A key combination of the shortcuts page: its keys, painted as key caps, and its effect.
struct Shortcut {
    keys: &'static [&'static str],
    action: &'static str,
}

static SHORTCUTS: [Shortcut; 2] = [
    Shortcut {
        keys: &["Ctrl", "Alt", "Right"],
        action: "切换控制目标",
    },
    Shortcut {
        keys: &["Ctrl", "Alt", "Esc"],
        action: "紧急恢复本机",
    },
];

/// Height of a key cap and the space on each side of its name, in points.
const KEYCAP_HEIGHT: f32 = 26.0;
const KEYCAP_PADDING: f32 = 10.0;
/// Depth of a key cap: the darker band painted below its face, in points.
const KEYCAP_DEPTH: f32 = 2.0;

/// Key caps joined by plus signs, then the effect of the combination as text.
fn shortcut_row(ui: &mut egui::Ui, shortcut: &Shortcut) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = tokens::SPACE_XS;
        for (index, key) in shortcut.keys.iter().enumerate() {
            if index > 0 {
                ui.weak("+");
            }
            keycap(ui, key);
        }
        ui.add_space(tokens::SPACE_MD);
        ui.label(shortcut.action);
    });
}

/// One raised key with its name centered on the face. The name is also the accessible
/// label, so the graphic never replaces the text.
fn keycap(ui: &mut egui::Ui, key: &str) -> egui::Response {
    let ink = ui.visuals().text_color();
    let font = egui::FontId::monospace(13.0);
    let galley = ui.painter().layout_no_wrap(key.to_owned(), font, ink);
    let width = (galley.size().x + 2.0 * KEYCAP_PADDING).max(KEYCAP_HEIGHT);
    let size = Vec2::new(width, KEYCAP_HEIGHT);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::hover());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, key));
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let radius = f32::from(tokens::RADIUS_CHIP);
        let line = tokens::line_color(ui.visuals().dark_mode);
        painter.rect_filled(rect, radius, line);
        let face = egui::Rect::from_min_max(rect.min, rect.max - Vec2::new(0.0, KEYCAP_DEPTH));
        painter.rect_filled(face, radius, ui.visuals().faint_bg_color);
        let stroke = egui::Stroke::new(1.0, line);
        painter.rect_stroke(face, radius, stroke, egui::StrokeKind::Inside);
        let text = face.center() - galley.size() / 2.0;
        painter.galley(text, galley, ink);
    }
    response
}

static NATIVE_START_RISK: [SafetyPoint; 3] = [
    SafetyPoint {
        icon: Icon::Warning,
        text: "这会启动真实原生键鼠捕获与注入。",
    },
    SafetyPoint {
        icon: Icon::Device,
        text: "实际工作进程将重新检查系统权限。",
    },
    SafetyPoint {
        icon: Icon::Lock,
        text: "连接/认证不代表对端允许本机控制。",
    },
];
static NATIVE_START_WAIT: [SafetyPoint; 3] = [
    SafetyPoint {
        icon: Icon::Device,
        text: "本次会等待另一台电脑约一分钟，可随时停止。",
    },
    SafetyPoint {
        icon: Icon::Lock,
        text: "连接并重新验权后才开始捕获键鼠。",
    },
    SafetyPoint {
        icon: Icon::Stop,
        text: "这不会开启断线自动重连。",
    },
];
static NATIVE_START_RECEIVE_ALLOWED: SafetyPoint = SafetyPoint {
    icon: Icon::Arrow(icons::Direction::Left),
    text: "本机接收偏好：允许已登记对端控制本机。",
};
static NATIVE_START_RECEIVE_DENIED: [SafetyPoint; 2] = [
    SafetyPoint {
        icon: Icon::Lock,
        text: "本机接收偏好：不允许对端控制本机。",
    },
    SafetyPoint {
        icon: Icon::Arrow(icons::Direction::Right),
        text: "可请求控制已授权本机的对端。",
    },
];
static NATIVE_START_REPLACE: [SafetyPoint; 2] = [
    SafetyPoint {
        icon: Icon::Stop,
        text: "当前仅通信会话将先停止。",
    },
    SafetyPoint {
        icon: Icon::Recover,
        text: "清理成功后再以键鼠模式重连；失败不自动继续。",
    },
];
static NATIVE_START_SHORTCUTS: [SafetyPoint; 3] = [
    SafetyPoint {
        icon: Icon::Keyboard,
        text: "Ctrl+Alt+Right 切换。",
    },
    SafetyPoint {
        icon: Icon::Recover,
        text: "Ctrl+Alt+Esc 紧急恢复。",
    },
    SafetyPoint {
        icon: Icon::Warning,
        text: "实机兼容性、DPI、输入法与权限变化仍未完成验收。",
    },
];
static NATIVE_START_RETURN: [SafetyPoint; 2] = [
    SafetyPoint {
        icon: Icon::Arrow(icons::Direction::Left),
        text: "接收端首次物理动作仅用于收回控制，不执行或回放。",
    },
    SafetyPoint {
        icon: Icon::Keyboard,
        text: "切回本机后先松开物理按键／鼠标按钮，再继续操作。",
    },
];

/// W3 explanation in its original order. Conditional groups appear under the same
/// conditions as the sentences they replace.
pub(super) fn native_start_points(
    allow: bool,
    waits: bool,
    replaces_session: bool,
) -> Vec<SafetyPoint> {
    let mut points = NATIVE_START_RISK.to_vec();
    if waits {
        points.extend_from_slice(&NATIVE_START_WAIT);
    }
    if allow {
        points.push(NATIVE_START_RECEIVE_ALLOWED);
    } else {
        points.extend_from_slice(&NATIVE_START_RECEIVE_DENIED);
    }
    if replaces_session {
        points.extend_from_slice(&NATIVE_START_REPLACE);
    }
    points.extend_from_slice(&NATIVE_START_SHORTCUTS);
    points.extend_from_slice(&NATIVE_START_RETURN);
    points
}

static REVOKE_POINTS: [SafetyPoint; 2] = [
    SafetyPoint {
        icon: Icon::Stop,
        text: "停止会话并清除对端信任与本机接收许可。",
    },
    SafetyPoint {
        icon: Icon::Device,
        text: "保留本机身份；下次需重新核验。",
    },
];
static RESET_POINTS: [SafetyPoint; 3] = [
    SafetyPoint {
        icon: Icon::Recover,
        text: "先备份设置和设备登记，再恢复默认并撤销信任。",
    },
    SafetyPoint {
        icon: Icon::Device,
        text: "本机身份保持。",
    },
    SafetyPoint {
        icon: Icon::Lock,
        text: "无效或未来版本只读保留。",
    },
];
static FORCE_STOP_POINTS: [SafetyPoint; 3] = [
    SafetyPoint {
        icon: Icon::Warning,
        text: "仅在正常停止无响应时使用。",
    },
    SafetyPoint {
        icon: Icon::Warning,
        text: "强制结束不能确认输入已释放。",
    },
    SafetyPoint {
        icon: Icon::Lock,
        text: "之后本窗口只允许通信诊断。",
    },
];
static RECOVERY_POINTS: [SafetyPoint; 10] = [
    SafetyPoint {
        icon: Icon::Keyboard,
        text: "松开物理按键/鼠标按钮，检查两端提示与系统权限。",
    },
    SafetyPoint {
        icon: Icon::Lock,
        text: "屏跃不会关闭安全防护，也不把退出当恢复成功。",
    },
    SafetyPoint {
        icon: Icon::Warning,
        text: "进程报告只表示软件路由与清理请求。",
    },
    SafetyPoint {
        icon: Icon::Warning,
        text: "它不代表OS或应用效果已确认。",
    },
    SafetyPoint {
        icon: Icon::Warning,
        text: "恢复未确认时保留警告。",
    },
    SafetyPoint {
        icon: Icon::Lock,
        text: "可运行仅通信诊断；新的原生键鼠会话保持禁止。",
    },
    SafetyPoint {
        icon: Icon::Recover,
        text: "清理失败且原生捕获仍可用时，可请求强制返回：",
    },
    SafetyPoint {
        icon: Icon::Keyboard,
        text: "先松开全部物理按键／按钮；",
    },
    SafetyPoint {
        icon: Icon::Keyboard,
        text: "再重新按下并松开Ctrl+Alt+Esc。",
    },
    SafetyPoint {
        icon: Icon::Warning,
        text: "强制返回仍保留未知警告，不证明系统按键已释放。",
    },
];

/// W4 explanation for `dialog`, in the original sentence order.
fn dialog_points(dialog: Dialog) -> &'static [SafetyPoint] {
    match dialog {
        Dialog::NativeStart => &[],
        Dialog::Revoke => &REVOKE_POINTS,
        Dialog::Reset => &RESET_POINTS,
        Dialog::ForceStop => &FORCE_STOP_POINTS,
        Dialog::Recovery => &RECOVERY_POINTS,
    }
}

pub(super) fn local_device_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "这台 Mac"
    } else if cfg!(windows) {
        "这台 Windows 电脑"
    } else {
        "这台电脑"
    }
}
fn dropped_edge(center: egui::Pos2, local: egui::Rect) -> Option<&'static str> {
    if !center.x.is_finite() || !center.y.is_finite() || local.expand(10.0).contains(center) {
        return None;
    }
    let d = center - local.center();
    Some(if d.x.abs() >= d.y.abs() {
        if d.x < 0.0 {
            "left"
        } else {
            "right"
        }
    } else if d.y < 0.0 {
        "top"
    } else {
        "bottom"
    })
}

pub(super) fn update_layout_drag(
    draft: &mut Option<(Vec2, egui::Pos2, egui::Rect)>,
    events: &[egui::Event],
    peer: egui::Rect,
    local: egui::Rect,
    canvas: egui::Rect,
    editable: bool,
    primary_down: bool,
) -> Option<&'static str> {
    if !editable
        || events
            .iter()
            .any(|event| matches!(event, egui::Event::WindowFocused(false)))
    {
        *draft = None;
        return None;
    }
    for event in events {
        match *event {
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                ..
            } => {
                *draft = if peer.contains(pos) && canvas.contains(pos) {
                    Some((pos - peer.center(), peer.center(), canvas))
                } else {
                    None
                };
            }
            egui::Event::PointerMoved(pos) => {
                if let Some((offset, center, _)) = draft.as_mut() {
                    *center = pos - *offset;
                }
            }
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                ..
            } => {
                if let Some((offset, _, original)) = draft.take() {
                    let center = pos - offset;
                    if original == canvas && canvas.contains(center) {
                        return dropped_edge(center, local);
                    }
                }
            }
            egui::Event::PointerGone
            | egui::Event::MouseWheel { .. }
            | egui::Event::PointerButton { .. }
            | egui::Event::Key {
                key: egui::Key::Escape,
                pressed: true,
                ..
            } => {
                *draft = None;
            }
            _ => (),
        }
    }
    if !primary_down {
        *draft = None;
    }
    None
}

#[cfg(test)]
mod layout_tests {
    use super::*;
    #[test]
    fn clipped_press_and_transient_focus_loss_never_commit_a_drag() {
        let canvas = egui::Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(500.0, 300.0));
        let local = egui::Rect::from_center_size(egui::pos2(250.0, 150.0), Vec2::new(100.0, 80.0));
        let peer = local.translate(Vec2::new(150.0, 0.0));
        let start = peer.center();
        let end = egui::pos2(100.0, 150.0);
        let press = egui::Event::PointerButton {
            pos: start,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: Default::default(),
        };
        let release = egui::Event::PointerButton {
            pos: end,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: Default::default(),
        };
        let events = vec![
            press.clone(),
            egui::Event::PointerMoved(end),
            release.clone(),
        ];
        let mut draft = None;
        assert_eq!(
            update_layout_drag(&mut draft, &events, peer, local, canvas, true, false),
            Some("left")
        );
        let clipped = egui::Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(320.0, 300.0));
        assert_eq!(
            update_layout_drag(&mut draft, &events, peer, local, clipped, true, false),
            None
        );
        let interrupted = vec![
            press,
            egui::Event::WindowFocused(false),
            egui::Event::WindowFocused(true),
            egui::Event::PointerMoved(end),
            release,
        ];
        assert_eq!(
            update_layout_drag(&mut draft, &interrupted, peer, local, canvas, true, false),
            None
        );
        assert!(draft.is_none());
    }
    #[test]
    fn adjacent_screens_drop_on_their_own_edge_at_every_size() {
        // The canvas geometry itself is covered by the workspace canvas tests; this keeps
        // the drop rule for a peer screen placed beside the local one in each direction.
        for side in [24.0, 60.0, 166.0, 300.0] {
            let local = egui::Rect::from_center_size(egui::pos2(400.0, 300.0), Vec2::splat(side));
            let gap = side + 2.0;
            for (edge, offset) in [
                ("left", Vec2::new(-gap, 0.0)),
                ("right", Vec2::new(gap, 0.0)),
                ("top", Vec2::new(0.0, -gap)),
                ("bottom", Vec2::new(0.0, gap)),
            ] {
                let peer = local.translate(offset);
                assert_eq!(dropped_edge(peer.center(), local), Some(edge));
                assert_eq!(dropped_edge(local.center(), local), None);
                assert!(!local.intersects(peer));
            }
        }
    }
    #[test]
    fn dropping_over_local_screen_or_nonfinite_position_cannot_change_layout() {
        let local = egui::Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(166.0, 112.0));
        for center in [
            local.center(),
            local.min,
            local.max,
            egui::pos2(f32::NAN, 0.0),
            egui::pos2(0.0, f32::INFINITY),
        ] {
            assert_eq!(dropped_edge(center, local), None);
        }
    }
}

/// Shared by the dialog tests here and in `worker_permissions`.
#[cfg(test)]
pub(super) mod safety_dialog_tests {
    use super::*;

    /// Text and non-text shapes of one frame. Texts keep the clip they were painted with.
    pub(crate) struct Painted {
        texts: Vec<(String, egui::Rect, egui::Rect)>,
        marks: Vec<egui::Rect>,
    }

    fn collect(shape: &egui::epaint::Shape, clip: egui::Rect, painted: &mut Painted) {
        match shape {
            egui::epaint::Shape::Text(text) => {
                let bounds = text.visual_bounding_rect();
                painted
                    .texts
                    .push((text.galley.job.text.clone(), bounds, clip));
            }
            egui::epaint::Shape::Vec(shapes) => {
                for shape in shapes {
                    collect(shape, clip, painted);
                }
            }
            other => painted.marks.push(other.visual_bounding_rect()),
        }
    }

    impl Painted {
        pub(crate) fn of(output: egui::FullOutput) -> Self {
            let mut painted = Self {
                texts: Vec::new(),
                marks: Vec::new(),
            };
            for clipped in &output.shapes {
                collect(&clipped.shape, clipped.clip_rect, &mut painted);
            }
            output.drop_without_applying_deltas();
            painted
        }

        /// Bounds and clip of the painted text equal to `label`.
        pub(crate) fn text(&self, label: &str) -> Option<(egui::Rect, egui::Rect)> {
            let found = self.texts.iter().find(|(text, _, _)| text == label);
            found.map(|(_, bounds, clip)| (*bounds, *clip))
        }

        /// True when an icon sized shape sits in the cell just left of `text`.
        fn icon_before(&self, text: egui::Rect) -> bool {
            self.marks.iter().any(|mark| {
                mark.is_positive()
                    && mark.width() <= POINT_ICON + 2.0
                    && mark.height() <= POINT_ICON + 2.0
                    && mark.max.x <= text.min.x
                    && mark.min.x >= text.min.x - 2.0 * POINT_ICON - 12.0
                    && mark.min.y < text.min.y + 20.0
                    && mark.max.y > text.min.y - 6.0
            })
        }

        /// Every label is painted inside its clip with an icon beside its first line.
        pub(crate) fn assert_points(&self, labels: &[&str]) {
            assert!(!labels.is_empty());
            for label in labels {
                let Some((bounds, clip)) = self.text(label) else {
                    panic!("{label} absent");
                };
                assert!(clip.contains_rect(bounds), "{label} clipped");
                assert!(self.icon_before(bounds), "{label} has no icon");
            }
        }
    }

    pub(crate) fn texts_of(points: &[SafetyPoint]) -> Vec<&'static str> {
        points.iter().map(|point| point.text).collect()
    }

    /// Text present, at most 30 characters: a point never degrades to an icon alone.
    pub(crate) fn assert_short_sentences(points: &[SafetyPoint]) {
        for point in points {
            let count = point.text.chars().count();
            assert!(!point.text.trim().is_empty(), "a point without text");
            assert!(count <= 30, "{} has {count} characters", point.text);
        }
    }

    fn tall_frame(ctx: &egui::Context, run: impl FnMut(&mut egui::Ui)) -> egui::FullOutput {
        let size = Vec2::new(580.0, 3000.0);
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
            ..Default::default()
        };
        ctx.run_ui(input, run)
    }

    #[test]
    fn every_safety_point_keeps_a_short_sentence_and_its_conditions() {
        for allow in [false, true] {
            for waits in [false, true] {
                for replaces in [false, true] {
                    let points = native_start_points(allow, waits, replaces);
                    assert_short_sentences(&points);
                    let texts = texts_of(&points);
                    let has = |text: &str| texts.iter().copied().any(|item| item == text);
                    assert!(has("这会启动真实原生键鼠捕获与注入。"));
                    assert!(has("连接/认证不代表对端允许本机控制。"));
                    assert!(has("Ctrl+Alt+Esc 紧急恢复。"));
                    assert!(has("实机兼容性、DPI、输入法与权限变化仍未完成验收。"));
                    assert!(has("接收端首次物理动作仅用于收回控制，不执行或回放。"));
                    assert_eq!(has("这不会开启断线自动重连。"), waits);
                    assert_eq!(
                        has("清理成功后再以键鼠模式重连；失败不自动继续。"),
                        replaces
                    );
                    assert_eq!(has("本机接收偏好：允许已登记对端控制本机。"), allow);
                    assert_eq!(has("本机接收偏好：不允许对端控制本机。"), !allow);
                    assert_eq!(has("可请求控制已授权本机的对端。"), !allow);
                }
            }
        }
        assert!(dialog_points(Dialog::NativeStart).is_empty());
        for dialog in [
            Dialog::Revoke,
            Dialog::Reset,
            Dialog::ForceStop,
            Dialog::Recovery,
        ] {
            assert!(!dialog_points(dialog).is_empty());
            assert_short_sentences(dialog_points(dialog));
        }
    }

    #[test]
    fn every_safety_point_renders_its_icon_beside_its_text() -> Result<()> {
        let fixture = crate::tests::Fixture::new()?;
        let mut app = App::load(fixture.path.clone());
        let before = app.config.clone();
        let ctx = crate::tests::ui_context();
        app.theme(&ctx);
        app.dialog = Some(Dialog::NativeStart);
        tall_frame(&ctx, |ui| app.native_start_contents(ui)).drop_without_applying_deltas();
        let painted = Painted::of(tall_frame(&ctx, |ui| app.native_start_contents(ui)));
        let waits = !app.config.listen && !app.config.reconnect;
        let points = native_start_points(app.config.allow, waits, app.child.is_some());
        painted.assert_points(&texts_of(&points));
        for dialog in [
            Dialog::Revoke,
            Dialog::Reset,
            Dialog::ForceStop,
            Dialog::Recovery,
        ] {
            app.dialog = Some(dialog);
            tall_frame(&ctx, |ui| app.dialog_contents(ui, dialog)).drop_without_applying_deltas();
            let painted = Painted::of(tall_frame(&ctx, |ui| app.dialog_contents(ui, dialog)));
            painted.assert_points(&texts_of(dialog_points(dialog)));
            assert!(painted.text("立即停止并恢复本机").is_some());
            assert_eq!(app.dialog, Some(dialog));
        }
        assert!(app.config == before && app.child.is_none() && !app.exit_confirmed);
        assert!(!app.identity_dir().exists() && !app.dir.join("settings.json").exists());
        drop(app);
        fixture.close()?;
        Ok(())
    }
}

#[cfg(test)]
mod subpage_tests {
    use super::*;

    /// Painted texts with their bounds, and the bounds of every other shape.
    struct Shapes {
        texts: Vec<(String, egui::Rect)>,
        marks: Vec<egui::Rect>,
    }

    impl Shapes {
        fn of(output: egui::FullOutput) -> Self {
            fn collect(shape: &egui::epaint::Shape, shapes: &mut Shapes) {
                match shape {
                    egui::epaint::Shape::Text(text) => {
                        let bounds = text.visual_bounding_rect();
                        shapes.texts.push((text.galley.job.text.clone(), bounds));
                    }
                    egui::epaint::Shape::Vec(list) => {
                        for shape in list {
                            collect(shape, shapes);
                        }
                    }
                    other => shapes.marks.push(other.visual_bounding_rect()),
                }
            }
            let mut shapes = Self {
                texts: Vec::new(),
                marks: Vec::new(),
            };
            for clipped in &output.shapes {
                collect(&clipped.shape, &mut shapes);
            }
            output.drop_without_applying_deltas();
            shapes
        }

        /// Bounds of the lowest painted text equal to `label`; the title bar comes first.
        fn lowest(&self, label: &str) -> Option<egui::Rect> {
            let found = self.texts.iter().filter(|(text, _)| text == label);
            let bounds = found.map(|(_, bounds)| *bounds);
            bounds.max_by(|a, b| a.min.y.total_cmp(&b.min.y))
        }

        /// True when a key cap sized shape holds `text` on its face.
        fn key_cap_around(&self, text: egui::Rect) -> bool {
            self.marks.iter().any(|mark| {
                mark.contains_rect(text)
                    && mark.height() <= KEYCAP_HEIGHT + 1.0
                    && mark.width() <= text.width() + 2.0 * KEYCAP_PADDING + 8.0
            })
        }
    }

    fn frame(app: &mut App, ctx: &egui::Context, events: Vec<egui::Event>) -> egui::FullOutput {
        ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    Vec2::new(1180.0, 820.0),
                )),
                events,
                ..Default::default()
            },
            |root| app.desktop(root),
        )
    }

    /// Frames without input until the view switch motion has finished.
    fn settle(app: &mut App, ctx: &egui::Context) {
        for _ in 0..20 {
            frame(app, ctx, vec![]).drop_without_applying_deltas();
        }
    }

    fn click_text(app: &mut App, ctx: &egui::Context, label: &str) -> Result<()> {
        let shapes = Shapes::of(frame(app, ctx, vec![]));
        let at = shapes.lowest(label).context("text absent")?.center();
        for pressed in [true, false] {
            let events = vec![
                egui::Event::PointerMoved(at),
                egui::Event::PointerButton {
                    pos: at,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ];
            frame(app, ctx, events).drop_without_applying_deltas();
        }
        Ok(())
    }

    #[test]
    fn settings_segments_switch_without_writing_and_appearance_saves() -> Result<()> {
        let fixture = crate::tests::Fixture::new()?;
        let mut app = App::load(fixture.path.clone());
        let original = app.config.clone();
        let path = fixture.path.join("settings.json");
        let ctx = crate::tests::ui_context();
        app.theme(&ctx);
        app.navigate_to(Page::Settings);
        settle(&mut app, &ctx);
        for (section, label) in [
            (SettingsSection::Permissions, "系统权限"),
            (SettingsSection::Advanced, "诊断与资料"),
            (SettingsSection::General, "常规"),
        ] {
            click_text(&mut app, &ctx, label)?;
            assert!(app.settings_section == section, "{label} did not switch");
            assert!(app.page == Page::Settings && app.config == original);
            assert!(!path.exists(), "switching {label} wrote the settings");
            assert!(app.child.is_none() && !app.worker_permissions.needs_poll());
        }
        // Appearance keeps its meaning: the choice applies and is saved at once.
        click_text(&mut app, &ctx, "深色")?;
        assert!(app.config.dark);
        let stored: serde_json::Value = serde_json::from_slice(&std::fs::read(&path)?)?;
        assert_eq!(stored["data"]["dark"], true);
        assert!(!app.config.native && !app.config.allow && !app.config.reconnect);
        assert!(app.config.fingerprint.is_none() && app.child.is_none());
        drop(app);
        fixture.close()?;
        Ok(())
    }

    #[test]
    fn shortcut_key_caps_pair_every_key_with_its_action_text() -> Result<()> {
        let fixture = crate::tests::Fixture::new()?;
        let mut app = App::load(fixture.path.clone());
        let ctx = crate::tests::ui_context();
        app.theme(&ctx);
        app.navigate_to(Page::Shortcuts);
        settle(&mut app, &ctx);
        let shapes = Shapes::of(frame(&mut app, &ctx, vec![]));
        for shortcut in &SHORTCUTS {
            let action = shapes.lowest(shortcut.action).context("no action")?;
            let mut keys: Vec<_> = shapes
                .texts
                .iter()
                .filter(|(text, bounds)| {
                    shortcut.keys.contains(&text.as_str())
                        && (bounds.center().y - action.center().y).abs() < 6.0
                        && bounds.max.x < action.min.x
                })
                .collect();
            keys.sort_by(|a, b| a.1.min.x.total_cmp(&b.1.min.x));
            let names: Vec<&str> = keys.iter().map(|(text, _)| text.as_str()).collect();
            assert_eq!(names, shortcut.keys, "{}", shortcut.action);
            for (key, bounds) in keys {
                assert!(shapes.key_cap_around(*bounds), "{key} has no key cap");
            }
        }
        for sentence in [
            "仅在已明确开启的键鼠会话中生效。",
            "切回后先松开物理按键与鼠标按钮。",
        ] {
            assert!(shapes.lowest(sentence).is_some(), "{sentence} absent");
        }
        assert!(app.child.is_none() && !app.config.native && !app.config.allow);
        assert!(!fixture.path.join("settings.json").exists());
        drop(app);
        fixture.close()?;
        Ok(())
    }

    #[test]
    fn former_layout_routes_still_open_the_home_canvas() -> Result<()> {
        let fixture = crate::tests::Fixture::new()?;
        let mut app = App::load(fixture.path.clone());
        // Rendering metadata only; this value never authenticates a peer.
        app.config.fingerprint = Some("layout-presentation-only".into());
        let original = app.config.clone();
        let ctx = crate::tests::ui_context();
        app.theme(&ctx);
        // Setup completion and pairing completion leave the layout tab selected.
        for (page, tab) in [
            (Page::Devices, DeviceTab::Layout),
            (Page::Displays, DeviceTab::Layout),
            (Page::Displays, DeviceTab::Device),
        ] {
            app.page = page;
            app.device_tab = tab;
            let shapes = Shapes::of(frame(&mut app, &ctx, vec![]));
            let canvas = shapes.lowest("左侧").is_some();
            assert!(canvas, "canvas direction control absent");
            assert!(app.config == original && app.child.is_none() && !app.pairing_open);
        }
        assert!(!fixture.path.join("settings.json").exists());
        drop(app);
        fixture.close()?;
        Ok(())
    }
}
