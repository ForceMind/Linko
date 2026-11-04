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

/// Local input-routing observations, separate from connection/authentication.
/// These states contain no input contents, coordinates, lease IDs or peer IDs.
/// Local is the process's routing/cleanup-request report. It does not confirm
/// OS event acceptance, physical state or application-effect delivery.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkerInputState {
    Disabled,
    Local,
    ControllingRemote,
    ReceivingRemote,
    Recovering,
    RecoveryUnconfirmed,
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum InputEventKind {
    #[serde(rename = "input")]
    Input,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerInputEvent {
    version: u16,
    kind: InputEventKind,
    pub state: WorkerInputState,
    #[serde(default, skip_serializing_if = "is_false")]
    pub terminal: bool,
}
fn is_false(value: &bool) -> bool { !*value }
impl WorkerInputEvent {
    pub fn new(state: WorkerInputState) -> Self {
        Self { version: 1, kind: InputEventKind::Input, state, terminal: false }
    }
    /// The producer may issue this only after its supervisor has returned and
    /// cannot start another native session. Unconfirmed never proves cleanup.
    pub fn new_terminal(state: WorkerInputState) -> Option<Self> {
        matches!(state, WorkerInputState::Local | WorkerInputState::Disabled | WorkerInputState::RecoveryUnconfirmed)
            .then_some(Self { version: 1, kind: InputEventKind::Input, state, terminal: true })
    }
    pub fn parse(line: &str) -> Option<Self> {
        if line.len() > 1024 {
            return None;
        }
        serde_json::from_str::<Self>(line).ok().filter(|event| {
            event.version == 1 && (!event.terminal || matches!(event.state,
                WorkerInputState::Local | WorkerInputState::Disabled | WorkerInputState::RecoveryUnconfirmed))
        })
    }
    /// One bounded JSON record without a trailing newline. Only finite enums
    /// and a version are serialized, so construction cannot fail.
    pub fn line(&self) -> String {
        serde_json::to_string(self).expect("finite worker input event")
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

    #[test]
    fn input_states_have_an_independent_closed_schema_without_private_data() {
        for state in [
            WorkerInputState::Disabled, WorkerInputState::Local,
            WorkerInputState::ControllingRemote, WorkerInputState::ReceivingRemote,
            WorkerInputState::Recovering, WorkerInputState::RecoveryUnconfirmed,
            WorkerInputState::Unavailable,
        ] {
            let line = WorkerInputEvent::new(state).line();
            assert_eq!(WorkerInputEvent::parse(&line).unwrap().state, state);
            assert!(!WorkerInputEvent::parse(&line).unwrap().terminal);
            assert!(WorkerEvent::parse(&line).is_none());
            let data: serde_json::Value = serde_json::from_str(&line).unwrap();
            let object = data.as_object().unwrap();
            assert_eq!(object.len(), 3);
            assert_eq!(data["version"], 1);
            assert_eq!(data["kind"], "input");
            assert!(object.keys().all(|key| ["version", "kind", "state"].contains(&key.as_str())));
            assert!(line.len() < 128 && !line.contains('\n'));
        }
        let connection = serde_json::to_string(&WorkerEvent::new(WorkerState::Authenticated)).unwrap();
        assert!(WorkerInputEvent::parse(&connection).is_none());
    }

    #[test]
    fn input_parser_rejects_future_unknown_or_log_records_and_checks_byte_limit() {
        for line in [
            r#"{"version":2,"kind":"input","state":"Local"}"#,
            r#"{"version":1,"kind":"connection","state":"Local"}"#,
            r#"{"version":1,"kind":"input","state":"Authenticated"}"#,
            r#"{"version":1,"state":"Local"}"#,
            r#"{"version":1,"kind":"input","state":"Local","lease":7}"#,
            "input: Local",
            "log {\"version\":1,\"kind\":\"input\",\"state\":\"Local\"}",
        ] {
            assert!(WorkerInputEvent::parse(line).is_none());
        }
        let mut boundary = WorkerInputEvent::new(WorkerInputState::Disabled).line();
        boundary.push_str(&" ".repeat(1024 - boundary.len()));
        assert!(WorkerInputEvent::parse(&boundary).is_some());
        boundary.push(' ');
        assert!(WorkerInputEvent::parse(&boundary).is_none());
    }

    #[test]
    fn only_explicit_consistent_terminal_records_can_report_supervisor_completion() {
        for state in [WorkerInputState::Local, WorkerInputState::Disabled, WorkerInputState::RecoveryUnconfirmed] {
            let event = WorkerInputEvent::new_terminal(state).unwrap();
            assert!(event.terminal);
            assert_eq!(WorkerInputEvent::parse(&event.line()), Some(event));
            let normal = WorkerInputEvent::new(state);
            assert!(!normal.line().contains("terminal"));
            assert!(!WorkerInputEvent::parse(&normal.line()).unwrap().terminal);
        }
        for state in [WorkerInputState::ControllingRemote, WorkerInputState::ReceivingRemote,
            WorkerInputState::Recovering, WorkerInputState::Unavailable] {
            assert!(WorkerInputEvent::new_terminal(state).is_none());
            let mut invalid = serde_json::to_value(WorkerInputEvent::new(state)).unwrap();
            invalid["terminal"] = true.into();
            assert!(WorkerInputEvent::parse(&invalid.to_string()).is_none());
        }
        assert!(WorkerInputEvent::parse(r#"{"version":1,"kind":"input","state":"Local","terminal":"true"}"#).is_none());
        let explicit_false = WorkerInputEvent::parse(r#"{"version":1,"kind":"input","state":"Local","terminal":false}"#).unwrap();
        assert!(!explicit_false.terminal);
    }

    #[test]
    fn reversed_input_records_do_not_turn_a_normal_local_snapshot_into_terminal_proof() {
        let normal = WorkerInputEvent::new(WorkerInputState::Local).line();
        let terminal = WorkerInputEvent::new_terminal(WorkerInputState::RecoveryUnconfirmed).unwrap().line();
        for records in [[normal.as_str(), terminal.as_str()], [terminal.as_str(), normal.as_str()]] {
            let parsed: Vec<_> = records.into_iter().map(|line| WorkerInputEvent::parse(line).unwrap()).collect();
            assert_eq!(parsed.iter().filter(|event| event.terminal).count(), 1);
            assert_eq!(parsed.iter().find(|event| event.terminal).unwrap().state, WorkerInputState::RecoveryUnconfirmed);
            assert!(parsed.iter().all(|event| event.state != WorkerInputState::Local || !event.terminal));
        }
    }
}
