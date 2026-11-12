//! File reservation control on the inherited local worker pipes only.
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;

pub const MAX_LIFETIME_MS: u32 = 60_000;
pub const PROOF_HEX: usize = 160;

// This value may carry a short-lived proof. Do not derive Debug or use it in
// diagnostics. The worker receives it only through the bounded private stdin.
pub enum Command {
    Inspect,
    Arm {
        session: u32,
        request: u32,
        bind: SocketAddr,
        lifetime_ms: u32,
        proof: String,
    },
    Send {
        session: u32,
        request: u32,
        target: SocketAddr,
        lifetime_ms: u32,
        proof: String,
    },
    Ready {
        session: u32,
        request: u32,
        lease: u64,
    },
    Cancel {
        session: u32,
        request: u32,
    },
}

impl Command {
    pub fn parse(line: &str) -> Option<Self> {
        if line.len() > 256 || !line.is_ascii() {
            return None;
        }
        let mut fields = line.split_ascii_whitespace();
        if fields.next()? != "fd" {
            return None;
        }
        let command = match fields.next()? {
            "inspect" => Self::Inspect,
            direction @ ("arm" | "send") => {
                let session = identifier(fields.next()?)?;
                let request = identifier(fields.next()?)?;
                let lifetime_ms = fields.next()?.parse().ok()?;
                let address = fields.next()?.parse().ok()?;
                let proof = fields.next()?;
                if !(1..=MAX_LIFETIME_MS).contains(&lifetime_ms)
                    || proof.len() != PROOF_HEX
                    || !proof
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                {
                    return None;
                }
                if direction == "arm" {
                    Self::Arm {
                        session,
                        request,
                        bind: address,
                        lifetime_ms,
                        proof: proof.to_owned(),
                    }
                } else {
                    Self::Send {
                        session,
                        request,
                        target: address,
                        lifetime_ms,
                        proof: proof.to_owned(),
                    }
                }
            }
            "ready" => Self::Ready {
                session: identifier(fields.next()?)?,
                request: identifier(fields.next()?)?,
                lease: hex_id(fields.next()?, 16)?,
            },
            "cancel" => Self::Cancel {
                session: identifier(fields.next()?)?,
                request: identifier(fields.next()?)?,
            },
            _ => return None,
        };
        fields.next().is_none().then_some(command)
    }

    pub fn line(&self) -> String {
        match self {
            Self::Inspect => "fd inspect".into(),
            Self::Arm {
                session,
                request,
                bind,
                lifetime_ms,
                proof,
            } => {
                format!("fd arm {session:x} {request:x} {lifetime_ms} {bind} {proof}")
            }
            Self::Send {
                session,
                request,
                target,
                lifetime_ms,
                proof,
            } => format!("fd send {session:x} {request:x} {lifetime_ms} {target} {proof}"),
            Self::Ready {
                session,
                request,
                lease,
            } => format!("fd ready {session:x} {request:x} {lease:x}"),
            Self::Cancel { session, request } => format!("fd cancel {session:x} {request:x}"),
        }
    }
}

fn identifier(field: &str) -> Option<u32> {
    u32::try_from(hex_id(field, 8)?).ok()
}

fn hex_id(field: &str, limit: usize) -> Option<u64> {
    if field.len() > limit
        || !field
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return None;
    }
    let value = u64::from_str_radix(field, 16).ok()?;
    (value != 0).then_some(value)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Session,
    Armed,
    Rejected,
    Cancelled,
    Matched,
    Claimed,
    SourceArmed,
    SourceCommitted,
    SourceReleased,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Event {
    version: u8,
    kind: Kind,
    pub session: u32,
    pub request: u32,
    pub state: State,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lease: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Kind {
    #[serde(rename = "file_drop")]
    FileDrop,
}

impl Event {
    pub fn new(session: u32, request: u32, state: State, lease: Option<u64>) -> Self {
        Self {
            version: 1,
            kind: Kind::FileDrop,
            session,
            request,
            state,
            lease,
            port: None,
        }
    }

    pub fn armed(session: u32, request: u32, port: u16) -> Self {
        Self {
            port: Some(port),
            ..Self::new(session, request, State::Armed, None)
        }
    }

    pub fn parse(line: &str) -> Option<Self> {
        if line.len() > 256 {
            return None;
        }
        serde_json::from_str::<Self>(line).ok().filter(|event| {
            event.version == 1
                && event.session != 0
                && (event.request == 0) == (event.state == State::Session)
                && match event.state {
                    State::Matched | State::Claimed | State::SourceCommitted | State::SourceReleased => {
                        event.lease.is_some_and(|lease| lease != 0) && event.port.is_none()
                    }
                    State::Armed => {
                        event.lease.is_none() && event.port.is_some_and(|port| port != 0)
                    }
                    _ => event.lease.is_none() && event.port.is_none(),
                }
        })
    }

    pub fn line(self) -> String {
        serde_json::to_string(&self).expect("finite file reservation event")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_drop_commands_are_bounded_and_accept_only_current_shape() {
        let arm = Command::Arm {
            session: u32::MAX,
            request: u32::MAX,
            bind: "[ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff%4294967295]:65535"
                .parse()
                .unwrap(),
            lifetime_ms: MAX_LIFETIME_MS,
            proof: "ab".repeat(80),
        };
        let line = arm.line();
        assert!(line.len() < 256);
        assert!(matches!(Command::parse(&line), Some(Command::Arm { .. })));
        assert!(matches!(
            Command::parse(&line.replacen("fd arm ", "fd send ", 1)),
            Some(Command::Send { .. })
        ));
        assert!(Command::parse(&format!("{line} extra")).is_none());
        assert!(Command::parse(&line.replace("60000", "60001")).is_none());
        assert!(Command::parse(&line.replace("ab", "AB")).is_none());
        assert!(Command::parse("fd cancel 0 1").is_none());
        let ready = Command::Ready {
            session: 1,
            request: 2,
            lease: u64::MAX,
        };
        assert!(matches!(
            Command::parse(&ready.line()),
            Some(Command::Ready {
                session: 1,
                request: 2,
                lease: u64::MAX
            })
        ));
        assert!(Command::parse("fd ready 1 2 0").is_none());
        assert!(Command::parse("fd ready 1 2 10000000000000000").is_none());
        assert!(Command::parse("fd ready 1 2 AB").is_none());
    }

    #[test]
    fn only_lease_events_carry_a_lease_and_never_a_proof() {
        let event = Event::new(1, 2, State::Claimed, Some(7));
        assert_eq!(Event::parse(&event.line()), Some(event));
        assert!(!event.line().contains("proof"));
        let committed = Event::new(1, 2, State::SourceCommitted, Some(7));
        assert_eq!(Event::parse(&committed.line()), Some(committed));
        assert!(Event::parse(&Event::new(1, 2, State::SourceArmed, Some(7)).line()).is_none());
        assert!(Event::parse(&Event::new(1, 2, State::Claimed, None).line()).is_none());
        assert!(Event::parse(&Event::new(1, 2, State::Armed, Some(7)).line()).is_none());
    }
}
