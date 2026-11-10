//! Device-first desktop: actual roster, spatial canvas and contextual controls.
//! Presentation only; existing permission, enrollment and worker gates own actions.
use super::*;
use desktop_style::{card, Tone};

impl App {
    pub(super) fn device_roster(&mut self, ui: &mut egui::Ui) {
        ui.add_space(18.0);
        ui.small("我的电脑");
        ui.add_space(8.0);
        let local = desktop_ui::local_device_name();
        if roster_item(ui, local, "这台电脑", !self.workspace_peer_selected, true).clicked() {
            self.workspace_peer_selected = false;
            self.device_tab = DeviceTab::Device;
            self.navigate_to(Page::Devices);
        }
        if self.config.fingerprint.is_some() {
            let status = if self.pairing_pending() {
                "核验尚未完成"
            } else if self.connected && self.child.is_some() {
                "已连接"
            } else {
                "未连接"
            };
            if roster_item(
                ui,
                &self.config.peer_name,
                status,
                self.workspace_peer_selected,
                false,
            )
            .clicked()
            {
                self.workspace_peer_selected = true;
                self.device_tab = DeviceTab::Device;
                self.navigate_to(Page::Devices);
            }
        }
        ui.add_space(8.0);
        if ui
            .add_enabled(
                self.idle_writable(),
                egui::Button::new(if self.config.fingerprint.is_some() {
                    "＋ 重新核验电脑"
                } else {
                    "＋ 添加另一台电脑"
                }),
            )
            .clicked()
        {
            self.open_setup();
        }
        ui.add_space(14.0);
    }

