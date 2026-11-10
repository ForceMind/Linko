//! Interface icons drawn with `Painter` strokes: no icon font, no emoji glyphs, no textures.
//!
//! Every icon paints into a caller supplied rectangle with a caller supplied color and uses
//! one stroke width. Geometry lives on a 16 unit grid, so the 16 and 20 point sizes of the
//! design share the same shapes. Icons keep one unit of empty margin inside their square.
//! Holding no state, nothing here schedules a repaint.
// Later slices paint these icons; until they do, unused items would fail `-D warnings`.
#![allow(dead_code)]

use crate::desktop_style::tokens;
use crate::motion::{cubic_out, polyline_prefix, staged_progress};
use eframe::egui::{self, Color32, Painter, Pos2, Rect, Stroke};
use std::f32::consts::{PI, TAU};

/// Stroke width in points shared by all icons.
pub(super) const STROKE_WIDTH: f32 = tokens::ICON_STROKE;
/// Number of grid units along one side of the icon square.
pub(super) const GRID_UNITS: f32 = 16.0;
/// Empty margin inside the icon square, in grid units.
pub(super) const INSET_UNITS: f32 = 1.0;
/// Rectangles with a shorter side than this are not painted, in points.
pub(super) const MIN_SIDE: f32 = 6.0;

/// Direction of an arrow icon.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Direction {
    Left,
    Right,
    Up,
    Down,
}

/// The icon set of this slice. The other icons of the design join as the slices that use
/// them land.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Icon {
    /// Display on a stand.
    Device,
    Lock,
    Check,
    Cross,
    Warning,
    Arrow(Direction),
    /// Two sliders, for settings.
    Sliders,
    Plus,
}

impl Icon {
    /// Paint into the largest square centered in `rect`. Rectangles that are not finite or
    /// are smaller than `MIN_SIDE` draw nothing.
    pub(super) fn paint(self, painter: &Painter, rect: Rect, color: Color32) {
        if !is_paintable(rect) {
            return;
        }
        let grid = Grid::new(rect);
        match self {
            Icon::Device => paint_device(painter, grid, color),
            Icon::Lock => paint_lock(painter, grid, color),
            Icon::Check => paint_check(painter, grid, color, 1.0),
            Icon::Cross => paint_cross(painter, grid, color, 1.0),
            Icon::Warning => paint_warning(painter, grid, color),
            Icon::Arrow(direction) => paint_arrow(painter, grid, color, direction),
            Icon::Sliders => paint_sliders(painter, grid, color),
            Icon::Plus => paint_plus(painter, grid, color),
        }
    }
}

/// A check mark that draws itself, `progress` from 0 to 1 (M9).
pub(super) fn paint_check_progress(painter: &Painter, rect: Rect, color: Color32, progress: f32) {
    if is_paintable(rect) {
        paint_check(painter, Grid::new(rect), color, progress);
    }
}

/// A cross whose two strokes draw one after the other, `progress` from 0 to 1 (M9).
pub(super) fn paint_cross_progress(painter: &Painter, rect: Rect, color: Color32, progress: f32) {
    if is_paintable(rect) {
        paint_cross(painter, Grid::new(rect), color, progress);
    }
}

/// True when `rect` is finite and large enough to hold an icon.
pub(super) fn is_paintable(rect: Rect) -> bool {
    let coordinates = [rect.min.x, rect.min.y, rect.max.x, rect.max.y];
    let finite = coordinates.iter().all(|value| value.is_finite());
    finite && rect.width() >= MIN_SIDE && rect.height() >= MIN_SIDE
}

/// Maps 16 unit grid coordinates into the square centered in a rectangle.
#[derive(Clone, Copy, Debug)]
struct Grid {
    origin: Pos2,
    unit: f32,
}

impl Grid {
    fn new(rect: Rect) -> Self {
        let side = rect.width().min(rect.height());
        let half = egui::vec2(side, side) * 0.5;
        Self {
            origin: rect.center() - half,
            unit: side / GRID_UNITS,
        }
    }

