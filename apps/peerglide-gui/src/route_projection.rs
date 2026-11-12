//! An explicitly opened, local projection of the saved display route plan.
use super::{cursor_overlay, egui, route_plan};
use anyhow::{ensure, Context, Result};
use peerglide_platform::display_layout::{Bounds, Display, Layout};
use peerglide_protocol::{
    route_plan::{Plan, RouteSpec},
    Edge,
};
use std::{
    sync::{Arc, Mutex},
    time::Instant,
};

const WINDOW_SIZE: [f32; 2] = [660.0, 420.0];
const EDGES: [Edge; 4] = [Edge::Left, Edge::Right, Edge::Top, Edge::Bottom];

pub(super) enum Decision {
    Save(Plan),
    Close,
}

pub(super) struct Projection {
    id: egui::ViewportId,
    layout: Arc<Layout>,
    base: Plan,
    center: [f64; 2],
    editor: Arc<Mutex<Editor>>,
}

struct Editor {
    plan: Plan,
    selected: usize,
    preview: Option<RouteSpec>,
    message: String,
    positioned: bool,
    decision: Option<Decision>,
}

impl Projection {
    pub(super) fn new(plan: &Plan, layout: Arc<Layout>) -> Result<Self> {
        route_plan::validate_layout(plan, &layout)?;
        let first = plan.routes.first().context("请先保存一个分段")?;
        let source = layout
            .displays()
            .iter()
            .find(|display| origin(display) == first.local_origin)
            .context("无法定位分段的本机屏幕")?;
        let bounds = source.bounds;
        Ok(Self {
            id: egui::ViewportId::from_hash_of(("linko-layout-projection", Instant::now())),
            layout,
            base: plan.clone(),
            center: [
                (bounds.left + bounds.right) / 2.0,
                (bounds.top + bounds.bottom) / 2.0,
            ],
            editor: Arc::new(Mutex::new(Editor {
                plan: plan.clone(),
                selected: 0,
                preview: None,
                message: String::new(),
                positioned: false,
                decision: None,
            })),
        })
    }

    pub(super) fn validate_save(
        &self,
        current_plan: &Plan,
        proposed: &Plan,
        current_layout: &Layout,
    ) -> Result<()> {
        ensure!(&self.base == current_plan, "投影期间分段草稿已变化");
        ensure!(
            self.layout.same_configuration(current_layout),
            "投影期间屏幕布局已变化，请重新读取显示器"
        );
        ensure!(
            proposed.version == self.base.version
                && proposed.peers == self.base.peers
                && proposed.routes.len() == self.base.routes.len()
                && proposed
                    .routes
                    .iter()
                    .zip(&self.base.routes)
                    .all(|(next, old)| {
                        next.peer == old.peer
                            && next.remote_edge == old.remote_edge
                            && next.remote_span == old.remote_span
                    }),
            "投影只调整本机出边和范围"
        );
        route_plan::validate_layout(proposed, current_layout)
    }

    pub(super) fn close(&self, ctx: &egui::Context) {
        if let Ok(mut editor) = self.editor.lock() {
            editor.decision = Some(Decision::Close);
        }
        ctx.send_viewport_cmd_to(self.id, egui::ViewportCommand::Visible(false));
    }

