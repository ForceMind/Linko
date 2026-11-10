//! Device-first desktop: spatial canvas and contextual controls; the roster is in the shell.
//! Presentation only; existing permission, enrollment and worker gates own actions.
use super::*;
use desktop_style::{card, tokens, Tone};
use icons::{Direction, Icon};

/// Direction buttons in reading order, each with its arrow.
const EDGES: [(&str, Direction); 4] = [
    ("left", Direction::Left),
    ("right", Direction::Right),
    ("top", Direction::Up),
    ("bottom", Direction::Down),
];
/// Side of the square toolbar buttons and height of the direction buttons, in points.
const CANVAS_BUTTON_SIDE: f32 = 32.0;
/// Opacity of the screens while the layout cannot be edited.
const LOCKED_OPACITY: f32 = 0.45;
/// Radius of a screen status badge at full size, in points.
const BADGE_RADIUS: f32 = 10.0;
/// Height of the canvas chips and their distance from the canvas corner, in points.
const CHIP_HEIGHT: f32 = 24.0;
const CHIP_INSET: f32 = 10.0;
/// Design width that two side-by-side screens are scaled into. A drop only commits outside
/// the local screen plus 10 points (`dropped_edge`), so the screens must leave a real drop
/// zone beside it: at 530 a 920 point window kept about 33 points, now about 79.
const SIDE_BY_SIDE_W: f32 = 700.0;
/// Offsets shorter than this snap instead of rebounding, so a plain click never animates.
const REBOUND_MIN_PX: f32 = 0.5;
/// Segments of the filled half of the pending badge.
const HALF_DISC_STEPS: usize = 12;
/// Shown under the direction buttons while the layout is locked without a session.
const LOCKED_NOTE: &str = "当前位置已锁定，暂不能调整方向。";
/// Hover text of the sketch chip; the canvas is never measured display geometry.
const SKETCH_HINT: &str = "仅表示两台主屏的相对方向，不代表真实尺寸。";

/// The former layout subpage (L9) is this canvas now. Its old body, `App::displays` in
/// `desktop_ui.rs`, lies outside this slice's files and goes with that file's next slice;
/// until then this reference keeps it compiled although the shell no longer calls it.
#[allow(dead_code)]
const FORMER_LAYOUT_PAGE: fn(&mut App, &mut egui::Ui) = App::displays;

