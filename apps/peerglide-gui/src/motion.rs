//! Motion primitives for the desktop shell (design spec M1-M12).
//!
//! Every animation here ends. Transitions driven by egui (`Context::animate_*`) are keyed by
//! a fixed `Id`, and egui repaints only while the eased value is strictly inside its range.
//! One-shot records store a start time and a fixed duration; their `tick` asks for a repaint
//! only while progress is below 1. The discrete spinner never asks for a repaint: it follows
//! beats that already exist. A duration of 0 jumps straight to the end state, which is the hook
//! for a later reduce-motion switch. No timers, threads, I/O or native queries live here.
// Later slices call these helpers; until they do, unused items would fail `-D warnings`.
#![allow(dead_code)]

use crate::desktop_style::tokens;
use eframe::egui;

/// Capsule scale at the start of the M5 change.
const CAPSULE_FROM_SCALE: f32 = 0.92;
/// Badge scale at the start of the M6 pop.
const BADGE_FROM_SCALE: f32 = 0.3;
/// Shake displacement in points at 20%, 40%, 60%, 80% and 100% of M8; it starts at 0.
const SHAKE_KNOTS_PX: [f32; 6] = [0.0, -7.0, 6.0, -4.0, 2.0, 0.0];

/// Identity easing.
pub(super) fn linear(t: f32) -> f32 {
    unit(t)
}

/// Clamp to 0..=1. NaN maps to 0.
pub(super) fn unit(t: f32) -> f32 {
    if t.is_nan() {
        return 0.0;
    }
    t.clamp(0.0, 1.0)
}

/// Linear blend that is exact at both ends.
pub(super) fn lerp(from: f32, to: f32, t: f32) -> f32 {
    from * (1.0 - t) + to * t
}

/// Fast start, gentle stop. Used by M3, M4, M5, M7 and M9.
pub(super) fn cubic_out(t: f32) -> f32 {
    let f = unit(t) - 1.0;
    f * f * f + 1.0
}

/// Ease out with a small overshoot (peak about 1.037). Used by M6.
pub(super) fn pop_out(t: f32) -> f32 {
    let f = unit(t) - 1.0;
    1.0 + 2.0 * f * f * f + f * f
}

/// Seconds after applying `scale`. NaN, infinity and negative results become 0, so a broken
/// setting can only make motion instant, never endless.
pub(super) fn scaled_secs(base: f32, scale: f32) -> f32 {
    let secs = base * scale;
    if !secs.is_finite() {
        return 0.0;
    }
    secs.max(0.0)
}

fn flag_value(on: bool) -> f32 {
    f32::from(u8::from(on))
}

/// One animation described by a start time and a fixed duration.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct OneShot {
    started_at: f64,
    secs: f32,
}

impl OneShot {
    /// Begin at `now` (egui time in seconds) and last `secs`.
    pub(super) fn start(now: f64, secs: f32) -> Self {
        Self {
            started_at: now,
            secs,
        }
    }

    /// An animation that is already over.
    pub(super) fn finished() -> Self {
        Self {
            started_at: f64::NEG_INFINITY,
            secs: 0.0,
        }
    }

    /// Progress in 0..=1. A zero, negative or non-finite duration is finished at once, and a
    /// clock that runs backwards reads 0.
    pub(super) fn progress(&self, now: f64) -> f32 {
        if !self.secs.is_finite() || self.secs <= 0.0 {
            return 1.0;
        }
        let elapsed = now - self.started_at;
        if !elapsed.is_finite() {
            return 1.0;
        }
        unit((elapsed / f64::from(self.secs)) as f32)
    }

    pub(super) fn is_active(&self, now: f64) -> bool {
        self.progress(now) < 1.0
    }

    /// Progress at the current egui time. Requests a repaint only while unfinished.
    pub(super) fn tick(&self, ctx: &egui::Context) -> f32 {
        let now = ctx.input(|input| input.time);
        let progress = self.progress(now);
        if progress < 1.0 {
            ctx.request_repaint();
        }
        progress
    }
}

