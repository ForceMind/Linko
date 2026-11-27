//! Session-local aggregate observations. No input payload leaves the owner.
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::Relaxed};
use std::{sync::Mutex, time::Duration};

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

#[cfg(windows)]
#[derive(Clone, Copy)]
pub(crate) enum ReceiveLoopPhase {
    Desktop = 1,
    Pump = 2,
    Messages = 3,
    Wait = 4,
    Shutdown = 5,
}

#[cfg(windows)]
#[derive(Clone, Copy)]
pub(crate) enum ReceiveCommand {
    Begin = 1,
    Pointer = 2,
    Input = 3,
    Release = 4,
    Drain = 5,
}

/// Longest line the worker, the GUI log and the copy action accept. The
/// largest possible summary (every counter at u32::MAX) must fit below it.
pub const MAX_SUMMARY_BYTES: usize = 1600;
const MAX_HEARTBEAT_SAMPLES: usize = 1024;

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
    heartbeat_rtt_us: Mutex<Vec<u32>>,
    #[cfg(windows)]
    next_cursor_sample: std::sync::atomic::AtomicU64,
    #[cfg(windows)]
    receive_phase: AtomicU32,
    #[cfg(windows)]
    first_receive_timeout: AtomicU32,
    started_ms: u64,
    held: AtomicU64,
    held_changes: AtomicU32,
    held_changed_ms: AtomicU64,
}

// Arrays longer than 32 elements do not implement Default.
impl Default for InputDiagnostics {
    fn default() -> Self {
        Self {
            stopped: AtomicBool::new(false),
            counts: std::array::from_fn(|_| AtomicU32::new(0)),
            heartbeat_rtt_us: Mutex::new(Vec::with_capacity(MAX_HEARTBEAT_SAMPLES)),
            #[cfg(windows)]
            next_cursor_sample: std::sync::atomic::AtomicU64::new(0),
            #[cfg(windows)]
            receive_phase: AtomicU32::new(0),
            #[cfg(windows)]
            first_receive_timeout: AtomicU32::new(0),
            started_ms: crate::now_ms(),
            held: AtomicU64::new(0),
            held_changes: AtomicU32::new(0),
            held_changed_ms: AtomicU64::new(0),
        }
    }
}

/// Category totals of the holds the capture callback currently tracks.
/// They contain no key code or input content.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HeldTotals {
    /// HID modifier usages 0xe0..=0xe7.
    pub modifiers: u32,
    /// Other HID usages below 0x100.
    pub keys: u32,
    /// Platform keys outside the HID table.
    pub unknown: u32,
    /// Bit n is button n for n < 15; bit 15 means some higher button.
    pub buttons: u16,
    /// Protected pairs and consumed unknown keys (`pairs.extra_held`).
    pub extra: u32,
}

impl HeldTotals {
    const SMALL: u32 = 0xfff;

    fn pack(self) -> u64 {
        u64::from(self.modifiers.min(0xff))
            | (u64::from(self.keys.min(Self::SMALL)) << 8)
            | (u64::from(self.unknown.min(Self::SMALL)) << 20)
            | (u64::from(self.extra.min(Self::SMALL)) << 32)
            | (u64::from(self.buttons) << 44)
    }

    fn unpack(word: u64) -> Self {
        let small = |shift: u32| ((word >> shift) as u32) & Self::SMALL;
        Self {
            modifiers: (word as u32) & 0xff,
            keys: small(8),
            unknown: small(20),
            extra: small(32),
            buttons: (word >> 44) as u16,
        }
    }
}

/// Why a held-state line was written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeldCause {
    Sample,
    BlockGate,
    DenyBusy,
}

impl HeldCause {
    fn name(self) -> &'static str {
        match self {
            Self::Sample => "sample",
            Self::BlockGate => "blk_gate",
            Self::DenyBusy => "deny_busy",
        }
    }
}

/// Longest valid held-state line, including every field at its maximum.
pub const MAX_HELD_BYTES: usize = 256;
/// Prefix of a line that repeats a validated line received from the peer.
pub const PEER_PREFIX: &str = "KVM-PEER ";

