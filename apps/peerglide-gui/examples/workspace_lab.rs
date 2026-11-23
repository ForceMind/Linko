//! Interactive RW06 layout and scheduling with four offline fixture surfaces.
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, Rect, RichText, Stroke};
use linko_workspace_lab::coordinator::{Coordinator, SurfaceSpec, ViewId};
use linko_workspace_lab::geometry::{
    Camera, PixelSize, Point, Rect as CanvasRect, Rotation, SourceGeometry,
};
use linko_workspace_lab::scheduling::{Budget, Decision, PauseReason, Plan};
use linko_workspace_lab::session::SurfaceKind;
use linko_workspace_lab::{Binding, Error, SurfaceKey};

#[path = "workspace_lab/native_fixture.rs"]
mod native_fixture;
use native_fixture::{NativeFixture, PANELS as NATIVE_PANELS};

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

type NativePreview<'a> = [Option<&'a egui::TextureHandle>; NATIVE_PANELS];
type PreviewAreas = [Option<Rect>; NATIVE_PANELS];

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
                    panel: canvas_rect(tile(index, 880.0, ROW_HEIGHT)),
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
        let height = row_height(viewport);
        for (index, panel) in self.panels.iter().enumerate() {
            self.coordinator.relayout(
                panel.view,
                ORIGIN,
                canvas_rect(tile(index, viewport.width(), height)),
                now,
            )?;
        }
        self.coordinator
            .plan_viewport(canvas_rect(viewport), Some(self.focused), self.budget, now)
    }

    fn toggle_viewing(&mut self, panel: Panel, now: u64) -> Result<(), Error> {
        if self.coordinator.snapshot(panel.view, now)?.viewing_allowed {
            self.coordinator.revoke_view(panel.view, now)
        } else {
            self.coordinator.grant_view(panel.view, now, GRANT_MS)
        }
    }

    fn show(
        &mut self,
        root: &mut egui::Ui,
        now: u64,
        preview: Option<NativePreview<'_>>,
    ) -> PreviewAreas {
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
                let subtitle = if preview.is_some() {
                    "Native window fixtures  ·  2 decoder slots"
                } else {
                    "Offline fixtures  ·  2 decoder slots"
                };
                ui.label(RichText::new(subtitle).color(MUTED));
                ui.add_space(18.0);
                let result = egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show_viewport(ui, |ui, viewport| {
                        self.show_tiles(ui, viewport, shortcut, now, preview)
                    });
                match result.inner {
                    Ok(area) => area,
                    Err(error) => {
                        ui.colored_label(Color32::DARK_RED, error.to_string());
                        [None; NATIVE_PANELS]
                    }
                }
            })
            .inner
    }

    fn show_tiles(
        &mut self,
        ui: &mut egui::Ui,
        viewport: Rect,
        shortcut: Option<usize>,
        now: u64,
        preview: Option<NativePreview<'_>>,
    ) -> Result<PreviewAreas, Error> {
        if viewport.width() <= GAP || viewport.height() <= 0.0 {
            return Ok([None; NATIVE_PANELS]);
        }
        let plan = self.plan(viewport, now)?;
        let height = row_height(viewport);
        let (margin, spacing, status_height): (i8, f32, f32) = if height < ROW_HEIGHT {
            (12, 8.0, 50.0)
        } else {
            (16, 12.0, 64.0)
        };
        let (canvas, _) = ui.allocate_exact_size(
            egui::vec2(viewport.width(), height * 2.0 + GAP),
            egui::Sense::hover(),
        );
        if let Some(index) = shortcut {
            ui.scroll_to_rect(
                tile(index, viewport.width(), height).translate(canvas.min.to_vec2()),
                Some(egui::Align::Center),
            );
        }
        let mut preview_areas = [None; NATIVE_PANELS];
        for index in 0..self.panels.len() {
            let panel = self.panels[index];
            let bounds = tile(index, viewport.width(), height);
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
            let builder = egui::UiBuilder::new().id_salt(index).max_rect(screen);
            let mut card = ui.new_child(builder);
            card.set_clip_rect(ui.clip_rect().intersect(screen));
            let (focus, toggle) = egui::Frame::new()
                .fill(Color32::WHITE)
                .inner_margin(margin)
                .corner_radius(12)
                .stroke(Stroke::new(1.0, if selected { ACCENT } else { OUTLINE }))
                .show(&mut card, |ui| {
                    let frame_padding = 2.0 * f32::from(margin) + 2.0;
                    ui.set_min_size(screen.size() - egui::vec2(frame_padding, frame_padding));
                    let focus = ui
                        .selectable_label(selected, RichText::new(panel.label).size(16.0))
                        .clicked();
                    ui.add_space(spacing);
                    if let Some(texture) = preview.and_then(|preview| preview.get(index).copied()) {
                        let texture = if matches!(decision, Decision::Stream(_)) {
                            texture
                        } else {
                            None
                        };
                        preview_areas[index] =
                            Some(render_native_preview(ui, texture, status_height + 24.0));
                    } else {
                        render_status(ui, decision, self.budget, status_height);
                    }
                    ui.add_space(spacing);
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
        Ok(preview_areas)
    }
}

fn row_height(viewport: Rect) -> f32 {
    viewport.height().clamp(160.0, ROW_HEIGHT)
}

fn tile(index: usize, width: f32, height: f32) -> Rect {
    let width = (width - GAP) / 2.0;
    Rect::from_min_size(
        egui::pos2(
            if index.is_multiple_of(2) {
                0.0
            } else {
                width + GAP
            },
            if index < 2 { 0.0 } else { height + GAP },
        ),
        egui::vec2(width, height),
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

fn render_status(ui: &mut egui::Ui, decision: Decision, budget: Budget, height: f32) {
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
            ui.set_min_size(egui::vec2(ui.available_width(), height));
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

fn render_native_preview(
    ui: &mut egui::Ui,
    texture: Option<&egui::TextureHandle>,
    height: f32,
) -> Rect {
    let (slot, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), height),
        egui::Sense::hover(),
    );
    let height = slot.height().min(slot.width() / native_fixture::ASPECT);
    let size = egui::vec2(height * native_fixture::ASPECT, height);
    let area = Rect::from_center_size(slot.center(), size);
    ui.painter().rect_filled(slot, 8, CANVAS);
    if let Some(texture) = texture {
        ui.painter().image(
            texture.id(),
            area,
            Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
            Color32::WHITE,
        );
    }
    area
}

struct WorkspaceApp {
    workspace: Workspace,
    started: Instant,
    native: Option<NativeFixture>,
    native_passed: Arc<AtomicBool>,
}

impl eframe::App for WorkspaceApp {
    fn ui(&mut self, root: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let now = elapsed_ms(self.started);
        let context = root.ctx().clone();
        let Some(native) = &mut self.native else {
            let _ = self.workspace.show(root, now, None);
            context.request_repaint_after(Duration::from_secs(1));
            return;
        };
        let result = native
            .advance(&mut self.workspace.coordinator, &context, now)
            .and_then(|finished| {
                if !finished {
                    let preview = native.textures(now);
                    if let [Some(first), Some(second)] = self.workspace.show(root, now, Some(preview)) {
                        native.verify_paint(&context, [first, second])?;
                    }
                }
                Ok(finished)
            });
        match result {
            Ok(true) => {
                self.native_passed.store(true, Ordering::Relaxed);
                context.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            Ok(false) => context.request_repaint_after(Duration::from_millis(20)),
            Err(error) => {
                eprintln!("Workspace panel: {error}");
                context.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let paths = match args.as_slice() {
        [] => None,
        [mode, launcher, codec] if mode == "--native-window-probe" => {
            Some((PathBuf::from(launcher), PathBuf::from(codec)))
        }
        _ => return Err(io::Error::other("unexpected workspace example arguments").into()),
    };
    let require_native_result = paths.is_some();
    let native_passed = Arc::new(AtomicBool::new(false));
    let result = Arc::clone(&native_passed);
    let viewport = egui::ViewportBuilder::default()
        .with_inner_size([916.0, 584.0])
        .with_min_inner_size([560.0, 300.0]);
    let title = match option_env!("LINKO_VERSION") {
        Some(version) => format!("Linko Workspace Lab · {version}"),
        None => String::from("Linko Workspace Lab"),
    };
    let native_options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    #[cfg(all(windows, feature = "workspace-lab-dx12"))]
    let native_options = eframe::NativeOptions {
        renderer: if require_native_result {
            eframe::Renderer::Wgpu
        } else {
            eframe::Renderer::Glow
        },
        ..native_options
    };
    eframe::run_native(
        &title,
        native_options,
        Box::new(move |cc| {
            #[cfg(all(windows, feature = "workspace-lab-dx12"))]
            if require_native_result {
                let state = cc
                    .wgpu_render_state
                    .as_ref()
                    .ok_or_else(|| io::Error::other("missing DX12 render state"))?;
                let adapter = state.adapter.get_info();
                if adapter.backend != wgpu::Backend::Dx12 {
                    return Err(io::Error::other("expected a DX12 render adapter").into());
                }
                println!(
                    "Workspace panel: Direct3D 12 adapter class {:?}.",
                    adapter.device_type
                );
            }
            cc.egui_ctx.set_theme(egui::Theme::Light);
            let mut workspace = Workspace::new()?;
            let started = Instant::now();
            let native = if let Some((launcher, codec)) = paths {
                let views = [workspace.panels[0].view, workspace.panels[1].view];
                for view in views {
                    workspace.coordinator.revise_geometry(
                        view,
                        SourceGeometry {
                            pixels: native_fixture::FRAME_SIZE,
                            logical: CanvasRect {
                                x: 0.0,
                                y: 0.0,
                                width: 128.0,
                                height: 96.0,
                            },
                            rotation: Rotation::None,
                            revision: 2,
                        },
                        0,
                    )?;
                }
                Some(NativeFixture::start(
                    &launcher,
                    &codec,
                    views,
                    started,
                    &cc.egui_ctx,
                )?)
            } else {
                None
            };
            Ok(Box::new(WorkspaceApp {
                workspace,
                started,
                native,
                native_passed: result,
            }))
        }),
    )?;
    if require_native_result && !native_passed.load(Ordering::Relaxed) {
        return Err(io::Error::other("native window draw and clear were not verified").into());
    }
    Ok(())
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
            |root| {
                let _ = workspace.show(root, 1, None);
            },
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

        let compact = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(512.0, 174.0));
        let top = workspace.plan(compact, 3)?;
        assert!(top.surfaces.iter().any(|surface| {
            surface.key == focus.key && surface.decision == Decision::Pause(PauseReason::Offscreen)
        }));

        let bottom = compact.translate(egui::vec2(0.0, row_height(compact) + GAP));
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