/// Replays a one-shot whenever an observed key changes (M5, M6). The first observation only
/// records the key, so nothing animates on first paint or while the content stays the same.
#[derive(Clone, Debug)]
pub(super) struct Retrigger<K> {
    key: Option<K>,
    shot: OneShot,
}

impl<K> Default for Retrigger<K> {
    fn default() -> Self {
        Self {
            key: None,
            shot: OneShot::finished(),
        }
    }
}

impl<K: PartialEq> Retrigger<K> {
    /// Record `key`. Returns true when it differs from the previous one and a new animation
    /// of `secs` started at `now`.
    pub(super) fn observe(&mut self, key: K, now: f64, secs: f32) -> bool {
        let previous = self.key.as_ref();
        let changed = previous.is_some_and(|old| *old != key);
        if changed {
            self.shot = OneShot::start(now, secs);
        }
        self.key = Some(key);
        changed
    }

    pub(super) fn progress(&self, now: f64) -> f32 {
        self.shot.progress(now)
    }

    pub(super) fn tick(&self, ctx: &egui::Context) -> f32 {
        self.shot.tick(ctx)
    }
}

/// A scalar that glides toward a moving target with `cubic_out` (M7, M10). Retargeting in
/// flight starts from the current value, so nothing jumps.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Glide {
    from: f32,
    to: f32,
    shot: OneShot,
}

impl Glide {
    /// Rest at `value`.
    pub(super) fn at(value: f32) -> Self {
        Self {
            from: value,
            to: value,
            shot: OneShot::finished(),
        }
    }

    pub(super) fn target(&self) -> f32 {
        self.to
    }

    pub(super) fn value(&self, now: f64) -> f32 {
        self.blend(self.shot.progress(now))
    }

    /// Move toward `target` over `secs`. Ignores NaN and infinite targets and repeats of the
    /// current target.
    pub(super) fn set_target(&mut self, target: f32, now: f64, secs: f32) {
        if !target.is_finite() || target == self.to {
            return;
        }
        self.from = self.value(now);
        self.to = target;
        self.shot = OneShot::start(now, secs);
    }

    /// Current value at egui time. Requests a repaint only while gliding.
    pub(super) fn tick(&self, ctx: &egui::Context) -> f32 {
        self.blend(self.shot.tick(ctx))
    }

    fn blend(&self, progress: f32) -> f32 {
        lerp(self.from, self.to, cubic_out(progress))
    }
}

/// Eased 0..=1 transition toward `on`, driven by egui's animation table (M1, M2, M12).
/// `id` must be a fixed id for the control, never text that changes. egui repaints only while
/// the value is strictly between 0 and 1. With `secs` of 0 the result is the end value at once.
pub(super) fn animate_flag(ctx: &egui::Context, id: egui::Id, on: bool, secs: f32) -> f32 {
    let secs = scaled_secs(secs, 1.0);
    let eased = ctx.animate_bool_with_time_and_easing(id, on, secs, cubic_out);
    if secs > 0.0 {
        return eased;
    }
    flag_value(on)
}

/// Linear glide of a scalar toward `target`, driven by egui's animation table (M10). A
/// target change restarts from the current value; egui repaints only while it differs from
/// the target. With `secs` of 0 the result is `target` at once. Non-finite targets pass through
/// untouched, because a NaN target would never compare equal and would repaint forever.
pub(super) fn animate_scalar(ctx: &egui::Context, id: egui::Id, target: f32, secs: f32) -> f32 {
    if !target.is_finite() {
        return target;
    }
    let secs = scaled_secs(secs, 1.0);
    let value = ctx.animate_value_with_time(id, target, secs);
    if secs > 0.0 {
        return value;
    }
    target
}