    pub(super) fn ui(&self, ctx: &egui::Context) -> Option<Decision> {
        match self.editor.lock() {
            Ok(mut editor) => {
                if editor.decision.is_some() {
                    return editor.decision.take();
                }
            }
            Err(_) => return Some(Decision::Close),
        }
        let mut builder = egui::ViewportBuilder::default()
            .with_title("屏跃布局投影")
            .with_inner_size(WINDOW_SIZE)
            .with_min_inner_size([460.0, 320.0])
            .with_max_inner_size([960.0, 660.0])
            .with_always_on_top();
        if let Some(position) =
            window_position(ctx, self.center, egui::vec2(WINDOW_SIZE[0], WINDOW_SIZE[1]))
        {
            builder = builder.with_position(position);
        }
        let editor = self.editor.clone();
        let layout = self.layout.clone();
        let center = self.center;
        ctx.show_viewport_deferred(self.id, builder, move |ui, class| {
            if class != egui::ViewportClass::Deferred {
                return;
            }
            let Ok(mut editor) = editor.lock() else {
                return;
            };
            let closed = ui.input(|input| input.viewport().close_requested())
                || !ui.ctx().any_popup_open()
                    && ui.input_mut(|input| {
                        input.consume_key(egui::Modifiers::NONE, egui::Key::Escape)
                    });
            if closed {
                editor.decision = Some(Decision::Close);
                ui.ctx().request_repaint_of(egui::ViewportId::ROOT);
            }
            if editor.decision.is_some() {
                return;
            }
            if !editor.positioned {
                let size = ui.input(|input| input.viewport().outer_rect.map(|rect| rect.size()));
                if let Some(position) =
                    size.and_then(|size| window_position(ui.ctx(), center, size))
                {
                    ui.ctx()
                        .send_viewport_cmd(egui::ViewportCommand::OuterPosition(position));
                    editor.positioned = true;
                }
            }
            editor.draw(ui, &layout);
        });
        None
    }
}

fn window_position(ctx: &egui::Context, desktop: [f64; 2], size: egui::Vec2) -> Option<egui::Pos2> {
    let center = cursor_overlay::points(
        desktop,
        cfg!(windows),
        ctx.pixels_per_point(),
        ctx.zoom_factor(),
    )?;
    Some(center - size / 2.0)
}

impl Editor {
    fn draw(&mut self, ui: &mut egui::Ui, layout: &Layout) {
        ui.painter()
            .rect_filled(ui.max_rect(), 0.0, ui.visuals().window_fill());
        ui.horizontal_wrapped(|ui| {
            if ui.button("保存布局").clicked() {
                self.decision = Some(Decision::Save(self.plan.clone()));
            }
            if ui.button("取消").clicked() {
                self.decision = Some(Decision::Close);
            }
            egui::ComboBox::from_id_salt("projected-segment")
                .selected_text(route_label(&self.plan, self.selected))
                .show_ui(ui, |ui| {
                    for index in 0..self.plan.routes.len() {
                        if ui
                            .selectable_value(
                                &mut self.selected,
                                index,
                                route_label(&self.plan, index),
                            )
                            .changed()
                        {
                            self.preview = None;
                            self.message.clear();
                        }
                    }
                });
        });
        if self.decision.is_some() {
            ui.ctx().request_repaint_of(egui::ViewportId::ROOT);
            return;
        }
        if self.message.is_empty() {
            ui.label("拖动分段圆点到可用外沿。");
        } else {
            ui.label(&self.message);
        }
        let (area, painter) = ui.allocate_painter(ui.available_size(), egui::Sense::hover());
        let screens = project_screens(layout, area.rect.shrink(24.0));
        for (index, (_, rect)) in screens.iter().enumerate() {
            painter.rect_filled(*rect, 5.0, ui.visuals().widgets.inactive.bg_fill);
            painter.rect_stroke(
                *rect,
                5.0,
                ui.visuals().widgets.inactive.bg_stroke,
                egui::StrokeKind::Inside,
            );
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                format!("屏幕 {}", index + 1),
                egui::FontId::proportional(14.0),
                ui.visuals().text_color(),
            );
        }
        let color = ui.visuals().selection.stroke.color;
        for (index, route) in self.plan.routes.iter().enumerate() {
            if let Some(line) = route_line(&screens, route) {
                painter.line_segment(
                    line,
                    egui::Stroke::new(if index == self.selected { 5.0 } else { 2.0 }, color),
                );
            }
        }
        let Some(route) = self.plan.routes.get(self.selected) else {
            return;
        };
        let Some([from, to]) = route_line(&screens, route) else {
            return;
        };
        let middle = from.lerp(to, 0.5);
        let drag = ui
            .interact(
                egui::Rect::from_center_size(middle, egui::Vec2::splat(28.0)),
                ui.id().with("projected-segment-handle"),
                egui::Sense::drag(),
            )
            .on_hover_and_drag_cursor(egui::CursorIcon::Grab);
        painter.circle_filled(middle, 7.0, color);
        if !ui.input(|input| input.focused) {
            self.preview = None;
            return;
        }
        if drag.dragged_by(egui::PointerButton::Primary) {
            self.preview = drag
                .interact_pointer_pos()
                .and_then(|point| move_route(layout, &screens, point, route));
        }
        if drag.drag_stopped_by(egui::PointerButton::Primary) {
            self.preview = None;
            let released_at = ui.input(|input| {
                input.events.iter().rev().find_map(|event| match event {
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: false,
                        ..
                    } => Some(*pos),
                    _ => None,
                })
            });
            let Some(point) = released_at.filter(|point| point.distance(middle) >= 3.0) else {
                return;
            };
            let result = move_route(layout, &screens, point, route)
                .context("请选择可用的屏幕外沿")
                .and_then(|route| {
                    let mut next = self.plan.clone();
                    next.routes[self.selected] = route;
                    route_plan::validate_layout(&next, layout)?;
                    Ok(next)
                });
            match result {
                Ok(next) => {
                    self.plan = next;
                    self.message.clear();
                }
                Err(error) => self.message = format!("分段未移动：{error:#}"),
            }
        }
        if let Some(line) = self
            .preview
            .as_ref()
            .and_then(|route| route_line(&screens, route))
        {
            painter.line_segment(line, egui::Stroke::new(4.0, color.gamma_multiply(0.65)));
        }
    }
}

