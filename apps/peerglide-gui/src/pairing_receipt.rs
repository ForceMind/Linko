//! Local enrollment receipt metadata, never an authentication credential.
//!
//! This module performs no I/O and creates no Session or permission. A context
//! read from disk cannot resume or authenticate a pairing. The owner must check
//! the real child/proof and frozen snapshots inside its commit boundary, then
//! validate this record against the current real identities on every load/use.
use anyhow::Result;
use serde::{Deserialize, Serialize};

const VERSION: u16 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Phase {
    Pending,
    Confirmed,
}

/// Public-material bookkeeping only. `Confirmed` records local evidence that
/// both sides saved, not that the peer stored its own confirmation or that input
/// is authorized. Missing/unknown fields must not silently become a confirmation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Record {
    receipt_version: u16,
    phase: Phase,
    context: [u8; 32],
    local_fingerprint: String,
    peer_fingerprint: String,
}

fn valid_fingerprint(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

impl Record {
    pub(crate) fn pending(
        context: [u8; 32],
        local_fingerprint: &str,
        peer_fingerprint: &str,
    ) -> Result<Self> {
        anyhow::ensure!(
            valid_fingerprint(local_fingerprint) && valid_fingerprint(peer_fingerprint),
            "配对收据指纹必须是 64 位小写十六进制"
        );
        anyhow::ensure!(
            local_fingerprint != peer_fingerprint,
            "配对收据不能登记本机身份"
        );
        Ok(Self {
            receipt_version: VERSION,
            phase: Phase::Pending,
            context,
            local_fingerprint: local_fingerprint.to_owned(),
            peer_fingerprint: peer_fingerprint.to_owned(),
        })
    }

    pub(crate) fn phase(&self) -> Phase {
        self.phase
    }

    pub(crate) fn is_pending(&self) -> bool {
        self.phase == Phase::Pending
    }

    /// Correlation only; this value alone is not proof or a resumable Session.
    pub(crate) fn context(&self) -> [u8; 32] {
        self.context
    }

    fn validate_metadata(&self) -> Result<()> {
        anyhow::ensure!(
            self.receipt_version == VERSION,
            "配对收据版本不受支持，原记录应保留"
        );
        anyhow::ensure!(
            valid_fingerprint(&self.local_fingerprint)
                && valid_fingerprint(&self.peer_fingerprint)
                && self.local_fingerprint != self.peer_fingerprint,
            "配对收据身份格式无效"
        );
        Ok(())
    }

    /// Validate after deserialization and before use. `local_fingerprint` and
    /// `peer_fingerprint` must come from the actual current identity/certificate,
    /// not from this record or another unchecked config field. This check alone
    /// does not authenticate a peer or approve any grant.
    pub(crate) fn validate(
        &self,
        local_fingerprint: &str,
        peer_fingerprint: Option<&str>,
        allow: bool,
        native: bool,
        reconnect: bool,
    ) -> Result<()> {
        self.validate_metadata()?;
        anyhow::ensure!(
            self.local_fingerprint == local_fingerprint
                && peer_fingerprint == Some(self.peer_fingerprint.as_str()),
            "配对收据与本机或另一台的当前身份不一致"
        );
        anyhow::ensure!(
            !self.is_pending() || !(allow || native || reconnect),
            "配对提交尚未确认，接收、键鼠与自动重连必须关闭"
        );
        Ok(())
    }

    /// Update metadata only, after the caller verified real peer-saved evidence,
    /// the exact context and current snapshots inside its commit boundary. This
    /// method does not perform that verification, authenticate, persist or grant
    /// anything. Repeating it for already-confirmed metadata is harmless.
    pub(crate) fn mark_confirmed(&mut self) -> Result<()> {
        self.validate_metadata()?;
        self.phase = Phase::Confirmed;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    fn local() -> String {
        "0123456789abcdef".repeat(4)
    }

    fn peer() -> String {
        "fedcba9876543210".repeat(4)
    }

    fn pending() -> Record {
        Record::pending([17; 32], &local(), &peer()).unwrap()
    }

    #[test]
    fn pending_blocks_each_grant_and_confirmation_does_not_change_the_context() -> Result<()> {
        let mut record = pending();
        assert_eq!(record.phase(), Phase::Pending);
        assert!(record.is_pending());
        record.validate(&local(), Some(&peer()), false, false, false)?;
        for (allow, native, reconnect) in [
            (true, false, false),
            (false, true, false),
            (false, false, true),
            (true, true, true),
        ] {
            assert!(record
                .validate(&local(), Some(&peer()), allow, native, reconnect)
                .is_err());
        }
        record.mark_confirmed()?;
        record.mark_confirmed()?;
        assert_eq!(record.phase(), Phase::Confirmed);
        assert!(!record.is_pending());
        assert_eq!(record.context(), [17; 32]);
        // These booleans are supplied by the owner, not changed by the record.
        record.validate(&local(), Some(&peer()), false, false, false)?;
        record.validate(&local(), Some(&peer()), true, true, true)?;
        Ok(())
    }

    #[test]
    fn identity_replacement_or_missing_peer_rejects_both_phases() -> Result<()> {
        let mut record = pending();
        for phase in [Phase::Pending, Phase::Confirmed] {
            if phase == Phase::Confirmed {
                record.mark_confirmed()?;
            }
            assert!(record
                .validate(&"a".repeat(64), Some(&peer()), false, false, false)
                .is_err());
            assert!(record
                .validate(&local(), Some(&"b".repeat(64)), false, false, false)
                .is_err());
            assert!(record
                .validate(&local(), None, false, false, false)
                .is_err());
        }
        assert!(Record::pending([0; 32], &local(), &local()).is_err());
        Ok(())
    }

    #[test]
    fn fingerprints_are_exact_lowercase_ascii_and_deserialized_data_is_revalidated() {
        for invalid in [
            String::new(),
            "a".repeat(63),
            "a".repeat(65),
            "A".repeat(64),
            "g".repeat(64),
            "１".repeat(64),
        ] {
            assert!(Record::pending([0; 32], &invalid, &peer()).is_err());
            assert!(Record::pending([0; 32], &local(), &invalid).is_err());
            let mut stored = serde_json::to_value(pending()).unwrap();
            stored["peer_fingerprint"] = Value::String(invalid);
            let record: Record = serde_json::from_value(stored).unwrap();
            assert!(record
                .validate(&local(), Some(&peer()), false, false, false)
                .is_err());
        }
    }

    #[test]
    fn future_receipt_cannot_be_validated_or_marked_confirmed() -> Result<()> {
        let mut stored = serde_json::to_value(pending())?;
        stored["receipt_version"] = json!(2);
        let mut future: Record = serde_json::from_value(stored)?;
        assert!(future
            .validate(&local(), Some(&peer()), false, false, false)
            .is_err());
        assert!(future.mark_confirmed().is_err());
        assert!(future.is_pending());
        Ok(())
    }

    #[test]
    fn serde_roundtrip_rejects_unknown_missing_and_wrong_length_context_fields() -> Result<()> {
        let record = pending();
        let stored = serde_json::to_value(&record)?;
        assert_eq!(serde_json::from_value::<Record>(stored.clone())?, record);
        for size in [0, 31, 33] {
            let mut changed = stored.clone();
            changed["context"] = json!(vec![0; size]);
            assert!(serde_json::from_value::<Record>(changed).is_err());
        }
        let mut extra = stored.clone();
        extra["grant"] = json!(true);
        assert!(serde_json::from_value::<Record>(extra).is_err());
        let mut missing = stored.clone();
        missing.as_object_mut().unwrap().remove("phase");
        assert!(serde_json::from_value::<Record>(missing).is_err());
        let mut future_phase = stored;
        future_phase["phase"] = json!("Future");
        assert!(serde_json::from_value::<Record>(future_phase).is_err());
        Ok(())
    }
}
