//! Session-local aggregate observations. No input payload leaves the owner.
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering::Relaxed};

#[derive(Clone, Copy)]
pub enum Count {
    Captured,
    ZeroDelta,
    Filtered,
    QueueFull,
    Dequeued,
    StaleRoute,
    NoLease,
    Granted,
    Committed,
    SendOk,
    SendUnsupported,
    SendDisabled,
    SendOversize,
    SendClosed,
    Received,
    DecodeRejected,
    LeaseRejected,
    ReplayRejected,
    PointerAccepted,
    PointerFailed,
    PointerRetired,
    NativeSubmitted,
    NativeFailed,
    CursorMoved,
    CursorUnchanged,
    CursorUnknown,
    ReliableSent,
    ReliableReceived,
    ReliableSubmitted,
    ReliableFailed,
}

const KEYS: [&str; Count::ReliableFailed as usize + 1] = [
    "cap",
    "zero",
    "filtered",
    "full",
    "deq",
    "stale",
    "no_lease",
    "grant",
    "commit",
    "tx",
    "tx_unsupported",
    "tx_disabled",
    "tx_oversize",
    "tx_closed",
    "rx",
    "decode",
    "lease",
    "replay",
    "apply",
    "apply_fail",
    "retired",
    "sdk",
    "sdk_fail",
    "moved",
    "unchanged",
    "unknown",
    "rel_tx",
    "rel_rx",
    "rel_apply",
    "rel_fail",
];

#[derive(Default)]
pub struct InputDiagnostics {
    stopped: AtomicBool,
    counts: [AtomicU32; KEYS.len()],
    #[cfg(windows)]
    next_cursor_sample: std::sync::atomic::AtomicU64,
}

impl InputDiagnostics {
    #[cfg(windows)]
    pub(crate) fn sample_cursor(&self, now: u64) -> bool {
        if !self.active() || now < self.next_cursor_sample.load(Relaxed) {
            return false;
        }
        self.next_cursor_sample
            .store(now.saturating_add(250), Relaxed);
        true
    }

    pub fn active(&self) -> bool {
        !self.stopped.load(Relaxed)
    }

    pub fn increment(&self, count: Count) {
        if self.active() {
            self.counts[count as usize].fetch_add(1, Relaxed);
        }
    }

    pub fn stop(&self) {
        self.stopped.store(true, Relaxed);
    }

    pub fn snapshot(&self, phase: Phase) -> Snapshot {
        Snapshot {
            phase,
            counts: std::array::from_fn(|i| self.counts[i].load(Relaxed)),
        }
    }
}

#[derive(Clone, Copy)]
pub enum Phase {
    Sample,
    Expired,
    End,
}

pub struct Snapshot {
    phase: Phase,
    counts: [u32; KEYS.len()],
}

impl fmt::Display for Snapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let phase = match self.phase {
            Phase::Sample => "sample",
            Phase::Expired => "expired",
            Phase::End => "end",
        };
        write!(f, "KVM-DIAG/1 {phase}")?;
        for (key, value) in KEYS.iter().zip(self.counts) {
            write!(f, " {key}={value}")?;
        }
        Ok(())
    }
}

/// Only bounded, numeric summaries are eligible for the GUI copy action.
pub fn is_summary(line: &str) -> bool {
    let mut fields = line.split_ascii_whitespace();
    line.len() <= 600
        && !line.bytes().any(|b| b.is_ascii_control())
        && fields.next() == Some("KVM-DIAG/1")
        && matches!(fields.next(), Some("sample" | "expired" | "end"))
        && KEYS.iter().all(|key| {
            fields
                .next()
                .and_then(|field| field.split_once('='))
                .is_some_and(|(name, value)| {
                    name == *key
                        && value.bytes().all(|b| b.is_ascii_digit())
                        && value.parse::<u32>().is_ok()
                })
        })
        && fields.next().is_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stopped_counters_and_maximum_report_fit_the_gui_copy_boundary() {
        let diagnostics = InputDiagnostics::default();
        diagnostics.increment(Count::Captured);
        diagnostics.stop();
        diagnostics.increment(Count::Captured);
        assert_eq!(diagnostics.snapshot(Phase::End).counts[0], 1);
        let summary = Snapshot {
            phase: Phase::Expired,
            counts: [u32::MAX; KEYS.len()],
        }
        .to_string();
        assert!(is_summary(&summary));
        assert!(!is_summary(&(summary.clone() + " path=/private")));
        assert!(!is_summary(&summary.replace("cap=", "address=")));
    }
}