fn route_label(plan: &Plan, index: usize) -> String {
    let route = &plan.routes[index];
    let [x, y] = route.local_origin;
    format!(
        "({x},{y}) {} → {}",
        route_plan::edge_label(route.local_edge),
        plan.peers[route.peer].address
    )
}

fn origin(display: &Display) -> [f64; 2] {
    [display.bounds.left, display.bounds.top]
}

fn project_screens(layout: &Layout, area: egui::Rect) -> Vec<(Display, egui::Rect)> {
    let displays = layout.displays();
    let Some(first) = displays.first() else {
        return Vec::new();
    };
    let desktop = displays.iter().fold(first.bounds, |a, d| Bounds {
        left: a.left.min(d.bounds.left),
        right: a.right.max(d.bounds.right),
        top: a.top.min(d.bounds.top),
        bottom: a.bottom.max(d.bounds.bottom),
    });
    let width = desktop.right - desktop.left;
    let height = desktop.bottom - desktop.top;
    let scale = (f64::from(area.width()) / width).min(f64::from(area.height()) / height);
    if !scale.is_finite() || scale <= 0.0 {
        return Vec::new();
    }
    let start =
        area.center() - egui::vec2((width * scale / 2.0) as f32, (height * scale / 2.0) as f32);
    let point = |x: f64, y: f64| {
        start
            + egui::vec2(
                ((x - desktop.left) * scale) as f32,
                ((y - desktop.top) * scale) as f32,
            )
    };
    displays
        .iter()
        .map(|display| {
            let b = display.bounds;
            (
                *display,
                egui::Rect::from_min_max(point(b.left, b.top), point(b.right, b.bottom)),
            )
        })
        .collect()
}

fn edge_point(rect: egui::Rect, edge: Edge, at: f32) -> egui::Pos2 {
    match edge {
        Edge::Left => rect.left_top().lerp(rect.left_bottom(), at),
        Edge::Right => rect.right_top().lerp(rect.right_bottom(), at),
        Edge::Top => rect.left_top().lerp(rect.right_top(), at),
        Edge::Bottom => rect.left_bottom().lerp(rect.right_bottom(), at),
    }
}

fn route_line(screens: &[(Display, egui::Rect)], route: &RouteSpec) -> Option<[egui::Pos2; 2]> {
    let (_, rect) = screens
        .iter()
        .find(|(display, _)| origin(display) == route.local_origin)?;
    Some(
        route
            .local_span
            .map(|at| edge_point(*rect, route.local_edge, at as f32)),
    )
}