    fn at(self, x: f32, y: f32) -> Pos2 {
        self.origin + egui::vec2(x, y) * self.unit
    }
}

fn line_stroke(color: Color32) -> Stroke {
    Stroke::new(STROKE_WIDTH, color)
}

fn paint_path(painter: &Painter, points: Vec<Pos2>, color: Color32) {
    if points.len() >= 2 {
        painter.add(egui::Shape::line(points, line_stroke(color)));
    }
}

fn paint_device(painter: &Painter, grid: Grid, color: Color32) {
    let stroke = line_stroke(color);
    let screen = Rect::from_min_max(grid.at(1.5, 2.5), grid.at(14.5, 11.0));
    painter.rect_stroke(screen, 1.5, stroke, egui::StrokeKind::Inside);
    painter.line_segment([grid.at(8.0, 11.0), grid.at(8.0, 13.5)], stroke);
    painter.line_segment([grid.at(5.0, 13.5), grid.at(11.0, 13.5)], stroke);
}

fn paint_lock(painter: &Painter, grid: Grid, color: Color32) {
    let stroke = line_stroke(color);
    let body = Rect::from_min_max(grid.at(3.5, 7.0), grid.at(12.5, 14.0));
    painter.rect_stroke(body, 1.5, stroke, egui::StrokeKind::Inside);
    paint_path(painter, lock_shackle(grid), color);
    painter.circle_filled(grid.at(8.0, 10.5), 1.0, color);
}

fn paint_check(painter: &Painter, grid: Grid, color: Color32, progress: f32) {
    if progress <= 0.0 {
        return;
    }
    let visible = polyline_prefix(&check_points(grid), cubic_out(progress));
    paint_path(painter, visible, color);
}

fn paint_cross(painter: &Painter, grid: Grid, color: Color32, progress: f32) {
    if progress <= 0.0 {
        return;
    }
    for (stage, stroke_points) in cross_strokes(grid).into_iter().enumerate() {
        let drawn = cubic_out(staged_progress(progress, stage, 2));
        if drawn > 0.0 {
            paint_path(painter, polyline_prefix(&stroke_points, drawn), color);
        }
    }
}

fn paint_warning(painter: &Painter, grid: Grid, color: Color32) {
    let stroke = line_stroke(color);
    let outline = warning_triangle(grid).to_vec();
    painter.add(egui::Shape::closed_line(outline, stroke));
    painter.line_segment([grid.at(8.0, 6.5), grid.at(8.0, 10.0)], stroke);
    painter.circle_filled(grid.at(8.0, 12.0), 0.9, color);
}

fn paint_arrow(painter: &Painter, grid: Grid, color: Color32, direction: Direction) {
    let stroke = line_stroke(color);
    let (shaft, head) = arrow_geometry(grid, direction);
    painter.line_segment(shaft, stroke);
    paint_path(painter, head.to_vec(), color);
}

fn paint_sliders(painter: &Painter, grid: Grid, color: Color32) {
    let stroke = line_stroke(color);
    let radius = 1.8 * grid.unit;
    painter.line_segment([grid.at(2.0, 5.0), grid.at(8.2, 5.0)], stroke);
    painter.line_segment([grid.at(11.8, 5.0), grid.at(14.0, 5.0)], stroke);
    painter.circle_stroke(grid.at(10.0, 5.0), radius, stroke);
    painter.line_segment([grid.at(2.0, 11.0), grid.at(4.2, 11.0)], stroke);
    painter.line_segment([grid.at(7.8, 11.0), grid.at(14.0, 11.0)], stroke);
    painter.circle_stroke(grid.at(6.0, 11.0), radius, stroke);
}

fn paint_plus(painter: &Painter, grid: Grid, color: Color32) {
    let stroke = line_stroke(color);
    painter.line_segment([grid.at(8.0, 3.0), grid.at(8.0, 13.0)], stroke);
    painter.line_segment([grid.at(3.0, 8.0), grid.at(13.0, 8.0)], stroke);
}

