use anyhow::{ensure, Context, Result};
use peerglide_protocol::Edge;

const MAX_ZONES: usize = 32;
const MAX_BARRIERS: usize = 64;

#[derive(Clone, Copy)]
pub(super) struct Zone {
    pub(super) origin: (i32, i32),
    end: (i32, i32),
}

impl Zone {
    pub(super) fn new(x: i32, y: i32, width: u32, height: u32) -> Result<Self> {
        ensure!(width != 0 && height != 0, "invalid Wayland capture zone size");
        Ok(Self {
            origin: (x, y),
            end: (
                x.checked_add(i32::try_from(width)?).context("invalid Wayland zone x extent")?,
                y.checked_add(i32::try_from(height)?).context("invalid Wayland zone y extent")?,
            ),
        })
    }

    fn overlaps(self, other: Self) -> bool {
        self.origin.0 < other.end.0
            && self.end.0 > other.origin.0
            && self.origin.1 < other.end.1
            && self.end.1 > other.origin.1
    }

    pub(super) fn size(self) -> (f64, f64) {
        (
            f64::from(self.end.0) - f64::from(self.origin.0),
            f64::from(self.end.1) - f64::from(self.origin.1),
        )
    }

    fn boundary(self, edge: Edge) -> (i32, (i32, i32)) {
        match edge {
            Edge::Left => (self.origin.0, (self.origin.1, self.end.1)),
            Edge::Right => (self.end.0, (self.origin.1, self.end.1)),
            Edge::Top => (self.origin.1, (self.origin.0, self.end.0)),
            Edge::Bottom => (self.end.1, (self.origin.0, self.end.0)),
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct Segment {
    pub(super) id: u32,
    pub(super) zone: Zone,
    pub(super) edge: Edge,
    coordinate: i32,
    // The along-edge interval is half-open. The portal receives inclusive pixels.
    span: (i32, i32),
}

impl Segment {
    pub(super) fn position(self) -> (i32, i32, i32, i32) {
        let (start, end) = self.span;
        match self.edge {
            Edge::Left | Edge::Right => (self.coordinate, start, self.coordinate, end - 1),
            Edge::Top | Edge::Bottom => (start, self.coordinate, end - 1, self.coordinate),
        }
    }

    pub(super) fn fraction(self, cursor: (f64, f64)) -> Result<f64> {
        ensure!(cursor.0.is_finite() && cursor.1.is_finite(), "invalid Wayland activation cursor");
        let (_, (start, end)) = self.zone.boundary(self.edge);
        let along = match self.edge {
            Edge::Left | Edge::Right => cursor.1,
            Edge::Top | Edge::Bottom => cursor.0,
        }.clamp(f64::from(self.span.0), f64::from(self.span.1 - 1));
        Ok((along - f64::from(start)) / (f64::from(end - 1) - f64::from(start)).max(1.0))
    }

    pub(super) fn restore(self, along: f64) -> (f64, f64) {
        let (x0, y0) = self.zone.origin;
        let (x1, y1) = self.zone.end;
        let (width, height) = self.zone.size();
        let x = f64::from(x0) + along.clamp(0.0, 1.0) * (width - 1.0);
        let y = f64::from(y0) + along.clamp(0.0, 1.0) * (height - 1.0);
        let inset_x = (width - 1.0).min(1.0);
        let inset_y = (height - 1.0).min(1.0);
        match self.edge {
            Edge::Left => (f64::from(x0) + inset_x, y),
            Edge::Right => (f64::from(x1 - 1) - inset_x, y),
            Edge::Top => (x, f64::from(y0) + inset_y),
            Edge::Bottom => (x, f64::from(y1 - 1) - inset_y),
        }
    }
}

pub(super) fn exposed(
    zones: &[Zone],
    edge: Edge,
    origin: Option<(i32, i32)>,
) -> Result<Vec<Segment>> {
    ensure!(!zones.is_empty() && zones.len() <= MAX_ZONES, "invalid number of Wayland capture zones");
    let mut segments = Vec::new();
    for (index, zone) in zones.iter().copied().enumerate() {
        ensure!(
            zones[..index].iter().all(|other| !zone.overlaps(*other)),
            "Wayland capture zones overlap"
        );
        if origin.is_some_and(|origin| origin != zone.origin) {
            continue;
        }
        let (coordinate, (start, end)) = zone.boundary(edge);
        let mut blockers: Vec<_> = zones
            .iter()
            .map(|other| other.boundary(edge.opposite()))
            .filter(|(boundary, _)| *boundary == coordinate)
            .filter_map(|(_, (low, high))| {
                let low = low.max(start);
                let high = high.min(end);
                (low < high).then_some((low, high))
            })
            .collect();
        blockers.sort_unstable();
        let mut cursor = start;
        for (low, high) in blockers.into_iter().chain(std::iter::once((end, end))) {
            if cursor < low {
                ensure!(segments.len() < MAX_BARRIERS, "too many exposed Wayland barriers");
                segments.push(Segment {
                    id: (segments.len() + 1) as u32,
                    zone,
                    edge,
                    coordinate,
                    span: (cursor, low),
                });
            }
            cursor = cursor.max(high);
        }
    }
    ensure!(!segments.is_empty(), "the requested Wayland edge has no exposed barrier");
    Ok(segments)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portal_barriers_follow_exposed_pixels_and_preserve_signed_origins() -> Result<()> {
        let zones = [Zone::new(-100, -20, 100, 80)?, Zone::new(0, 0, 120, 30)?];
        let segments = exposed(&zones, Edge::Right, Some((-100, -20)))?;
        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0].position(), (0, -20, 0, -1));
        assert_eq!(segments[1].position(), (0, 30, 0, 59));
        assert_eq!(segments[0].fraction((20.0, -21.0))?, 0.0);
        assert_eq!(segments[1].fraction((20.0, 99.0))?, 1.0);
        assert_eq!(segments[1].restore(1.0), (-2.0, 59.0));
        assert!(exposed(&zones, Edge::Left, Some((0, 0))).is_err());
        assert!(exposed(&[zones[0], zones[0]], Edge::Left, None).is_err());
        assert!(Zone::new(i32::MAX, 0, 2, 1).is_err());
        Ok(())
    }
}
