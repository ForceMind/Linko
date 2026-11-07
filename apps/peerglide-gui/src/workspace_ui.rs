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
            self.page = Page::Devices;
        }
        if self.config.fingerprint.is_some() {
            let status = if self.pairing_pending() { "核验尚未完成" }
                else if self.connected && self.child.is_some() { "已连接" } else { "未连接" };
            if roster_item(ui, &self.config.peer_name, status, self.workspace_peer_selected, false).clicked() {
                self.workspace_peer_selected = true;
                self.device_tab = DeviceTab::Device;
                self.page = Page::Devices;
            }
        }
        ui.add_space(8.0);
        if ui.add_enabled(self.idle_writable(), egui::Button::new(
            if self.config.fingerprint.is_some() { "＋ 重新核验电脑" } else { "＋ 添加另一台电脑" }
        )).clicked() { self.open_setup(); }
        ui.add_space(14.0);
    }

    pub(super) fn device_workspace(&mut self, ui: &mut egui::Ui) {
        let view = self.workspace_view();
        let allowed = match view.action {
            desktop_state::Action::AddDevice | desktop_state::Action::Connect => self.idle_writable(),
            desktop_state::Action::CheckPermissions | desktop_state::Action::StartInput => self.native_start_available(),
            desktop_state::Action::Recovery => true,
            desktop_state::Action::None => false,
        };
        ui.horizontal_wrapped(|ui| {
            ui.vertical(|ui| {
                ui.heading(RichText::new("设备与屏幕").size(26.0));
                ui.weak("让屏幕的位置，和你的桌面一致。");
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if view.action != desktop_state::Action::None
                    && ui.add_enabled(allowed && self.dialog.is_none() && self.wizard.is_none(),
                        desktop_style::primary(view.action.label())).clicked() {
                    self.workspace_action(view.action);
                }
            });
        });
        ui.add_space(16.0);
        if self.device_tab == DeviceTab::Permissions {
            if ui.button("‹ 返回屏幕布局").clicked() { self.device_tab = DeviceTab::Device; }
            ui.add_space(12.0);
            card(ui).show(ui, |ui| self.device_permissions(ui));
            return;
        }
        if self.device_tab == DeviceTab::Layout {
            if ui.button("‹ 返回设备工作区").clicked() { self.device_tab = DeviceTab::Device; }
            card(ui).show(ui, |ui| self.displays(ui));
            return;
        }
        card(ui).inner_margin(12.0).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.strong("屏幕布局");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    pill(ui, "主屏示意", Tone::Neutral);
                });
            });
            ui.add_space(8.0);
            self.workspace_canvas(ui);
            ui.add_space(10.0);
            if self.config.fingerprint.is_some() {
                ui.add_enabled_ui(self.idle_writable(), |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.weak("另一台电脑在我的");
                        for side in ["left", "right", "top", "bottom"] {
                            if ui.selectable_value(&mut self.config.edge, side.into(), edge_label(side)).changed() { self.save(); }
                        }
                    });
                });
            } else {
                ui.weak("添加电脑后，把它拖到这台电脑的左、右、上或下方。");
            }
        });
        ui.add_space(14.0);
        self.workspace_device_detail(ui);
        ui.add_space(12.0);
        ui.horizontal_wrapped(|ui| {
            ui.small(if self.child.is_some() { "会话中布局已锁定，停止后可以调整。" }
                else { "拖动屏幕可保存相对方向；另一端需设置相反方向。" });
            if ui.small_button(if self.connection_details_open { "收起连接详情" } else { "连接与身份详情" }).clicked() {
                self.connection_details_open = !self.connection_details_open;
            }
        });
    }

    fn workspace_canvas(&mut self, ui: &mut egui::Ui) {
        let registered = self.config.fingerprint.is_some();
        let (canvas, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 285.0), egui::Sense::hover());
        let (local, peer) = workspace_rects(canvas.shrink(20.0), &self.config.edge, registered);
        let painter = ui.painter_at(canvas);
        painter.rect_filled(canvas, 12.0, ui.visuals().faint_bg_color);
        let mut grid = egui::Mesh::default();
        let dot = ui.visuals().widgets.noninteractive.bg_stroke.color;
        for y in (12..canvas.height() as usize).step_by(22).take(20) {
            for x in (12..canvas.width() as usize).step_by(22).take(90) {
                grid.add_colored_rect(egui::Rect::from_center_size(canvas.min + egui::vec2(x as f32, y as f32), Vec2::splat(1.3)), dot);
            }
        }
        painter.add(egui::Shape::mesh(grid));
        let (focused, down, events) = ui.input(|i| (i.focused, i.pointer.primary_down(), i.events.clone()));
        let layer = ui.layer_id();
        let own_press = events.iter().all(|event| match event {
            egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, .. } => ui.ctx().layer_id_at(*pos) == Some(layer),
            _ => true,
        });
        let editable = registered && self.idle_writable() && self.dialog.is_none() && self.wizard.is_none();
        let interaction_canvas = canvas.intersect(ui.clip_rect());
        if let Some(edge) = desktop_ui::update_layout_drag(&mut self.screen_drag, &events, peer, local, interaction_canvas,
            editable && focused && own_press, down) {
            if self.config.edge != edge {
                self.config.edge = edge.into();
                self.save();
            }
        }
        let shown_peer = self.screen_drag.map_or(peer, |(_, center, _)| egui::Rect::from_center_size(center, peer.size()));
        if registered {
            painter.line_segment([local.center(), shown_peer.center()], egui::Stroke::new(1.0, dot));
        }
        desktop_style::monitor(&painter, local, desktop_ui::local_device_name(), true,
            self.active_mode == Some(SessionMode::Native) && self.input_state == Some(WorkerInputState::Local), self.config.dark);
        if ui.interact(local, ui.id().with("local-screen"), egui::Sense::click()).clicked() {
            self.workspace_peer_selected = false;
        }
        if registered {
            desktop_style::monitor(&painter, shown_peer, &self.config.peer_name, false,
                self.connected && self.input_state == Some(WorkerInputState::ControllingRemote), self.config.dark);
            if ui.interact(shown_peer, ui.id().with("peer-screen"), egui::Sense::click()).clicked() {
                self.workspace_peer_selected = true;
            }
        } else {
            let add = egui::Rect::from_center_size(peer.center(), Vec2::new(peer.width(), 66.0));
            ui.add_enabled_ui(self.idle_writable(), |ui| {
                if ui.put(add, egui::Button::new("＋\n添加另一台电脑")).clicked() { self.open_setup(); }
            });
        }
        let selected = if self.workspace_peer_selected && registered { shown_peer } else { local };
        painter.rect_stroke(selected.expand(5.0), 12.0, egui::Stroke::new(1.5, ui.visuals().selection.bg_fill), egui::StrokeKind::Inside);
    }

    fn workspace_device_detail(&mut self, ui: &mut egui::Ui) {
        let peer = self.workspace_peer_selected && self.config.fingerprint.is_some();
        card(ui).show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.strong(if peer { &self.config.peer_name } else { desktop_ui::local_device_name() });
                pill(ui, if peer && self.pairing_pending() { "核验待完成" }
                    else if peer { "已记住" } else { "这台电脑" }, Tone::Neutral);
            });
            ui.add_space(10.0);
            ui.horizontal_wrapped(|ui| {
                ui.label("键盘与鼠标");
                if self.child.is_some() || self.stopping.is_some() || self.recovery_blocked { pill(ui, self.input_label(), Tone::Neutral); }
                else { pill(ui, "尚未开启", Tone::Neutral); }
                if !peer && ui.add_enabled((self.child.is_none() || self.active_mode == Some(SessionMode::Communication))
                    && self.stopping.is_none(), egui::Button::new("检查本机权限")).clicked() {
                    self.native_confirmation = false;
                    self.worker_permissions.start();
                }
            });
            ui.add_space(8.0);
            if !peer {
                let readiness = self.worker_permissions.readiness();
                ui.horizontal_wrapped(|ui| {
                    ui.label("本机权限");
                    let (label, tone) = match readiness {
                        worker_permissions::Readiness::Ready => ("刚刚检查通过", Tone::Good),
                        worker_permissions::Readiness::Checking => ("正在检查", Tone::Accent),
                        worker_permissions::Readiness::Unchecked => ("尚未检查或已过期", Tone::Neutral),
                        _ => ("需要处理", Tone::Warning),
                    };
                    pill(ui, label, tone);
                });
                ui.small(readiness.detail());
                ui.add_space(8.0);
            }
            ui.horizontal_wrapped(|ui| {
                ui.label("接收键鼠控制");
                pill(ui, if self.config.allow { "本机已允许" } else { "默认关闭" }, if self.config.allow { Tone::Good } else { Tone::Neutral });
                if ui.small_button("管理权限").clicked() { self.device_tab = DeviceTab::Permissions; }
            });
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                ui.weak("剪贴板与文件"); pill(ui, "尚未接通", Tone::Neutral);
                if ui.small_button("查看说明").clicked() { self.page = Page::Transfers; }
            });
            ui.add_space(8.0);
            ui.small(if peer { "对方是否允许接收控制，由它独立设置；连接成功不会自动授予权限。" }
                else { "系统权限由本机检查；开启键鼠前仍需你明确确认。" });
        });
    }
}