    pub(super) fn device_workspace(&mut self, ui: &mut egui::Ui) {
        if self.device_tab == DeviceTab::Permissions {
            if ui.button("‹ 返回屏幕布局").clicked() {
                self.device_tab = DeviceTab::Device;
            }
            ui.add_space(12.0);
            card(ui).show(ui, |ui| self.device_permissions(ui));
            return;
        }
        if self.device_tab == DeviceTab::Layout {
            if ui.button("‹ 返回设备工作区").clicked() {
                self.device_tab = DeviceTab::Device;
            }
            card(ui).show(ui, |ui| self.displays(ui));
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
                    if self.page == Page::Devices {
                        card(ui).show(ui, |ui| self.device_roster(ui));
                        ui.add_space(12.0);
                        if ui.button("调整屏幕位置 →").clicked() {
                            self.navigate_to(Page::Displays);
                        }
                    } else {
                        self.workspace_screen_card(ui);
                    }
                });
                ui.vertical(|ui| {
                    ui.set_width(details);
                    self.workspace_device_detail(ui);
                });
            });
        } else {
            if self.page == Page::Devices {
                card(ui).show(ui, |ui| self.device_roster(ui));
            } else {
                self.workspace_screen_card(ui);
            }
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
                let dragging = self.screen_drag.is_some();
                if ui
                    .add_enabled(
                        !dragging && self.workspace_zoom > 0.6,
                        egui::Button::new("−").small(),
                    )
                    .clicked()
                {
                    self.workspace_zoom = (self.workspace_zoom - 0.2).max(0.6);
                }
                ui.label(format!("{:.0}%", self.workspace_zoom * 100.0));
                if ui
                    .add_enabled(
                        !dragging && self.workspace_zoom < 1.0,
                        egui::Button::new("＋").small(),
                    )
                    .clicked()
                {
                    self.workspace_zoom = (self.workspace_zoom + 0.2).min(1.0);
                }
                if ui
                    .add_enabled(!dragging, egui::Button::new("适配视图").small())
                    .clicked()
                {
                    self.workspace_zoom = 1.0;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.small("主屏示意");
                });
            });
            self.workspace_canvas(ui);
            ui.add_enabled_ui(self.layout_editable() && self.screen_drag.is_none(), |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.small("对端方向");
                    for side in ["left", "right", "top", "bottom"] {
                        if ui
                            .selectable_label(self.displayed_edge() == side, edge_label(side))
                            .clicked()
                        {
                            self.try_set_edge(side);
                        }
                    }
                });
            });
            self.layout_hint_row(ui);
        });
        ui.add_space(8.0);
        ui.label(desktop_style::semibold("设备详情", 17.0));
        self.workspace_device_cards(ui);
        ui.add_space(4.0);
        ui.small(if self.child.is_some() {
            "连接中可在本机控制时调整方向；新布局等待工作进程确认。"
        } else {
            "两台主屏的相对方向；另一端需设置相反方向。"
        });
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
        let (local, peer) = workspace_rects(
            canvas.shrink(20.0),
            self.displayed_edge(),
            registered,
            self.workspace_zoom,
        );
        let painter = ui.painter_at(canvas);
        desktop_style::canvas_wash(&painter, canvas, self.config.dark);
        let dot = ui.visuals().widgets.noninteractive.bg_stroke.color;
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
        let shown_peer = self.screen_drag.map_or(peer, |(_, center, _)| {
            egui::Rect::from_center_size(center, peer.size())
        });
        if registered {
            painter.line_segment(
                [local.center(), shown_peer.center()],
                egui::Stroke::new(1.0, dot),
            );
        }
        draw_workspace_device(
            ui,
            &painter,
            local,
            (desktop_ui::local_device_name(), "本机"),
            visual_assets::local_device_kind(),
            !self.workspace_peer_selected || !registered,
        );
        if ui
            .interact(
                local,
                ui.scope_id().with("local-screen"),
                egui::Sense::click(),
            )
            .clicked()
        {
            self.workspace_peer_selected = false;
        }
        if registered {
            draw_workspace_device(
                ui,
                &painter,
                shown_peer,
                (&self.config.peer_name, self.peer_connection_label()),
                visual_assets::DeviceKind::Unknown,
                self.workspace_peer_selected,
            );
            if ui
                .interact(
                    shown_peer,
                    ui.scope_id().with("peer-screen"),
                    egui::Sense::click(),
                )
                .clicked()
            {
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
        card(ui).inner_margin(margin).show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.spacing_mut().item_spacing.x = 8.0;
            ui.spacing_mut().item_spacing.y = 4.0;
            ui.horizontal_wrapped(|ui| {
                ui.label(desktop_style::semibold("协作能力", 16.0));
                ui.small("内容共享");
                if ui.small_button("说明").clicked() {
                    self.navigate_to(Page::Transfers);
                }
            });
            ui.small("文件、系统剪贴板 · 尚未接通");
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

fn draw_workspace_device(
    ui: &egui::Ui,
    painter: &egui::Painter,
    node: egui::Rect,
    labels: (&str, &str),
    kind: visual_assets::DeviceKind,
    selected: bool,
) {
    let (name, status) = labels;
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
    if selected {
        let accent = ui.visuals().selection.bg_fill;
        painter.rect_filled(
            selected_frame.expand(5.0),
            8.0,
            egui::Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), 18),
        );
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
    if selected {
        painter.rect_stroke(
            selected_frame.expand(3.0),
            7.0,
            egui::Stroke::new(1.2, ui.visuals().selection.bg_fill),
            egui::StrokeKind::Inside,
        );
    }
    if label_height < 22.0 {
        return;
    }
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
    if label_height >= 46.0 {
        let tone = if status == "已连接" {
            Tone::Good
        } else if status == "本机" {
            Tone::Accent
        } else {
            Tone::Neutral
        };
        let (fill, text) = desktop_style::status_colors(ui.visuals().dark_mode, tone);
        let status = labels.layout_no_wrap(status.into(), egui::FontId::proportional(11.0), text);
        let status_rect = egui::Align2::CENTER_TOP.anchor_size(
            egui::pos2(node.center().x, image_area.max.y + 33.0),
            status.size(),
        );
        let chip = status_rect.expand2(egui::vec2(7.0, 2.0));
        if node.contains_rect(chip) {
            labels.rect_filled(chip, 8.0, fill);
            labels.galley(status_rect.min, status, text);
        }
    }
}

fn roster_item(
    ui: &mut egui::Ui,
    name: &str,
    status: &str,
    selected: bool,
    local: bool,
) -> egui::Response {
    let title: String = name.chars().take(12).collect();
    let (fill, text) = desktop_style::selection_colors(ui);
    let color = if selected {
        text
    } else {
        ui.visuals().text_color()
    };
    let background = if selected {
        fill
    } else {
        ui.visuals().window_fill
    };
    let icon = if local { "●" } else { "○" };
    let label = desktop_style::semibold(format!("{icon}  {title}\n{status}"), 14.0).color(color);
    let response = ui.add_sized(
        [ui.available_width(), 62.0],
        egui::Button::new(label)
            .selected(selected)
            .fill(background)
            .stroke(egui::Stroke::new(
                1.0,
                if selected {
                    ui.visuals().selection.bg_fill
                } else {
                    ui.visuals().widgets.noninteractive.bg_stroke.color
                },
            )),
    );
    response.on_hover_text(name)
}

fn workspace_rects(
    canvas: egui::Rect,
    edge: &str,
    paired: bool,
    zoom: f32,
) -> (egui::Rect, egui::Rect) {
    let vertical = paired && matches!(edge, "top" | "bottom");
    let scale = (canvas.width() / if vertical { 270.0 } else { 530.0 })
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
                    "内容共享",
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
                let heading = output
                    .shapes
                    .iter()
                    .filter_map(|s| {
                        text_rect(&s.shape, "屏幕布局").filter(|rect| rect.min.x >= 196.0)
                    })
                    .min_by(|a, b| a.min.y.total_cmp(&b.min.y));
                let subtitle = output
                    .shapes
                    .iter()
                    .find_map(|s| text_rect(&s.shape, "按桌面位置摆放主屏，设置键鼠的跨屏方向。"));
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
                let heading = heading.context("workspace heading absent")?;
                let subtitle = subtitle.context("workspace subtitle absent")?;
                assert!(
                    (subtitle.min.x - heading.min.x).abs() <= 3.0,
                    "workspace subtitle is not left-aligned with title at {size:?}"
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
                    heading.min.y <= 32.0,
                    "workspace title was pushed below the shared header"
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
            "nav_devices",
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
    fn navigation_reaches_six_pages_and_keeps_real_safety_controls_without_side_effects(
    ) -> Result<()> {
        let fixture = crate::tests::Fixture::new()?;
        let mut app = App::load(fixture.path.clone());
        let ctx = crate::tests::ui_context();
        app.theme(&ctx);
        for (page, label) in [
            (Page::Devices, "设备"),
            (Page::Displays, "屏幕布局"),
            (Page::Transfers, "文件传输"),
            (Page::Clipboard, "剪贴板"),
            (Page::Shortcuts, "快捷键"),
            (Page::Settings, "设置"),
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
        let output = frame(&mut app, &ctx, vec![]);
        let at = output
            .shapes
            .iter()
            .find_map(|s| text_center(&s.shape, "−"))
            .context("view zoom control absent")?;
        output.drop_without_applying_deltas();
        click(&mut app, &ctx, at);
        assert!(app.workspace_zoom < 1.0 && app.workspace_zoom >= 0.6);
        assert!(app.config == original && app.child.is_none() && app.screen_drag.is_none());
        let output = frame(&mut app, &ctx, vec![]);
        let at = output
            .shapes
            .iter()
            .find_map(|s| text_center(&s.shape, "适配视图"))
            .context("fit view control absent")?;
        output.drop_without_applying_deltas();
        click(&mut app, &ctx, at);
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
        let label = format!("○  {}\n未连接", app.config.peer_name);
        let at = output
            .shapes
            .iter()
            .find_map(|s| text_center(&s.shape, &label))
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
