//! Bounded, local cursor visuals in a passive desktop viewport.
use super::egui;
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

pub(super) const SUPPORTED: bool = cfg!(any(windows, target_os = "macos"));
const INTERVAL: Duration = Duration::from_millis(33);
const LOCATE: Duration = Duration::from_millis(900);
const TRAIL: Duration = Duration::from_millis(300);
const MAX_SAMPLES: usize = 10;
const SIDE: f32 = 192.0;

// The fallback is eframe's existing default clear color. The root draws it
// when the shared clear callback is transparent for the child viewport.
pub(super) fn root_background() -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(12, 12, 12, 180)
}

#[derive(Clone, Copy)]
struct Sample {
    desktop: [f64; 2],
    at: Instant,
}

#[derive(Default)]
pub(super) struct Overlay {
    locate_until: Option<Instant>,
    next_sample: Option<Instant>,
    last: Option<Sample>,
    trail: VecDeque<Sample>,
    window_origin: Option<egui::Pos2>,
    // One token per window. A stale deferred paint cannot resurrect it.
    live: Option<(egui::ViewportId, Arc<AtomicBool>)>,
    generation: u64,
    rejected_focus: Arc<AtomicBool>,
}

impl Overlay {
    pub(super) fn locate(&mut self, now: Instant) {
        self.rejected_focus.store(false, Ordering::Release);
        self.locate_until = Some(now + LOCATE);
        self.next_sample = None;
    }

    pub(super) fn reenable(&mut self) {
        self.rejected_focus.store(false, Ordering::Release);
    }

    pub(super) fn visible(&self) -> bool {
        self.live.is_some()
    }

    fn close(&mut self, ctx: &egui::Context) {
        if let Some((id, live)) = self.live.take() {
            live.store(false, Ordering::Release);
            ctx.send_viewport_cmd_to(id, egui::ViewportCommand::Visible(false));
        }
        self.window_origin = None;
    }

    pub(super) fn clear(&mut self, ctx: &egui::Context) {
        self.close(ctx);
        self.locate_until = None;
        self.next_sample = None;
        self.last = None;
        self.trail.clear();
    }

    fn observe(&mut self, now: Instant, desktop: Option<[f64; 2]>, trail: bool) {
        let Some(desktop) = desktop.filter(|point| point.iter().all(|n| n.is_finite())) else {
            self.last = None;
            self.trail.clear();
            return;
        };
        let sample = Sample { desktop, at: now };
        if trail && self.last.is_some_and(|last| last.desktop != desktop) {
            if self.trail.len() == MAX_SAMPLES {
                self.trail.pop_front();
            }
            self.trail.push_back(sample);
        }
        self.last = Some(sample);
    }

    fn expire(&mut self, now: Instant, trail: bool) {
        if self.locate_until.is_some_and(|end| now >= end) {
            self.locate_until = None;
        }
        if trail {
            self.trail
                .retain(|sample| now.duration_since(sample.at) < TRAIL);
        } else {
            self.trail.clear();
        }
    }

    pub(super) fn ui(&mut self, ctx: &egui::Context, trail: bool) {
        if ctx.embed_viewports()
            || self.rejected_focus.load(Ordering::Acquire)
            || ctx.input(|input| input.viewport().minimized == Some(true))
        {
            self.clear(ctx);
            return;
        }
        let now = Instant::now();
        self.expire(now, trail);
        if self.locate_until.is_none() && !trail {
            self.clear(ctx);
            return;
        }
        if self.next_sample.is_none_or(|next| now >= next) {
            self.observe(now, peerglide_platform::cursor_position(), trail);
            self.next_sample = Some(now + INTERVAL);
        }
        let next = self.next_sample.unwrap_or(now + INTERVAL);
        let repaint = self.locate_until.map_or(next, |end| end.min(next));
        ctx.request_repaint_after(repaint.saturating_duration_since(now));
        let Some(sample) = self.last else {
            self.close(ctx);
            return;
        };
        if self.locate_until.is_none() && self.trail.is_empty() {
            self.close(ctx);
            return;
        }
        let Some(point) = points(
            sample.desktop,
            cfg!(windows),
            ctx.pixels_per_point(),
            ctx.zoom_factor(),
        ) else {
            self.close(ctx);
            return;
        };
        let origin = *self
            .window_origin
            .get_or_insert_with(|| point - egui::Vec2::splat(SIDE / 2.0));
        let generation = &mut self.generation;
        let (id, live) = self
            .live
            .get_or_insert_with(|| {
                *generation = generation.wrapping_add(1);
                (
                    egui::ViewportId::from_hash_of(("linko-cursor-overlay", *generation)),
                    Arc::new(AtomicBool::new(true)),
                )
            })
            .clone();
        let rejected_focus = self.rejected_focus.clone();
        let locate = self.locate_until;
        let trail = self.trail.clone();
        ctx.show_viewport_deferred(id, viewport(origin), move |ui, class| {
            if class != egui::ViewportClass::Deferred || !live.load(Ordering::Acquire) {
                return;
            }
            let info = ui.ctx().input(|input| input.viewport().clone());
            if info.focused == Some(true) || info.close_requested() {
                rejected_focus.store(true, Ordering::Release);
                live.store(false, Ordering::Release);
                ui.ctx()
                    .send_viewport_cmd(egui::ViewportCommand::Visible(false));
                ui.ctx().request_repaint_of(egui::ViewportId::ROOT);
                return;
            }
            let Some(inner) = info.inner_rect else {
                return;
            };
            let convert = |desktop| {
                points(
                    desktop,
                    cfg!(windows),
                    ui.ctx().pixels_per_point(),
                    ui.ctx().zoom_factor(),
                )
            };
            let Some(point) = convert(sample.desktop) else {
                return;
            };
            if !inner.shrink(38.0).contains(point) {
                let outer = info.outer_rect.unwrap_or(inner);
                ui.ctx()
                    .send_viewport_cmd(egui::ViewportCommand::OuterPosition(
                        outer.min + (point - inner.center()),
                    ));
            }
            let painter = ui.painter().with_clip_rect(ui.max_rect());
            let to_local = |point: egui::Pos2| ui.max_rect().min + (point - inner.min);
            let now = Instant::now();
            for dot in &trail {
                let age = now.saturating_duration_since(dot.at);
                if age >= TRAIL {
                    continue;
                }
                let Some(point) = convert(dot.desktop) else {
                    continue;
                };
                let alpha = ((1.0 - age.as_secs_f32() / TRAIL.as_secs_f32()) * 210.0) as u8;
                painter.circle_filled(
                    to_local(point),
                    5.0,
                    egui::Color32::from_rgba_unmultiplied(56, 189, 248, alpha),
                );
            }
            if locate.is_some_and(|end| now < end) {
                let center = to_local(point);
                painter.circle_stroke(
                    center,
                    24.0,
                    egui::Stroke::new(6.0, egui::Color32::from_black_alpha(220)),
                );
                painter.circle_stroke(center, 24.0, egui::Stroke::new(3.0, egui::Color32::WHITE));
            }
        });
    }
}