fn move_route(
    layout: &Layout,
    screens: &[(Display, egui::Rect)],
    pointer: egui::Pos2,
    route: &RouteSpec,
) -> Option<RouteSpec> {
    let mut targets = Vec::new();
    for (display, rect) in screens {
        if !rect.is_positive() {
            continue;
        }
        for edge in EDGES {
            let along = match edge {
                Edge::Left | Edge::Right => (pointer.y - rect.top()) / rect.height(),
                Edge::Top | Edge::Bottom => (pointer.x - rect.left()) / rect.width(),
            }
            .clamp(0.0, 1.0);
            let distance = edge_point(*rect, edge, along).distance(pointer);
            if distance <= 22.0 {
                targets.push((distance, *display, edge, f64::from(along)));
            }
        }
    }
    targets.sort_by(|a, b| a.0.total_cmp(&b.0));
    let &(distance, display, edge, along) = targets.first()?;
    if targets
        .get(1)
        .is_some_and(|other| (other.0 - distance).abs() < 1.0)
    {
        return None;
    }
    let half = (route.local_span[1] - route.local_span[0]) / 2.0;
    let center = along.clamp(half, 1.0 - half);
    let span = [(center - half).max(0.0), (center + half).min(1.0)];
    if !span_is_exposed(layout, display, edge, span) {
        return None;
    }
    Some(RouteSpec {
        local_origin: origin(&display),
        local_edge: edge,
        local_span: span,
        ..route.clone()
    })
}

fn axis(bounds: Bounds, edge: Edge) -> (f64, f64) {
    match edge {
        Edge::Left | Edge::Right => (bounds.top, bounds.bottom),
        Edge::Top | Edge::Bottom => (bounds.left, bounds.right),
    }
}

fn span_is_exposed(layout: &Layout, display: Display, edge: Edge, span: [f64; 2]) -> bool {
    let bounds = display.bounds;
    let (min, max) = axis(bounds, edge);
    let [start, end] = span.map(|n| min + (max - min) * n);
    let exposed_at = |along| {
        let (x, y) = match edge {
            Edge::Left => (bounds.left, along),
            Edge::Right => (bounds.right - 0.5, along),
            Edge::Top => (along, bounds.top),
            Edge::Bottom => (along, bounds.bottom - 0.5),
        };
        layout
            .edge_at(edge, x, y, 1.0)
            .is_some_and(|anchor| anchor.display == display.id)
    };
    exposed_at((start + end) / 2.0)
        && layout
            .displays()
            .iter()
            .filter(|other| other.id != display.id)
            .all(|other| {
                let (left, right) = axis(other.bounds, edge);
                let from = start.max(left);
                let to = end.min(right);
                from >= to || exposed_at((from + to) / 2.0)
            })
}

#[cfg(test)]
mod tests {
    use super::*;
    use peerglide_platform::display_layout::{DisplayId, Rotation};
    use peerglide_protocol::route_plan::PeerSpec;

    fn fixture() -> (Layout, Plan) {
        let first = Display {
            id: DisplayId(1),
            bounds: Bounds {
                left: -200.0,
                top: 0.0,
                right: 0.0,
                bottom: 200.0,
            },
            scale: 1.25,
            rotation: Rotation::Upright,
        };
        let second = Display {
            id: DisplayId(2),
            bounds: Bounds {
                left: 0.0,
                top: 80.0,
                right: 200.0,
                bottom: 120.0,
            },
            ..first
        };
        let layout = Layout::new(first.id, vec![first, second]).unwrap();
        let plan = Plan {
            peers: vec![PeerSpec {
                address: "127.0.0.1:24870".parse().unwrap(),
                certificate: std::env::temp_dir().join("peer.der"),
                fingerprint: Some(blake3::hash(b"test-peer").to_hex().to_string()),
                secondary_layout: None,
            }],
            routes: vec![RouteSpec {
                local_origin: [-200.0, 0.0],
                local_edge: Edge::Left,
                local_span: [0.2, 0.6],
                peer: 0,
                remote_edge: Edge::Right,
                remote_span: [0.1, 0.9],
            }],
            ..Plan::default()
        };
        (layout, plan)
    }

