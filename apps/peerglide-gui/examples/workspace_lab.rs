//! Interactive RW06 layout and scheduling with four offline fixture surfaces.
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, Rect, RichText, Stroke};
use linko_workspace_lab::coordinator::{Coordinator, SurfaceSpec, ViewId};
use linko_workspace_lab::geometry::{
    Camera, PixelSize, Point, Rect as CanvasRect, Rotation, SourceGeometry,
};
use linko_workspace_lab::scheduling::{Budget, Decision, PauseReason, Plan};
use linko_workspace_lab::session::SurfaceKind;
use linko_workspace_lab::{Binding, Error, SurfaceKey};

const ORIGIN: Camera = Camera {
    origin: Point { x: 0.0, y: 0.0 },
    zoom: 1.0,
};
const GRANT_MS: u64 = 86_400_000;
const SLOTS: usize = 2;
const GAP: f32 = 16.0;
const ROW_HEIGHT: f32 = 204.0;
const ACCENT: Color32 = Color32::from_rgb(66, 99, 235);
const CANVAS: Color32 = Color32::from_rgb(247, 248, 250);
const OUTLINE: Color32 = Color32::from_rgb(226, 231, 239);
const MUTED: Color32 = Color32::from_rgb(95, 111, 133);

#[derive(Clone, Copy)]
struct Panel {
    view: ViewId,
    key: SurfaceKey,
    label: &'static str,
    shortcut: egui::Key,
}

struct Workspace {
    coordinator: Coordinator,
    panels: Vec<Panel>,
    focused: ViewId,
    budget: Budget,
}

impl Workspace {
    fn new() -> Result<Self, Error> {
        let viewer = [255; 32];
        let mut coordinator = Coordinator::new(viewer, 4)?;
        let mut panels = Vec::with_capacity(4);
        for (index, (seed, label, shortcut)) in [
            (1, "Panel 1", egui::Key::Num1),
            (2, "Panel 2", egui::Key::Num2),
            (3, "Panel 3", egui::Key::Num3),
            (4, "Panel 4", egui::Key::Num4),
        ]
        .into_iter()
        .enumerate()
        {
            let key = SurfaceKey {
                binding: Binding {
                    source: [seed; 32],
                    viewer,
                    nonce: [seed; 16],
                },
                object: 1,
                generation: 1,
            };
            let view = coordinator.register(
                SurfaceSpec {
                    key,
                    kind: SurfaceKind::Display,
                    geometry: SourceGeometry {
                        pixels: PixelSize {
                            width: 1600,
                            height: 900,
                        },
                        logical: CanvasRect {
                            x: 0.0,
                            y: 0.0,
                            width: 1600.0,
                            height: 900.0,
                        },
                        rotation: Rotation::None,
                        revision: 1,
                    },
                    camera: ORIGIN,
                    panel: canvas_rect(tile(index, 880.0)),
                },
                0,
            )?;
            coordinator.grant_view(view, 0, GRANT_MS)?;
            panels.push(Panel {
                view,
                key,
                label,
                shortcut,
            });
        }
        Ok(Self {
            coordinator,
            focused: panels[0].view,
            panels,
            budget: Budget {
                decoders: SLOTS,
                ..Budget::default()
            },
        })
    }

    fn plan(&mut self, viewport: Rect, now: u64) -> Result<Plan, Error> {
        self.coordinator.poll(now)?;
        for (index, panel) in self.panels.iter().enumerate() {
            self.coordinator.relayout(
                panel.view,
                ORIGIN,
                canvas_rect(tile(index, viewport.width())),
                now,
            )?;
        }
        self.coordinator.plan_viewport(
            canvas_rect(viewport),
            Some(self.focused),
            self.budget,
            now,
        )
    }

    fn toggle_viewing(&mut self, panel: Panel, now: u64) -> Result<(), Error> {
        if self.coordinator.snapshot(panel.view, now)?.viewing_allowed {
            self.coordinator.revoke_view(panel.view, now)
        } else {
            self.coordinator.grant_view(panel.view, now, GRANT_MS)
        }
    }