/// M1: hover fill and outline, 80 ms.
pub(super) fn hover_mix(ctx: &egui::Context, id: egui::Id, hovered: bool) -> f32 {
    animate_flag(ctx, id, hovered, tokens::HOVER_SECS)
}

/// M2: selection fill, selection ring and focus ring, 120 ms.
pub(super) fn select_mix(ctx: &egui::Context, id: egui::Id, selected: bool) -> f32 {
    animate_flag(ctx, id, selected, tokens::SELECT_SECS)
}

/// Disclosure open amount, 160 ms.
pub(super) fn expand_amount(ctx: &egui::Context, id: egui::Id, open: bool) -> f32 {
    animate_flag(ctx, id, open, tokens::EXPAND_SECS)
}

/// M12: list item presence, 180 ms. Callers keep a leaving item alive until this reaches 0.
pub(super) fn presence_amount(ctx: &egui::Context, id: egui::Id, present: bool) -> f32 {
    animate_flag(ctx, id, present, tokens::LIST_PRESENCE_SECS)
}

/// M3: expansion state of the sidebar, 0 collapsed to 1 expanded, 200 ms, `cubic_out`.
pub(super) fn sidebar_mix(ctx: &egui::Context, id: egui::Id, expanded: bool) -> f32 {
    animate_flag(ctx, id, expanded, tokens::SIDEBAR_SECS)
}

/// Sidebar width in points for an expansion state from `sidebar_mix`.
pub(super) fn sidebar_width_for(mix: f32) -> f32 {
    lerp(
        tokens::SIDEBAR_COLLAPSED_W,
        tokens::SIDEBAR_EXPANDED_W,
        unit(mix),
    )
}

/// Opacity of sidebar labels: hidden until the sidebar is half open, so text is never squeezed.
pub(super) fn sidebar_label_alpha(mix: f32) -> f32 {
    unit((mix - 0.5) * 2.0)
}

/// M3: animated sidebar width in points.
pub(super) fn sidebar_width(ctx: &egui::Context, id: egui::Id, expanded: bool) -> f32 {
    sidebar_width_for(sidebar_mix(ctx, id, expanded))
}

/// M10: animated dialog height. Fixed-size dialogs do not use it.
pub(super) fn dialog_height(ctx: &egui::Context, id: egui::Id, target: f32) -> f32 {
    animate_scalar(ctx, id, target, tokens::RESIZE_SECS)
}

/// M4: horizontal offset of an arriving view, `distance` points at the start, 0 at the end.
pub(super) fn slide_offset(progress: f32, distance: f32) -> f32 {
    (1.0 - cubic_out(progress)) * distance
}

/// M4: opacity of an arriving view.
pub(super) fn fade_alpha(progress: f32) -> f32 {
    cubic_out(progress)
}

/// M5: scale of the status capsule while it changes, 0.92 to 1.
pub(super) fn capsule_scale(progress: f32) -> f32 {
    lerp(CAPSULE_FROM_SCALE, 1.0, cubic_out(progress))
}

/// M6: scale of a status badge while it pops in, 0.3 to 1 with a small overshoot.
pub(super) fn badge_scale(progress: f32) -> f32 {
    lerp(BADGE_FROM_SCALE, 1.0, pop_out(progress))
}

/// M8: horizontal shake in points. Five decaying points -7, +6, -4, +2, 0, joined linearly.
pub(super) fn shake_offset(progress: f32) -> f32 {
    let scaled = unit(progress) * 5.0;
    let index = (scaled.floor() as usize).min(4);
    let fraction = scaled - index as f32;
    let from = SHAKE_KNOTS_PX[index];
    let to = SHAKE_KNOTS_PX[index + 1];
    lerp(from, to, fraction)
}

/// M9: eased drawing progress of a stroke.
pub(super) fn stroke_progress(progress: f32) -> f32 {
    cubic_out(progress)
}