    #[test]
    fn drag_preserves_target_and_rejects_partly_shared_spans_and_ambiguous_corners() {
        let (layout, plan) = fixture();
        let screens = project_screens(
            &layout,
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(400.0, 200.0)),
        );
        let route = &plan.routes[0];
        assert_eq!(screens[0].1.right_top(), egui::pos2(200.0, 0.0));
        assert!(move_route(&layout, &screens, egui::pos2(200.0, 50.0), route).is_none());
        assert!(move_route(&layout, &screens, egui::pos2(0.0, 0.0), route).is_none());
        let moved = move_route(&layout, &screens, egui::pos2(300.0, 80.0), route).unwrap();
        assert_eq!(moved.local_origin, [0.0, 80.0]);
        assert_eq!(moved.local_edge, Edge::Top);
        assert!((moved.local_span[1] - moved.local_span[0] - 0.4).abs() < 1e-10);
        assert_eq!(
            (moved.peer, moved.remote_edge, moved.remote_span),
            (route.peer, route.remote_edge, route.remote_span)
        );
    }

    #[test]
    fn saving_checks_the_frozen_plan_and_display_snapshot() -> Result<()> {
        let (layout, base) = fixture();
        let layout = Arc::new(layout);
        let projection = Projection::new(&base, layout.clone())?;
        let mut moved = base.clone();
        moved.routes[0].local_edge = Edge::Top;
        projection.validate_save(&base, &moved, &layout)?;
        assert!(projection.validate_save(&moved, &moved, &layout).is_err());
        let mut changed = layout.displays().to_vec();
        changed[0].scale = 2.0;
        let changed = Layout::new(layout.primary(), changed).unwrap();
        assert!(projection.validate_save(&base, &moved, &changed).is_err());
        moved.routes[0].remote_edge = Edge::Bottom;
        assert!(projection.validate_save(&base, &moved, &layout).is_err());
        Ok(())
    }

    #[test]
    fn saving_a_projected_overlap_preserves_the_original_ranges() -> Result<()> {
        let (layout, mut base) = fixture();
        let layout = Arc::new(layout);
        base.routes.push(RouteSpec {
            local_span: [0.6, 1.0],
            ..base.routes[0].clone()
        });
        let projection = Projection::new(&base, layout.clone())?;
        let mut proposed = base.clone();
        proposed.routes[0].local_span = [0.4, 0.8];
        assert!(projection.validate_save(&base, &proposed, &layout).is_err());
        assert_eq!(base.routes[0].local_span, [0.2, 0.6]);
        Ok(())
    }

    #[test]
    fn releasing_a_projected_drag_updates_only_the_in_memory_editor() -> Result<()> {
        let (layout, plan) = fixture();
        let layout = Arc::new(layout);
        let projection = Projection::new(&plan, layout.clone())?;
        let mut editor = projection.editor.lock().unwrap();
        let ctx = egui::Context::default();
        let mut frame = |events| {
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(660.0, 420.0),
                    )),
                    focused: true,
                    events,
                    ..Default::default()
                },
                |ui| editor.draw(ui, &layout),
            )
        };
        let first = frame(Vec::new());
        let center = first
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Circle(circle) if circle.radius == 7.0 => Some(circle.center),
                _ => None,
            })
            .context("missing projected drag handle")?;
        let moved = center + egui::vec2(0.0, 45.0);
        let button = |pos, pressed| egui::Event::PointerButton {
            pos,
            pressed,
            button: egui::PointerButton::Primary,
            modifiers: egui::Modifiers::NONE,
        };
        let _ = frame(vec![egui::Event::PointerMoved(center)]);
        let _ = frame(vec![button(center, true)]);
        let _ = frame(vec![egui::Event::PointerMoved(moved)]);
        let _ = frame(vec![button(moved, false)]);
        assert!(editor.plan.routes[0].local_span[0] > plan.routes[0].local_span[0]);
        assert!(editor.decision.is_none());
        assert!(projection.base.routes[0] == plan.routes[0]);
        Ok(())
    }
}