/// One read of the held-state record, formatted outside callbacks.
#[derive(Clone, Copy, Debug)]
pub struct HeldReport {
    pub cause: HeldCause,
    /// Milliseconds since the diagnostic session started.
    pub at_ms: u64,
    /// Hold count in the capture gate word (`idle()` requires zero).
    pub gate: u64,
    pub held: HeldTotals,
    pub changes: u32,
    /// Session time of the latest change; zero when `changes` is zero.
    pub changed_ms: u64,
    pub pending: bool,
    pub outgoing: bool,
}

impl fmt::Display for HeldReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (cause, at, gate) = (self.cause.name(), self.at_ms, self.gate);
        let held = self.held;
        let (mods, keys, unk) = (held.modifiers, held.keys, held.unknown);
        let (buttons, extra) = (held.buttons, held.extra);
        let (changes, changed) = (self.changes, self.changed_ms);
        let (pending, outgoing) = (u8::from(self.pending), u8::from(self.outgoing));
        write!(f, "KVM-HELD/1 {cause} t_ms={at} gate={gate}")?;
        write!(f, " mods={mods} keys={keys} unk={unk}")?;
        write!(f, " btn=0x{buttons:04x} extra={extra}")?;
        write!(f, " changes={changes} changed_ms={changed}")?;
        write!(f, " pending={pending} outgoing={outgoing}")
    }
}

const HELD_COUNTS: [&str; 5] = ["t_ms", "gate", "mods", "keys", "unk"];
const HELD_TOTALS: [&str; 3] = ["extra", "changes", "changed_ms"];

fn held_numbers(fields: &mut std::str::SplitAsciiWhitespace<'_>, keys: &[&str]) -> bool {
    keys.iter().all(|key| held_number(fields.next(), key))
}

fn held_number(field: Option<&str>, key: &str) -> bool {
    let Some((name, value)) = field.and_then(|field| field.split_once('=')) else {
        return false;
    };
    name == key && value.bytes().all(|b| b.is_ascii_digit()) && value.parse::<u64>().is_ok()
}

fn held_flag(field: Option<&str>, key: &str) -> bool {
    matches!(field.and_then(|f| f.split_once('=')), Some((name, "0" | "1")) if name == key)
}

fn held_buttons(field: Option<&str>) -> bool {
    let hex = field.and_then(|field| field.strip_prefix("btn=0x"));
    let hex = hex.unwrap_or_default();
    let lower = |b: u8| b.is_ascii_digit() || (b'a'..=b'f').contains(&b);
    hex.len() == 4 && hex.bytes().all(lower)
}

/// Strict check for a held-state line. `sample` selects periodic lines;
/// otherwise only the gate and busy causes are accepted.
pub fn is_held_line(line: &str, sample: bool) -> bool {
    if line.len() > MAX_HELD_BYTES || line.bytes().any(|b| b.is_ascii_control()) {
        return false;
    }
    let mut fields = line.split_ascii_whitespace();
    if fields.next() != Some("KVM-HELD/1") {
        return false;
    }
    let cause = match fields.next() {
        Some("sample") => sample,
        Some("blk_gate" | "deny_busy") => !sample,
        _ => false,
    };
    cause
        && held_numbers(&mut fields, &HELD_COUNTS)
        && held_buttons(fields.next())
        && held_numbers(&mut fields, &HELD_TOTALS)
        && held_flag(fields.next(), "pending")
        && held_flag(fields.next(), "outgoing")
        && fields.next().is_none()
}

impl InputDiagnostics {
    /// Capture-callback store: atomics only, no allocation, lock or I/O.
    pub fn record_held(&self, held: HeldTotals) {
        let packed = held.pack();
        if self.held.swap(packed, Relaxed) != packed {
            self.held_changes.fetch_add(1, Relaxed);
            self.held_changed_ms.store(crate::now_ms(), Relaxed);
        }
    }