fn roster_item(ui: &mut egui::Ui, name: &str, status: &str, selected: bool, local: bool) -> egui::Response {
    let title: String = name.chars().take(12).collect();
    let response = ui.add_sized([ui.available_width(), 62.0], egui::Button::new(
        RichText::new(format!("{}  {}\n{}", if local { "●" } else { "○" }, title, status)).size(13.0)
    ).selected(selected));
    response.on_hover_text(name)
}

fn workspace_rects(canvas: egui::Rect, edge: &str, paired: bool) -> (egui::Rect, egui::Rect) {
    let vertical = paired && matches!(edge, "top" | "bottom");
    let scale = (canvas.width() / if vertical { 250.0 } else { 480.0 })
        .min(canvas.height() / if vertical { 290.0 } else { 190.0 }).clamp(0.0, 1.15);
    let size = Vec2::new(196.0, 126.0) * scale;
    let delta = if vertical { Vec2::new(0.0, (size.y + 20.0 * scale) / 2.0) }
        else { Vec2::new((size.x + 34.0 * scale) / 2.0, 0.0) };
    let delta = if paired && matches!(edge, "left" | "top") { -delta } else { delta };
    (egui::Rect::from_center_size(canvas.center() - delta, size), egui::Rect::from_center_size(canvas.center() + delta, size))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn text_center(shape: &egui::epaint::Shape, label: &str) -> Option<egui::Pos2> {
        match shape {
            egui::epaint::Shape::Text(text) if text.galley.job.text == label => Some(text.visual_bounding_rect().center()),
            egui::epaint::Shape::Vec(shapes) => shapes.iter().find_map(|s| text_center(s, label)),
            _ => None,
        }
    }
    fn frame(app: &mut App, ctx: &egui::Context, events: Vec<egui::Event>) -> egui::FullOutput {
        ctx.run_ui(egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(1180.0, 820.0))),
            events,
            ..Default::default()
        }, |root| app.desktop(root))
    }
    fn click(app: &mut App, ctx: &egui::Context, at: egui::Pos2) {
        for pressed in [true, false] {
            frame(app, ctx, vec![egui::Event::PointerMoved(at), egui::Event::PointerButton {
                pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE,
            }]).drop_without_applying_deltas();
        }
    }
    #[test]
    fn empty_canvas_add_opens_real_pairing_without_clickthrough_or_identity_creation() -> Result<()> {
        let fixture = crate::tests::Fixture::new()?;
        let mut app = App::load(fixture.path.clone());
        let ctx = egui::Context::default(); install_fonts(&ctx);
        let output = frame(&mut app, &ctx, vec![]);
        let at = output.shapes.iter().find_map(|s| text_center(&s.shape, "＋\n添加另一台电脑")).context("canvas add button absent")?;
        output.drop_without_applying_deltas();
        click(&mut app, &ctx, at);
        assert!(app.pairing_open && app.child.is_none() && !app.native_confirmation);
        assert!(!app.config.native && !app.config.allow);
        assert!(!fixture.path.join("identity").exists() && !fixture.path.join("settings.json").exists());
        drop(app); fixture.close()?; Ok(())
    }
    #[test]
    fn selecting_roster_peer_changes_only_context_not_layout_trust_or_permissions() -> Result<()> {
        let fixture = crate::tests::Fixture::new()?;
        let mut app = App::load(fixture.path.clone());
        app.config.fingerprint = Some("presentation-only-not-authentication".into());
        let original = app.config.clone();
        let ctx = egui::Context::default(); install_fonts(&ctx);
        let output = frame(&mut app, &ctx, vec![]);
        let label = format!("○  {}\n未连接", app.config.peer_name);
        let at = output.shapes.iter().find_map(|s| text_center(&s.shape, &label)).context("roster peer absent")?;
        output.drop_without_applying_deltas();
        click(&mut app, &ctx, at);
        assert!(app.workspace_peer_selected && app.child.is_none());
        assert!(app.config == original && !fixture.path.join("settings.json").exists());
        drop(app); fixture.close()?; Ok(())
    }
    #[test]
    fn primary_screen_and_peer_stay_inside_compact_canvas_in_all_directions() {
        for size in [Vec2::new(270.0, 220.0), Vec2::new(620.0, 285.0), Vec2::new(1000.0, 400.0)] {
            let canvas = egui::Rect::from_min_size(egui::pos2(30.0, 50.0), size);
            for edge in ["left", "right", "top", "bottom"] {
                let (local, peer) = workspace_rects(canvas, edge, true);
                assert!(canvas.contains_rect(local) && canvas.contains_rect(peer));
                assert!(!local.intersects(peer));
            }
        }
    }
    #[test]
    fn default_workspace_and_selection_render_without_identity_network_or_grants() -> Result<()> {
        let fixture = crate::tests::Fixture::new()?;
        let mut app = App::load(fixture.path.clone());
        assert!(app.page == Page::Devices && !app.workspace_peer_selected);
        let ctx = egui::Context::default();
        install_fonts(&ctx);
        for paired in [false, true] {
            app.config.fingerprint = paired.then(|| "presentation-only-not-authentication".into());
            for selected in [false, true] {
                app.workspace_peer_selected = selected;
                let output = ctx.run_ui(egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(920.0, 700.0))),
                    ..Default::default()
                }, |root| app.desktop(root));
                assert!(!output.shapes.is_empty());
                output.drop_without_applying_deltas();
                assert!(app.child.is_none() && !app.config.native && !app.config.allow);
                assert!(!fixture.path.join("settings.json").exists() && !fixture.path.join("identity").exists());
            }
        }
        drop(app);
        fixture.close()?;
        Ok(())
    }
}
