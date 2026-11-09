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
}