    /// Read by the session worker, never by a native callback.
    pub fn held_report(
        &self,
        cause: HeldCause,
        gate: u64,
        pending: bool,
        outgoing: bool,
    ) -> HeldReport {
        let changes = self.held_changes.load(Relaxed);
        let changed = self.held_changed_ms.load(Relaxed);
        HeldReport {
            cause,
            at_ms: crate::now_ms().saturating_sub(self.started_ms),
            gate,
            held: HeldTotals::unpack(self.held.load(Relaxed)),
            changes,
            changed_ms: if changes == 0 {
                0
            } else {
                changed.saturating_sub(self.started_ms)
            },
            pending,
            outgoing,
        }
    }

    #[cfg(windows)]
    pub(crate) fn set_receive_phase(&self, phase: ReceiveLoopPhase) {
        self.receive_phase.store(phase as u32, Relaxed);
    }

    #[cfg(windows)]
    pub(crate) fn record_receive_timeout(&self, command: ReceiveCommand) {
        if self.active() {
            let code = command as u32 * 10 + self.receive_phase.load(Relaxed);
            let _ = self
                .first_receive_timeout
                .compare_exchange(0, code, Relaxed, Relaxed);
        }
    }

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

    /// Called by the session worker for acknowledged heartbeats, outside native callbacks.
    pub fn record_heartbeat_rtt(&self, duration: Duration) {
        if !self.active() {
            return;
        }
        let Ok(mut samples) = self.heartbeat_rtt_us.try_lock() else {
            return;
        };
        if samples.len() < MAX_HEARTBEAT_SAMPLES {
            samples.push(u32::try_from(duration.as_micros()).unwrap_or(u32::MAX));
        }
    }

