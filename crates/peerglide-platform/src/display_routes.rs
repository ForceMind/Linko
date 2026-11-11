//! Local, bounded edge-to-peer routing over a current display snapshot.
use crate::display_layout::{DisplayId, EdgeAnchor, Layout, Revision};
use peerglide_protocol::Edge;

const MAX_LINKS: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Span {
    pub start: f64,
    pub end: f64,
}

impl Span {
    fn valid(self) -> bool {
        self.start >= 0.0 && self.end <= 1.0 && self.start < self.end
    }

    fn contains(self, fraction: f64) -> bool {
        fraction >= self.start && (fraction < self.end || (self.end == 1.0 && fraction == self.end))
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Target {
    /// Digest of the exact previously enrolled public certificate.
    pub peer: [u8; 32],
    /// None uses the compatible v5 primary display. An explicit remote
    /// display still requires sidecar negotiation and the peer's consent.
    pub display: Option<DisplayId>,
    pub edge: Edge,
}

#[derive(Clone, Copy)]
pub struct Link {
    pub source_display: DisplayId,
    pub source_edge: Edge,
    pub source_span: Span,
    pub target: Target,
    pub target_span: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Capacity,
    UnknownSource,
    InvalidSpan,
    InvalidPeer,
    Overlap,
}

pub struct Route {
    pub target: Target,
    pub fraction: f64,
}

pub struct Table {
    revision: Revision,
    links: Vec<Link>,
}

impl Table {
    pub fn new(layout: &Layout, mut links: Vec<Link>) -> Result<Self, Error> {
        if links.len() > MAX_LINKS {
            return Err(Error::Capacity);
        }
        for link in &links {
            if !link.source_span.valid() || !link.target_span.valid() {
                return Err(Error::InvalidSpan);
            }
            if link.target.peer == [0; 32] {
                return Err(Error::InvalidPeer);
            }
            if !layout
                .displays()
                .iter()
                .any(|display| display.id == link.source_display)
            {
                return Err(Error::UnknownSource);
            }
        }
        links.sort_unstable_by(|left, right| {
            (left.source_display, edge_order(left.source_edge))
                .cmp(&(right.source_display, edge_order(right.source_edge)))
                .then_with(|| left.source_span.start.total_cmp(&right.source_span.start))
        });
        if links.windows(2).any(|pair| {
            pair[0].source_display == pair[1].source_display
                && pair[0].source_edge == pair[1].source_edge
                && pair[0].source_span.end > pair[1].source_span.start
        }) {
            return Err(Error::Overlap);
        }
        Ok(Self {
            revision: layout.revision(),
            links,
        })
    }

    pub fn resolve(&self, layout: &Layout, anchor: EdgeAnchor) -> Option<Route> {
        if self.revision != layout.revision() || !layout.anchor_is_current(anchor) {
            return None;
        }
        let link = self.links.iter().find(|link| {
            link.source_display == anchor.display
                && link.source_edge == anchor.edge
                && link.source_span.contains(anchor.fraction)
        })?;
        let unit = (anchor.fraction - link.source_span.start)
            / (link.source_span.end - link.source_span.start);
        let fraction =
            link.target_span.start + unit * (link.target_span.end - link.target_span.start);
        Some(Route {
            target: link.target,
            fraction: fraction.clamp(link.target_span.start, link.target_span.end),
        })
    }
}

fn edge_order(edge: Edge) -> u8 {
    match edge {
        Edge::Left => 0,
        Edge::Right => 1,
        Edge::Top => 2,
        Edge::Bottom => 3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display_layout::{Bounds, Display, Rotation};

    fn display(id: u64, left: f64, top: f64) -> Display {
        Display {
            id: DisplayId(id),
            bounds: Bounds {
                left,
                top,
                right: left + 100.0,
                bottom: top + 100.0,
            },
            scale: 1.5,
            rotation: Rotation::Upright,
        }
    }

    fn link(source: DisplayId, peer: u8, start: f64, end: f64) -> Link {
        Link {
            source_display: source,
            source_edge: Edge::Right,
            source_span: Span { start, end },
            target: Target {
                peer: [peer; 32],
                display: Some(DisplayId(u64::from(peer))),
                edge: Edge::Left,
            },
            target_span: Span {
                start: 0.25,
                end: 0.75,
            },
        }
    }

    #[test]
    fn different_peers_can_own_adjacent_segments_with_exact_boundary_selection() {
        let main = display(1, 0.0, 0.0);
        let upper = display(2, -140.0, -100.0);
        let layout = Layout::new(main.id, vec![main, upper]).unwrap();
        let table = Table::new(
            &layout,
            vec![link(upper.id, 2, 0.5, 1.0), link(upper.id, 1, 0.0, 0.5)],
        )
        .unwrap();
        let first = layout.edge_at(Edge::Right, -40.5, -75.0, 2.0).unwrap();
        let route = table.resolve(&layout, first).unwrap();
        assert_eq!(route.target.peer, [1; 32]);
        assert!((route.fraction - 0.5).abs() < 1e-12);
        let boundary = layout.edge_at(Edge::Right, -40.5, -50.0, 2.0).unwrap();
        let route = table.resolve(&layout, boundary).unwrap();
        assert_eq!(route.target.peer, [2; 32]);
        assert!((route.fraction - 0.25).abs() < 1e-12);
    }

    #[test]
    fn ambiguous_config_and_stale_layout_anchors_are_rejected() {
        let main = display(1, 0.0, 0.0);
        let mut layout = Layout::new(main.id, vec![main]).unwrap();
        assert_eq!(
            Table::new(
                &layout,
                vec![link(main.id, 1, 0.0, 0.8), link(main.id, 2, 0.5, 1.0)]
            )
            .err(),
            Some(Error::Overlap)
        );
        assert_eq!(
            Table::new(&layout, vec![link(main.id, 1, f64::NAN, 0.5)]).err(),
            Some(Error::InvalidSpan)
        );
        let table = Table::new(&layout, vec![link(main.id, 1, 0.0, 1.0)]).unwrap();
        let anchor = layout.edge_at(Edge::Right, 99.5, 50.0, 2.0).unwrap();
        assert!(table.resolve(&layout, anchor).is_some());
        layout.replace(main.id, vec![display(1, 1.0, 0.0)]).unwrap();
        assert!(table.resolve(&layout, anchor).is_none());
        let current = layout.edge_at(Edge::Right, 100.5, 50.0, 2.0).unwrap();
        assert!(table.resolve(&layout, current).is_none());
    }
}
