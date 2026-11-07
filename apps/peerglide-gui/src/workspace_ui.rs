//! Device-first desktop: actual roster, spatial canvas and contextual controls.
//! Presentation only; existing permission, enrollment and worker gates own actions.
use super::*;
use desktop_style::{card, pill, Tone};

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
        ui.heading(
            RichText::new(if self.page == Page::Devices {
                "设备"
            } else {
                "屏幕布局"
            })
            .size(26.0),
        );
        ui.weak(if self.page == Page::Devices {
            "管理已记住的电脑与独立协作许可。"
        } else {
            "把另一台电脑放在桌面上对应的方向。"
        });
        ui.add_space(16.0);
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
            let details = if width < 780.0 { 248.0 } else { 280.0 };
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 16.0;
                ui.vertical(|ui| {
                    ui.set_width(width - details - 16.0);
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
        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            if self.page == Page::Displays {
                ui.small(if self.child.is_some() {
                    "会话中布局已锁定，停止后可以调整。"
                } else {
                    "两台主屏的相对方向；另一端需设置相反方向。"
                });
            }
            if ui
                .small_button(if self.connection_details_open {
                    "收起连接详情"
                } else {
                    "连接与身份详情"
                })
                .clicked()
            {
                self.connection_details_open = !self.connection_details_open;
            }
        });
    }

    fn workspace_screen_card(&mut self, ui: &mut egui::Ui) {
        card(ui).inner_margin(12.0).show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.strong("屏幕布局");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    pill(ui, "主屏示意", Tone::Neutral);
                });
            });
            ui.add_space(8.0);
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
                ui.small(format!("视图 {:.0}%", self.workspace_zoom * 100.0));
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
            });
            self.workspace_canvas(ui);
            ui.add_space(10.0);
            if self.config.fingerprint.is_some() {
                ui.add_enabled_ui(self.layout_editable() && self.screen_drag.is_none(), |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.weak("另一台电脑在我的");
                        for side in ["left", "right", "top", "bottom"] {
                            if ui
                                .selectable_label(self.config.edge == side, edge_label(side))
                                .clicked()
                            {
                                self.try_set_edge(side);
                            }
                        }
                    });
                });
            } else {
                ui.weak("添加电脑后，把它拖到这台电脑的左、右、上或下方。");
            }
        });
        ui.add_space(10.0);
        self.workspace_device_cards(ui);
    }

    fn workspace_device_cards(&mut self, ui: &mut egui::Ui) {
        let registered = self.config.fingerprint.is_some();
        ui.columns(if registered { 2 } else { 1 }, |columns| {
            if roster_item(
                &mut columns[0],
                desktop_ui::local_device_name(),
                "本机 · 主屏示意",
                !self.workspace_peer_selected || !registered,
                true,
            )
            .clicked()
            {
                self.workspace_peer_selected = false;
                self.screen_drag = None;
            }
            if registered {
                let status = if self.pairing_pending() {
                    "核验尚未完成"
                } else if self.stopping.is_some() {
                    "正在断开"
                } else if self.connected && self.child.is_some() {
                    "已连接 · 主屏示意"
                } else {
                    "未连接 · 主屏示意"
                };
                if roster_item(
                    &mut columns[1],
                    &self.config.peer_name,
                    status,
                    self.workspace_peer_selected,
                    false,
                )
                .clicked()
                {
                    self.workspace_peer_selected = true;
                    self.screen_drag = None;
                }
            }
        });
    }

    fn workspace_canvas(&mut self, ui: &mut egui::Ui) {
        let registered = self.config.fingerprint.is_some();
        let height = if ui.available_width() < 440.0 {
            190.0
        } else {
            265.0
        };
        let (canvas, _) = ui.allocate_exact_size(
            Vec2::new(ui.available_width(), height),
            egui::Sense::hover(),
        );
        let (local, peer) = workspace_rects(
            canvas.shrink(20.0),
            &self.config.edge,
            registered,
            self.workspace_zoom,
        );
        let painter = ui.painter_at(canvas);
        painter.rect_filled(canvas, 12.0, ui.visuals().faint_bg_color);
        let mut grid = egui::Mesh::default();
        let dot = ui.visuals().widgets.noninteractive.bg_stroke.color;
        for y in (12..canvas.height() as usize).step_by(22).take(20) {
            for x in (12..canvas.width() as usize).step_by(22).take(90) {
                grid.add_colored_rect(
                    egui::Rect::from_center_size(
                        canvas.min + egui::vec2(x as f32, y as f32),
                        Vec2::splat(1.3),
                    ),
                    dot,
                );
            }
        }
        painter.add(egui::Shape::mesh(grid));
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
        desktop_style::monitor(
            &painter,
            local,
            desktop_ui::local_device_name(),
            true,
            self.active_mode == Some(SessionMode::Native)
                && self.input_state == Some(WorkerInputState::Local),
            self.config.dark,
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
            desktop_style::monitor(
                &painter,
                shown_peer,
                &self.config.peer_name,
                false,
                self.connected && self.input_state == Some(WorkerInputState::ControllingRemote),
                self.config.dark,
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
        let selected = if self.workspace_peer_selected && registered {
            shown_peer
        } else {
            local
        };
        painter.rect_stroke(
            selected.expand(5.0),
            12.0,
            egui::Stroke::new(1.5, ui.visuals().selection.bg_fill),
            egui::StrokeKind::Inside,
        );
    }

    fn workspace_device_detail(&mut self, ui: &mut egui::Ui) {
        let peer = self.workspace_peer_selected && self.config.fingerprint.is_some();
        card(ui).inner_margin(14.0).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 6.0;
            ui.set_min_width(ui.available_width());
            ui.strong("快捷操作");
            ui.add_space(6.0);
            let view = self.workspace_view();
            pill(ui, view.title, view.tone);
            ui.add_space(6.0);
            ui.add(egui::Label::new(RichText::new(view.detail).small()).truncate())
                .on_hover_text(view.detail);
            ui.add_space(6.0);
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
                let response = ui.add_enabled(
                    allowed && self.dialog.is_none() && self.wizard.is_none(),
                    desktop_style::primary(view.action.label()),
                );
                self.record_layout_control("primary_action", &response, ui.clip_rect());
                if response.clicked() {
                    self.workspace_action(view.action);
                }
            }
            if self.child.is_some() && ui.button("停止并断开").clicked() {
                self.stop();
            }
            ui.add_space(6.0);
            ui.separator();
            ui.small("所选设备");
            ui.horizontal_wrapped(|ui| {
                ui.strong(if peer {
                    &self.config.peer_name
                } else {
                    desktop_ui::local_device_name()
                });
                pill(
                    ui,
                    if peer && self.pairing_pending() {
                        "核验待完成"
                    } else if peer {
                        "已记住"
                    } else {
                        "本机"
                    },
                    Tone::Neutral,
                );
            });
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                ui.label("键鼠");
                if self.child.is_some() || self.stopping.is_some() || self.recovery_blocked {
                    pill(ui, self.input_label(), Tone::Neutral);
                } else {
                    pill(ui, "尚未开启", Tone::Neutral);
                }
            });
            ui.add_space(6.0);
            if !peer {
                let readiness = self.worker_permissions.readiness();
                ui.horizontal_wrapped(|ui| {
                    ui.label("本机权限");
                    let (label, tone) = match readiness {
                        worker_permissions::Readiness::Ready => ("刚刚检查通过", Tone::Good),
                        worker_permissions::Readiness::Checking => ("正在检查", Tone::Accent),
                        worker_permissions::Readiness::Unchecked => {
                            ("尚未检查或已过期", Tone::Neutral)
                        }
                        _ => ("需要处理", Tone::Warning),
                    };
                    pill(ui, label, tone);
                });
                if !matches!(
                    readiness,
                    worker_permissions::Readiness::Unchecked
                        | worker_permissions::Readiness::Checking
                        | worker_permissions::Readiness::Ready
                ) {
                    ui.small(readiness.detail());
                }
                ui.horizontal_wrapped(|ui| {
                    ui.small("仅检查，不开启键鼠")
                        .on_hover_text(readiness.detail());
                    if ui.small_button("权限详情").clicked() {
                        self.navigate_to(Page::Settings);
                        self.settings_section = SettingsSection::Permissions;
                    }
                });
                ui.add_space(6.0);
            }
            ui.horizontal_wrapped(|ui| {
                ui.label(if peer {
                    "对方 → 本机"
                } else {
                    "接收控制"
                });
                pill(
                    ui,
                    if self.config.allow {
                        "本机已允许"
                    } else {
                        "默认关闭"
                    },
                    if self.config.allow {
                        Tone::Good
                    } else {
                        Tone::Neutral
                    },
                );
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
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                ui.weak("内容共享");
                pill(ui, "尚未接通", Tone::Neutral);
                if ui.small_button("说明").clicked() {
                    self.navigate_to(Page::Transfers);
                }
            });
            ui.add_space(6.0);
            ui.small(if peer {
                "对方是否允许接收控制，由它独立设置；连接成功不会自动授予权限。"
            } else {
                "开启键鼠仍需明确确认。"
            });
        });
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
    let label = RichText::new(format!("{icon}  {title}\n{status}"))
        .size(13.0)
        .color(color);
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
    let scale = (canvas.width() / if vertical { 250.0 } else { 480.0 })
        .min(canvas.height() / if vertical { 290.0 } else { 190.0 })
        .clamp(0.0, 1.15);
    let zoom = if zoom.is_finite() {
        zoom.clamp(0.6, 1.0)
    } else {
        1.0
    };
    let scale = scale * zoom;
    let size = Vec2::new(196.0, 126.0) * scale;
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
        let ctx = egui::Context::default();
        install_fonts(&ctx);
        app.theme(&ctx);
        for paired in [false, true] {
            app.config.fingerprint = paired.then(|| "view-only-not-authentication".into());
            for size in [Vec2::new(920.0, 700.0), Vec2::new(1180.0, 820.0)] {
                let output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                        ..Default::default()
                    },
                    |root| app.desktop(root),
                );
                for label in ["快捷操作", "所选设备", "内容共享", "连接与身份详情"]
                {
                    let (clip, rect) = output
                        .shapes
                        .iter()
                        .find_map(|s| text_rect(&s.shape, label).map(|r| (s.clip_rect, r)))
                        .context("essential workspace control absent")?;
                    assert!(clip.contains_rect(rect), "{label} clipped at {size:?}");
                }
                output.drop_without_applying_deltas();
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
        let ctx = egui::Context::default();
        install_fonts(&ctx);
        app.theme(&ctx);
        for (label, selected) in [
            ("○  已登记电脑\n未连接 · 主屏示意".to_owned(), true),
            (
                format!(
                    "●  {}\n本机 · 主屏示意",
                    desktop_ui::local_device_name()
                        .chars()
                        .take(12)
                        .collect::<String>()
                ),
                false,
            ),
        ] {
            let output = frame(&mut app, &ctx, vec![]);
            let at = output
                .shapes
                .iter()
                .find_map(|s| text_center(&s.shape, &label))
                .context("actual topology device card absent")?;
            output.drop_without_applying_deltas();
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
        let ctx = egui::Context::default();
        install_fonts(&ctx);
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
        let ctx = egui::Context::default();
        install_fonts(&ctx);
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
        let ctx = egui::Context::default();
        install_fonts(&ctx);
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
                .find_map(|s| text_center(&s.shape, label))
                .context("navigation entry absent")?;
            for safety_label in ["停止协同", "紧急恢复本机"] {
                assert!(output
                    .shapes
                    .iter()
                    .any(|s| text_center(&s.shape, safety_label).is_some()));
            }
            output.drop_without_applying_deltas();
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
        let ctx = egui::Context::default();
        install_fonts(&ctx);
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
        let ctx = egui::Context::default();
        install_fonts(&ctx);
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
        let ctx = egui::Context::default();
        install_fonts(&ctx);
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
        let ctx = egui::Context::default();
        install_fonts(&ctx);
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
