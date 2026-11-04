//! Bounded Unicode text engine, independent of OS polling and transport.
//! No payload implements Debug. Backends must never log text or content hashes.
use crate::{
    data_policy::{Capability, DataSession, Denied, Permissions},
    Digest,
};
use std::collections::VecDeque;

pub const MAX_TEXT_BYTES: usize = 1024 * 1024;
const ECHO_ENTRIES: usize = 32;
const ECHO_MS: u64 = 5000;
const INTERVAL_MS: u64 = 100;

/// Unknown/sensitive/password-manager formats must be LocalOnly. PlainText is
/// an explicit policy decision, not a claim that arbitrary strings are secret-free.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Classification {
    ShareableText,
    LocalOnly,
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Local,
    Imported,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Permission(Denied),
    LocalOnly,
    Echo,
    InvalidText,
    Replay,
    RateLimited,
    SequenceExhausted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteResult {
    /// OS accepted a write; this does not mean another application pasted it.
    Submitted,
    /// User changed the clipboard since the offered revision. Never retry blindly.
    Changed,
    Failed,
    /// Submission may have happened. A repeated sequence is never retried.
    UnknownOutcome,
}

pub trait Backend {
    /// Implement the strongest available native revision check, immediately before
    /// writing. If the OS cannot guarantee atomic CAS, document the remaining race.
    /// Failed/UnknownOutcome must not be automatically retried by the adapter.
    fn compare_and_write(&mut self, expected_revision: u64, text: &str) -> WriteResult;
}

/// A transient send candidate, not an authenticated packet. Adapters must cap
/// framing before allocation and revalidate its ticket immediately before sending.
pub struct Outbound {
    sequence: u64,
    text: String,
    permit: crate::data_policy::Permit,
}
impl Outbound {
    pub fn sequence(&self) -> u64 {
        self.sequence
    }
    pub fn text(&self) -> &str {
        &self.text
    }
}

pub struct Clipboard {
    session: DataSession,
    sent: u64,
    received: u64,
    last_send: Option<u64>,
    last_receive: Option<u64>,
    echoes: VecDeque<(Digest, u64)>,
}
impl Clipboard {
    pub fn new(session: DataSession) -> Self {
        Self {
            session,
            sent: 0,
            received: 0,
            last_send: None,
            last_receive: None,
            echoes: VecDeque::new(),
        }
    }

    pub fn consent(
        &mut self,
        permissions: Permissions,
        lifetime_ms: u64,
        now: u64,
    ) -> Result<(), Denied> {
        self.session.consent(permissions, lifetime_ms, now)
    }

    pub fn revoke(&mut self) {
        self.session.revoke();
        self.echoes.clear();
    }

    pub fn close(&mut self) {
        self.session.close();
        self.echoes.clear();
    }

    fn valid_text(text: &str) -> Result<(), Error> {
        if text.is_empty() || text.len() > MAX_TEXT_BYTES || text.contains('\0') {
            return Err(Error::InvalidText);
        }
        Ok(())
    }

    fn recent(last: Option<u64>, now: u64) -> bool {
        last.is_some_and(|last| now.saturating_sub(last) < INTERVAL_MS)
    }

    fn remember(&mut self, digest: Digest, now: u64) {
        self.echoes.retain(|(_, at)| now.saturating_sub(*at) < ECHO_MS);
        if self.echoes.len() == ECHO_ENTRIES {
            self.echoes.pop_front();
        }
        self.echoes.push_back((digest, now));
    }

