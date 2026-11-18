//! Optional display-only diagnostics between two local diagnostic sessions
//! (ADR-0019). `Message`, `VERSION` and `ALPN` are unchanged. A peer that
//! does not offer the exchange in Hello never receives these frames.
use crate::{encode, InputCapabilities, MAX_FRAME, VERSION};
use serde::{Deserialize, Serialize};

/// Longest carried line; the GUI summary copy limit.
pub const MAX_LINE_BYTES: usize = 1600;

#[derive(Serialize)]
enum HelloOffer<'a> {
    Hello {
        version: u16,
        capabilities: &'a InputCapabilities,
        peer_diagnostics: u8,
    },
}

#[derive(Deserialize)]
enum HelloFlag {
    Hello {
        #[serde(default)]
        peer_diagnostics: u8,
    },
}

/// A v5 Hello that also offers the exchange. A v5 decoder that predates the
/// offer ignores the extra field and reads an ordinary Hello.
pub fn hello_with_offer(capabilities: &InputCapabilities) -> Result<Vec<u8>, serde_json::Error> {
    encode(&HelloOffer::Hello {
        version: VERSION,
        capabilities,
        peer_diagnostics: 1,
    })
}

/// True only for a Hello frame carrying the offer.
pub fn hello_offers(frame: &[u8]) -> bool {
    if frame.len() > MAX_FRAME {
        return false;
    }
    let flag = serde_json::from_slice::<HelloFlag>(frame);
    matches!(flag, Ok(HelloFlag::Hello { peer_diagnostics: 1 }))
}

/// Lines from the sender's diagnostic session. Each one is validated again
/// by the receiver before it is shown; nothing here is executed or saved.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Report {
    pub summary: Option<String>,
    pub held: Option<String>,
    pub event: Option<String>,
}

fn printable(line: &str) -> bool {
    line.len() <= MAX_LINE_BYTES && line.bytes().all(|b| (b' '..=b'~').contains(&b))
}

impl Report {
    fn bounded(&self) -> bool {
        let all = [&self.summary, &self.held, &self.event];
        all.iter().all(|l| l.as_deref().is_none_or(printable))
    }
}

#[derive(Serialize)]
enum FrameRef<'a> {
    PeerDiagnostics(&'a Report),
}

#[derive(Deserialize)]
enum Frame {
    PeerDiagnostics(Report),
}

/// The frame for one report, or None when it would exceed a bound.
pub fn encode_report(report: &Report) -> Option<Vec<u8>> {
    if !report.bounded() {
        return None;
    }
    let frame = encode(&FrameRef::PeerDiagnostics(report)).ok()?;
    (frame.len() <= MAX_FRAME).then_some(frame)
}

/// Strict decode: unknown fields, oversize lines and non-printable bytes
/// reject the whole frame.
pub fn decode_report(frame: &[u8]) -> Option<Report> {
    if frame.len() > MAX_FRAME {
        return None;
    }
    let Ok(Frame::PeerDiagnostics(report)) = serde_json::from_slice(frame) else {
        return None;
    };
    report.bounded().then_some(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{decode, Message};

    fn caps() -> InputCapabilities {
        InputCapabilities::new(&[4, 0xe0], true, 7, true).unwrap()
    }

    #[test]
    fn offered_hello_is_an_ordinary_hello_for_existing_v5_decoders() {
        let frame = hello_with_offer(&caps()).unwrap();
        let decoded = decode::<Message>(&frame).unwrap();
        assert!(matches!(decoded, Message::Hello { version: VERSION, .. }));
        let Message::Hello { capabilities, .. } = decoded else {
            panic!("an offered Hello must stay a v5 Hello");
        };
        assert!(capabilities == caps() && capabilities.valid());
        assert!(hello_offers(&frame));
        let plain = encode(&Message::Hello {
            version: VERSION,
            capabilities: caps(),
        })
        .unwrap();
        assert!(!hello_offers(&plain));
        let ping = encode(&Message::Ping { lease: 0 }).unwrap();
        assert!(!hello_offers(&ping));
        let other = String::from_utf8(frame).unwrap().replace(":1}", ":2}");
        assert!(!hello_offers(other.as_bytes()));
    }

    #[test]
    fn reports_are_strict_bounded_and_never_v5_messages() {
        let report = Report {
            summary: Some("KVM-DIAG/5 sample cap=1".into()),
            held: None,
            event: Some("KVM-HELD/1 blk_gate".into()),
        };
        let frame = encode_report(&report).unwrap();
        assert_eq!(decode_report(&frame).as_ref(), Some(&report));
        assert!(decode::<Message>(&frame).is_err());
        let ping = encode(&Message::Ping { lease: 0 }).unwrap();
        assert_eq!(decode_report(&ping), None);
        let empty = br#"{"PeerDiagnostics":{}}"#;
        assert_eq!(decode_report(empty), Some(Report::default()));
        let extra = br#"{"PeerDiagnostics":{"held":null,"path":"x"}}"#;
        assert_eq!(decode_report(extra), None);
        let text = String::from_utf8(frame).unwrap();
        let control = text.replace("cap=1", "cap=1\\n");
        assert_eq!(decode_report(control.as_bytes()), None);
        let long = Report {
            summary: Some("a".repeat(MAX_LINE_BYTES + 1)),
            ..Report::default()
        };
        assert_eq!(encode_report(&long), None);
        let widest = Report {
            summary: Some("a".repeat(MAX_LINE_BYTES)),
            held: Some("b".repeat(256)),
            event: Some("c".repeat(256)),
        };
        assert!(encode_report(&widest).is_some());
    }
}