    pub fn snapshot(&self, phase: Phase) -> Snapshot {
        let mut samples = self
            .heartbeat_rtt_us
            .lock()
            .map(|samples| samples.clone())
            .unwrap_or_default();
        samples.sort_unstable();
        let rank = (samples.len() * 95).div_ceil(100);
        let heartbeat_p95_us = samples.get(rank.saturating_sub(1)).copied().unwrap_or(0);
        Snapshot {
            phase,
            counts: std::array::from_fn(|i| self.counts[i].load(Relaxed)),
            heartbeat_samples: samples.len(),
            heartbeat_p95_us,
            #[cfg(windows)]
            first_receive_timeout: self.first_receive_timeout.load(Relaxed),
            #[cfg(not(windows))]
            first_receive_timeout: 0,
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
    heartbeat_samples: usize,
    heartbeat_p95_us: u32,
    first_receive_timeout: u32,
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
        write!(
            f,
            " hb_rtt_n={} hb_rtt_p95_us={}",
            self.heartbeat_samples, self.heartbeat_p95_us
        )?;
        if self.first_receive_timeout != 0 {
            write!(f, " first_rx_to={}", self.first_receive_timeout)?;
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
        && match fields.next() {
            None => true,
            Some(count) => {
                matches_counter(count, "hb_rtt_n")
                    && fields
                        .next()
                        .is_some_and(|field| matches_counter(field, "hb_rtt_p95_us"))
                    && fields.next().is_none_or(|field| {
                        field.strip_prefix("first_rx_to=").is_some_and(|value| {
                            matches!(value.as_bytes(), [b'1'..=b'5', b'0'..=b'5'])
                        }) && fields.next().is_none()
                    })
            }
        }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heartbeat_percentile_uses_the_nearest_rank_and_ends_with_diagnostics() {
        let diagnostics = InputDiagnostics::default();
        for micros in (1..=100).rev() {
            diagnostics.record_heartbeat_rtt(Duration::from_micros(micros));
        }
        diagnostics.stop();
        diagnostics.record_heartbeat_rtt(Duration::from_secs(10));
        let snapshot = diagnostics.snapshot(Phase::End);
        assert_eq!(snapshot.heartbeat_samples, 100);
        assert_eq!(snapshot.heartbeat_p95_us, 95);
        assert!(is_summary(&snapshot.to_string()));
    }

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
            heartbeat_samples: MAX_HEARTBEAT_SAMPLES,
            heartbeat_p95_us: u32::MAX,
            first_receive_timeout: 55,
        }
        .to_string();
        assert!(summary.len() <= MAX_SUMMARY_BYTES);
        assert!(is_summary(&summary));
        let (without_timeout, _) = summary.rsplit_once(' ').unwrap();
        assert!(is_summary(without_timeout));
        assert!(!is_summary(&format!("{without_timeout} first_rx_to=99")));
        let (missing_percentile, _) = without_timeout.rsplit_once(' ').unwrap();
        assert!(!is_summary(missing_percentile));
        let (without_latency, _) = missing_percentile.rsplit_once(' ').unwrap();
        assert!(is_summary(without_latency));
        let (receive_only, _) = without_latency.rsplit_once(' ').unwrap();
        assert!(is_summary(receive_only));
        let (legacy, _) = receive_only.rsplit_once(' ').unwrap();
        assert!(is_summary(legacy));
        assert!(!is_summary(&format!("{legacy} rx_coalesced=invalid")));
        assert!(!is_summary(&summary.replace("KVM-DIAG/5", "KVM-DIAG/4")));
        assert!(!is_summary(&(summary.clone() + " path=/private")));
        assert!(!is_summary(&summary.replace("cap=", "address=")));
    }

    #[test]
    fn held_record_reports_categories_changes_and_saturates() {
        let d = InputDiagnostics::default();
        let idle = d.held_report(HeldCause::Sample, 0, false, false);
        assert_eq!(idle.held, HeldTotals::default());
        assert_eq!((idle.changes, idle.changed_ms), (0, 0));
        let held = HeldTotals {
            modifiers: 1,
            keys: 2,
            unknown: 3,
            buttons: 0b10,
            extra: 4,
        };
        d.record_held(held);
        d.record_held(held);
        let report = d.held_report(HeldCause::BlockGate, 10, true, false);
        assert_eq!(report.held, held);
        assert_eq!(report.changes, 1);
        let line = report.to_string();
        assert!(line.starts_with("KVM-HELD/1 blk_gate t_ms="), "{line}");
        let fields = " gate=10 mods=1 keys=2 unk=3 btn=0x0002 extra=4 changes=1 ";
        assert!(line.contains(fields), "{line}");
        assert!(line.ends_with(" pending=1 outgoing=0"), "{line}");
        d.record_held(HeldTotals {
            keys: 5000,
            ..HeldTotals::default()
        });
        let saturated = d.held_report(HeldCause::Sample, 0, false, false);
        assert_eq!(saturated.held.keys, 0xfff);
        assert_eq!(saturated.changes, 2);
    }

    #[test]
    fn held_lines_are_strict_and_fit_their_bound() {
        let widest = HeldReport {
            cause: HeldCause::DenyBusy,
            at_ms: u64::MAX,
            gate: u64::MAX,
            held: HeldTotals {
                modifiers: u32::MAX,
                keys: u32::MAX,
                unknown: u32::MAX,
                buttons: u16::MAX,
                extra: u32::MAX,
            },
            changes: u32::MAX,
            changed_ms: u64::MAX,
            pending: true,
            outgoing: true,
        };
        let line = widest.to_string();
        assert!(line.len() <= MAX_HELD_BYTES, "{}", line.len());
        assert!(is_held_line(&line, false));
        assert!(!is_held_line(&line, true));
        let sample = HeldReport {
            cause: HeldCause::Sample,
            ..widest
        }
        .to_string();
        assert!(is_held_line(&sample, true));
        assert!(!is_held_line(&sample, false));
        for bad in [
            line.replace("KVM-HELD/1", "KVM-HELD/2"),
            line.replace("btn=0xffff", "btn=0xFFFF"),
            line.replace("btn=0xffff", "btn=0xfff"),
            line.replace("pending=1", "pending=2"),
            line.replace("mods=", "usage="),
            line.replace("gate=", "gate=-"),
            format!("{line} key=4"),
            format!("{line}\u{7}"),
            line.replace(" outgoing=1", ""),
            format!("{PEER_PREFIX}{line}"),
        ] {
            assert!(!is_held_line(&bad, false), "{bad}");
        }
    }
}