impl App {
    /// Home view: the screen canvas with the device context column. The device list lives
    /// in the sidebar, so the former device and layout pages share this one view; the
    /// layout tab that pairing and setup still select opens this same canvas.
    pub(super) fn device_workspace(&mut self, ui: &mut egui::Ui) {
        if self.device_tab == DeviceTab::Permissions {
            let back = Icon::Arrow(Direction::Left);
            if shell_ui::icon_button(ui, back, "返回首页", true).clicked() {
                self.device_tab = DeviceTab::Device;
            }
            ui.add_space(12.0);
            card(ui).show(ui, |ui| self.device_permissions(ui));
            return;
        }
        let width = ui.available_width();
        if width >= 600.0 {
            let compact = width < 780.0;
            let details = if compact {
                228.0
            } else {
                (width * 0.32).clamp(290.0, 340.0)
            };
            let gap = if compact { 16.0 } else { 24.0 };
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                ui.vertical(|ui| {
                    ui.set_width(width - details - gap);
                    self.workspace_screen_card(ui);
                });
                ui.vertical(|ui| {
                    ui.set_width(details);
                    self.workspace_device_detail(ui);
                });
            });
        } else {
            self.workspace_screen_card(ui);
            ui.add_space(12.0);
            self.workspace_device_detail(ui);
        }
    }

    fn workspace_screen_card(&mut self, ui: &mut egui::Ui) {
        let compact = ui.available_width() < 500.0;
        let margin = if compact { 12.0 } else { 16.0 };
        card(ui).inner_margin(margin).show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                self.workspace_zoom_controls(ui);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let details = if self.connection_details_open {
                        "收起连接详情"
                    } else {
                        "连接与身份详情"
                    };
                    if ui.small_button(details).clicked() {
                        self.connection_details_open = !self.connection_details_open;
                    }
                });
            });
            self.workspace_canvas(ui);
            self.workspace_direction_controls(ui);
            // Explains the selector directly above, so it stays in the first viewport.
            ui.small(self.workspace_direction_note());
        });
        ui.add_space(8.0);
        ui.label(desktop_style::semibold("设备详情", 17.0));
        self.workspace_device_cards(ui);
    }

    /// Icon-only zoom buttons around the zoom level; each keeps its name for assistive use.
    fn workspace_zoom_controls(&mut self, ui: &mut egui::Ui) {
        let idle = self.screen_drag.is_none();
        let zoom = self.workspace_zoom;
        if toolbar_button(ui, "zoom-out", Icon::Minus, "缩小视图", idle && zoom > 0.6) {
            self.workspace_zoom = (zoom - 0.2).max(0.6);
        }
        ui.label(format!("{:.0}%", self.workspace_zoom * 100.0));
        if toolbar_button(ui, "zoom-in", Icon::Plus, "放大视图", idle && zoom < 1.0) {
            self.workspace_zoom = (zoom + 0.2).min(1.0);
        }
        if toolbar_button(ui, "zoom-fit", Icon::Fit, "适配视图", idle) {
            self.workspace_zoom = 1.0;
        }
    }

    /// Four arrow buttons with their short direction word. They go through `try_set_edge`
    /// exactly like a drop, and are disabled whenever the layout is locked or being dragged.
    fn workspace_direction_controls(&mut self, ui: &mut egui::Ui) {
        let editable = self.layout_editable() && self.screen_drag.is_none();
        ui.add_enabled_ui(editable, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.small("另一台在本机");
                for (side, direction) in EDGES {
                    let word = edge_label(side);
                    let name = format!("另一台在本机{word}");
                    let id = canvas_control_id(side);
                    let selected = self.displayed_edge() == side;
                    let icon = Icon::Arrow(direction);
                    if canvas_button(ui, id, icon, &name, Some(word), selected).clicked() {
                        self.try_set_edge(side);
                    }
                }
            });
        });
    }

    /// One line under the direction buttons. The locked sentence keeps the former layout
    /// page's explanation visible, not only in a hover.
    fn workspace_direction_note(&self) -> &'static str {
        if self.child.is_some() {
            "连接中可在本机控制时调整方向；新布局等待工作进程确认。"
        } else if self.config.fingerprint.is_some() && !self.layout_editable() {
            LOCKED_NOTE
        } else {
            "两台主屏的相对方向；另一端需设置相反方向。"
        }
    }

    fn workspace_device_cards(&mut self, ui: &mut egui::Ui) {
        let registered = self.config.fingerprint.is_some();
        let compact = ui.available_width() < 500.0;
        ui.columns(if registered { 2 } else { 1 }, |columns| {
            for (index, column) in columns.iter_mut().enumerate() {
                let peer = index == 1;
                let selected = if peer {
                    self.workspace_peer_selected
                } else {
                    !self.workspace_peer_selected || !registered
                };
                let name = if peer {
                    self.config.peer_name.clone()
                } else {
                    desktop_ui::local_device_name().into()
                };
                let frame = card(column)
                    .inner_margin(if compact { 10.0 } else { 14.0 })
                    .fill(if selected && !column.visuals().dark_mode {
                        egui::Color32::from_rgb(251, 252, 255)
                    } else {
                        column.visuals().window_fill
                    })
                    .stroke(egui::Stroke::new(
                        1.0,
                        if selected {
                            column.visuals().selection.bg_fill
                        } else {
                            column.visuals().widgets.noninteractive.bg_stroke.color
                        },
                    ));
                let response = frame.show(column, |ui| {
                    ui.set_min_width(ui.available_width());
                    ui.spacing_mut().item_spacing.x = 8.0;
                    ui.spacing_mut().item_spacing.y = if compact { 3.0 } else { 5.0 };
                    ui.set_min_height(if compact { 70.0 } else { 96.0 });
                    read_only_row(ui, |ui| {
                        device_glyph(
                            ui,
                            if peer {
                                visual_assets::DeviceKind::Unknown
                            } else {
                                visual_assets::local_device_kind()
                            },
                        );
                        let name_width = (ui.available_width() - 64.0).max(1.0);
                        desktop_ui::left_text_slot(
                            ui,
                            name_width,
                            24.0,
                            desktop_style::semibold(&name, if compact { 14.0 } else { 16.0 }),
                        )
                        .on_hover_text(&name);
                        let (label, tone) = if !peer {
                            ("本机", Tone::Accent)
                        } else if self.pairing_pending() {
                            ("待确认", Tone::Warning)
                        } else if self.stopping.is_some() {
                            ("断开中", Tone::Neutral)
                        } else if self.connected && self.child.is_some() {
                            ("已连接", Tone::Good)
                        } else if self.child.is_some() {
                            ("连接中", Tone::Neutral)
                        } else {
                            ("已登记", Tone::Neutral)
                        };
                        desktop_style::status_chip(ui, label, tone);
                    });
                    ui.add_space(2.0);
                    if peer {
                        card_metadata(
                            ui,
                            "登记",
                            if self.pairing_pending() {
                                "等待对端回执"
                            } else {
                                "已登记设备"
                            },
                        );
                    } else {
                        card_metadata(ui, "系统", local_system_name());
                    }
                    let kind = if peer {
                        visual_assets::DeviceKind::Unknown
                    } else {
                        visual_assets::local_device_kind()
                    };
                    card_metadata(ui, "设备", device_kind_label(kind));
                    card_metadata(ui, "会话", self.input_label());
                });
                let id = column.scope_id().with(if peer {
                    "peer-device-card"
                } else {
                    "local-device-card"
                });
                if column
                    .interact(response.response.rect, id, egui::Sense::click())
                    .clicked()
                {
                    self.workspace_peer_selected = peer;
                    self.screen_drag = None;
                }
            }
        });
    }

    fn peer_connection_label(&self) -> &'static str {
        if self.pairing_pending() {
            "核验尚未完成"
        } else if self.stopping.is_some() {
            "正在断开连接"
        } else if self.connected && self.child.is_some() {
            "已连接"
        } else if self.child.is_some() {
            "等待连接"
        } else if self.config.fingerprint.is_some() {
            "尚未连接"
        } else {
            "尚未添加电脑"
        }
    }

    /// Badge and full status of the local screen. An unconfirmed recovery outranks the rest.
    fn local_badge(&self) -> (Badge, &'static str) {
        const UNCONFIRMED: (Badge, &str) = (Badge::RecoveryUnconfirmed, "输入恢复未确认");
        if self.recovery_blocked {
            return UNCONFIRMED;
        }
        match self.child.as_ref().and(self.input_state) {
            Some(WorkerInputState::RecoveryUnconfirmed) => UNCONFIRMED,
            Some(WorkerInputState::Recovering) => (Badge::Recovering, "正在恢复本机输入"),
            Some(WorkerInputState::ReceivingRemote) => (Badge::Receiving, "另一台正在控制本机"),
            _ => (Badge::Local, "本机"),
        }
    }

    /// Badge of the peer screen; its text is the connection label of the context column.
    fn peer_badge(&self) -> (Badge, &'static str) {
        let label = self.peer_connection_label();
        let badge = if self.pairing_pending() {
            Badge::Pending
        } else if self.stopping.is_some() {
            Badge::Stopping
        } else if self.connected && self.child.is_some() {
            if self.input_state == Some(WorkerInputState::ControllingRemote) {
                return (Badge::Controlling, "正在控制另一台");
            }
            Badge::Connected
        } else if self.child.is_some() {
            Badge::Connecting
        } else {
            Badge::Offline
        };
        (badge, label)
    }

    fn workspace_canvas(&mut self, ui: &mut egui::Ui) {
        let registered = self.config.fingerprint.is_some();
        let height = if ui.available_width() < 440.0 {
            180.0
        } else {
            280.0
        };
        let (canvas, _) = ui.allocate_exact_size(
            Vec2::new(ui.available_width(), height),
            egui::Sense::hover(),
        );
        let area = canvas.shrink(20.0);
        let zoom = self.workspace_zoom;
        let (local, peer) = workspace_rects(area, self.displayed_edge(), registered, zoom);
        let (focused, down, events) =
            ui.input(|i| (i.focused, i.pointer.primary_down(), i.events.clone()));
        let layer = ui.layer_id();
        let own_press = events.iter().all(|event| match event {
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                ..
            } => ui.ctx().layer_id_at(*pos) == Some(layer),
            _ => true,
        });
        let editable = self.layout_editable();
        let interaction_canvas = canvas.intersect(ui.clip_rect());
        if self
            .screen_drag
            .is_some_and(|(_, _, old)| old != interaction_canvas)
        {
            self.screen_drag = None;
        }
        let before = self.screen_drag;
        if let Some(edge) = desktop_ui::update_layout_drag(
            &mut self.screen_drag,
            &events,
            peer,
            local,
            interaction_canvas,
            editable && focused && own_press,
            down,
        ) {
            self.try_set_edge(edge);
        }
        let ended = before.is_some() && self.screen_drag.is_none();
        let released = if ended {
            released_center(before, &events)
        } else {
            None
        };
        // A committed direction moves both screens; draw and hit-test the new layout now.
        let (local, peer) = workspace_rects(area, self.displayed_edge(), registered, zoom);
        let ctx = ui.ctx().clone();
        let now = ctx.input(|input| input.time);
        let layout = (edge_code(self.displayed_edge()), registered);
        let motion_id = egui::Id::unique("linko-canvas-motion");
        let rest = || CanvasMotion::rest(layout, canvas.min, (local, peer));
        let stored = ctx.data(|data| data.get_temp::<CanvasMotion>(motion_id));
        let mut state = stored.unwrap_or_else(rest);
        state.retarget(layout, canvas.min, (local, peer), released, now);
        let remaining = state.remaining(&ctx);
        let shown_local = local.translate(state.from_local * remaining);
        let shown_peer = match self.screen_drag {
            Some((_, center, _)) => egui::Rect::from_center_size(center, peer.size()),
            None => peer.translate(state.from_peer * remaining),
        };
        state.local_drawn = shown_local.center() - canvas.min;
        state.peer_drawn = shown_peer.center() - canvas.min;

        let locked = registered && !self.layout_editable() && self.screen_drag.is_none();
        let painter = ui.painter_at(canvas);
        desktop_style::canvas_wash(&painter, canvas, self.config.dark);
        let mut devices = painter.clone();
        if locked {
            devices.multiply_opacity(LOCKED_OPACITY);
        }
        if registered {
            let dot = ui.visuals().widgets.noninteractive.bg_stroke.color;
            let stroke = egui::Stroke::new(1.0, dot);
            devices.line_segment([shown_local.center(), shown_peer.center()], stroke);
        }
        let head = if self.activity_tick_active() {
            motion::spinner_head_at(now)
        } else {
            0
        };
        let pop = tokens::BADGE_POP_SECS;
        let (badge, status) = self.local_badge();
        state.local_badge.observe(badge, now, pop);
        let local_screen = ScreenView {
            id: ui.scope_id().with("local-screen"),
            target: local,
            shown: shown_local,
            name: desktop_ui::local_device_name(),
            status,
            hint: None,
            kind: visual_assets::local_device_kind(),
            selected: !self.workspace_peer_selected || !registered,
            badge,
            badge_scale: motion::badge_scale(state.local_badge.tick(&ctx)),
            spinner_head: head,
        };
        if canvas_screen(ui, &devices, local_screen).clicked() {
            self.workspace_peer_selected = false;
        }
        if registered {
            let (badge, status) = self.peer_badge();
            state.peer_badge.observe(badge, now, pop);
            let hint = if !editable {
                None
            } else if self.child.is_some() {
                Some("拖动屏幕或点方向按钮调整；新布局等待工作进程确认。")
            } else {
                Some("拖动屏幕或点方向按钮调整；位置用于下一次会话。")
            };
            let peer_screen = ScreenView {
                id: ui.scope_id().with("peer-screen"),
                target: peer,
                shown: shown_peer,
                name: &self.config.peer_name,
                status,
                hint,
                kind: visual_assets::DeviceKind::Unknown,
                selected: self.workspace_peer_selected,
                badge,
                badge_scale: motion::badge_scale(state.peer_badge.tick(&ctx)),
                spinner_head: head,
            };
            if canvas_screen(ui, &devices, peer_screen).clicked() {
                self.workspace_peer_selected = true;
            }
        } else {
            let add = egui::Rect::from_center_size(peer.center(), Vec2::new(peer.width(), 66.0));
            ui.add_enabled_ui(self.idle_writable(), |ui| {
                let response = ui.put(add, egui::Button::new("＋\n添加另一台电脑"));
                self.record_layout_control("add_device", &response, ui.clip_rect());
                if response.clicked() {
                    self.open_setup();
                }
            });
        }
        ctx.data_mut(|data| data.insert_temp(motion_id, state));
        let corner = Vec2::splat(CHIP_INSET);
        let sketch = canvas.left_top() + corner;
        canvas_chip(
            ui,
            sketch,
            egui::Align2::LEFT_TOP,
            None,
            "示意",
            SKETCH_HINT,
        );
        if locked {
            let at = canvas.right_top() + egui::vec2(-CHIP_INSET, CHIP_INSET);
            let lock = Some(Icon::Lock);
            canvas_chip(ui, at, egui::Align2::RIGHT_TOP, lock, "已锁定", LOCKED_NOTE);
        }
    }

    fn workspace_device_detail(&mut self, ui: &mut egui::Ui) {
        let compact = ui.available_width() < 270.0;
        let margin = if compact { 12.0 } else { 16.0 };
        let gap = if compact { 8.0 } else { 12.0 };
        card(ui).inner_margin(margin).show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.spacing_mut().item_spacing.x = 8.0;
            ui.spacing_mut().item_spacing.y = 4.0;
            ui.label(desktop_style::semibold("连接状态", 16.0));
            ui.add_space(4.0);
            let connection = self.peer_connection_label();
            let tone = if self.connected && self.child.is_some() && !self.pairing_pending() {
                Tone::Good
            } else {
                Tone::Neutral
            };
            let connection_text =
                desktop_style::status_text(connection, self.config.dark, tone).font(
                    egui::FontId::new(18.0, egui::FontFamily::Name("linko-semibold".into())),
                );
            if self.child.is_some() {
                ui.horizontal(|ui| {
                    let width = (ui.available_width() - 120.0).max(1.0);
                    ui.add_sized([width, 38.0], egui::Label::new(connection_text).truncate())
                        .on_hover_text(connection);
                    if ui
                        .add_sized([112.0, 38.0], egui::Button::new("停止并断开"))
                        .clicked()
                    {
                        self.stop();
                    }
                });
            } else {
                ui.label(connection_text);
            }
            let view = self.workspace_view();
            let needs_attention = matches!(view.tone, Tone::Warning | Tone::Danger)
                || (!self.connected && view.action == desktop_state::Action::None)
                || self.last_failure
                || self.peer_wait_expired
                || self.pairing_pending();
            if needs_attention {
                ui.separator();
                ui.label(desktop_style::status_text(
                    view.title,
                    self.config.dark,
                    view.tone,
                ));
                ui.label(view.detail);
            }
        });
        ui.add_space(gap);
        card(ui).inner_margin(margin).show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.spacing_mut().item_spacing.x = 8.0;
            ui.spacing_mut().item_spacing.y = if compact { 4.0 } else { 6.0 };
            ui.label(desktop_style::semibold("快捷操作", 17.0));
            read_only_row(ui, |ui| {
                ui.small("所选设备");
                let name = if self.workspace_peer_selected && self.config.fingerprint.is_some() {
                    self.config.peer_name.as_str()
                } else {
                    desktop_ui::local_device_name()
                };
                ui.add(egui::Label::new(desktop_style::semibold(name, 14.0)).truncate())
                    .on_hover_text(name);
            });
            let view = self.workspace_view();
            let allowed = match view.action {
                desktop_state::Action::AddDevice | desktop_state::Action::Connect => {
                    self.idle_writable()
                }
                desktop_state::Action::CheckPermissions | desktop_state::Action::StartInput => {
                    self.native_start_available()
                }
                desktop_state::Action::Recovery => true,
                desktop_state::Action::None => false,
            };
            if view.action != desktop_state::Action::None {
                let response = ui
                    .add_enabled(
                        allowed && self.dialog.is_none() && self.wizard.is_none(),
                        desktop_style::primary(view.action.label()),
                    )
                    .on_hover_text(view.detail);
                self.record_layout_control("primary_action", &response, ui.clip_rect());
                if response.clicked() {
                    self.workspace_action(view.action);
                }
            }
            text_row(ui, "当前会话", self.input_label());
            ui.separator();
            let readiness = self.worker_permissions.readiness();
            let label = match readiness {
                worker_permissions::Readiness::Ready => "刚刚检查通过",
                worker_permissions::Readiness::Checking => "正在检查",
                worker_permissions::Readiness::Unchecked => "尚未检查或已过期",
                _ => "需要处理",
            };
            text_row(ui, "本机权限", label);
            ui.horizontal_wrapped(|ui| {
                ui.small("检查不会开启键鼠");
                if ui
                    .small_button("权限详情")
                    .on_hover_text(readiness.detail())
                    .clicked()
                {
                    self.navigate_to(Page::Settings);
                    self.settings_section = SettingsSection::Permissions;
                }
            });
            ui.horizontal_wrapped(|ui| {
                ui.small("本机接收控制");
                ui.small(if self.config.allow {
                    "已允许"
                } else {
                    "默认关闭"
                });
                if ui
                    .add_enabled(
                        self.config.fingerprint.is_some(),
                        egui::Button::new("设置").small(),
                    )
                    .clicked()
                {
                    self.device_tab = DeviceTab::Permissions;
                }
            });
            if self.workspace_peer_selected && self.config.fingerprint.is_some() {
                ui.small("对端的接收许可由对方单独设置。");
            }
            ui.small("开启键鼠仍需本次明确确认。");
        });
        ui.add_space(gap);
        // Not connected capabilities stay labelled (decision 2); the page explains them.
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("文件 · 剪贴板 未接通").small().weak());
            if ui.small_button("说明").clicked() {
                self.navigate_to(Page::Transfers);
            }
        });
    }
}