/// Progress of stage `stage` out of `stages` strokes drawn one after another. Each stroke
/// owns an equal share of the timeline.
pub(super) fn staged_progress(progress: f32, stage: usize, stages: usize) -> f32 {
    if stages == 0 {
        return 1.0;
    }
    let scaled = unit(progress) * stages as f32;
    unit(scaled - stage as f32)
}

/// The leading `fraction` (by length) of a polyline, for drawing strokes progressively.
/// A fraction of 0 or less collapses onto the first point.
pub(super) fn polyline_prefix(points: &[egui::Pos2], fraction: f32) -> Vec<egui::Pos2> {
    let fraction = unit(fraction);
    if points.len() < 2 || fraction >= 1.0 {
        return points.to_vec();
    }
    let mut total = 0.0;
    for pair in points.windows(2) {
        total += (pair[1] - pair[0]).length();
    }
    let mut remaining = total * fraction;
    let mut visible = vec![points[0]];
    for pair in points.windows(2) {
        let length = (pair[1] - pair[0]).length();
        if length <= remaining {
            visible.push(pair[1]);
            remaining -= length;
            continue;
        }
        if length > 0.0 {
            let t = remaining / length;
            let x = lerp(pair[0].x, pair[1].x, t);
            let y = lerp(pair[0].y, pair[1].y, t);
            visible.push(egui::pos2(x, y));
        }
        break;
    }
    visible
}

/// M11: index of the beat that contains `now`. Used only to read an existing repaint rhythm;
/// calling it never schedules anything, and it changes at most once per `period_secs`.
pub(super) fn beat_index(now: f64, period_secs: f32) -> u64 {
    if !now.is_finite() || now < 0.0 {
        return 0;
    }
    if !period_secs.is_finite() || period_secs <= 0.0 {
        return 0;
    }
    (now / f64::from(period_secs)).floor() as u64
}

/// M11: lit cell of an 8-cell spinner for a beat.
pub(super) fn spinner_head(beat: u64, cells: usize) -> usize {
    if cells == 0 {
        return 0;
    }
    (beat % cells as u64) as usize
}

/// M11: brightness of `cell` in 0..=1. The head is 1 and each cell behind it is dimmer.
pub(super) fn spinner_cell_strength(cell: usize, head: usize, cells: usize) -> f32 {
    if cells == 0 {
        return 0.0;
    }
    let behind = (head % cells + cells - cell % cells) % cells;
    1.0 - behind as f32 / cells as f32
}