/// Points on a circular arc from angle `start` to `end` (radians, screen coordinates).
fn arc_points(center: Pos2, radius: f32, start: f32, end: f32, segments: usize) -> Vec<Pos2> {
    let steps = segments.max(1);
    let mut points = Vec::with_capacity(steps + 1);
    for step in 0..=steps {
        let t = step as f32 / steps as f32;
        let angle = start * (1.0 - t) + end * t;
        points.push(center + egui::vec2(angle.cos(), angle.sin()) * radius);
    }
    points
}

fn lock_shackle(grid: Grid) -> Vec<Pos2> {
    let radius = 2.5 * grid.unit;
    let mut points = vec![grid.at(5.5, 7.0)];
    points.extend(arc_points(grid.at(8.0, 5.5), radius, PI, TAU, 8));
    points.push(grid.at(10.5, 7.0));
    points
}

fn check_points(grid: Grid) -> [Pos2; 3] {
    let start = grid.at(3.5, 8.5);
    let elbow = grid.at(6.5, 11.5);
    let end = grid.at(12.5, 4.5);
    [start, elbow, end]
}

fn cross_strokes(grid: Grid) -> [[Pos2; 2]; 2] {
    [
        [grid.at(4.0, 4.0), grid.at(12.0, 12.0)],
        [grid.at(12.0, 4.0), grid.at(4.0, 12.0)],
    ]
}

fn warning_triangle(grid: Grid) -> [Pos2; 3] {
    let top = grid.at(8.0, 2.0);
    let right = grid.at(14.5, 13.5);
    let left = grid.at(1.5, 13.5);
    [top, right, left]
}

/// Rotates a point of the right-pointing arrow about the grid center.
fn oriented(grid: Grid, direction: Direction, x: f32, y: f32) -> Pos2 {
    match direction {
        Direction::Right => grid.at(x, y),
        Direction::Down => grid.at(GRID_UNITS - y, x),
        Direction::Left => grid.at(GRID_UNITS - x, GRID_UNITS - y),
        Direction::Up => grid.at(y, GRID_UNITS - x),
    }
}

