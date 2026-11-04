//! One in-flight local recovery, acknowledged only by the existing capture hook.
//! Tags route events; native context/consent and the sealed receipt authorize them.
use super::*;

const DOWN_SEEN: u64 = 1;
const DOWN_ALLOWED: u64 = 2;
const UP_SEEN: u64 = 4;
const UP_ALLOWED: u64 = 8;
const CANCEL: u64 = 16;
const FAULT: u64 = 32;
const CLEANUP: u64 = 64;
const BLOCK_UP: u64 = 128;
const UNSET: u64 = u64::MAX;
const MAX_CLEANUP_ATTEMPTS: u64 = 3;
const CLICK_HOLD: Duration = Duration::from_millis(400);
const DISPATCH_GRACE: Duration = Duration::from_millis(250);

/// Sealed, non-Clone evidence of an entire non-cancelled native stroke. Public
/// Event fields/fixtures cannot construct it or change its original geometry.
pub struct RecoveryReceipt {
    pub(super) shared: Arc<Shared>,
    pub(super) sequence: u64,
    pub(super) key: AdmissionKey,
    pub(super) started: Instant,
    pub(super) ended: Instant,
    pub(super) point: RawPoint,
    pub(super) max_distance_sq: f64,
    pub(super) dpi: u32,
}
impl RecoveryReceipt {
    pub(crate) fn origin(&self) -> (AdmissionKey, Instant) {
        (self.key, self.started)
    }
    pub(crate) fn point(&self) -> (i32, i32, u32) {
        (self.point.x as i32, self.point.y as i32, self.dpi)
    }
    pub(crate) fn deadline(&self, original: Instant) -> Option<Instant> {
        if self.ended < self.started
            || self.ended.duration_since(self.started) > CLICK_HOLD
            || !(48..=768).contains(&self.dpi)
            || self.point.units != CoordinateUnits::WindowsPhysicalPixels
            || !self.point.valid()
            || self.point.x.fract() != 0.0
            || self.point.y.fract() != 0.0
        {
            return None;
        }
        let radius = 12.0 * f64::from(self.dpi) / 96.0;
        if !self.max_distance_sq.is_finite()
            || self.max_distance_sq < 0.0
            || self.max_distance_sq >= radius * radius
        {
            return None;
        }
        Some(original.min(self.ended.checked_add(DISPATCH_GRACE)?))
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryOutcome {
    /// Both events were inserted and allowed by this hook, not app completion.
    Submitted,
    Cancelled,
    /// Never automatically replay the click, even when cleanup later succeeds.
    UnknownOutcome,
}
#[derive(Clone, Copy)]
pub(crate) struct RecoveryCleanup {
    pub(crate) tag: usize,
}

pub(super) struct RecoveryState {
    phase: AtomicU64,
    word: AtomicU64,
    batch: AtomicU64,
    cleanup_count: AtomicU64,
    attempts: AtomicU64,
    down_tag: AtomicU64,
    up_tag: AtomicU64,
    sequence: AtomicU64,
    key: [AtomicU64; ADMISSION_WORDS],
    deadline: AtomicU64,
    point_x: AtomicU64,
    point_y: AtomicU64,
    completed_sequence: AtomicU64,
    completed_outcome: AtomicU64,
}
impl RecoveryState {
    pub(super) fn new() -> Self {
        Self {
            point_x: AtomicU64::new(0),
            point_y: AtomicU64::new(0),
            completed_sequence: AtomicU64::new(0),
            completed_outcome: AtomicU64::new(0),
            phase: AtomicU64::new(0),
            word: AtomicU64::new(0),
            batch: AtomicU64::new(UNSET),
            cleanup_count: AtomicU64::new(UNSET),
            attempts: AtomicU64::new(0),
            down_tag: AtomicU64::new(0),
            up_tag: AtomicU64::new(0),
            sequence: AtomicU64::new(0),
            key: std::array::from_fn(|_| AtomicU64::new(0)),
            deadline: AtomicU64::new(0),
        }
    }
    pub(super) fn begin(
        &self,
        sequence: u64,
        key: AdmissionKey,
        deadline: u64,
        tags: (usize, usize),
        point: (i32, i32),
    ) -> bool {
        self.begin_with(sequence, key, deadline, tags, point, || {})
    }
    fn begin_with(
        &self,
        sequence: u64,
        key: AdmissionKey,
        deadline: u64,
        tags: (usize, usize),
        point: (i32, i32),
        interleave: impl FnOnce(),
    ) -> bool {
        let old = self.phase.load(Ordering::Acquire);
        if tags.0 == 0
            || tags.1 == 0
            || tags.0 == tags.1
            || old & 3 != 0
            || self
                .phase
                .compare_exchange(old, 1, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            return false;
        }
        // Initializing is not observable as an active old or new transaction.
        interleave();
        self.word.store(0, Ordering::Relaxed);
        self.batch.store(UNSET, Ordering::Relaxed);
        self.cleanup_count.store(UNSET, Ordering::Relaxed);
        self.attempts.store(0, Ordering::Relaxed);
        self.point_x.store(point.0 as i64 as u64, Ordering::Relaxed);
        self.point_y.store(point.1 as i64 as u64, Ordering::Relaxed);
        self.sequence.store(sequence, Ordering::Relaxed);
        self.deadline.store(deadline, Ordering::Relaxed);
        for (slot, word) in self.key.iter().zip(key.0) {
            slot.store(word, Ordering::Relaxed);
        }
        self.up_tag.store(tags.1 as u64, Ordering::Relaxed);
        self.down_tag.store(tags.0 as u64, Ordering::Relaxed);
        // 1->2 or cancelled-in-initialization 5->6. The cancellation bit cannot
        // be erased by the fresh word reset above.
        self.phase.fetch_add(1, Ordering::Release);
        true
    }
    pub(super) fn is_active(&self) -> bool {
        self.phase.load(Ordering::Acquire) & 3 != 0
    }
    fn published(&self) -> bool {
        self.phase.load(Ordering::Acquire) & 3 == 2
    }
    pub(super) fn matches_tag(&self, tag: usize) -> bool {
        tag != 0
            && self.published()
            && (self.down_tag.load(Ordering::Acquire) == tag as u64
                || self.up_tag.load(Ordering::Acquire) == tag as u64)
    }
    pub(super) fn point_matches(&self, point: RawPoint) -> bool {
        point.units == CoordinateUnits::WindowsPhysicalPixels
            && point.x == (self.point_x.load(Ordering::Relaxed) as i64) as f64
            && point.y == (self.point_y.load(Ordering::Relaxed) as i64) as f64
    }
    pub(super) fn deadline(&self) -> u64 {
        self.deadline.load(Ordering::Acquire)
    }
    pub(super) fn sequence(&self) -> u64 {
        self.sequence.load(Ordering::Acquire)
    }
    pub(super) fn completed(&self, sequence: u64) -> Option<RecoveryOutcome> {
        if self.completed_sequence.load(Ordering::SeqCst) != sequence {
            return None;
        }
        let value = self.completed_outcome.load(Ordering::SeqCst);
        if self.completed_sequence.load(Ordering::SeqCst) != sequence {
            return None;
        }
        match value {
            1 => Some(RecoveryOutcome::Submitted),
            2 => Some(RecoveryOutcome::Cancelled),
            3 => Some(RecoveryOutcome::UnknownOutcome),
            _ => None,
        }
    }
    pub(super) fn key(&self) -> AdmissionKey {
        AdmissionKey(std::array::from_fn(|i| self.key[i].load(Ordering::Relaxed)))
    }
    pub(super) fn cancel(&self) {
        self.phase.fetch_or(4, Ordering::AcqRel);
        self.word.fetch_or(CANCEL, Ordering::AcqRel);
    }
    pub(super) fn uncertain(&self) {
        self.cancel();
        self.word.fetch_or(FAULT | BLOCK_UP, Ordering::AcqRel);
    }
    pub(super) fn faulted(&self) -> bool {
        self.word.load(Ordering::Acquire) & FAULT != 0
    }
    pub(super) fn quiescent(&self) -> bool {
        if !self.is_active() {
            return true;
        }
        if !self.published() {
            return false;
        }
        let count = self.batch.load(Ordering::Acquire);
        let word = self.word.load(Ordering::Acquire);
        if count > 2 {
            return false;
        }
        let extra = if word & CLEANUP != 0 {
            let extra = self.cleanup_count.load(Ordering::Acquire);
            if extra > 1 {
                return false;
            }
            extra
        } else {
            0
        };
        u64::from((word & (DOWN_SEEN | UP_SEEN)).count_ones()) == count + extra
    }
    pub(super) fn owns_down(&self) -> bool {
        let word = self.word.load(Ordering::Acquire);
        word & DOWN_ALLOWED != 0 && word & UP_ALLOWED == 0
    }
    pub(super) fn tagged(&self, tag: usize, down: Option<bool>, eligible: bool) -> Option<bool> {
        if tag == 0 || !self.published() {
            return None;
        }
        let is_down = tag as u64 == self.down_tag.load(Ordering::Acquire);
        let is_up = tag as u64 == self.up_tag.load(Ordering::Acquire);
        if !is_down && !is_up {
            return None;
        }
        if down != Some(is_down) {
            self.word.fetch_or(
                CANCEL | FAULT | if is_down { DOWN_SEEN } else { UP_SEEN },
                Ordering::AcqRel,
            );
            return Some(true);
        }
        let old = self.word.load(Ordering::Acquire);
        if is_down {
            if old & DOWN_SEEN != 0 {
                self.word.fetch_or(CANCEL | FAULT, Ordering::AcqRel);
                return Some(true);
            }
            let allow =
                eligible && old & CANCEL == 0 && self.phase.load(Ordering::Acquire) & 4 == 0;
            if self
                .word
                .compare_exchange(
                    old,
                    old | DOWN_SEEN | if allow { DOWN_ALLOWED } else { 0 },
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                return Some(!allow);
            }
            // The worker may cancel concurrently. Never reopen that decision.
            self.word.fetch_or(DOWN_SEEN, Ordering::AcqRel);
            Some(true)
        } else {
            if old & UP_SEEN != 0 {
                self.word.fetch_or(CANCEL | FAULT, Ordering::AcqRel);
                return Some(true);
            }
            let allow = old & DOWN_ALLOWED != 0 && old & BLOCK_UP == 0;
            self.word.fetch_or(
                UP_SEEN | if allow { UP_ALLOWED } else { CANCEL },
                Ordering::AcqRel,
            );
            Some(!allow)
        }
    }
    pub(super) fn submitted(&self, count: u32) {
        self.batch.store(u64::from(count), Ordering::Release);
        if count != 2 {
            self.cancel();
        }
    }
    pub(super) fn cleanup_request(&self) -> Option<RecoveryCleanup> {
        if !self.published() || self.batch.load(Ordering::Acquire) != 1 {
            return None;
        }
        let word = self.word.load(Ordering::Acquire);
        // count=1 and an observed Down identifies the inserted subset. No
        // prefix assumption: if only Up arrived it is blocked, never retried.
        if word & (DOWN_SEEN | DOWN_ALLOWED | UP_SEEN | CLEANUP | FAULT)
            != (DOWN_SEEN | DOWN_ALLOWED)
            || self.attempts.load(Ordering::Acquire) >= MAX_CLEANUP_ATTEMPTS
        {
            return None;
        }
        self.word
            .compare_exchange(word, word | CLEANUP, Ordering::AcqRel, Ordering::Acquire)
            .ok()?;
        self.cleanup_count.store(UNSET, Ordering::Release);
        self.attempts.fetch_add(1, Ordering::AcqRel);
        Some(RecoveryCleanup {
            tag: self.up_tag.load(Ordering::Acquire) as usize,
        })
    }
    pub(super) fn cleanup_submitted(&self, request: RecoveryCleanup, count: u32) {
        if request.tag as u64 != self.up_tag.load(Ordering::Acquire) || !self.published() {
            return;
        }
        self.cleanup_count
            .store(u64::from(count), Ordering::Release);
        if count == 0 {
            let old = self.word.load(Ordering::Acquire);
            if old & UP_SEEN == 0 {
                self.word.fetch_and(!CLEANUP, Ordering::AcqRel);
            } else {
                self.word.fetch_or(FAULT, Ordering::AcqRel);
            }
        } else if count != 1 {
            self.word.fetch_or(FAULT, Ordering::AcqRel);
        }
    }
    fn resolution(&self) -> Option<RecoveryOutcome> {
        if !self.published() {
            return None;
        }
        let word = self.word.load(Ordering::Acquire);
        let count = self.batch.load(Ordering::Acquire);
        if word & FAULT != 0 {
            return None;
        }
        match count {
            0 if word & (DOWN_SEEN | UP_SEEN) == 0 => Some(RecoveryOutcome::Cancelled),
            1 => match word & (DOWN_SEEN | UP_SEEN) {
                DOWN_SEEN if word & DOWN_ALLOWED == 0 => Some(RecoveryOutcome::Cancelled),
                UP_SEEN if word & UP_ALLOWED == 0 => Some(RecoveryOutcome::Cancelled),
                both if both == (DOWN_SEEN | UP_SEEN)
                    && word & (CLEANUP | UP_ALLOWED) == (CLEANUP | UP_ALLOWED)
                    && self.cleanup_count.load(Ordering::Acquire) == 1 =>
                {
                    Some(RecoveryOutcome::UnknownOutcome)
                }
                _ => None,
            },
            2 if word & (DOWN_SEEN | UP_SEEN) == (DOWN_SEEN | UP_SEEN) => {
                match word & (DOWN_ALLOWED | UP_ALLOWED) {
                    0 => Some(RecoveryOutcome::Cancelled),
                    both if both == (DOWN_ALLOWED | UP_ALLOWED) => Some(RecoveryOutcome::Submitted),
                    _ => None,
                }
            }
            _ => None,
        }
    }
    pub(super) fn finish(&self) -> Option<(u64, RecoveryOutcome)> {
        self.finish_with(|| {})
    }
    fn finish_with(&self, interleave: impl FnOnce()) -> Option<(u64, RecoveryOutcome)> {
        let outcome = self.resolution()?;
        let phase = self.phase.load(Ordering::Acquire);
        if phase & 3 != 2
            || self
                .phase
                .compare_exchange(phase, (phase & !3) | 3, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            return None;
        }
        interleave();
        self.down_tag.store(0, Ordering::Release);
        self.up_tag.store(0, Ordering::Release);
        let sequence = self.sequence.load(Ordering::Acquire);
        self.completed_sequence.store(0, Ordering::SeqCst);
        self.completed_outcome.store(
            match outcome {
                RecoveryOutcome::Submitted => 1,
                RecoveryOutcome::Cancelled => 2,
                RecoveryOutcome::UnknownOutcome => 3,
            },
            Ordering::SeqCst,
        );
        self.completed_sequence.store(sequence, Ordering::SeqCst);
        self.phase.store(0, Ordering::Release);
        Some((sequence, outcome))
    }
}

impl GestureControl {
    pub fn recovery_pending(&self) -> bool {
        self.shared.recovery.is_active()
    }
    #[cfg(windows)]
    pub(crate) fn begin_recovery(
        &mut self,
        receipt: RecoveryReceipt,
        deadline: Instant,
        tags: (usize, usize),
    ) -> Result<u64, peerglide_actions::execution::NativeFailure> {
        use peerglide_actions::execution::NativeFailure;
        if !Arc::ptr_eq(&self.shared, &receipt.shared)
            || !self.source.matches_admission(receipt.key)
        {
            return Err(NativeFailure::TargetLost);
        }
        let now = now_ms();
        let until = admission_expiry(deadline, now, Instant::now())
            .ok_or(NativeFailure::BeforeSubmission)?;
        let old = self.shared.latch.load(Ordering::Acquire);
        if old & (BUSY | PUBLISHING | RECOVERING) != 0 {
            return Err(NativeFailure::BeforeSubmission);
        }
        self.shared
            .latch
            .compare_exchange(
                old,
                (old & !ARMED) | RECOVERING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map_err(|_| NativeFailure::BeforeSubmission)?;
        let valid = !self.shared.stop.load(Ordering::Acquire)
            && !self.shared.lost.load(Ordering::Acquire)
            && self.shared.deadline.load(Ordering::Acquire) > now
            && self.shared.physical_held.load(Ordering::Acquire) == 0
            && !self.shared.debt.load(Ordering::Acquire)
            && self.shared.cancelled.load(Ordering::Acquire) < receipt.sequence
            && self.shared.retired.load(Ordering::Acquire) < receipt.sequence
            && self.source.matches_admission(receipt.key);
        if !valid
            || !self.shared.recovery.begin(
                receipt.sequence,
                receipt.key,
                until,
                tags,
                (receipt.point.x as i32, receipt.point.y as i32),
            )
        {
            self.shared.latch.fetch_and(!RECOVERING, Ordering::AcqRel);
            return Err(NativeFailure::BeforeSubmission);
        }
        Ok(receipt.sequence)
    }
    #[cfg(windows)]
    pub(crate) fn recovery_submitted(&mut self, count: u32) {
        self.shared.recovery.submitted(count);
    }
    #[cfg(windows)]
    pub(crate) fn cancel_recovery(&mut self) {
        self.shared.recovery.cancel();
    }
    #[cfg(windows)]
    pub(crate) fn last_recovery(&self, sequence: u64) -> Option<RecoveryOutcome> {
        self.shared.recovery.completed(sequence)
    }
    #[cfg(windows)]
    pub(crate) fn finish_recovery(&mut self) -> Option<RecoveryOutcome> {
        let (sequence, outcome) = self.shared.recovery.finish()?;
        self.shared.cancelled.fetch_max(sequence, Ordering::AcqRel);
        self.shared.retired.fetch_max(sequence, Ordering::AcqRel);
        self.shared.latch.fetch_and(!RECOVERING, Ordering::AcqRel);
        self.shared.rearm.store(true, Ordering::Release);
        self.source.invalidate();
        Some(outcome)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fresh() -> RecoveryState {
        let state = RecoveryState::new();
        assert!(state.begin(
            1,
            AdmissionKey([1; ADMISSION_WORDS]),
            100,
            (11, 12),
            (10, 20)
        ));
        state
    }
    #[test]
    fn initialization_and_finish_are_exclusive_across_slot_reuse() {
        let state = fresh();
        state.submitted(0);
        assert_eq!(state.finish(), Some((1, RecoveryOutcome::Cancelled)));
        assert!(state.begin_with(
            2,
            AdmissionKey([2; ADMISSION_WORDS]),
            200,
            (21, 22),
            (10, 20),
            || {
                assert_eq!(state.finish(), None);
                assert!(!state.matches_tag(11));
                state.cancel();
            }
        ));
        assert_eq!(state.tagged(21, Some(true), true), Some(true));
        assert_eq!(state.tagged(22, Some(false), true), Some(true));
        state.submitted(2);
        assert_eq!(
            state.finish_with(|| {
                assert!(!state.begin(
                    3,
                    AdmissionKey([3; ADMISSION_WORDS]),
                    300,
                    (31, 32),
                    (10, 20)
                ));
            }),
            Some((2, RecoveryOutcome::Cancelled))
        );
        assert_eq!(state.completed(2), Some(RecoveryOutcome::Cancelled));
        assert!(state.begin(
            3,
            AdmissionKey([3; ADMISSION_WORDS]),
            300,
            (31, 32),
            (10, 20)
        ));
        assert_eq!(state.tagged(31, Some(true), true), Some(false));
    }
    #[test]
    fn full_batch_only_submits_after_both_locally_allowed_receipts() {
        let state = fresh();
        assert_eq!(state.tagged(0, Some(true), true), None);
        assert_eq!(state.tagged(55, Some(true), true), None);
        assert_eq!(state.tagged(11, Some(true), true), Some(false));
        assert!(state.owns_down());
        assert_eq!(state.resolution(), None);
        assert_eq!(state.tagged(12, Some(false), false), Some(false));
        assert!(!state.owns_down());
        state.submitted(2);
        assert_eq!(state.resolution(), Some(RecoveryOutcome::Submitted));
        assert_eq!(state.finish(), Some((1, RecoveryOutcome::Submitted)));
        assert_eq!(state.finish(), None);
    }
    #[test]
    fn held_physical_input_or_cancel_before_down_blocks_both() {
        for cancel in [false, true] {
            let state = fresh();
            if cancel {
                state.cancel();
            }
            assert_eq!(state.tagged(11, Some(true), cancel), Some(true));
            assert_eq!(state.tagged(12, Some(false), true), Some(true));
            state.submitted(2);
            assert_eq!(state.resolution(), Some(RecoveryOutcome::Cancelled));
            assert!(state.cleanup_request().is_none());
        }
    }
    #[test]
    fn partial_subset_is_learned_from_receipts_never_count_prefix() {
        let state = fresh();
        state.submitted(1);
        assert!(state.resolution().is_none());
        assert!(state.cleanup_request().is_none());
        assert_eq!(state.tagged(12, Some(false), true), Some(true));
        assert_eq!(state.resolution(), Some(RecoveryOutcome::Cancelled));
        assert!(state.cleanup_request().is_none());
        let state = fresh();
        assert_eq!(state.tagged(11, Some(true), false), Some(true));
        state.submitted(1);
        assert_eq!(state.resolution(), Some(RecoveryOutcome::Cancelled));
        assert!(state.cleanup_request().is_none());
        let state = fresh();
        assert_eq!(state.tagged(11, Some(true), true), Some(false));
        state.submitted(1);
        let request = state.cleanup_request().unwrap();
        assert!(state.cleanup_request().is_none());
        state.cleanup_submitted(request, 1);
        assert_eq!(state.resolution(), None);
        assert_eq!(state.tagged(12, Some(false), false), Some(false));
        assert_eq!(state.resolution(), Some(RecoveryOutcome::UnknownOutcome));
        assert!(!state.owns_down());
    }
    #[test]
    fn late_or_missing_receipts_remain_pending_without_replaying_click() {
        let state = fresh();
        state.submitted(2);
        state.cancel();
        assert!(state.resolution().is_none());
        assert!(state.cleanup_request().is_none());
        assert_eq!(state.tagged(11, Some(true), true), Some(true));
        assert_eq!(state.tagged(12, Some(false), true), Some(true));
        assert_eq!(state.resolution(), Some(RecoveryOutcome::Cancelled));
        let state = fresh();
        state.tagged(11, Some(true), true);
        state.submitted(2);
        state.cancel();
        assert!(state.cleanup_request().is_none());
        assert!(state.owns_down());
        state.tagged(12, Some(false), false);
        assert_eq!(state.resolution(), Some(RecoveryOutcome::Submitted));
    }
    #[test]
    fn cleanup_only_retries_known_zero_insertions_and_is_bounded() {
        let state = fresh();
        state.tagged(11, Some(true), true);
        state.submitted(1);
        for _ in 0..MAX_CLEANUP_ATTEMPTS {
            let request = state.cleanup_request().unwrap();
            assert!(!state.quiescent());
            state.cleanup_submitted(request, 0);
            assert!(state.owns_down());
        }
        assert!(state.cleanup_request().is_none());
        assert!(state.resolution().is_none());
        let state = fresh();
        state.tagged(11, Some(true), true);
        state.submitted(1);
        let request = state.cleanup_request().unwrap();
        state.cleanup_submitted(request, 1);
        assert!(state.cleanup_request().is_none());
        assert!(state.resolution().is_none());
    }
    #[test]
    fn wrong_message_duplicate_receipt_or_impossible_count_never_clear_debt() {
        let state = fresh();
        assert_eq!(state.tagged(11, None, true), Some(true));
        state.submitted(2);
        assert!(state.resolution().is_none());
        let state = fresh();
        state.tagged(11, Some(true), true);
        assert_eq!(state.tagged(11, Some(true), true), Some(true));
        state.submitted(2);
        assert!(state.resolution().is_none());
        assert!(state.owns_down());
        let state = fresh();
        state.submitted(3);
        assert!(state.resolution().is_none());
    }
}