fn local_system_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "macOS"
    } else if cfg!(windows) {
        "Windows"
    } else if cfg!(target_os = "linux") {
        "Linux"
    } else {
        "系统信息未知"
    }
}

fn device_kind_label(kind: visual_assets::DeviceKind) -> &'static str {
    match kind {
        visual_assets::DeviceKind::Laptop => "笔记本电脑",
        visual_assets::DeviceKind::Desktop => "台式电脑",
        visual_assets::DeviceKind::Unknown => "类型未识别",
    }
}

fn device_glyph(ui: &mut egui::Ui, kind: visual_assets::DeviceKind) {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(22.0), egui::Sense::hover());
    response.on_hover_text(device_kind_label(kind));
    let painter = ui.painter();
    if kind == visual_assets::DeviceKind::Unknown {
        generic_device_symbol(
            painter,
            rect,
            ui.visuals().text_color(),
            ui.visuals().window_fill,
        );
        return;
    }
    let screen = egui::Rect::from_min_max(
        rect.min + egui::vec2(2.0, 3.0),
        rect.min + egui::vec2(20.0, 16.0),
    );
    let stroke = egui::Stroke::new(1.3, ui.visuals().text_color());
    painter.rect_stroke(screen, 2.0, stroke, egui::StrokeKind::Inside);
    match kind {
        visual_assets::DeviceKind::Laptop => {
            painter.line_segment(
                [
                    rect.min + egui::vec2(1.0, 19.0),
                    rect.min + egui::vec2(21.0, 19.0),
                ],
                stroke,
            );
        }
        visual_assets::DeviceKind::Desktop => {
            painter.line_segment(
                [
                    rect.min + egui::vec2(11.0, 16.0),
                    rect.min + egui::vec2(11.0, 20.0),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    rect.min + egui::vec2(6.0, 20.0),
                    rect.min + egui::vec2(16.0, 20.0),
                ],
                stroke,
            );
        }
        visual_assets::DeviceKind::Unknown => {}
    }
}

fn generic_device_symbol(
    painter: &egui::Painter,
    rect: egui::Rect,
    ink: egui::Color32,
    surface: egui::Color32,
) {
    let size = rect.size();
    let stroke = egui::Stroke::new((size.y * 0.014).clamp(1.2, 2.0), ink);
    let first = egui::Rect::from_min_size(
        rect.min + egui::vec2(size.x * 0.12, size.y * 0.18),
        egui::vec2(size.x * 0.50, size.y * 0.44),
    );
    let second = egui::Rect::from_min_size(
        rect.min + egui::vec2(size.x * 0.38, size.y * 0.40),
        egui::vec2(size.x * 0.50, size.y * 0.44),
    );
    let radius = (size.y * 0.04).clamp(1.0, 5.0);
    painter.rect_filled(first, radius, surface);
    painter.rect_stroke(first, radius, stroke, egui::StrokeKind::Inside);
    painter.rect_filled(second, radius, surface);
    painter.rect_stroke(second, radius, stroke, egui::StrokeKind::Inside);
    painter.line_segment(
        [
            rect.min + egui::vec2(size.x * 0.34, size.y * 0.69),
            rect.min + egui::vec2(size.x * 0.65, size.y * 0.34),
        ],
        stroke,
    );
}

fn read_only_row(ui: &mut egui::Ui, add_contents: impl FnOnce(&mut egui::Ui)) {
    ui.allocate_ui_with_layout(
        Vec2::new(ui.available_width(), 24.0),
        egui::Layout::left_to_right(egui::Align::Center),
        add_contents,
    );
}

fn card_metadata(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.allocate_ui_with_layout(
        Vec2::new(ui.available_width(), 20.0),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            desktop_ui::left_text_slot(ui, 38.0, 20.0, RichText::new(label).size(12.0).weak());
            ui.add(egui::Label::new(RichText::new(value).size(13.0)).truncate())
                .on_hover_text(value);
        },
    );
}

fn text_row(ui: &mut egui::Ui, label: &str, value: &str) {
    read_only_row(ui, |ui| {
        desktop_style::detail_icon(ui, label == "本机权限");
        ui.add_sized(
            [52.0, 20.0],
            egui::Label::new(RichText::new(label).size(13.0).weak()),
        );
        ui.add(egui::Label::new(RichText::new(value).size(14.0)).truncate())
            .on_hover_text(value);
    });
}

/// Status badge on a canvas screen: one shape per state, so color only supports it
/// (spec 4.3). The full status stays in the screen's accessible name and hover.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Badge {
    Local,
    Connected,
    Offline,
    Connecting,
    Stopping,
    Pending,
    Controlling,
    Receiving,
    Recovering,
    RecoveryUnconfirmed,
}

impl Badge {
    fn tone(self) -> Tone {
        match self {
            Badge::Local | Badge::Connecting => Tone::Accent,
            Badge::Controlling | Badge::Receiving => Tone::Accent,
            Badge::Connected => Tone::Good,
            Badge::Offline => Tone::Neutral,
            Badge::Stopping | Badge::Pending | Badge::Recovering => Tone::Warning,
            Badge::RecoveryUnconfirmed => Tone::Danger,
        }
    }
}

/// Canvas motion kept in egui's temporary memory under one fixed id: the M7 rebound of
/// both screens and the M6 badge pops. Constant size; finished motion asks for no repaint.
#[derive(Clone)]
struct CanvasMotion {
    /// Direction code and registration the screens were last laid out for.
    layout: (u8, bool),
    /// Centers drawn last frame, relative to the canvas corner so scrolling never animates.
    local_drawn: Vec2,
    peer_drawn: Vec2,
    /// Offsets from the target at the start of the rebound; they shrink to zero.
    from_local: Vec2,
    from_peer: Vec2,
    rebound: motion::OneShot,
    local_badge: motion::Retrigger<Badge>,
    peer_badge: motion::Retrigger<Badge>,
}