/// Shaft segment and head polyline of an arrow; the tip is the middle point of the head.
fn arrow_geometry(grid: Grid, direction: Direction) -> ([Pos2; 2], [Pos2; 3]) {
    let at = |x: f32, y: f32| oriented(grid, direction, x, y);
    let shaft = [at(2.5, 8.0), at(13.5, 8.0)];
    let head = [at(9.0, 3.5), at(13.5, 8.0), at(9.0, 12.5)];
    (shaft, head)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square() -> Rect {
        Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(20.0, 20.0))
    }

    fn assert_inside(points: &[Pos2]) {
        let bounds = square().expand(0.01);
        for point in points {
            assert!(bounds.contains(*point), "{point:?} outside {bounds:?}");
        }
    }

    fn inset_bounds() -> Rect {
        let margin = INSET_UNITS * (20.0 / GRID_UNITS);
        square().shrink(margin - 0.01)
    }

    const INK: Color32 = Color32::from_rgb(40, 60, 120);

    fn shapes_painted(mut draw: impl FnMut(&Painter)) -> usize {
        let ctx = egui::Context::default();
        let output = ctx.run_ui(egui::RawInput::default(), |ui| draw(ui.painter()));
        let count = output.shapes.len();
        output.drop_without_applying_deltas();
        count
    }

    fn every_icon() -> [Icon; 11] {
        [
            Icon::Device,
            Icon::Lock,
            Icon::Check,
            Icon::Cross,
            Icon::Warning,
            Icon::Arrow(Direction::Left),
            Icon::Arrow(Direction::Right),
            Icon::Arrow(Direction::Up),
            Icon::Arrow(Direction::Down),
            Icon::Sliders,
            Icon::Plus,
        ]
    }

    #[test]
    fn geometry_stays_inside_the_icon_square_and_margin() {
        let grid = Grid::new(square());
        let inset = inset_bounds();
        let (shaft, head) = arrow_geometry(grid, Direction::Right);
        let mut paths = vec![lock_shackle(grid)];
        paths.push(check_points(grid).to_vec());
        paths.push(warning_triangle(grid).to_vec());
        paths.push(shaft.to_vec());
        paths.push(head.to_vec());
        for stroke_points in cross_strokes(grid) {
            paths.push(stroke_points.to_vec());
        }
        for path in paths {
            assert_inside(&path);
            for point in path {
                assert!(inset.contains(point), "{point:?} leaves the margin");
            }
        }
    }

    #[test]
    fn grid_centers_a_square_inside_a_wide_rectangle() {
        let wide = Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(40.0, 16.0));
        let grid = Grid::new(wide);
        assert_eq!(grid.unit, 1.0);
        assert_eq!(grid.at(0.0, 0.0), egui::pos2(12.0, 0.0));
        assert_eq!(grid.at(16.0, 16.0), egui::pos2(28.0, 16.0));
    }

    #[test]
    fn arrows_point_in_their_direction() {
        let grid = Grid::new(square());
        let center = grid.at(8.0, 8.0);
        for direction in [
            Direction::Left,
            Direction::Right,
            Direction::Up,
            Direction::Down,
        ] {
            let (shaft, head) = arrow_geometry(grid, direction);
            let tip = head[1];
            let shaft_end = shaft[1];
            assert_eq!(tip, shaft_end);
            match direction {
                Direction::Left => assert!(tip.x < center.x && tip.y == center.y),
                Direction::Right => assert!(tip.x > center.x && tip.y == center.y),
                Direction::Up => assert!(tip.y < center.y && tip.x == center.x),
                Direction::Down => assert!(tip.y > center.y && tip.x == center.x),
            }
        }
    }

    #[test]
    fn lock_shackle_arches_over_the_body() {
        let grid = Grid::new(square());
        let shackle = lock_shackle(grid);
        let mut top = f32::MAX;
        for point in &shackle {
            top = top.min(point.y);
        }
        assert!(top < grid.at(8.0, 5.0).y);
        let leg_y = grid.at(0.0, 7.0).y;
        assert_eq!(shackle[0].y, leg_y);
        assert_eq!(shackle[shackle.len() - 1].y, leg_y);
    }

    #[test]
    fn every_icon_paints_shapes_in_a_valid_rectangle() {
        let baseline = shapes_painted(|_| ());
        for icon in every_icon() {
            let painted = shapes_painted(|painter| icon.paint(painter, square(), INK));
            assert!(painted > baseline, "{icon:?} painted nothing");
        }
    }

    #[test]
    fn degenerate_rectangles_paint_nothing() {
        let baseline = shapes_painted(|_| ());
        let tiny = Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(3.0, 20.0));
        let nan = Rect::from_min_size(egui::pos2(f32::NAN, 0.0), egui::vec2(20.0, 20.0));
        let far = Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(f32::INFINITY, 20.0));
        for rect in [tiny, nan, far, Rect::NOTHING] {
            assert!(!is_paintable(rect), "{rect:?}");
            let painted = shapes_painted(|painter| {
                for icon in every_icon() {
                    icon.paint(painter, rect, INK);
                }
                paint_check_progress(painter, rect, INK, 1.0);
                paint_cross_progress(painter, rect, INK, 1.0);
            });
            assert_eq!(painted, baseline, "{rect:?}");
        }
    }

    fn check_shapes(progress: f32) -> usize {
        shapes_painted(|painter| {
            paint_check_progress(painter, square(), INK, progress);
        })
    }

    fn cross_shapes(progress: f32) -> usize {
        shapes_painted(|painter| {
            paint_cross_progress(painter, square(), INK, progress);
        })
    }

    #[test]
    fn check_and_cross_draw_progressively() {
        let baseline = shapes_painted(|_| ());
        assert_eq!(check_shapes(0.0), baseline);
        assert_eq!(check_shapes(0.5), baseline + 1);
        assert_eq!(check_shapes(1.0), baseline + 1);
        assert_eq!(cross_shapes(0.0), baseline);
        assert_eq!(cross_shapes(0.25), baseline + 1);
        assert_eq!(cross_shapes(1.0), baseline + 2);
    }
}