fn viewport(origin: egui::Pos2) -> egui::ViewportBuilder {
    egui::ViewportBuilder::default()
        .with_title("屏跃光标提示")
        .with_position(origin)
        .with_inner_size([SIDE, SIDE])
        .with_resizable(false)
        .with_decorations(false)
        .with_has_shadow(false)
        .with_taskbar(false)
        .with_transparent(true)
        .with_active(false)
        .with_mouse_passthrough(true)
        .with_always_on_top()
}

fn points(
    [x, y]: [f64; 2],
    physical: bool,
    pixels_per_point: f32,
    zoom: f32,
) -> Option<egui::Pos2> {
    let scale = if physical { pixels_per_point } else { zoom };
    let position = egui::pos2(x as f32 / scale, y as f32 / scale);
    (scale.is_finite() && scale > 0.0 && position.x.is_finite() && position.y.is_finite())
        .then_some(position)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_physical_and_macos_logical_coordinates_follow_the_viewport_scale() {
        assert_eq!(
            points([-300.0, 150.0], true, 1.5, 1.0),
            Some(egui::pos2(-200.0, 100.0))
        );
        assert_eq!(
            points([-300.0, 150.0], false, 3.0, 1.5),
            Some(egui::pos2(-200.0, 100.0))
        );
        assert!(points([0.0, 0.0], true, 0.0, 1.0).is_none());
    }

    #[test]
    fn trail_is_bounded_and_expires_and_locate_does_not_enable_trail() {
        let now = Instant::now();
        let mut overlay = Overlay::default();
        for i in 0..50 {
            overlay.observe(now, Some([f64::from(i), 0.0]), true);
        }
        assert_eq!(overlay.trail.len(), MAX_SAMPLES);
        overlay.locate(now);
        overlay.expire(now + TRAIL, true);
        assert!(overlay.trail.is_empty() && overlay.locate_until.is_some());
        overlay.observe(now + TRAIL, Some([0.0, 1.0]), false);
        assert!(overlay.trail.is_empty());
        overlay.expire(now + LOCATE, false);
        assert!(overlay.locate_until.is_none());
    }

    #[test]
    fn overlay_is_transparent_inactive_and_click_through() {
        let builder = viewport(egui::Pos2::ZERO);
        assert_eq!(builder.active, Some(false));
        assert_eq!(builder.mouse_passthrough, Some(true));
        assert_eq!(builder.transparent, Some(true));
        assert_eq!(builder.inner_size, Some(egui::Vec2::splat(SIDE)));
    }

    #[test]
    fn clearing_invalidates_the_previous_viewport_paint() {
        let live = Arc::new(AtomicBool::new(true));
        let mut overlay = Overlay {
            live: Some((egui::ViewportId::from_hash_of("old-overlay"), live.clone())),
            ..Overlay::default()
        };
        overlay.locate(Instant::now());
        overlay.clear(&egui::Context::default());
        assert!(!live.load(Ordering::Acquire));
        assert!(!overlay.visible() && overlay.locate_until.is_none());
    }
}