impl CanvasMotion {
    fn rest(layout: (u8, bool), origin: egui::Pos2, targets: (egui::Rect, egui::Rect)) -> Self {
        let (local, peer) = targets;
        Self {
            layout,
            local_drawn: local.center() - origin,
            peer_drawn: peer.center() - origin,
            from_local: Vec2::ZERO,
            from_peer: Vec2::ZERO,
            rebound: motion::OneShot::finished(),
            local_badge: motion::Retrigger::default(),
            peer_badge: motion::Retrigger::default(),
        }
    }

    /// Start the rebound when a drag ended (`released` is where the dragged screen was let
    /// go) or the laid-out direction changed. Both screens glide from where they were drawn.
    fn retarget(
        &mut self,
        layout: (u8, bool),
        origin: egui::Pos2,
        targets: (egui::Rect, egui::Rect),
        released: Option<egui::Pos2>,
        now: f64,
    ) {
        let (local, peer) = targets;
        let from_local = origin + self.local_drawn - local.center();
        let from_peer = match released {
            Some(center) => center - peer.center(),
            None => origin + self.peer_drawn - peer.center(),
        };
        let moved = released.is_some() || layout != self.layout;
        self.layout = layout;
        if moved && from_local.length().max(from_peer.length()) > REBOUND_MIN_PX {
            self.from_local = from_local;
            self.from_peer = from_peer;
            self.rebound = motion::OneShot::start(now, tokens::REBOUND_SECS);
        }
    }

    /// Remaining share of the rebound offsets. Requests a repaint only while it plays.
    fn remaining(&self, ctx: &egui::Context) -> f32 {
        1.0 - motion::cubic_out(self.rebound.tick(ctx))
    }
}

/// Everything one canvas screen needs for a frame.
struct ScreenView<'a> {
    id: egui::Id,
    /// Laid-out rectangle; clicks and focus use it, never the animated one.
    target: egui::Rect,
    shown: egui::Rect,
    name: &'a str,
    status: &'a str,
    hint: Option<&'a str>,
    kind: visual_assets::DeviceKind,
    selected: bool,
    badge: Badge,
    badge_scale: f32,
    spinner_head: usize,
}

/// Paint one screen with its selection ring (M2), focus ring and status badge, as a
/// focusable button named after the device and its status.
fn canvas_screen(ui: &egui::Ui, painter: &egui::Painter, view: ScreenView<'_>) -> egui::Response {
    let response = ui.interact(view.target, view.id, egui::Sense::click());
    let label = format!("{} · {}", view.name, view.status);
    let enabled = ui.is_enabled();
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, &label));
    let ring = motion::select_mix(ui.ctx(), view.id.with("select"), view.selected);
    let focused = response.has_focus();
    let frame = draw_workspace_device(ui, painter, view.shown, view.name, view.kind, ring, focused);
    let badge = (view.badge, view.badge_scale, view.spinner_head);
    paint_badge(painter, frame.right_top(), badge, ui.visuals());
    let hover = match view.hint {
        Some(hint) => format!("{label}\n{hint}"),
        None => label,
    };
    response.on_hover_text(hover)
}

/// Badge disc at `center` with a cut-out ring in the surface color. `badge` carries the
/// state, the M6 scale and the spinner cell, which only moves on existing beats.
fn paint_badge(
    painter: &egui::Painter,
    center: egui::Pos2,
    badge: (Badge, f32, usize),
    visuals: &egui::Visuals,
) {
    let (badge, scale, head) = badge;
    let radius = BADGE_RADIUS * scale;
    let (fill, ink) = desktop_style::status_colors(visuals.dark_mode, badge.tone());
    // Danger is a solid red disc, so it reads as different even without color vision.
    let (disc, glyph) = match badge.tone() {
        Tone::Danger => (ink, visuals.window_fill),
        _ => (fill, ink),
    };
    painter.circle_filled(center, radius + 2.0 * scale, visuals.window_fill);
    painter.circle_filled(center, radius, disc);
    let rect = egui::Rect::from_center_size(center, Vec2::splat(radius * 1.4));
    let stroke = egui::Stroke::new(icons::STROKE_WIDTH, glyph);
    match badge {
        Badge::Local => {
            painter.circle_stroke(center, radius * 0.5, stroke);
            painter.circle_filled(center, radius * 0.2, glyph);
        }
        Badge::Connected => {
            painter.circle_filled(center, radius * 0.45, glyph);
        }
        Badge::Offline => {
            painter.circle_stroke(center, radius * 0.45, stroke);
        }
        Badge::Connecting => icons::paint_spinner(painter, rect, glyph, head),
        Badge::Stopping => Icon::Stop.paint(painter, rect, glyph),
        Badge::Pending => paint_half_disc(painter, center, radius * 0.5, glyph),
        Badge::Controlling => Icon::Arrow(Direction::Right).paint(painter, rect, glyph),
        Badge::Receiving => Icon::Arrow(Direction::Left).paint(painter, rect, glyph),
        Badge::Recovering => Icon::Recover.paint(painter, rect, glyph),
        Badge::RecoveryUnconfirmed => Icon::Warning.paint(painter, rect, glyph),
    }
}

/// Ring with its left half filled: started but not finished, as a pending receipt.
fn paint_half_disc(painter: &egui::Painter, center: egui::Pos2, radius: f32, ink: egui::Color32) {
    let mut points = Vec::with_capacity(HALF_DISC_STEPS + 1);
    for step in 0..=HALF_DISC_STEPS {
        let turn = step as f32 / HALF_DISC_STEPS as f32;
        let angle = std::f32::consts::FRAC_PI_2 + std::f32::consts::PI * turn;
        points.push(center + Vec2::angled(angle) * radius);
    }
    painter.add(egui::Shape::convex_polygon(points, ink, egui::Stroke::NONE));
    painter.circle_stroke(center, radius, egui::Stroke::new(icons::STROKE_WIDTH, ink));
}

/// Short labelled chip on the canvas; its hover and accessible description add the sentence.
fn canvas_chip(
    ui: &egui::Ui,
    anchor: egui::Pos2,
    align: egui::Align2,
    icon: Option<Icon>,
    text: &str,
    hint: &str,
) {
    let (fill, ink) = desktop_style::status_colors(ui.visuals().dark_mode, Tone::Neutral);
    let painter = ui.painter();
    let font = egui::FontId::proportional(12.0);
    let galley = painter.layout_no_wrap(text.to_owned(), font, ink);
    let glyph = icon.map_or(0.0, |_| tokens::ICON_SMALL + tokens::SPACE_XS);
    let width = galley.size().x + glyph + 2.0 * tokens::SPACE_SM;
    let rect = align.anchor_size(anchor, Vec2::new(width, CHIP_HEIGHT));
    let id = egui::Id::unique(("linko-canvas-chip", text));
    let response = ui.interact(rect, id, egui::Sense::hover());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, text));
    painter.rect_filled(rect, CHIP_HEIGHT / 2.0, fill);
    let mut x = rect.left() + tokens::SPACE_SM;
    if let Some(icon) = icon {
        let at = egui::pos2(x, rect.center().y - tokens::ICON_SMALL / 2.0);
        let icon_rect = egui::Rect::from_min_size(at, Vec2::splat(tokens::ICON_SMALL));
        icon.paint(painter, icon_rect, ink);
        x += glyph;
    }
    let text_at = egui::pos2(x, rect.center().y - galley.size().y / 2.0);
    painter.galley(text_at, galley, ink);
    response.on_hover_text(hint);
}

/// Fixed id of a canvas control, so tests and assistive tools can find it and egui's
/// animation table keeps one entry per control.
fn canvas_control_id(key: &str) -> egui::Id {
    egui::Id::unique(("linko-canvas", key))
}

fn edge_code(edge: &str) -> u8 {
    match edge {
        "left" => 0,
        "right" => 1,
        "top" => 2,
        "bottom" => 3,
        _ => 4,
    }
}

/// Where a just-ended drag left the dragged screen: its center after this frame's moves.
fn released_center(
    draft: Option<(Vec2, egui::Pos2, egui::Rect)>,
    events: &[egui::Event],
) -> Option<egui::Pos2> {
    let (offset, mut center, _) = draft?;
    for event in events {
        let pos = match *event {
            egui::Event::PointerMoved(pos) => pos,
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                ..
            } => pos,
            _ => continue,
        };
        center = pos - offset;
    }
    Some(center)
}

/// Icon-only toolbar button with a fixed id. Returns true when clicked.
fn toolbar_button(
    ui: &mut egui::Ui,
    key: &'static str,
    icon: Icon,
    name: &str,
    enabled: bool,
) -> bool {
    let id = canvas_control_id(key);
    let shown = ui.add_enabled_ui(enabled, |ui| canvas_button(ui, id, icon, name, None, false));
    shown.inner.clicked()
}

