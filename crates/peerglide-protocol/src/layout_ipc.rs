//! Finite layout messages on the private GUI/worker pipe.
use crate::Edge;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LayoutUpdate {
    version: u8,
    kind: Kind,
    pub request: u32,
    pub edge: Edge,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub applied: Option<bool>,
}

#[derive(Clone, Copy, Serialize, Deserialize)]
enum Kind {
    #[serde(rename = "layout")]
    Layout,
}

impl LayoutUpdate {
    pub fn request(request: u32, edge: Edge) -> Self {
        Self {
            version: 1,
            kind: Kind::Layout,
            request,
            edge,
            applied: None,
        }
    }

    pub fn reply(self, applied: bool) -> Self {
        Self {
            applied: Some(applied),
            ..self
        }
    }

    pub fn parse(line: &str) -> Option<Self> {
        if line.len() > 256 {
            return None;
        }
        serde_json::from_str::<Self>(line)
            .ok()
            .filter(|value| value.version == 1 && value.request != 0)
    }

    pub fn line(self) -> String {
        serde_json::to_string(&self).expect("finite layout IPC")
    }
}

/// Worker-to-GUI observation of the peer's configured direction versus this worker's.
/// A mismatched record lets the GUI offer an edit; a consistent one withdraws an earlier
/// offer. It changes no routing, grant or setting.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LayoutHint {
    version: u8,
    kind: HintKind,
    pub local: Edge,
    pub peer: Edge,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum HintKind {
    #[serde(rename = "layout_hint")]
    LayoutHint,
}

impl LayoutHint {
    pub fn observe(local: Edge, peer: Edge) -> Self {
        Self {
            version: 1,
            kind: HintKind::LayoutHint,
            local,
            peer,
        }
    }

    /// The two directions do not mirror each other.
    pub fn mismatched(self) -> bool {
        self.peer != self.local.opposite()
    }

    pub fn suggested(self) -> Edge {
        self.peer.opposite()
    }

    pub fn parse(line: &str) -> Option<Self> {
        if line.len() > 256 {
            return None;
        }
        serde_json::from_str::<Self>(line)
            .ok()
            .filter(|value| value.version == 1)
    }

    pub fn line(self) -> String {
        serde_json::to_string(&self).expect("finite layout hint IPC")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_layout_messages_require_known_schema_and_nonzero_request() {
        let command = LayoutUpdate::request(1, Edge::Right);
        assert!(LayoutUpdate::parse(&command.line())
            .unwrap()
            .applied
            .is_none());
        assert_eq!(
            LayoutUpdate::parse(&command.reply(false).line())
                .unwrap()
                .applied,
            Some(false)
        );
        assert!(LayoutUpdate::parse(&LayoutUpdate::request(0, Edge::Right).line()).is_none());
        assert!(
            LayoutUpdate::parse(&command.line().replace("\"version\":1", "\"version\":2"))
                .is_none()
        );
        assert!(LayoutUpdate::parse(&command.line().replace("\"Right\"", "\"Unknown\"")).is_none());
        assert!(LayoutUpdate::parse(&"x".repeat(257)).is_none());
    }

    #[test]
    fn layout_hint_round_trips_and_suggests_the_mirror_of_the_peer_edge() {
        let hint = LayoutHint::observe(Edge::Right, Edge::Right);
        assert!(hint.mismatched());
        assert_eq!(hint.suggested(), Edge::Left);
        assert_eq!(LayoutHint::parse(&hint.line()), Some(hint));
        assert_eq!(
            hint.line(),
            r#"{"version":1,"kind":"layout_hint","local":"Right","peer":"Right"}"#
        );
        assert!(!LayoutHint::observe(Edge::Right, Edge::Left).mismatched());
        assert!(!LayoutHint::observe(Edge::Top, Edge::Bottom).mismatched());
        let vertical = LayoutHint::observe(Edge::Top, Edge::Left);
        assert!(vertical.mismatched());
        assert_eq!(vertical.suggested(), Edge::Right);
    }

    #[test]
    fn layout_hint_keeps_consistent_records_and_rejects_malformed_ones() {
        let line = LayoutHint::observe(Edge::Right, Edge::Right).line();
        let consistent = line.replace(r#""peer":"Right""#, r#""peer":"Left""#);
        let expected = LayoutHint::observe(Edge::Right, Edge::Left);
        assert_eq!(LayoutHint::parse(&consistent), Some(expected));
        assert!(LayoutHint::parse(&line.replace("\"version\":1", "\"version\":2")).is_none());
        assert!(LayoutHint::parse(&line.replace("layout_hint", "layout")).is_none());
        assert!(LayoutHint::parse(&line.replace("\"Right\"", "\"Unknown\"")).is_none());
        assert!(LayoutHint::parse(&line.replace('}', ",\"extra\":1}")).is_none());
        assert!(LayoutHint::parse(&line[..line.len() - 1]).is_none());
        assert!(LayoutHint::parse(&format!("{line}{}", " ".repeat(256))).is_none());
        assert!(LayoutHint::parse(&"x".repeat(257)).is_none());
        let update = LayoutUpdate::request(1, Edge::Left).line();
        assert!(LayoutHint::parse(&update).is_none());
        assert!(LayoutUpdate::parse(&line).is_none());
    }
}