    fn show(&mut self, root: &mut egui::Ui, now: u64) {
        let shortcut = self
            .panels
            .iter()
            .position(|panel| root.input(|input| input.key_pressed(panel.shortcut)));
        if let Some(index) = shortcut {
            self.focused = self.panels[index].view;
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(CANVAS).inner_margin(24))
            .show(root, |ui| {
                ui.heading(RichText::new("Workspace").size(34.0).strong());
                ui.label(RichText::new("Offline fixtures  ·  2 decoder slots").color(MUTED));
                ui.add_space(18.0);
                let result = egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show_viewport(ui, |ui, viewport| {
                        self.show_tiles(ui, viewport, shortcut, now)
                    });
                if let Err(error) = result.inner {
                    ui.colored_label(Color32::DARK_RED, error.to_string());
                }
            });
    }

    fn show_tiles(
        &mut self,
        ui: &mut egui::Ui,
        viewport: Rect,
        shortcut: Option<usize>,
        now: u64,
    ) -> Result<(), Error> {
        if viewport.width() <= GAP || viewport.height() <= 0.0 {
            return Ok(());
        }
        let plan = self.plan(viewport, now)?;
        let (canvas, _) = ui.allocate_exact_size(
            egui::vec2(viewport.width(), ROW_HEIGHT * 2.0 + GAP),
            egui::Sense::hover(),
        );
        if let Some(index) = shortcut {
            ui.scroll_to_rect(
                tile(index, viewport.width()).translate(canvas.min.to_vec2()),
                Some(egui::Align::Center),
            );
        }
        for index in 0..self.panels.len() {
            let panel = self.panels[index];
            let bounds = tile(index, viewport.width());
            if !bounds.intersects(viewport) {
                continue;
            }
            let screen = bounds.translate(canvas.min.to_vec2());
            let selected = panel.view == self.focused;
            let decision = plan
                .surfaces
                .iter()
                .find(|surface| surface.key == panel.key)
                .ok_or(Error::InvalidState)?
                .decision;
            let mut card = ui.new_child(egui::UiBuilder::new().max_rect(screen));
            card.set_clip_rect(ui.clip_rect().intersect(screen));
            let (focus, toggle) = egui::Frame::new()
                .fill(Color32::WHITE)
                .inner_margin(16)
                .corner_radius(12)
                .stroke(Stroke::new(1.0, if selected { ACCENT } else { OUTLINE }))
                .show(&mut card, |ui| {
                    ui.set_min_width(screen.width() - 34.0);
                    ui.set_min_height(ROW_HEIGHT - 34.0);
                    let focus = ui
                        .selectable_label(selected, RichText::new(panel.label).size(16.0))
                        .clicked();
                    ui.add_space(12.0);
                    render_status(ui, decision, self.budget);
                    ui.add_space(12.0);
                    let toggle = ui
                        .button(if decision == Decision::Pause(PauseReason::Unauthorized) {
                            "Resume"
                        } else {
                            "Pause"
                        })
                        .clicked();
                    (focus, toggle)
                })
                .inner;
            if focus {
                self.focused = panel.view;
            }
            if toggle {
                self.toggle_viewing(panel, now)?;
            }
            if focus || toggle {
                ui.ctx().request_repaint();
            }
        }
        Ok(())
    }
}

fn tile(index: usize, width: f32) -> Rect {
    let width = (width - GAP) / 2.0;
    Rect::from_min_size(
        egui::pos2(
            if index.is_multiple_of(2) {
                0.0
            } else {
                width + GAP
            },
            if index < 2 { 0.0 } else { ROW_HEIGHT + GAP },
        ),
        egui::vec2(width, ROW_HEIGHT),
    )
}

fn canvas_rect(rect: Rect) -> CanvasRect {
    CanvasRect {
        x: f64::from(rect.min.x),
        y: f64::from(rect.min.y),
        width: f64::from(rect.width()),
        height: f64::from(rect.height()),
    }
}