/// M11: lit cell at egui time `now`, stepping every `SPINNER_STEP_SECS` at most.
pub(super) fn spinner_head_at(now: f64) -> usize {
    let beat = beat_index(now, tokens::SPINNER_STEP_SECS);
    spinner_head(beat, tokens::SPINNER_CELLS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    const FRAME_SECS: f64 = 1.0 / 60.0;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    /// A headless egui context that advances one 60 Hz frame per call.
    struct Clock {
        ctx: egui::Context,
        time: f64,
    }

    impl Clock {
        fn new() -> Self {
            Self {
                ctx: egui::Context::default(),
                time: 0.0,
            }
        }

        fn frame(&mut self, run: impl FnMut(&egui::Context)) {
            let input = egui::RawInput {
                time: Some(self.time),
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(400.0, 300.0),
                )),
                ..Default::default()
            };
            let output = self.ctx.run_pass(input, run);
            output.drop_without_applying_deltas();
            self.time += FRAME_SECS;
        }

        fn frames(&mut self, count: usize, mut run: impl FnMut(&egui::Context)) {
            for _ in 0..count {
                self.frame(&mut run);
            }
        }

        /// Count repaint requests whose delay is below `limit`.
        fn count_requests(&self, limit: Duration) -> Arc<AtomicUsize> {
            let requests = Arc::new(AtomicUsize::new(0));
            let observed = Arc::clone(&requests);
            self.ctx.set_request_repaint_callback(move |request| {
                if request.delay < limit {
                    observed.fetch_add(1, Ordering::SeqCst);
                }
            });
            requests
        }
    }

    /// Every animation path of this module, driven the way a screen would drive it.
    struct Scene {
        glide: Glide,
        capsule: Retrigger<u8>,
        shake: OneShot,
        check: OneShot,
        badge: OneShot,
        hover: f32,
        width: f32,
        height: f32,
        spinner: usize,
    }

    impl Scene {
        fn new() -> Self {
            let mut capsule = Retrigger::default();
            capsule.observe(1, 0.0, tokens::CAPSULE_SECS);
            Self {
                glide: Glide::at(0.0),
                capsule,
                shake: OneShot::finished(),
                check: OneShot::finished(),
                badge: OneShot::finished(),
                hover: 0.0,
                width: 0.0,
                height: 0.0,
                spinner: 0,
            }
        }

        /// A state change: every one-shot starts and the glide gets a new target.
        fn trigger(&mut self, now: f64) {
            self.glide.set_target(100.0, now, tokens::REBOUND_SECS);
            self.capsule.observe(2, now, tokens::CAPSULE_SECS);
            self.shake = OneShot::start(now, tokens::SHAKE_SECS);
            self.check = OneShot::start(now, tokens::STROKE_DRAW_SECS);
            self.badge = OneShot::start(now, tokens::BADGE_POP_SECS);
        }

        fn drive(&mut self, ctx: &egui::Context, on: bool) {
            let now = ctx.input(|input| input.time);
            self.hover = hover_mix(ctx, egui::Id::unique("scene_hover"), on);
            select_mix(ctx, egui::Id::unique("scene_select"), on);
            expand_amount(ctx, egui::Id::unique("scene_expand"), on);
            presence_amount(ctx, egui::Id::unique("scene_presence"), on);
            self.width = sidebar_width(ctx, egui::Id::unique("scene_sidebar"), on);
            let target = if on { 300.0 } else { 120.0 };
            self.height = dialog_height(ctx, egui::Id::unique("scene_height"), target);
            self.glide.tick(ctx);
            self.capsule.tick(ctx);
            self.shake.tick(ctx);
            self.check.tick(ctx);
            self.badge.tick(ctx);
            self.spinner = spinner_head_at(now);
        }
    }

    #[test]
    fn animations_settle_to_zero_repaint() {
        let mut clock = Clock::new();
        let mut scene = Scene::new();
        clock.frames(10, |ctx| scene.drive(ctx, false));
        let requests = clock.count_requests(Duration::MAX);

        clock.frame(|ctx| {
            let now = ctx.input(|input| input.time);
            scene.trigger(now);
            scene.drive(ctx, true);
        });
        clock.frames(100, |ctx| scene.drive(ctx, true));
        // Sensitivity: the animations above really did ask for frames.
        assert!(requests.load(Ordering::SeqCst) > 0);
        assert!(close(scene.hover, 1.0), "hover {}", scene.hover);
        assert!(close(scene.width, 240.0), "width {}", scene.width);
        assert!(close(scene.height, 300.0), "height {}", scene.height);

        // The last pending request is consumed by these frames; after that nothing is asked.
        clock.frames(10, |ctx| scene.drive(ctx, true));
        let before = requests.load(Ordering::SeqCst);
        clock.frames(120, |ctx| scene.drive(ctx, true));
        assert_eq!(requests.load(Ordering::SeqCst), before);

        // Reversing the transitions settles the same way.
        clock.frames(100, |ctx| scene.drive(ctx, false));
        clock.frames(10, |ctx| scene.drive(ctx, false));
        let before = requests.load(Ordering::SeqCst);
        clock.frames(120, |ctx| scene.drive(ctx, false));
        assert_eq!(requests.load(Ordering::SeqCst), before);
        assert!(close(scene.width, 64.0), "width {}", scene.width);
    }

    #[test]
    fn idle_state_has_no_periodic_repaint() {
        let mut clock = Clock::new();
        let mut scene = Scene::new();
        clock.frames(10, |ctx| scene.drive(ctx, false));
        let requests = clock.count_requests(Duration::from_secs(1));
        clock.frames(240, |ctx| scene.drive(ctx, false));
        assert_eq!(requests.load(Ordering::SeqCst), 0);
        // The spinner reads time only; it stepped without scheduling anything.
        assert!(scene.spinner < tokens::SPINNER_CELLS);
    }

    #[test]
    fn zero_duration_jumps_to_the_end() {
        let mut clock = Clock::new();
        let flag = egui::Id::unique("zero_flag");
        let scalar = egui::Id::unique("zero_scalar");
        let mut got = (0.0_f32, 0.0_f32);
        clock.frames(5, |ctx| {
            got = (
                animate_flag(ctx, flag, false, 0.0),
                animate_scalar(ctx, scalar, 10.0, 0.0),
            );
        });
        assert_eq!(got, (0.0, 10.0));
        let requests = clock.count_requests(Duration::MAX);
        clock.frame(|ctx| {
            got = (
                animate_flag(ctx, flag, true, 0.0),
                animate_scalar(ctx, scalar, 50.0, 0.0),
            );
        });
        assert_eq!(got, (1.0, 50.0));
        clock.frame(|ctx| {
            got = (
                animate_flag(ctx, flag, false, 0.0),
                animate_scalar(ctx, scalar, 10.0, 0.0),
            );
        });
        assert_eq!(got, (0.0, 10.0));
        assert_eq!(requests.load(Ordering::SeqCst), 0);

        let now = 5.0;
        assert_eq!(OneShot::start(now, 0.0).progress(now), 1.0);
        assert!(!OneShot::start(now, 0.0).is_active(now));
        let mut glide = Glide::at(1.0);
        glide.set_target(9.0, now, 0.0);
        assert_eq!(glide.value(now), 9.0);
        let mut capsule = Retrigger::default();
        capsule.observe(1, now, 0.0);
        assert!(capsule.observe(2, now, 0.0));
        assert_eq!(capsule.progress(now), 1.0);
        assert_eq!(scaled_secs(0.2, 0.0), 0.0);
    }

    #[test]
    fn easing_endpoints_and_monotonic_steps() {
        for ease in [linear, cubic_out] {
            assert_eq!(ease(0.0), 0.0);
            assert_eq!(ease(1.0), 1.0);
            let mut previous = 0.0;
            for step in 0..=100 {
                let value = ease(step as f32 / 100.0);
                assert!(value >= previous, "step {step}");
                previous = value;
            }
        }
        assert_eq!(pop_out(0.0), 0.0);
        assert_eq!(pop_out(1.0), 1.0);
        let mut peak = 0.0_f32;
        for step in 0..=100 {
            peak = peak.max(pop_out(step as f32 / 100.0));
        }
        assert!(peak > 1.0 && peak < 1.05, "peak {peak}");
    }

    #[test]
    fn unit_clamps_and_maps_nan_to_zero() {
        assert_eq!(unit(-3.0), 0.0);
        assert_eq!(unit(7.0), 1.0);
        assert_eq!(unit(0.25), 0.25);
        assert_eq!(unit(f32::NAN), 0.0);
        assert_eq!(cubic_out(f32::NAN), 0.0);
        assert!(close(lerp(2.0, 6.0, 0.5), 4.0));
        assert_eq!(lerp(0.1, 0.3, 1.0), 0.3);
        assert_eq!(lerp(0.1, 0.3, 0.0), 0.1);
    }

    #[test]
    fn scaled_secs_never_goes_negative_or_endless() {
        assert!(close(scaled_secs(0.2, 1.0), 0.2));
        assert!(close(scaled_secs(0.2, 0.5), 0.1));
        assert_eq!(scaled_secs(0.2, -1.0), 0.0);
        assert_eq!(scaled_secs(f32::NAN, 1.0), 0.0);
        assert_eq!(scaled_secs(f32::INFINITY, 1.0), 0.0);
    }

    #[test]
    fn one_shot_progress_is_clamped_and_finishes() {
        let shot = OneShot::start(10.0, 0.5);
        assert_eq!(shot.progress(9.0), 0.0);
        assert_eq!(shot.progress(10.0), 0.0);
        assert!(close(shot.progress(10.25), 0.5));
        assert_eq!(shot.progress(10.5), 1.0);
        assert_eq!(shot.progress(99.0), 1.0);
        assert!(shot.is_active(10.1));
        assert!(!shot.is_active(10.5));
        let mut previous = 0.0;
        for step in 0..=60 {
            let value = shot.progress(10.0 + f64::from(step) / 100.0);
            assert!(value >= previous);
            previous = value;
        }
    }

    #[test]
    fn invalid_durations_and_finished_shots_are_inactive() {
        for secs in [f32::NAN, f32::INFINITY, -1.0, 0.0] {
            assert_eq!(OneShot::start(1.0, secs).progress(1.0), 1.0);
        }
        assert_eq!(OneShot::finished().progress(0.0), 1.0);
        assert_eq!(OneShot::finished().progress(1.0e9), 1.0);
        assert!(!OneShot::finished().is_active(0.0));
    }

    #[test]
    fn retrigger_plays_only_when_the_key_changes() {
        let mut capsule = Retrigger::default();
        assert!(!capsule.observe("connected", 1.0, 0.18));
        assert_eq!(capsule.progress(1.0), 1.0);
        assert!(!capsule.observe("connected", 2.0, 0.18));
        assert!(capsule.observe("recovering", 3.0, 0.18));
        assert!(capsule.progress(3.05) < 1.0);
        assert_eq!(capsule.progress(3.5), 1.0);
        assert!(!capsule.observe("recovering", 4.0, 0.18));
        assert_eq!(capsule.progress(4.0), 1.0);
    }

    #[test]
    fn glide_retargets_from_the_current_value() {
        let mut glide = Glide::at(0.0);
        assert_eq!(glide.value(0.0), 0.0);
        glide.set_target(100.0, 0.0, 1.0);
        assert_eq!(glide.value(0.0), 0.0);
        assert!(close(glide.value(0.5), 87.5));
        assert_eq!(glide.value(1.0), 100.0);
        glide.set_target(0.0, 0.5, 1.0);
        assert!(close(glide.value(0.5), 87.5));
        assert_eq!(glide.value(1.5), 0.0);
        assert_eq!(glide.target(), 0.0);
        glide.set_target(f32::NAN, 2.0, 1.0);
        glide.set_target(f32::INFINITY, 2.0, 1.0);
        assert_eq!(glide.target(), 0.0);
    }

    #[test]
    fn shake_follows_the_five_decaying_points() {
        assert_eq!(shake_offset(0.0), 0.0);
        let expected = [-7.0, 6.0, -4.0, 2.0, 0.0];
        for (index, want) in expected.into_iter().enumerate() {
            let progress = (index + 1) as f32 / 5.0;
            let value = shake_offset(progress);
            assert!(close(value, want), "{progress}: {value}");
        }
        assert!(close(shake_offset(0.1), -3.5));
        assert_eq!(shake_offset(2.0), 0.0);
        assert!(shake_offset(0.2).abs() > shake_offset(0.4).abs());
        assert!(shake_offset(0.4).abs() > shake_offset(0.6).abs());
        assert!(shake_offset(0.6).abs() > shake_offset(0.8).abs());
    }

    #[test]
    fn view_capsule_and_badge_curves_hit_their_endpoints() {
        assert_eq!(slide_offset(0.0, 12.0), 12.0);
        assert_eq!(slide_offset(1.0, 12.0), 0.0);
        assert_eq!(fade_alpha(0.0), 0.0);
        assert_eq!(fade_alpha(1.0), 1.0);
        assert!(close(capsule_scale(0.0), 0.92));
        assert_eq!(capsule_scale(1.0), 1.0);
        assert!(close(badge_scale(0.0), 0.3));
        assert_eq!(badge_scale(1.0), 1.0);
        assert!(badge_scale(0.8) > 1.0);
        assert_eq!(stroke_progress(0.0), 0.0);
        assert_eq!(stroke_progress(1.0), 1.0);
    }

    #[test]
    fn sidebar_width_and_label_alpha_follow_the_mix() {
        assert_eq!(sidebar_width_for(0.0), tokens::SIDEBAR_COLLAPSED_W);
        assert_eq!(sidebar_width_for(1.0), tokens::SIDEBAR_EXPANDED_W);
        assert_eq!(sidebar_width_for(7.0), tokens::SIDEBAR_EXPANDED_W);
        assert_eq!(sidebar_label_alpha(0.0), 0.0);
        assert_eq!(sidebar_label_alpha(0.5), 0.0);
        assert!(close(sidebar_label_alpha(0.75), 0.5));
        assert_eq!(sidebar_label_alpha(1.0), 1.0);
    }

    #[test]
    fn staged_progress_splits_the_timeline() {
        assert!(close(staged_progress(0.25, 0, 2), 0.5));
        assert_eq!(staged_progress(0.25, 1, 2), 0.0);
        assert_eq!(staged_progress(0.5, 0, 2), 1.0);
        assert!(close(staged_progress(0.75, 1, 2), 0.5));
        assert_eq!(staged_progress(1.0, 1, 2), 1.0);
        assert_eq!(staged_progress(0.0, 0, 0), 1.0);
    }

    #[test]
    fn polyline_prefix_cuts_by_length() {
        let corner = [
            egui::pos2(0.0, 0.0),
            egui::pos2(10.0, 0.0),
            egui::pos2(10.0, 10.0),
        ];
        let quarter = polyline_prefix(&corner, 0.25);
        assert_eq!(quarter, vec![egui::pos2(0.0, 0.0), egui::pos2(5.0, 0.0)]);
        let three_quarters = polyline_prefix(&corner, 0.75);
        assert_eq!(three_quarters.len(), 3);
        assert_eq!(three_quarters[2], egui::pos2(10.0, 5.0));
        assert_eq!(polyline_prefix(&corner, 1.0), corner.to_vec());
        assert_eq!(polyline_prefix(&corner, 5.0), corner.to_vec());
        let none = polyline_prefix(&corner, 0.0);
        assert!(none.iter().all(|point| *point == corner[0]));
        assert_eq!(polyline_prefix(&corner[..1], 0.5), vec![corner[0]]);
        assert!(polyline_prefix(&[], 0.5).is_empty());
    }

    #[test]
    fn spinner_steps_by_beat_without_scheduling() {
        assert_eq!(beat_index(0.0, 0.1), 0);
        assert_eq!(beat_index(0.25, 0.1), 2);
        assert_eq!(beat_index(-1.0, 0.1), 0);
        assert_eq!(beat_index(1.0, 0.0), 0);
        assert_eq!(beat_index(f64::NAN, 0.1), 0);
        assert_eq!(spinner_head(9, 8), 1);
        assert_eq!(spinner_head(9, 0), 0);
        assert_eq!(spinner_head_at(0.0), 0);
        assert_eq!(spinner_head_at(0.35), 3);
        assert_eq!(spinner_cell_strength(3, 3, 8), 1.0);
        assert!(close(spinner_cell_strength(2, 3, 8), 0.875));
        assert!(close(spinner_cell_strength(4, 3, 8), 0.125));
        assert_eq!(spinner_cell_strength(0, 0, 0), 0.0);
        for cell in 0..8 {
            let strength = spinner_cell_strength(cell, 5, 8);
            assert!(strength > 0.0 && strength <= 1.0);
        }
    }
}