    /// Observe only an explicitly shareable native text snapshot. Imported
    /// provenance remains excluded even after the bounded digest fallback expires.
    pub fn export(
        &mut self,
        text: &str,
        classification: Classification,
        origin: Origin,
        now: u64,
    ) -> Result<Outbound, Error> {
        let permit = self
            .session
            .authorize(Capability::TextSend, now)
            .map_err(Error::Permission)?;
        if classification != Classification::ShareableText {
            return Err(Error::LocalOnly);
        }
        if origin == Origin::Imported {
            return Err(Error::Echo);
        }
        Self::valid_text(text)?;
        let digest = *blake3::hash(text.as_bytes()).as_bytes();
        self.echoes.retain(|(_, at)| now.saturating_sub(*at) < ECHO_MS);
        if self.echoes.iter().any(|(seen, _)| *seen == digest) {
            return Err(Error::Echo);
        }
        if Self::recent(self.last_send, now) {
            return Err(Error::RateLimited);
        }
        self.sent = self.sent.checked_add(1).ok_or(Error::SequenceExhausted)?;
        self.last_send = Some(now);
        self.remember(digest, now);
        Ok(Outbound {
            sequence: self.sent,
            text: text.to_owned(),
            permit,
        })
    }

    /// Queued candidates lose permission on revoke, replacement, disconnect or
    /// expiry. Consuming the candidate in the transport is the adapter's duty.
    pub fn validate_send(&mut self, candidate: &Outbound, now: u64) -> Result<(), Error> {
        self.session
            .validate(&candidate.permit, Capability::TextSend, now)
            .map_err(Error::Permission)
    }