/// Framed canvas button: an icon, optionally a short word, and `name` as its accessible
/// name and hover text. Hover and selection fade with M1 and M2; focus shows a 2px ring.
fn canvas_button(
    ui: &mut egui::Ui,
    id: egui::Id,
    icon: Icon,
    name: &str,
    text: Option<&str>,
    selected: bool,
) -> egui::Response {
    let enabled = ui.is_enabled();
    let ink = if !enabled {
        ui.visuals().weak_text_color()
    } else if selected {
        desktop_style::selection_colors(ui).1
    } else {
        ui.visuals().text_color()
    };
    let galley = text.map(|text| {
        let font = egui::FontId::proportional(13.0);
        ui.painter().layout_no_wrap(text.to_owned(), font, ink)
    });
    let width = match &galley {
        Some(galley) => galley.size().x + tokens::ICON_SMALL + 3.0 * tokens::SPACE_SM,
        None => CANVAS_BUTTON_SIDE,
    };
    let size = Vec2::new(width, CANVAS_BUTTON_SIDE);
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let response = ui.interact(rect, id, egui::Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, name));
    if ui.is_rect_visible(rect) {
        paint_canvas_button(ui, &response, selected);
        let icon_size = Vec2::splat(tokens::ICON_SMALL);
        let icon_rect = if galley.is_some() {
            let left = rect.left() + tokens::SPACE_SM;
            let top = rect.center().y - tokens::ICON_SMALL / 2.0;
            egui::Rect::from_min_size(egui::pos2(left, top), icon_size)
        } else {
            egui::Rect::from_center_size(rect.center(), icon_size)
        };
        icon.paint(ui.painter(), icon_rect, ink);
        if let Some(galley) = galley {
            let text_at = egui::pos2(
                icon_rect.right() + tokens::SPACE_SM,
                rect.center().y - galley.size().y / 2.0,
            );
            ui.painter().galley(text_at, galley, ink);
        }
    }
    response.on_hover_text(name)
}

fn paint_canvas_button(ui: &egui::Ui, response: &egui::Response, selected: bool) {
    let ctx = ui.ctx();
    let dark = ui.visuals().dark_mode;
    let hovered = response.hovered() && ui.is_enabled();
    let hover = motion::hover_mix(ctx, response.id.with("hover"), hovered);
    let select = motion::select_mix(ctx, response.id.with("select"), selected);
    let widgets = &ui.visuals().widgets;
    let radius = f32::from(tokens::RADIUS_CONTROL);
    let rect = response.rect;
    let painter = ui.painter();
    painter.rect_filled(rect, radius, widgets.inactive.weak_bg_fill);
    let hovered_fill = tokens::faded(widgets.hovered.weak_bg_fill, hover);
    painter.rect_filled(rect, radius, hovered_fill);
    let selected_fill = tokens::faded(desktop_style::selection_colors(ui).0, select);
    painter.rect_filled(rect, radius, selected_fill);
    let accent = tokens::accent_color(dark);
    let stroke = if response.has_focus() {
        egui::Stroke::new(2.0, accent)
    } else if selected {
        egui::Stroke::new(1.0, tokens::faded(accent, select))
    } else {
        egui::Stroke::new(1.0, tokens::line_color(dark))
    };
    painter.rect_stroke(rect.shrink(0.5), radius, stroke, egui::StrokeKind::Inside);
}

/// Paint one illustrated screen and its name. `ring` is the M2 selection amount and
/// `focused` adds the keyboard focus ring. Returns the frame the rings follow, where the
/// status badge sits on the top right corner.
fn draw_workspace_device(
    ui: &egui::Ui,
    painter: &egui::Painter,
    node: egui::Rect,
    name: &str,
    kind: visual_assets::DeviceKind,
    ring: f32,
    focused: bool,
) -> egui::Rect {
    let label_height = node.height() * 0.26;
    let image_area =
        egui::Rect::from_min_max(node.min, egui::pos2(node.max.x, node.max.y - label_height));
    let aspect = visual_assets::device_aspect(kind);
    let illustration_scale = match kind {
        visual_assets::DeviceKind::Laptop => 0.70,
        visual_assets::DeviceKind::Desktop => 1.0,
        visual_assets::DeviceKind::Unknown => 0.85,
    };
    let width = image_area.width().min(image_area.height() * aspect) * illustration_scale;
    let size = Vec2::new(width, width / aspect);
    let image_rect = egui::Rect::from_center_size(
        egui::pos2(image_area.center().x, image_area.max.y - size.y / 2.0),
        size,
    );
    let selected_frame = if kind == visual_assets::DeviceKind::Unknown {
        image_rect.shrink(5.0)
    } else if kind == visual_assets::DeviceKind::Desktop {
        egui::Rect::from_min_max(
            image_rect.min,
            egui::pos2(
                image_rect.max.x,
                image_rect.min.y + image_rect.height() * 0.79,
            ),
        )
    } else {
        egui::Rect::from_min_max(
            image_rect.min + egui::vec2(image_rect.width() * 0.07, 0.0),
            egui::pos2(
                image_rect.max.x - image_rect.width() * 0.07,
                image_rect.min.y + image_rect.height() * 0.83,
            ),
        )
    };
    let accent = ui.visuals().selection.bg_fill;
    if ring > 0.0 {
        let wash = egui::Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), 18);
        painter.rect_filled(selected_frame.expand(5.0), 8.0, tokens::faded(wash, ring));
    }
    if let Some(texture) = visual_assets::device_texture(ui.ctx(), kind) {
        painter.image(
            texture.id(),
            image_rect,
            visual_assets::device_uv(kind),
            egui::Color32::WHITE,
        );
    } else {
        // A generic pair of outlines is a symbol, not an identified chassis.
        generic_device_symbol(
            painter,
            image_rect.shrink(4.0),
            ui.visuals().weak_text_color(),
            ui.visuals().window_fill,
        );
    }
    if ring > 0.0 {
        let stroke = egui::Stroke::new(2.0, tokens::faded(accent, ring));
        let ring_rect = selected_frame.expand(3.0);
        painter.rect_stroke(ring_rect, 7.0, stroke, egui::StrokeKind::Inside);
    }
    if focused {
        let stroke = egui::Stroke::new(2.0, tokens::accent_color(ui.visuals().dark_mode));
        let focus_rect = selected_frame.expand(7.0);
        painter.rect_stroke(focus_rect, 9.0, stroke, egui::StrokeKind::Outside);
    }
    if label_height >= 22.0 {
        let labels = painter.with_clip_rect(node);
        let title = labels.layout_no_wrap(
            name.chars().take(24).collect(),
            egui::FontId::new(15.0, egui::FontFamily::Name("linko-semibold".into())),
            ui.visuals().text_color(),
        );
        let title_rect = egui::Align2::CENTER_TOP.anchor_size(
            egui::pos2(node.center().x, image_area.max.y + 5.0),
            title.size(),
        );
        if node.contains_rect(title_rect) {
            labels.galley(title_rect.min, title, ui.visuals().text_color());
        }
    }
    selected_frame
}

