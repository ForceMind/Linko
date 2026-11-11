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
    EdgeReturn,
    PongReceived,
    PongTimeout,
    PeerRelease,
    CapabilityStop,
    MovePositive,
    MoveNegative,
    WheelPhysical,
    WheelInjected,
    ScrollSent,
    ScrollZero,
    ScrollApplied,
    GuardTime,
    GuardState,
    GuardSource,
    GuardEpoch,
    GuardModifiers,
    EdgeHit,
    BlockPeer,
    BlockGate,
    BlockBusy,
    BlockReceiving,
    BlockProtection,
    AcquireSent,
    AcquireReceived,
    DeniedReceived,
    AcquireTimeout,
    AcquireCancelled,
    DenyArgument,
    DenyBusy,
    DenyAuthorization,
    DenyLease,
    DenyBegin,
    DenyPlacement,
    SlowPointer,
    SlowInput,
    LoopStall,
    PointerCoalesced,
    MotionCoalesced,
}

/// Longest line the worker, the GUI log and the copy action accept. The
/// largest possible summary (every counter at u32::MAX) must fit below it.
pub const MAX_SUMMARY_BYTES: usize = 1600;

const KEYS: [&str; Count::MotionCoalesced as usize + 1] = [
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
    "return",
    "pong",
    "pong_to",
    "peer_rel",
    "cap_stop",
    "mv_pos",
    "mv_neg",
    "wh_phys",
    "wh_inj",
    "scr_tx",
    "scr_zero",
    "scr_apply",
    "g_time",
    "g_state",
    "g_source",
    "g_epoch",
    "g_mods",
    "edge_hit",
    "blk_peer",
    "blk_gate",
    "blk_busy",
    "blk_recv",
    "blk_prot",
    "acq_tx",
    "acq_rx",
    "denied_rx",
    "acq_to",
    "acq_cancel",
    "deny_arg",
    "deny_busy",
    "deny_auth",
    "deny_lease",
    "deny_begin",
    "deny_place",
    "slow_ptr",
    "slow_inp",
    "loop_stall",
    "rx_coalesced",
    "tx_coalesced",
];

pub struct InputDiagnostics {
    stopped: AtomicBool,
    counts: [AtomicU32; KEYS.len()],
    #[cfg(windows)]
    next_cursor_sample: std::sync::atomic::AtomicU64,
}

// Arrays longer than 32 elements do not implement Default.
impl Default for InputDiagnostics {
    fn default() -> Self {
        Self {
            stopped: AtomicBool::new(false),
            counts: std::array::from_fn(|_| AtomicU32::new(0)),
            #[cfg(windows)]
            next_cursor_sample: std::sync::atomic::AtomicU64::new(0),
        }
    }
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
        write!(f, "KVM-DIAG/5 {phase}")?;
        for (key, value) in KEYS.iter().zip(self.counts) {
            write!(f, " {key}={value}")?;
        }
        Ok(())
    }
}

/// Only bounded, numeric summaries are eligible for the GUI copy action.
pub fn is_summary(line: &str) -> bool {
    let mut fields = line.split_ascii_whitespace();
    let matches_counter = |field: &str, key: &str| {
        field.split_once('=').is_some_and(|(name, value)| {
            name == key && value.bytes().all(|b| b.is_ascii_digit()) && value.parse::<u32>().is_ok()
        })
    };
    let coalesced = Count::PointerCoalesced as usize;
    line.len() <= MAX_SUMMARY_BYTES
        && !line.bytes().any(|b| b.is_ascii_control())
        && fields.next() == Some("KVM-DIAG/5")
        && matches!(fields.next(), Some("sample" | "expired" | "end"))
        && KEYS[..coalesced]
            .iter()
            .all(|key| fields.next().is_some_and(|field| matches_counter(field, key)))
        // Older workers omit some or all of the appended counters.
        && KEYS[coalesced..]
            .iter()
            .all(|key| fields.next().is_none_or(|field| matches_counter(field, key)))
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
        assert!(summary.len() <= MAX_SUMMARY_BYTES);
        assert!(is_summary(&summary));
        let (receive_only, _) = summary.rsplit_once(' ').unwrap();
        assert!(is_summary(receive_only));
        let (legacy, _) = receive_only.rsplit_once(' ').unwrap();
        assert!(is_summary(legacy));
        assert!(!is_summary(&format!("{legacy} rx_coalesced=invalid")));
        assert!(!is_summary(&summary.replace("KVM-DIAG/5", "KVM-DIAG/4")));
        assert!(!is_summary(&(summary.clone() + " path=/private")));
        assert!(!is_summary(&summary.replace("cap=", "address=")));
    }
}