fn render_status(ui: &mut egui::Ui, decision: Decision, budget: Budget) {
    let label = match decision {
        Decision::Stream(allocation) if allocation.fps == budget.focused.fps => "Full quality",
        Decision::Stream(_) => "Preview quality",
        Decision::Pause(PauseReason::ResourceLimit) => "Waiting for a slot",
        Decision::Pause(PauseReason::Unauthorized) => "Paused",
        Decision::Pause(PauseReason::Offscreen) => "Offscreen",
    };
    let (fill, color) = if matches!(decision, Decision::Stream(_)) {
        (Color32::from_rgb(232, 238, 255), ACCENT)
    } else {
        (CANVAS, MUTED)
    };
    egui::Frame::new()
        .fill(fill)
        .corner_radius(8)
        .inner_margin(12)
        .show(ui, |ui| {
            ui.set_min_size(egui::vec2(ui.available_width(), 64.0));
            ui.label(RichText::new(label).strong().color(color));
            if let Decision::Stream(allocation) = decision {
                ui.label(
                    RichText::new(format!(
                        "{} × {}  ·  target {} fps",
                        allocation.size.width, allocation.size.height, allocation.fps
                    ))
                    .color(MUTED),
                );
            }
        });
}

struct WorkspaceApp {
    workspace: Workspace,
    started: Instant,
}

impl eframe::App for WorkspaceApp {
    fn ui(&mut self, root: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let now = u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.workspace.show(root, now);
        root.ctx().request_repaint_after(Duration::from_secs(1));
    }
}

fn main() -> eframe::Result<()> {
    let viewport = egui::ViewportBuilder::default()
        .with_inner_size([916.0, 584.0])
        .with_min_inner_size([560.0, 300.0]);
    eframe::run_native(
        "Linko Workspace Lab",
        eframe::NativeOptions {
            viewport,
            ..Default::default()
        },
        Box::new(|cc| {
            cc.egui_ctx.set_theme(egui::Theme::Light);
            Ok(Box::new(WorkspaceApp {
                workspace: Workspace::new()?,
                started: Instant::now(),
            }))
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_focus_and_visibility_use_the_live_plan() -> Result<(), Error> {
        let mut workspace = Workspace::new()?;
        let screen = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(916.0, 584.0));
        let ctx = egui::Context::default();
        let output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(screen),
                events: vec![egui::Event::Key {
                    key: egui::Key::Num3,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }],
                ..Default::default()
            },
            |root| workspace.show(root, 1),
        );
        assert!(!output.shapes.is_empty());
        output.drop_without_applying_deltas();

        let focus = workspace.panels[2];
        assert_eq!(workspace.focused, focus.view);
        let wide = workspace.plan(screen, 2)?;
        assert_eq!(wide.usage.decoders, 2);
        assert_eq!(wide.surfaces[0].key, focus.key);
        assert!(matches!(
            wide.surfaces[0].decision,
            Decision::Stream(allocation) if allocation.fps == workspace.budget.focused.fps
        ));
        assert!(!workspace.coordinator.snapshot(focus.view, 2)?.input_active);

        let compact = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(512.0, ROW_HEIGHT));
        let top = workspace.plan(compact, 3)?;
        assert!(top.surfaces.iter().any(|surface| {
            surface.key == focus.key && surface.decision == Decision::Pause(PauseReason::Offscreen)
        }));

        let bottom = compact.translate(egui::vec2(0.0, ROW_HEIGHT + GAP));
        workspace.toggle_viewing(focus, 4)?;
        let paused = workspace.plan(bottom, 4)?;
        assert_eq!(paused.usage.decoders, 1);
        assert!(paused.surfaces.iter().any(|surface| {
            surface.key == focus.key
                && surface.decision == Decision::Pause(PauseReason::Unauthorized)
        }));
        workspace.toggle_viewing(focus, 5)?;
        let resumed = workspace.plan(bottom, 5)?;
        assert_eq!(resumed.usage.decoders, 2);
        assert_eq!(resumed.surfaces[0].key, focus.key);
        Ok(())
    }
}