fn workspace_rects(
    canvas: egui::Rect,
    edge: &str,
    paired: bool,
    zoom: f32,
) -> (egui::Rect, egui::Rect) {
    let vertical = paired && matches!(edge, "top" | "bottom");
    let scale = (canvas.width() / if vertical { 270.0 } else { SIDE_BY_SIDE_W })
        .min(canvas.height() / if vertical { 500.0 } else { 240.0 })
        .clamp(0.0, 1.35);
    let zoom = if zoom.is_finite() {
        zoom.clamp(0.6, 1.0)
    } else {
        1.0
    };
    let scale = scale * zoom;
    let size = Vec2::new(220.0, 220.0) * scale;
    let delta = if vertical {
        Vec2::new(0.0, (size.y + 20.0 * scale) / 2.0)
    } else {
        Vec2::new((size.x + 34.0 * scale) / 2.0, 0.0)
    };
    let delta = if paired && matches!(edge, "left" | "top") {
        -delta
    } else {
        delta
    };
    (
        egui::Rect::from_center_size(canvas.center() - delta, size),
        egui::Rect::from_center_size(canvas.center() + delta, size),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn essential_context_and_connection_details_fit_the_supported_first_viewport() -> Result<()> {
        fn text_rect(shape: &egui::epaint::Shape, label: &str) -> Option<egui::Rect> {
            match shape {
                egui::epaint::Shape::Text(text) if text.galley.job.text == label => {
                    Some(text.visual_bounding_rect())
                }
                egui::epaint::Shape::Vec(shapes) => shapes.iter().find_map(|s| text_rect(s, label)),
                _ => None,
            }
        }
        let fixture = crate::tests::Fixture::new()?;
        let mut app = App::load(fixture.path.clone());
        let ctx = crate::tests::ui_context();
        app.theme(&ctx);
        for paired in [false, true] {
            app.config.fingerprint = paired.then(|| "view-only-not-authentication".into());
            let short = app.workspace_view().short;
            for size in [
                Vec2::new(920.0, 700.0),
                Vec2::new(1180.0, 820.0),
                Vec2::new(1280.0, 800.0),
            ] {
                let output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                        ..Default::default()
                    },
                    |root| app.desktop(root),
                );
                let controls: Vec<_> = [
                    "快捷操作",
                    "所选设备",
                    "文件 · 剪贴板 未接通",
                    "连接与身份详情",
                    "两台主屏的相对方向；另一端需设置相反方向。",
                ]
                .into_iter()
                .map(|label| {
                    let bounds = output
                        .shapes
                        .iter()
                        .find_map(|s| text_rect(&s.shape, label).map(|r| (s.clip_rect, r)));
                    (label, bounds)
                })
                .collect();
                // The topmost occurrence of the short status is the title bar capsule.
                let capsule = output
                    .shapes
                    .iter()
                    .filter_map(|s| text_rect(&s.shape, short))
                    .min_by(|a, b| a.min.y.total_cmp(&b.min.y));
                let stop = output
                    .shapes
                    .iter()
                    .find_map(|s| text_rect(&s.shape, "停止协同"));
                let toolbar = output
                    .shapes
                    .iter()
                    .find_map(|s| text_rect(&s.shape, "100%"));
                let mut card_alignment = Vec::new();
                for (name, metadata) in [
                    (desktop_ui::local_device_name(), "系统"),
                    (app.config.peer_name.as_str(), "登记"),
                ]
                .into_iter()
                .take(if paired { 2 } else { 1 })
                {
                    // The lowest name occurrence belongs to its formal card.
                    let name_rect = output
                        .shapes
                        .iter()
                        .filter_map(|s| text_rect(&s.shape, name))
                        .max_by(|a, b| a.min.y.total_cmp(&b.min.y));
                    let metadata_rect = output
                        .shapes
                        .iter()
                        .find_map(|s| text_rect(&s.shape, metadata));
                    card_alignment.push((name.to_owned(), name_rect, metadata_rect));
                }
                output.drop_without_applying_deltas();
                let capsule = capsule.context("title bar status capsule absent")?;
                let stop = stop.context("stop control absent")?;
                let toolbar = toolbar.context("canvas toolbar absent")?;
                assert!(
                    capsule.min.x >= 0.0 && capsule.max.x < stop.min.x,
                    "status capsule collides with the safety controls at {size:?}"
                );
                for (name, name_rect, metadata_rect) in card_alignment {
                    let name_rect = name_rect.with_context(|| format!("{name} card absent"))?;
                    let metadata_rect = metadata_rect.context("card metadata absent")?;
                    // A 22px glyph plus 8px gap precedes the ordinary name label.
                    assert!(
                        (name_rect.min.x - metadata_rect.min.x - 30.0).abs() <= 3.0,
                        "{name} card name is not left-aligned after its icon at {size:?}"
                    );
                }
                assert!(
                    capsule.max.y <= 52.0,
                    "status capsule left the bounded title bar row at {size:?}"
                );
                assert!(
                    (52.0..=120.0).contains(&toolbar.min.y),
                    "workspace canvas was pushed below the shared title bar"
                );
                for (label, bounds) in controls {
                    let (clip, rect) = bounds
                        .with_context(|| format!("{label} absent at {size:?}, paired={paired}"))?;
                    assert!(clip.contains_rect(rect), "{label} clipped at {size:?}");
                    assert!(
                        egui::Rect::from_min_size(egui::Pos2::ZERO, size).contains_rect(rect),
                        "{label} left the first viewport at {size:?}"
                    );
                }
                assert!(app.child.is_none() && !app.config.native && !app.config.allow);
            }
        }
        drop(app);
        fixture.close()?;
        Ok(())
    }
    fn text_center(shape: &egui::epaint::Shape, label: &str) -> Option<egui::Pos2> {
        match shape {
            egui::epaint::Shape::Text(text) if text.galley.job.text == label => {
                Some(text.visual_bounding_rect().center())
            }
            egui::epaint::Shape::Vec(shapes) => shapes.iter().find_map(|s| text_center(s, label)),
            _ => None,
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
    fn click(app: &mut App, ctx: &egui::Context, at: egui::Pos2) {
        for pressed in [true, false] {
            frame(
                app,
                ctx,
                vec![
                    egui::Event::PointerMoved(at),
                    egui::Event::PointerButton {
                        pos: at,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            )
            .drop_without_applying_deltas();
        }
    }
    /// One frame whose canvas control `key` is read inside the pass. egui swaps its widget
    /// tables when a pass ends, so `read_response` after a frame returns the frame before it.
    fn frame_with_control(
        app: &mut App,
        ctx: &egui::Context,
        input: egui::RawInput,
        key: &str,
    ) -> (egui::FullOutput, Result<egui::Response>) {
        let id = canvas_control_id(key);
        let mut response = None;
        let output = ctx.run_ui(input, |root| {
            app.desktop(root);
            response = root.ctx().read_response(id);
        });
        (output, response.context("canvas control absent"))
    }
    /// The 1180x820 window of `frame`.
    fn wide_input() -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                Vec2::new(1180.0, 820.0),
            )),
            ..Default::default()
        }
    }
    fn pointer(pos: egui::Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        }
    }
    fn key_press(key: egui::Key) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }
    }
    fn painted(output: &egui::FullOutput, label: &str) -> bool {
        output
            .shapes
            .iter()
            .any(|s| text_center(&s.shape, label).is_some())
    }
    /// A focused 920x700 window at an explicit time, because the motion budget is in time.
    fn timed_input(time: f64, events: Vec<egui::Event>) -> egui::RawInput {
        egui::RawInput {
            time: Some(time),
            events,
            focused: true,
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                Vec2::new(920.0, 700.0),
            )),
            ..Default::default()
        }
    }
    fn timed_frame(
        app: &mut App,
        ctx: &egui::Context,
        time: f64,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        ctx.run_ui(timed_input(time, events), |root| app.desktop(root))
    }
    fn step(app: &mut App, ctx: &egui::Context, time: f64, events: Vec<egui::Event>) {
        let output = timed_frame(app, ctx, time, events);
        output.drop_without_applying_deltas();
    }
    /// Center of the first painted `label`; at 920 points the sidebar hides names, so the
    /// first occurrence is the canvas screen.
    fn label_at(app: &mut App, ctx: &egui::Context, time: f64, label: &str) -> Result<egui::Pos2> {
        let output = timed_frame(app, ctx, time, vec![]);
        let at = output
            .shapes
            .iter()
            .find_map(|paint| text_center(&paint.shape, label));
        output.drop_without_applying_deltas();
        at.context("canvas screen label absent")
    }
    #[test]
    fn arrow_buttons_set_direction_by_pointer_and_keyboard() -> Result<()> {
        let fixture = crate::tests::Fixture::new()?;
        let mut app = App::load(fixture.path.clone());
        app.config.fingerprint = Some("layout-presentation-only".into());
        app.save_config()?;
        let ctx = crate::tests::ui_context();
        app.theme(&ctx);
        let input = timed_input(0.0, vec![]);
        let (output, left) = frame_with_control(&mut app, &ctx, input, "left");
        output.drop_without_applying_deltas();
        let left = left?;
        assert!(left.enabled());
        let at = left.rect.center();
        step(
            &mut app,
            &ctx,
            0.1,
            vec![egui::Event::PointerMoved(at), pointer(at, true)],
        );
        step(&mut app, &ctx, 0.2, vec![pointer(at, false)]);
        assert_eq!(app.config.edge, "left");
        let stored: serde_json::Value =
            serde_json::from_slice(&std::fs::read(app.dir.join("settings.json"))?)?;
        assert_eq!(stored["data"]["edge"], "left");
        // Keyboard: the arrow takes focus by its fixed id and Enter activates it.
        ctx.memory_mut(|memory| memory.request_focus(canvas_control_id("right")));
        let focused = timed_frame(&mut app, &ctx, 0.3, vec![]);
        let mut events = focused.platform_output.events.clone();
        focused.drop_without_applying_deltas();
        let pressed = timed_frame(&mut app, &ctx, 0.4, vec![key_press(egui::Key::Enter)]);
        events.extend(pressed.platform_output.events.clone());
        pressed.drop_without_applying_deltas();
        assert_eq!(app.config.edge, "right");
        let name = Some("另一台在本机右侧");
        let named = events
            .iter()
            .any(|event| event.widget_info().label.as_deref() == name);
        assert!(named, "the right arrow lacks its accessible name");
        assert!(app.child.is_none() && !app.config.native && !app.config.allow);
        drop(app);
        fixture.close()?;
        Ok(())
    }
    #[test]
    fn locked_layout_dims_screens_shows_lock_and_disables_arrows() -> Result<()> {
        let fixture = crate::tests::Fixture::new()?;
        let mut app = App::load(fixture.path.clone());
        app.config.fingerprint = Some("layout-presentation-only".into());
        app.save_config()?;
        let path = app.dir.join("settings.json");
        let original = std::fs::read(&path)?;
        let ctx = crate::tests::ui_context();
        app.theme(&ctx);
        let mut at = egui::Pos2::ZERO;
        for (time, locked) in [(0.0, false), (1.0, true)] {
            app.recovery_blocked = locked;
            let input = timed_input(time, vec![]);
            let (output, left) = frame_with_control(&mut app, &ctx, input, "left");
            let lock = painted(&output, "已锁定");
            let note = painted(&output, LOCKED_NOTE);
            output.drop_without_applying_deltas();
            assert_eq!(lock, locked, "lock chip");
            assert_eq!(note, locked, "locked note");
            let left = left?;
            assert_eq!(left.enabled(), !locked, "arrow enabled state");
            at = left.rect.center();
        }
        // A locked arrow ignores the press; memory and disk keep the old direction.
        step(
            &mut app,
            &ctx,
            1.1,
            vec![egui::Event::PointerMoved(at), pointer(at, true)],
        );
        step(&mut app, &ctx, 1.2, vec![pointer(at, false)]);
        assert_eq!(app.config.edge, "right");
        assert_eq!(std::fs::read(&path)?, original);
        assert!(app.child.is_none() && !app.config.native && !app.config.allow);
        drop(app);
        fixture.close()?;
        Ok(())
    }
    #[test]
    fn released_drag_rebounds_once_then_requests_no_repaint() -> Result<()> {
        use std::sync::atomic::{AtomicUsize, Ordering};
        const FRAME_SECS: f64 = 1.0 / 60.0;
        let fixture = crate::tests::Fixture::new()?;
        let mut app = App::load(fixture.path.clone());
        app.config.fingerprint = Some("layout-presentation-only".into());
        app.config.peer_name = "B".into();
        let ctx = crate::tests::ui_context();
        app.theme(&ctx);
        let mut time = 0.0;
        for _ in 0..20 {
            step(&mut app, &ctx, time, vec![]);
            time += FRAME_SECS;
        }
        let start = label_at(&mut app, &ctx, time, "B")?;
        let end = start - Vec2::new(260.0, 0.0);
        time += FRAME_SECS;
        let press = vec![egui::Event::PointerMoved(start), pointer(start, true)];
        step(&mut app, &ctx, time, press);
        time += FRAME_SECS;
        step(&mut app, &ctx, time, vec![egui::Event::PointerMoved(end)]);
        assert!(
            app.screen_drag.is_some(),
            "drag did not acquire the peer screen"
        );
        let requests = std::sync::Arc::new(AtomicUsize::new(0));
        let observed = std::sync::Arc::clone(&requests);
        ctx.set_request_repaint_callback(move |request| {
            if request.delay < std::time::Duration::from_secs(1) {
                observed.fetch_add(1, Ordering::SeqCst);
            }
        });
        time += FRAME_SECS;
        step(&mut app, &ctx, time, vec![pointer(end, false)]);
        assert!(app.screen_drag.is_none() && app.config.edge == "left");
        // The pointer leaves, so no hover or tooltip asks for frames of its own.
        time += FRAME_SECS;
        step(&mut app, &ctx, time, vec![egui::Event::PointerGone]);
        time += FRAME_SECS;
        let early = label_at(&mut app, &ctx, time, "B")?;
        assert!(
            requests.load(Ordering::SeqCst) > 0,
            "rebound never asked for a frame"
        );
        for _ in 0..30 {
            time += FRAME_SECS;
            step(&mut app, &ctx, time, vec![]);
        }
        let settled = requests.load(Ordering::SeqCst);
        time += FRAME_SECS;
        let rested = label_at(&mut app, &ctx, time, "B")?;
        for _ in 0..120 {
            time += FRAME_SECS;
            step(&mut app, &ctx, time, vec![]);
        }
        assert_eq!(
            requests.load(Ordering::SeqCst),
            settled,
            "motion kept repainting"
        );
        // Released left of its new place, the screen glides right instead of jumping.
        assert!(rested.x - early.x > 10.0, "{early:?} -> {rested:?}");
        let stored: serde_json::Value =
            serde_json::from_slice(&std::fs::read(app.dir.join("settings.json"))?)?;
        assert_eq!(stored["data"]["edge"], "left");
        assert!(app.child.is_none() && !app.config.native && !app.config.allow);
        drop(app);
        fixture.close()?;
        Ok(())
    }
    #[test]
    fn topology_device_cards_change_selection_without_writing_trust_layout_or_grants() -> Result<()>
    {
        let fixture = crate::tests::Fixture::new()?;
        let mut app = App::load(fixture.path.clone());
        app.config.fingerprint = Some("device-card-presentation-only".into());
        app.config.peer_name = "已登记电脑".into();
        let original = app.config.clone();
        let ctx = crate::tests::ui_context();
        app.theme(&ctx);
        for (label, selected) in [
            ("已登记电脑".to_owned(), true),
            (desktop_ui::local_device_name().to_owned(), false),
        ] {
            let output = frame(&mut app, &ctx, vec![]);
            // The same real name appears below its illustration; the lower
            // occurrence is the formal card. All side-effect assertions remain.
            let at = output
                .shapes
                .iter()
                .filter_map(|s| text_center(&s.shape, &label))
                .max_by(|a, b| a.y.total_cmp(&b.y));
            output.drop_without_applying_deltas();
            let at = at.context("actual topology device card absent")?;
            click(&mut app, &ctx, at);
            assert_eq!(app.workspace_peer_selected, selected);
            assert!(app.config == original && app.child.is_none());
            assert!(!app.worker_permissions.needs_poll() && !app.native_confirmation);
        }
        assert!(
            !fixture.path.join("identity").exists() && !fixture.path.join("settings.json").exists()
        );
        drop(app);
        fixture.close()?;
        Ok(())
    }
    #[test]
    fn readonly_status_and_detail_remain_visible_without_a_primary_action() -> Result<()> {
        let fixture = crate::tests::Fixture::new()?;
        let mut app = App::load(fixture.path.clone());
        drop(app.config_store.take());
        let view = app.workspace_view();
        assert_eq!(view.action, desktop_state::Action::None);
        let ctx = crate::tests::ui_context();
        app.theme(&ctx);
        let output = frame(&mut app, &ctx, vec![]);
        let title_visible = output
            .shapes
            .iter()
            .any(|s| text_center(&s.shape, view.title).is_some());
        let detail_visible = output
            .shapes
            .iter()
            .any(|s| text_center(&s.shape, view.detail).is_some());
        output.drop_without_applying_deltas();
        assert!(title_visible && detail_visible);
        assert!(app.child.is_none() && !app.config.native && !app.config.allow);
        drop(app);
        fixture.close()?;
        Ok(())
    }
    #[test]
    fn recovery_and_pending_receipts_lock_layout_after_the_worker_exits() -> Result<()> {
        let fixture = crate::tests::Fixture::new()?;
        let mut app = App::load(fixture.path.clone());
        app.config.fingerprint = Some("layout-presentation-only".into());
        app.save_config()?;
        let original = std::fs::read(app.dir.join("settings.json"))?;
        assert!(app.child.is_none() && app.layout_editable());
        app.recovery_blocked = true;
        assert!(!app.layout_editable() && !app.try_set_edge("left"));
        assert_eq!(app.config.edge, "right");
        app.recovery_blocked = false;
        // Receipt bookkeeping only; these test values never authenticate a peer.
        app.config.pairing_receipt = Some(pairing_receipt::Record::pending(
            [7; 32],
            &"a".repeat(64),
            &"b".repeat(64),
        )?);
        assert!(app.pairing_pending());
        assert!(!app.layout_editable() && !app.try_set_edge("left"));
        assert_eq!(app.config.edge, "right");
        assert_eq!(std::fs::read(app.dir.join("settings.json"))?, original);
        assert!(app.child.is_none());
        drop(app);
        fixture.close()?;
        Ok(())
    }
    #[test]
    fn direction_click_rolls_back_memory_when_the_real_store_digest_conflicts() -> Result<()> {
        let fixture = crate::tests::Fixture::new()?;
        let mut app = App::load(fixture.path.clone());
        app.config.fingerprint = Some("layout-presentation-only".into());
        app.save_config()?;
        let path = app.dir.join("settings.json");
        let mut external = std::fs::read(&path)?;
        external.push(b'\n');
        std::fs::write(&path, &external)?;
        let ctx = crate::tests::ui_context();
        app.theme(&ctx);
        // The former layout tab, still chosen after pairing, opens the same canvas.
        for legacy in [false, true] {
            app.device_tab = if legacy {
                DeviceTab::Layout
            } else {
                DeviceTab::Device
            };
            let output = frame(&mut app, &ctx, vec![]);
            let at = output
                .shapes
                .iter()
                .find_map(|s| text_center(&s.shape, "左侧"))
                .context("direction control absent")?;
            output.drop_without_applying_deltas();
            click(&mut app, &ctx, at);
            assert_eq!(app.config.edge, "right");
            assert!(app.notice.contains("其他进程已修改配置"));
            assert_eq!(std::fs::read(&path)?, external);
        }
        assert!(app.child.is_none() && !app.config.native && !app.config.allow);
        drop(app);
        fixture.close()?;
        Ok(())
    }
    #[test]
    fn ordinary_geometry_report_is_opt_in_bounded_and_never_overwrites() -> Result<()> {
        let fixture = crate::tests::Fixture::new()?;
        let path = fixture.path.join("widget-geometry.json");
        let mut app = App::load(fixture.path.clone());
        let ctx = crate::tests::ui_context();
        app.theme(&ctx);
        frame(&mut app, &ctx, vec![]).drop_without_applying_deltas();
        assert!(!path.exists() && app.layout_geometry.is_empty());
        app.layout_report = Some(path.clone());
        frame(&mut app, &ctx, vec![]).drop_without_applying_deltas();
        let bytes = std::fs::read(&path)?;
        let report: serde_json::Value = serde_json::from_slice(&bytes)?;
        assert_eq!(report["version"], 2);
        assert_eq!(report["scope"], "isolated_gui_widget_geometry");
        for key in [
            "nav_sidebar",
            "nav_layout",
            "nav_shortcuts",
            "nav_settings",
            "add_device",
            "primary_action",
        ] {
            let rect = report["controls"][key]
                .as_array()
                .context("missing rendered control")?;
            assert_eq!(rect.len(), 4);
            let values: Vec<f64> = rect.iter().map(|v| v.as_f64().unwrap()).collect();
            assert!(values.iter().all(|v| v.is_finite() && *v >= 0.0));
            assert!(values[2] > 0.0 && values[3] > 0.0);
            assert!(values[0] + values[2] <= 1180.0 && values[1] + values[3] <= 820.0);
        }
        app.layout_report = Some(path.clone());
        frame(&mut app, &ctx, vec![]).drop_without_applying_deltas();
        assert_eq!(std::fs::read(&path)?, bytes);
        assert!(app.notice.contains("UI几何报告未写入"));
        assert!(app.child.is_none() && !app.config.native && !app.config.allow);
        assert!(
            !fixture.path.join("identity").exists() && !fixture.path.join("settings.json").exists()
        );
        drop(app);
        fixture.close()?;
        Ok(())
    }
    #[test]
    fn navigation_reaches_every_entry_and_keeps_real_safety_controls_without_side_effects(
    ) -> Result<()> {
        let fixture = crate::tests::Fixture::new()?;
        let mut app = App::load(fixture.path.clone());
        let ctx = crate::tests::ui_context();
        app.theme(&ctx);
        // Sidebar entries, subpage back arrows and in-page links, in the order a person
        // could click them; together they reach every page of the shell.
        for (page, label) in [
            (Page::Settings, "设置"),
            (Page::Displays, "返回"),
            (Page::Shortcuts, "快捷键"),
            (Page::Home, "查看协作总览"),
            (Page::Shortcuts, "返回"),
            (Page::GestureLab, "手势实验室"),
            (Page::Shortcuts, "返回"),
            (Page::GestureRules, "规则与应用"),
            (Page::Shortcuts, "返回"),
            (Page::Displays, "返回"),
            (Page::Transfers, "说明"),
            (Page::Clipboard, "剪贴板"),
            (Page::Transfers, "文件传输"),
            (Page::Displays, "返回"),
            (Page::Settings, "设置"),
            (Page::Displays, desktop_ui::local_device_name()),
        ] {
            let output = frame(&mut app, &ctx, vec![]);
            let at = output
                .shapes
                .iter()
                .find_map(|s| text_center(&s.shape, label));
            let safety_controls: Vec<_> = ["停止协同", "紧急恢复本机"]
                .into_iter()
                .map(|safety_label| {
                    output
                        .shapes
                        .iter()
                        .find_map(|s| text_center(&s.shape, safety_label))
                })
                .collect();
            output.drop_without_applying_deltas();
            let at = at.context("navigation entry absent")?;
            for control in safety_controls {
                let center = control.context("fixed safety control absent")?;
                assert!(
                    center.y <= 52.0,
                    "safety toolbar grew beyond its bounded row"
                );
            }
            app.native_confirmation = true;
            click(&mut app, &ctx, at);
            assert!(app.page == page && !app.native_confirmation && app.screen_drag.is_none());
            assert!(app.child.is_none() && !app.worker_permissions.needs_poll());
            assert!(!app.config.native && !app.config.allow && !app.pairing_open);
        }
        app.native_confirmation = true;
        app.dialog = Some(Dialog::NativeStart);
        app.screen_drag = Some((Vec2::ZERO, egui::Pos2::ZERO, egui::Rect::EVERYTHING));
        app.navigate_to(Page::Displays);
        assert!(!app.native_confirmation && app.dialog.is_none() && app.screen_drag.is_none());
        assert!(
            !fixture.path.join("identity").exists() && !fixture.path.join("settings.json").exists()
        );
        drop(app);
        fixture.close()?;
        Ok(())
    }
    #[test]
    fn view_zoom_changes_geometry_without_touching_direction_or_grants() -> Result<()> {
        let fixture = crate::tests::Fixture::new()?;
        let mut app = App::load(fixture.path.clone());
        let original = app.config.clone();
        let ctx = crate::tests::ui_context();
        app.theme(&ctx);
        // The zoom buttons are icons now; their fixed ids carry the former text's role.
        let (output, zoom_out) = frame_with_control(&mut app, &ctx, wide_input(), "zoom-out");
        output.drop_without_applying_deltas();
        click(&mut app, &ctx, zoom_out?.rect.center());
        assert!(app.workspace_zoom < 1.0 && app.workspace_zoom >= 0.6);
        assert!(app.config == original && app.child.is_none() && app.screen_drag.is_none());
        let (output, fit) = frame_with_control(&mut app, &ctx, wide_input(), "zoom-fit");
        output.drop_without_applying_deltas();
        click(&mut app, &ctx, fit?.rect.center());
        assert_eq!(app.workspace_zoom, 1.0);
        assert!(
            !fixture.path.join("identity").exists() && !fixture.path.join("settings.json").exists()
        );
        drop(app);
        fixture.close()?;
        Ok(())
    }
    #[test]
    fn empty_canvas_add_opens_real_pairing_without_clickthrough_or_identity_creation() -> Result<()>
    {
        let fixture = crate::tests::Fixture::new()?;
        let mut app = App::load(fixture.path.clone());
        let ctx = crate::tests::ui_context();
        let output = frame(&mut app, &ctx, vec![]);
        let at = output
            .shapes
            .iter()
            .find_map(|s| text_center(&s.shape, "＋\n添加另一台电脑"))
            .context("canvas add button absent")?;
        output.drop_without_applying_deltas();
        click(&mut app, &ctx, at);
        assert!(app.pairing_open && app.child.is_none() && !app.native_confirmation);
        assert!(!app.config.native && !app.config.allow);
        assert!(
            !fixture.path.join("identity").exists() && !fixture.path.join("settings.json").exists()
        );
        drop(app);
        fixture.close()?;
        Ok(())
    }
    #[test]
    fn selecting_roster_peer_changes_only_context_not_layout_trust_or_permissions() -> Result<()> {
        let fixture = crate::tests::Fixture::new()?;
        let mut app = App::load(fixture.path.clone());
        app.config.fingerprint = Some("presentation-only-not-authentication".into());
        let original = app.config.clone();
        app.page = Page::Devices;
        let ctx = crate::tests::ui_context();
        let output = frame(&mut app, &ctx, vec![]);
        // The roster now lives in the sidebar: the peer name painted inside its column.
        let label = app.config.peer_name.clone();
        let at = output
            .shapes
            .iter()
            .filter_map(|s| text_center(&s.shape, &label))
            .find(|at| at.x < desktop_style::tokens::SIDEBAR_EXPANDED_W)
            .context("roster peer absent")?;
        output.drop_without_applying_deltas();
        click(&mut app, &ctx, at);
        assert!(app.workspace_peer_selected && app.child.is_none());
        assert!(app.config == original && !fixture.path.join("settings.json").exists());
        drop(app);
        fixture.close()?;
        Ok(())
    }
    #[test]
    fn primary_screen_and_peer_stay_inside_compact_canvas_in_all_directions() {
        for size in [
            Vec2::new(270.0, 180.0),
            Vec2::new(270.0, 220.0),
            Vec2::new(620.0, 285.0),
            Vec2::new(1000.0, 400.0),
        ] {
            let canvas = egui::Rect::from_min_size(egui::pos2(30.0, 50.0), size);
            for edge in ["left", "right", "top", "bottom"] {
                for zoom in [0.0, 0.6, 0.8, 1.0, 2.0, f32::NAN] {
                    let (local, peer) = workspace_rects(canvas, edge, true, zoom);
                    assert!(canvas.contains_rect(local) && canvas.contains_rect(peer));
                    assert!(!local.intersects(peer));
                    let mut draft = None;
                    let start = peer.center();
                    let end = canvas.center() - (peer.center() - canvas.center());
                    let events = [
                        egui::Event::PointerButton {
                            pos: start,
                            button: egui::PointerButton::Primary,
                            pressed: true,
                            modifiers: egui::Modifiers::NONE,
                        },
                        egui::Event::PointerMoved(end),
                        egui::Event::PointerButton {
                            pos: end,
                            button: egui::PointerButton::Primary,
                            pressed: false,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ];
                    // An acquired drag that becomes locked can never save a direction.
                    desktop_ui::update_layout_drag(
                        &mut draft,
                        &events[..1],
                        peer,
                        local,
                        canvas,
                        true,
                        true,
                    );
                    assert!(draft.is_some());
                    assert_eq!(
                        desktop_ui::update_layout_drag(
                            &mut draft,
                            &events[1..],
                            peer,
                            local,
                            canvas,
                            false,
                            false,
                        ),
                        None
                    );
                    assert!(draft.is_none());
                }
            }
        }
    }
    #[test]
    fn default_workspace_and_selection_render_without_identity_network_or_grants() -> Result<()> {
        let fixture = crate::tests::Fixture::new()?;
        let mut app = App::load(fixture.path.clone());
        assert!(app.page == Page::Displays && !app.workspace_peer_selected);
        let ctx = crate::tests::ui_context();
        for paired in [false, true] {
            app.config.fingerprint = paired.then(|| "presentation-only-not-authentication".into());
            for selected in [false, true] {
                app.workspace_peer_selected = selected;
                let output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            Vec2::new(920.0, 700.0),
                        )),
                        ..Default::default()
                    },
                    |root| app.desktop(root),
                );
                assert!(!output.shapes.is_empty());
                output.drop_without_applying_deltas();
                assert!(app.child.is_none() && !app.config.native && !app.config.allow);
                assert!(
                    !fixture.path.join("settings.json").exists()
                        && !fixture.path.join("identity").exists()
                );
            }
        }
        drop(app);
        fixture.close()?;
        Ok(())
    }
}
