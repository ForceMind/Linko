//! Private pairing-child pipes, NOT network authentication or a trust token.
//! Only a supervisor's own live child stdout may produce observations. Codes
//! must never enter generic diagnostic logs; stale child channels must be dropped.
use serde::{Deserialize, Serialize};
use std::{
    io::{self, BufRead},
    net::SocketAddr,
};
use zeroize::{Zeroize, Zeroizing};

pub const MAX_EVENT_LINE: usize = 72 * 1024;
pub const MAX_COMMAND_LINE: usize = 256;
pub const VERSION: u16 = 2;
pub fn clear_code(code:&mut String){code.zeroize();}
const MAX_CERTIFICATE: usize = 16 * 1024;

#[derive(Serialize, Deserialize)]
enum EventKind {
    #[serde(rename = "pairing_event")]
    Pairing,
}
#[derive(Serialize, Deserialize)]
enum CommandKind {
    #[serde(rename = "pairing_command")]
    Pairing,
}
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Failure {
    Identity,
    Listen,
    Authentication,
    InvalidCommand,
    Protocol,
    Internal,
}
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StopReason {
    Cancelled,
    Expired,
    PeerDisconnected,
    AttemptsExhausted,
}

// Deliberately no Debug/Clone: Listening contains the actual secret code.
#[derive(Serialize, Deserialize)]
#[serde(tag = "state", deny_unknown_fields)]
pub enum State {
    Listening {
        code: String,
        address: SocketAddr,
    },
    Authenticating {},
    AttemptRejected {
        remaining: u8,
    },
    Authenticated {
        certificate: Vec<u8>,
        local_fingerprint: String,
        context: [u8; 32],
    },
    ReadyToSave {
        context: [u8; 32],
    },
    PeerSaved {
        context: [u8; 32],
    },
    Completed {
        context: [u8; 32],
    },
    Stopped {
        reason: StopReason,
    },
    Failed {
        reason: Failure,
    },
}
impl Drop for State {
    fn drop(&mut self) {
        if let Self::Listening { code, .. } = self {
            code.zeroize();
        }
    }
}
impl State {
    fn valid(&self) -> bool {
        match self {
            Self::Listening { code, address } => valid_code(code) && address.port() != 0,
            Self::AttemptRejected { remaining } => *remaining < 5,
            Self::Authenticated {
                certificate,
                local_fingerprint,
                ..
            } => {
                !certificate.is_empty()
                    && certificate.len() <= MAX_CERTIFICATE
                    && local_fingerprint.len() == 64
                    && local_fingerprint
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            }
            _ => true,
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairingEvent {
    version: u16,
    kind: EventKind,
    pub event: State,
}
impl PairingEvent {
    pub fn new(event: State) -> Self {
        Self {
            version: VERSION,
            kind: EventKind::Pairing,
            event,
        }
    }
    pub fn line(&self) -> Option<Zeroizing<String>> {
        if !self.event.valid() {
            return None;
        }
        let line = Zeroizing::new(serde_json::to_string(self).ok()?);
        (line.len() <= MAX_EVENT_LINE).then_some(line)
    }
    pub fn parse(line: &str) -> Option<Self> {
        if line.len() > MAX_EVENT_LINE {
            return None;
        }
        serde_json::from_str::<Self>(line)
            .ok()
            .filter(|event| event.version == VERSION && event.event.valid())
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "command", deny_unknown_fields)]
enum Command {
    Code { code: String },
    Approve { context: [u8; 32] },
    LocalSaved { context: [u8; 32] },
    ReceiptStored { context: [u8; 32] },
    Stop {},
}
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum CommandTag {
    Stop = 0,
    Code = 1,
    Approve = 2,
    LocalSaved = 3,
    ReceiptStored = 4,
}
impl Drop for Command {
    fn drop(&mut self) {
        if let Self::Code { code } = self {
            code.zeroize();
        }
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairingCommand {
    version: u16,
    kind: CommandKind,
    body: Command,
}
fn valid_code(code: &str) -> bool {
    code.len() == 8 && code.bytes().all(|b| b.is_ascii_digit())
}
impl PairingCommand {
    pub fn code(code: String) -> Option<Self> {
        let body = Command::Code { code };
        let Command::Code { code } = &body else {
            return None;
        };
        valid_code(code).then_some(Self {
            version: VERSION,
            kind: CommandKind::Pairing,
            body,
        })
    }
    pub fn stop() -> Self {
        Self {
            version: VERSION,
            kind: CommandKind::Pairing,
            body: Command::Stop {},
        }
    }
    pub fn entered_code(&self) -> Option<&str> {
        match &self.body {
            Command::Code { code } => Some(code),
            _ => None,
        }
    }
    pub fn tag(&self) -> CommandTag {
        match self.body {
            Command::Code { .. } => CommandTag::Code,
            Command::Approve { .. } => CommandTag::Approve,
            Command::LocalSaved { .. } => CommandTag::LocalSaved,
            Command::ReceiptStored { .. } => CommandTag::ReceiptStored,
            Command::Stop {} => CommandTag::Stop,
        }
    }
    pub fn context(&self) -> Option<[u8; 32]> {
        match &self.body {
            Command::Approve { context }
            | Command::LocalSaved { context }
            | Command::ReceiptStored { context } => Some(*context),
            _ => None,
        }
    }
    fn with_context(body: Command) -> Self {
        Self {
            version: VERSION,
            kind: CommandKind::Pairing,
            body,
        }
    }
    pub fn approve(context: [u8; 32]) -> Self {
        Self::with_context(Command::Approve { context })
    }
    pub fn local_saved(context: [u8; 32]) -> Self {
        Self::with_context(Command::LocalSaved { context })
    }
    pub fn receipt_stored(context: [u8; 32]) -> Self {
        Self::with_context(Command::ReceiptStored { context })
    }
    pub fn is_stop(&self) -> bool {
        matches!(self.body, Command::Stop {})
    }
    pub fn line(&self) -> Zeroizing<String> {
        Zeroizing::new(serde_json::to_string(self).expect("closed pairing command"))
    }
    pub fn parse(line: &str) -> Option<Self> {
        if line.len() > MAX_COMMAND_LINE {
            return None;
        }
        serde_json::from_str::<Self>(line).ok().filter(|command| {
            command.version == VERSION && command.entered_code().is_none_or(valid_code)
        })
    }
}

/// Bounded before allocation; no read_line/read_until on attacker-controlled
/// streams. An unterminated final command is invalid; EOF by itself means stop.
pub fn read_command(reader: &mut impl BufRead) -> io::Result<Option<PairingCommand>> {
    let mut bytes = Zeroizing::new(Vec::new());
    loop {
        let chunk = reader.fill_buf()?;
        if chunk.is_empty() {
            return if bytes.is_empty() {
                Ok(None)
            } else {
                Err(invalid())
            };
        }
        let end = chunk.iter().position(|b| *b == b'\n');
        let count = end.unwrap_or(chunk.len());
        if bytes.len() + count > MAX_COMMAND_LINE {
            return Err(invalid());
        }
        bytes.extend_from_slice(&chunk[..count]);
        reader.consume(count + usize::from(end.is_some()));
        if end.is_some() {
            let text = std::str::from_utf8(&bytes).map_err(|_| invalid())?;
            return PairingCommand::parse(text).map(Some).ok_or_else(invalid);
        }
    }
}
fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid pairing command")
}

/// Pairing stdout is a separate bounded channel, never an ordinary log line.
pub fn read_event(reader: &mut impl BufRead) -> io::Result<Option<PairingEvent>> {
    let mut bytes = Zeroizing::new(Vec::new());
    loop {
        let chunk = reader.fill_buf()?;
        if chunk.is_empty() {
            return if bytes.is_empty() {
                Ok(None)
            } else {
                Err(invalid())
            };
        }
        let end = chunk.iter().position(|byte| *byte == b'\n');
        let count = end.unwrap_or(chunk.len());
        if bytes.len() + count > MAX_EVENT_LINE {
            return Err(invalid());
        }
        bytes.extend_from_slice(&chunk[..count]);
        reader.consume(count + usize::from(end.is_some()));
        if end.is_some() {
            return std::str::from_utf8(&bytes)
                .ok()
                .and_then(PairingEvent::parse)
                .map(Some)
                .ok_or_else(invalid);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn commands_are_closed_bounded_and_require_exact_ascii_codes() {
        for text in ["", "1234567", "123456789", "１２３４５６７８", "abcdefgh"] {
            assert!(PairingCommand::code(text.into()).is_none());
        }
        let command = PairingCommand::code("12345678".into()).unwrap();
        assert_eq!(
            PairingCommand::parse(&command.line())
                .unwrap()
                .entered_code(),
            Some("12345678")
        );
        assert!(PairingCommand::parse(&PairingCommand::stop().line())
            .unwrap()
            .entered_code()
            .is_none());
        for bad in [
            r#"{"version":3,"kind":"pairing_command","body":{"command":"Stop"}}"#,
            r#"{"version":2,"kind":"pairing_command","body":{"command":"Stop","extra":true}}"#,
            r#"{"version":1,"kind":"pairing_command","body":{"command":"Stop"}}"#,
            "stop",
            "log {\"version\":1}",
        ] {
            assert!(PairingCommand::parse(bad).is_none());
        }
        assert!(PairingCommand::parse(&" ".repeat(MAX_COMMAND_LINE + 1)).is_none());
    }
    #[test]
    fn command_reader_limits_memory_and_rejects_partial_utf8_and_partial_records() {
        let mut full = PairingCommand::stop().line();
        full.push('\n');
        assert!(read_command(&mut io::Cursor::new(full.as_bytes()))
            .unwrap()
            .is_some());
        for bytes in [
            b"".to_vec(),
            vec![b' '; MAX_COMMAND_LINE + 1],
            vec![255, b'\n'],
            b"{}".to_vec(),
        ] {
            let result = read_command(&mut io::Cursor::new(&bytes));
            if bytes.is_empty() {
                assert!(result.unwrap().is_none());
            } else {
                assert!(result.is_err());
            }
        }
    }
    #[test]
    fn event_codec_is_distinct_from_input_state_and_bounds_public_material() {
        let event = PairingEvent::new(State::Listening {
            code: "12345678".into(),
            address: "127.0.0.1:24871".parse().unwrap(),
        });
        let line = event.line().unwrap();
        assert!(PairingEvent::parse(&line).is_some());
        assert!(crate::worker::WorkerEvent::parse(&line).is_none());
        assert!(crate::worker::WorkerInputEvent::parse(&line).is_none());
        let auth = PairingEvent::new(State::Authenticated {
            certificate: vec![255; MAX_CERTIFICATE],
            local_fingerprint: "a".repeat(64),
            context: [0; 32],
        });
        assert!(auth.line().unwrap().len() < MAX_EVENT_LINE);
        assert!(PairingEvent::parse(&auth.line().unwrap()).is_some());
        let bad = PairingEvent::new(State::Authenticated {
            certificate: vec![0; MAX_CERTIFICATE + 1],
            local_fingerprint: "a".repeat(64),
            context: [0; 32],
        });
        assert!(bad.line().is_none());
        assert!(PairingEvent::parse(&" ".repeat(MAX_EVENT_LINE + 1)).is_none());
        assert!(PairingEvent::parse(&PairingCommand::stop().line()).is_none());
        let mut framed = event.line().unwrap();
        framed.push('\n');
        assert!(read_event(&mut io::Cursor::new(framed.as_bytes()))
            .unwrap()
            .is_some());
        for bytes in [
            vec![b' '; MAX_EVENT_LINE + 1],
            vec![255, b'\n'],
            b"{}".to_vec(),
        ] {
            assert!(read_event(&mut io::Cursor::new(bytes)).is_err());
        }
        assert!(read_event(&mut io::Cursor::new(b"")).unwrap().is_none());
    }
}