    /// Bytes must already come from the authenticated bound session. This method
    /// cannot authenticate wire-supplied peer IDs. Invalid UTF-8 is never replaced.
    /// The expected revision is captured when queuing the offer, not at execution.
    pub fn receive(
        &mut self,
        sequence: u64,
        bytes: &[u8],
        expected_revision: u64,
        now: u64,
        backend: &mut impl Backend,
    ) -> Result<WriteResult, Error> {
        let permit = self
            .session
            .authorize(Capability::TextReceive, now)
            .map_err(Error::Permission)?;
        if bytes.len() > MAX_TEXT_BYTES {
            return Err(Error::InvalidText);
        }
        let text = std::str::from_utf8(bytes).map_err(|_| Error::InvalidText)?;
        Self::valid_text(text)?;
        if sequence == 0 || sequence <= self.received {
            return Err(Error::Replay);
        }
        if Self::recent(self.last_receive, now) {
            return Err(Error::RateLimited);
        }
        self.session
            .validate(&permit, Capability::TextReceive, now)
            .map_err(Error::Permission)?;
        // Consume before invoking the backend, including failures and uncertainty.
        self.received = sequence;
        self.last_receive = Some(now);
        self.remember(*blake3::hash(bytes).as_bytes(), now);
        Ok(backend.compare_and_write(expected_revision, text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Memory {
        revision: u64,
        text: String,
        calls: usize,
        result: WriteResult,
    }
    impl Backend for Memory {
        fn compare_and_write(&mut self, expected: u64, text: &str) -> WriteResult {
            self.calls += 1;
            if self.revision != expected {
                return WriteResult::Changed;
            }
            if self.result == WriteResult::Submitted {
                self.text = text.to_owned();
                self.revision += 1;
            }
            self.result
        }
    }
    fn memory() -> Memory {
        Memory {
            revision: 7,
            text: "local".into(),
            calls: 0,
            result: WriteResult::Submitted,
        }
    }
    fn engine() -> Clipboard {
        let mut clipboard = Clipboard::new(DataSession::new([1; 32], [2; 32], 0));
        clipboard
            .consent(
                Permissions {
                    text_send: true,
                    text_receive: true,
                    ..Permissions::default()
                },
                100_000,
                0,
            )
            .unwrap();
        clipboard
    }

    #[test]
    fn unicode_roundtrip_and_loop_suppression() {
        let mut clipboard = engine();
        let mut backend = memory();
        let text = "屏跃 🌍\r\nمرحبا";
        assert_eq!(
            clipboard.receive(1, text.as_bytes(), 7, 0, &mut backend),
            Ok(WriteResult::Submitted)
        );
        assert_eq!(backend.text, text);
        assert!(matches!(
            clipboard.export(text, Classification::ShareableText, Origin::Local, 100),
            Err(Error::Echo)
        ));
        assert!(matches!(
            clipboard.export(text, Classification::ShareableText, Origin::Imported, 6000),
            Err(Error::Echo)
        ));
        let outbound = clipboard
            .export("new local", Classification::ShareableText, Origin::Local, 6000)
            .unwrap();
        assert_eq!(outbound.text(), "new local");
        assert_eq!(outbound.sequence(), 1);
    }

    #[test]
    fn local_change_and_uncertain_failure_are_never_replayed() {
        for outcome in [WriteResult::Changed, WriteResult::Failed, WriteResult::UnknownOutcome] {
            let mut clipboard = engine();
            let mut backend = memory();
            backend.result = outcome;
            assert_eq!(clipboard.receive(8, b"remote", 7, 0, &mut backend), Ok(outcome));
            assert_eq!(
                clipboard.receive(8, b"remote", 7, 100, &mut backend),
                Err(Error::Replay)
            );
            assert_eq!(backend.calls, 1);
        }
        let mut clipboard = engine();
        let mut backend = memory();
        backend.revision = 8;
        assert_eq!(
            clipboard.receive(1, b"remote", 7, 0, &mut backend),
            Ok(WriteResult::Changed)
        );
        assert_eq!(backend.text, "local");
    }

    #[test]
    fn malformed_oversize_empty_and_replay_never_reach_backend() {
        let mut clipboard = engine();
        let mut backend = memory();
        for bytes in [vec![0xff], vec![b'x'; MAX_TEXT_BYTES + 1], vec![], vec![0]] {
            assert_eq!(
                clipboard.receive(1, &bytes, 7, 0, &mut backend),
                Err(Error::InvalidText)
            );
        }
        assert_eq!(
            clipboard.receive(0, b"text", 7, 0, &mut backend),
            Err(Error::Replay)
        );
        assert_eq!(backend.calls, 0);
        let maximum = "x".repeat(MAX_TEXT_BYTES);
        assert_eq!(
            clipboard.receive(1, maximum.as_bytes(), 7, 0, &mut backend),
            Ok(WriteResult::Submitted)
        );
        assert_eq!(backend.text.len(), MAX_TEXT_BYTES);
    }

    #[test]
    fn authorization_is_required_for_each_write_and_queued_send() {
        let mut clipboard = engine();
        let candidate = clipboard
            .export("queued", Classification::ShareableText, Origin::Local, 0)
            .unwrap();
        assert_eq!(clipboard.validate_send(&candidate, 0), Ok(()));
        clipboard.revoke();
        assert!(clipboard.validate_send(&candidate, 0).is_err());
        let mut backend = memory();
        assert!(clipboard.receive(1, b"text", 7, 0, &mut backend).is_err());
        assert_eq!(backend.calls, 0);
        assert!(matches!(
            engine().export("password", Classification::LocalOnly, Origin::Local, 0),
            Err(Error::LocalOnly)
        ));
        let mut expired = engine();
        assert!(expired.receive(1, b"text", 7, 100_000, &mut backend).is_err());
        assert_eq!(backend.calls, 0);
    }

    #[test]
    fn event_storms_are_bounded_and_rate_limited() {
        let mut clipboard = engine();
        let mut backend = memory();
        assert_eq!(
            clipboard.receive(1, b"text", 7, 0, &mut backend),
            Ok(WriteResult::Submitted)
        );
        assert_eq!(
            clipboard.receive(2, b"text", 8, 99, &mut backend),
            Err(Error::RateLimited)
        );
        assert_eq!(backend.calls, 1);
        for i in 0..100 {
            clipboard
                .export(
                    &format!("local {i}"),
                    Classification::ShareableText,
                    Origin::Local,
                    100 + i * INTERVAL_MS,
                )
                .unwrap();
        }
        assert!(clipboard.echoes.len() <= ECHO_ENTRIES);
        let mut rollback = memory();
        assert!(clipboard.receive(2, b"text", 8, 1, &mut rollback).is_err());
        assert_eq!(rollback.calls, 0);
    }
}
