//! Local display geometry in the platform's oriented desktop coordinates.
use crate::ReceivePoint;
use peerglide_protocol::Edge;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct DisplayId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bounds {
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
}

impl Bounds {
    fn valid(self) -> bool {
        [self.left, self.top, self.right, self.bottom]
            .iter()
            .all(|coordinate| coordinate.is_finite())
            && positive(self.right - self.left)
            && positive(self.bottom - self.top)
    }

    fn contains(self, x: f64, y: f64) -> bool {
        in_span(x, self.left, self.right) && in_span(y, self.top, self.bottom)
    }

    fn distance(self, edge: Edge, x: f64, y: f64) -> f64 {
        match edge {
            Edge::Left => x - self.left,
            Edge::Right => self.right - x,
            Edge::Top => y - self.top,
            Edge::Bottom => self.bottom - y,
        }
    }

    fn along(self, edge: Edge, x: f64, y: f64) -> f64 {
        match edge {
            Edge::Left | Edge::Right => (y - self.top) / (self.bottom - self.top),
            Edge::Top | Edge::Bottom => (x - self.left) / (self.right - self.left),
        }
    }

    fn covers_outside(self, source: Bounds, edge: Edge, x: f64, y: f64) -> bool {
        match edge {
            Edge::Left => {
                self.left < source.left
                    && self.right >= source.left
                    && in_span(y, self.top, self.bottom)
            }
            Edge::Right => {
                self.left <= source.right
                    && self.right > source.right
                    && in_span(y, self.top, self.bottom)
            }
            Edge::Top => {
                self.top < source.top
                    && self.bottom >= source.top
                    && in_span(x, self.left, self.right)
            }
            Edge::Bottom => {
                self.top <= source.bottom
                    && self.bottom > source.bottom
                    && in_span(x, self.left, self.right)
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rotation {
    Upright,
    Clockwise90,
    Clockwise180,
    Clockwise270,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Display {
    pub id: DisplayId,
    /// Already oriented in desktop coordinates, including any negative origin.
    pub bounds: Bounds,
    /// Native backing or desktop UI scale. Bounds already use oriented coordinates.
    pub scale: f64,
    pub rotation: Rotation,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayoutError {
    PrimaryMissing,
    DuplicateDisplay,
    InvalidGeometry,
    GenerationExhausted,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EdgeAnchor {
    instance: u64,
    generation: u64,
    pub display: DisplayId,
    pub edge: Edge,
    pub fraction: f64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Revision {
    instance: u64,
    generation: u64,
}

/// One receiver-selected display in an exact local topology revision.
#[derive(Clone, Copy)]
pub struct PlacementTarget {
    revision: Revision,
    display: Display,
    desktop: Bounds,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    pub desktop_x: f64,
    pub desktop_y: f64,
    /// Win32 absolute coordinates when the virtual desktop flag is used.
    pub virtual_absolute: [i32; 2],
}

/// A validated native snapshot owned by one local layout controller. An update
/// only invalidates anchors when the effective layout or display metrics change.
pub struct Layout {
    primary: DisplayId,
    displays: Vec<Display>,
    instance: u64,
    generation: u64,
}

impl Layout {
    pub fn new(primary: DisplayId, displays: Vec<Display>) -> Result<Self, LayoutError> {
        let displays = validated(primary, displays)?;
        let instance = next_instance()?;
        Ok(Self {
            primary,
            displays,
            instance,
            generation: 1,
        })
    }

    pub fn replace(
        &mut self,
        primary: DisplayId,
        displays: Vec<Display>,
    ) -> Result<bool, LayoutError> {
        let displays = validated(primary, displays)?;
        if self.primary == primary && self.displays == displays {
            return Ok(false);
        }
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or(LayoutError::GenerationExhausted)?;
        self.primary = primary;
        self.displays = displays;
        Ok(true)
    }

    pub fn displays(&self) -> &[Display] {
        &self.displays
    }

    pub fn primary(&self) -> DisplayId {
        self.primary
    }

    pub fn placement_target(&self, id: DisplayId) -> Option<PlacementTarget> {
        let display = *self.displays.iter().find(|display| display.id == id)?;
        let bounds = display.bounds;
        let desktop = self.displays.iter().fold(bounds, |outer, display| Bounds {
            left: outer.left.min(display.bounds.left),
            top: outer.top.min(display.bounds.top),
            right: outer.right.max(display.bounds.right),
            bottom: outer.bottom.max(display.bounds.bottom),
        });
        (desktop.valid() && bounds.right - bounds.left >= 1.0 && bounds.bottom - bounds.top >= 1.0)
            .then_some(PlacementTarget {
                revision: self.revision(),
                display,
                desktop,
            })
    }

    /// Translate a v5 position within the selected display. The previous
    /// snapshot cannot be used after topology changes or with another owner.
    pub fn place(&self, target: PlacementTarget, point: ReceivePoint) -> Option<Placement> {
        if target.revision != self.revision() || !point.valid() {
            return None;
        }
        let bounds = target.display.bounds;
        let x = bounds.left + point.x * (bounds.right - bounds.left - 1.0);
        let y = bounds.top + point.y * (bounds.bottom - bounds.top - 1.0);
        let desktop = target.desktop;
        bounds.contains(x, y).then(|| Placement {
            desktop_x: x,
            desktop_y: y,
            virtual_absolute: [
                absolute(x, desktop.left, desktop.right),
                absolute(y, desktop.top, desktop.bottom),
            ],
        })
    }

    /// Prefer the primary when regions overlap, then the lowest display ID.
    pub fn display_at(&self, x: f64, y: f64) -> Option<DisplayId> {
        self.displays
            .iter()
            .filter(|display| display.bounds.contains(x, y))
            .min_by_key(|display| (display.id != self.primary, display.id))
            .map(|display| display.id)
    }

    /// Only an exposed part of the edge can become an inter-device anchor.
    /// A touching monitor blocks its overlapping span even if scales differ.
    pub fn edge_at(&self, edge: Edge, x: f64, y: f64, width: f64) -> Option<EdgeAnchor> {
        if !positive(width) {
            return None;
        }
        let id = self.display_at(x, y)?;
        let display = self.displays.iter().find(|display| display.id == id)?;
        if display.bounds.distance(edge, x, y) > width
            || self.displays.iter().any(|other| {
                other.id != id && other.bounds.covers_outside(display.bounds, edge, x, y)
            })
        {
            return None;
        }
        Some(EdgeAnchor {
            instance: self.instance,
            generation: self.generation,
            display: id,
            edge,
            fraction: display.bounds.along(edge, x, y).clamp(0.0, 1.0),
        })
    }

    pub(super) fn revision(&self) -> Revision {
        Revision {
            instance: self.instance,
            generation: self.generation,
        }
    }

    pub fn anchor_is_current(&self, anchor: EdgeAnchor) -> bool {
        anchor.instance == self.instance && anchor.generation == self.generation
    }
}

fn absolute(value: f64, start: f64, end: f64) -> i32 {
    let range = end - start - 1.0;
    if range <= 0.0 {
        return 0;
    }
    (((value - start) / range).clamp(0.0, 1.0) * 65535.0).round() as i32
}

fn next_instance() -> Result<u64, LayoutError> {
    static NEXT_INSTANCE: AtomicU64 = AtomicU64::new(1);
    let mut current = NEXT_INSTANCE.load(Ordering::Relaxed);
    loop {
        let next = current
            .checked_add(1)
            .ok_or(LayoutError::GenerationExhausted)?;
        let exchanged = NEXT_INSTANCE.compare_exchange_weak(
            current,
            next,
            Ordering::Relaxed,
            Ordering::Relaxed,
        );
        match exchanged {
            Ok(id) => return Ok(id),
            Err(actual) => current = actual,
        }
    }
}

fn validated(primary: DisplayId, mut displays: Vec<Display>) -> Result<Vec<Display>, LayoutError> {
    displays.sort_unstable_by_key(|display| display.id);
    if displays
        .iter()
        .any(|display| !display.bounds.valid() || !positive(display.scale))
    {
        return Err(LayoutError::InvalidGeometry);
    }
    if displays.windows(2).any(|pair| pair[0].id == pair[1].id) {
        return Err(LayoutError::DuplicateDisplay);
    }
    if !displays.iter().any(|display| display.id == primary) {
        return Err(LayoutError::PrimaryMissing);
    }
    Ok(displays)
}

fn in_span(value: f64, start: f64, end: f64) -> bool {
    value >= start && value < end
}

fn positive(value: f64) -> bool {
    value.is_finite() && value > 0.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn display(id: u64, bounds: [f64; 4], scale: f64) -> Display {
        let [left, top, right, bottom] = bounds;
        Display {
            id: DisplayId(id),
            bounds: Bounds {
                left,
                top,
                right,
                bottom,
            },
            scale,
            rotation: Rotation::Upright,
        }
    }

    #[test]
    fn place_on_a_negative_display_rejects_a_stale_revision() {
        let primary = display(1, [0.0, 0.0, 1920.0, 1080.0], 1.0);
        let mut left = display(2, [-1080.0, -200.0, 0.0, 1720.0], 1.5);
        left.rotation = Rotation::Clockwise90;
        let mut layout = Layout::new(primary.id, vec![primary, left]).unwrap();
        let target = layout.placement_target(left.id).unwrap();
        let start = ReceivePoint { x: 0.0, y: 0.0 };
        let end = ReceivePoint { x: 1.0, y: 1.0 };
        let first = layout.place(target, start).unwrap();
        assert_eq!((first.desktop_x, first.desktop_y), (-1080.0, -200.0));
        assert_eq!(first.virtual_absolute, [0, 0]);
        let last = layout.place(target, end).unwrap();
        assert_eq!((last.desktop_x, last.desktop_y), (-1.0, 1719.0));
        assert_eq!(last.virtual_absolute[1], 65535);
        assert!(last.virtual_absolute[0] < 32768);
        assert!(layout
            .place(
                target,
                ReceivePoint {
                    x: f64::NAN,
                    y: 0.0
                }
            )
            .is_none());
        let other = Layout::new(primary.id, vec![primary, left]).unwrap();
        assert!(other.place(target, start).is_none());
        left.scale = 2.0;
        assert!(layout.replace(primary.id, vec![primary, left]).unwrap());
        assert!(layout.place(target, start).is_none());
        assert!(layout.placement_target(DisplayId(3)).is_none());
    }

    #[test]
    fn touching_monitor_blocks_only_the_shared_edge_span() {
        let primary = display(3, [0.0, 0.0, 100.0, 100.0], 1.0);
        let left = display(1, [-80.0, -20.0, 0.0, 50.0], 2.0);
        let lower = display(7, [20.0, 100.0, 70.0, 150.0], 1.5);
        let layout = Layout::new(primary.id, vec![primary, left, lower]).unwrap();

        assert_eq!(layout.display_at(-5.0, -10.0), Some(left.id));
        assert_eq!(layout.display_at(0.0, 20.0), Some(primary.id));
        assert_eq!(layout.display_at(-5.0, 90.0), None);
        assert!(layout.edge_at(Edge::Right, -0.5, 30.0, 2.0).is_none());
        assert!(layout.edge_at(Edge::Left, 0.5, 30.0, 2.0).is_none());
        let exposed = layout.edge_at(Edge::Left, 0.5, 70.0, 2.0).unwrap();
        assert_eq!(exposed.display, primary.id);
        assert!((exposed.fraction - 0.7).abs() < 1e-12);
        assert!(layout.edge_at(Edge::Bottom, 40.0, 99.5, 2.0).is_none());
        assert!(layout.edge_at(Edge::Bottom, 80.0, 99.5, 2.0).is_some());
        assert!(layout.edge_at(Edge::Top, 1.0, 0.5, 2.0).is_some());
        assert!(layout.edge_at(Edge::Top, 1.0, f64::NAN, 2.0).is_none());
    }

    #[test]
    fn overlap_uses_primary_and_mirrors_do_not_close_its_outer_edge() {
        let first = display(1, [-100.0, -100.0, 0.0, 0.0], 1.0);
        let mirrored = display(2, [-100.0, -100.0, 0.0, 0.0], 2.0);
        let mut next = display(3, [5.0, -100.0, 75.0, 0.0], 1.0);
        next.rotation = Rotation::Clockwise90;
        let layout = Layout::new(mirrored.id, vec![first, mirrored, next]).unwrap();
        assert_eq!(layout.display_at(-1.0, -90.0), Some(mirrored.id));
        assert_eq!(layout.display_at(0.0, -90.0), None);
        assert_eq!(layout.display_at(6.0, -90.0), Some(next.id));
        assert!(layout.edge_at(Edge::Right, -1.0, -90.0, 2.0).is_some());
    }

    #[test]
    fn valid_metric_changes_invalidate_anchors_and_invalid_updates_keep_the_old_layout() {
        let primary = display(1, [0.0, 0.0, 100.0, 100.0], 1.0);
        let second = display(2, [120.0, 0.0, 200.0, 80.0], 1.0);
        let mut layout = Layout::new(primary.id, vec![primary, second]).unwrap();
        let anchor = layout.edge_at(Edge::Right, 99.5, 20.0, 2.0).unwrap();
        let other = Layout::new(primary.id, vec![primary, second]).unwrap();
        assert!(!other.anchor_is_current(anchor));
        assert!(!layout.replace(primary.id, vec![second, primary]).unwrap());
        assert!(layout.anchor_is_current(anchor));
        assert_eq!(
            layout.replace(primary.id, vec![second, second]),
            Err(LayoutError::DuplicateDisplay)
        );
        assert!(layout.anchor_is_current(anchor));
        assert!(layout
            .replace(
                primary.id,
                vec![
                    Display {
                        scale: 2.0,
                        ..primary
                    },
                    second
                ]
            )
            .unwrap());
        assert!(!layout.anchor_is_current(anchor));
    }
}
