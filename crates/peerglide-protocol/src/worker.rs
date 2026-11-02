//! Local supervisor IPC only; independent of the authenticated network protocol.
use serde::{Deserialize, Serialize};
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkerState {
    Listening,
    Reconnecting,
    Disconnected,
    Authenticated,
    ProbePassed,
    Stopped,
    Failed,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerEvent {
    version: u16,
    pub state: WorkerState,
}
impl WorkerEvent {
    pub fn new(state: WorkerState) -> Self {
        Self { version: 1, state }
    }
    pub fn parse(line: &str) -> Option<Self> {
        if line.len() > 1024 {
            return None;
        }
        serde_json::from_str::<Self>(line)
            .ok()
            .filter(|e| e.version == 1)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn supervisor_events_are_versioned_and_do_not_accept_log_prefixes() {
        let line = serde_json::to_string(&WorkerEvent::new(WorkerState::Authenticated)).unwrap();
        assert_eq!(
            WorkerEvent::parse(&line).unwrap().state,
            WorkerState::Authenticated
        );
        for line in [
            "Authenticated peer connected",
            "{\"version\":2,\"state\":\"Authenticated\"}",
            "{\"version\":1,\"state\":\"Authenticated\",\"extra\":true}",
        ] {
            assert!(WorkerEvent::parse(line).is_none());
        }
        assert!(WorkerEvent::parse(&"x".repeat(1025)).is_none());
    }
}
