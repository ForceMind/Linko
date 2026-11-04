//! Allocation-free native G03 short path; recognition/recovery stay in the worker.
//! No product binary enables this path. No second native input hook is installed.
use crate::{
    context::actions::{
        ActionTrigger, ActionTriggerSource, AdmissionKey, CaptureAdmission, ADMISSION_WORDS,
    },
    gesture_queue::{self, Consumer, Producer},
    now_ms,
};
use std::{
    cell::Cell,
    marker::PhantomData,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

pub const CAPACITY: usize = 256;
const WATCHDOG_MS: u64 = 1000;
const BUSY: u64 = 1;
const PUBLISHING: u64 = 2;
const ARMED: u64 = 4;
const RECOVERING: u64 = 8;
const EVENT_STEP: u64 = 16;
const MAX_HOLD: Duration = Duration::from_millis(2500);
#[path = "gesture_recovery.rs"]
mod recovery;
pub(crate) use recovery::RecoveryCleanup;
pub use recovery::{RecoveryOutcome, RecoveryReceipt};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoordinateUnits {
    WindowsPhysicalPixels,
    MacDisplayPoints,
}
#[derive(Clone, Copy)]
pub struct RawPoint {
    pub x: f64,
    pub y: f64,
    pub units: CoordinateUnits,
}
impl RawPoint {
    fn valid(self) -> bool {
        self.x.is_finite()
            && self.y.is_finite()
            && self.x.abs() <= 1_000_000.0
            && self.y.abs() <= 1_000_000.0
    }
}
/// No Debug/Serialize: raw points/opaque trigger context remain process-local.
pub enum Event<T> {
    Begin {
        sequence: u64,
        trigger: T,
        point: RawPoint,
        at: Instant,
    },
    Move {
        sequence: u64,
        point: RawPoint,
        at: Instant,
    },
    End {
        sequence: u64,
        point: RawPoint,
        at: Instant,
        recovery: RecoveryReceipt,
    },
}
impl<T> Event<T> {
    pub fn sequence(&self) -> u64 {
        match self {
            Self::Begin { sequence, .. }
            | Self::Move { sequence, .. }
            | Self::End { sequence, .. } => *sequence,
        }
    }
}
pub type GestureEvent = Event<ActionTrigger>;

struct Shared {
    recovery: recovery::RecoveryState,
    dpi: AtomicU64,
    latch: AtomicU64,
    admission: [AtomicU64; ADMISSION_WORDS],
    deadline: AtomicU64,
    admission_deadline: AtomicU64,
    stop: AtomicBool,
    debt: AtomicBool,
    lost: AtomicBool,
    edges: AtomicBool,
    rearm: AtomicBool,
    physical_held: AtomicU64,
    cancelled: AtomicU64,
    retired: AtomicU64,
}
impl Shared {
    fn new() -> Self {
        Self {
            recovery: recovery::RecoveryState::new(),
            dpi: AtomicU64::new(96),
            admission_deadline: AtomicU64::new(0),
            latch: AtomicU64::new(0),
            admission: std::array::from_fn(|_| AtomicU64::new(0)),
            deadline: AtomicU64::new(now_ms().saturating_add(WATCHDOG_MS)),
            stop: AtomicBool::new(false),
            debt: AtomicBool::new(false),
            lost: AtomicBool::new(false),
            edges: AtomicBool::new(false),
            rearm: AtomicBool::new(false),
            physical_held: AtomicU64::new(0),
            cancelled: AtomicU64::new(0),
            retired: AtomicU64::new(0),
        }
    }
    fn disarm(&self) {
        self.latch.fetch_and(!ARMED, Ordering::AcqRel);
    }
    #[cfg(test)]
    fn is_armed(&self) -> bool {
        self.latch.load(Ordering::Acquire) & ARMED != 0
    }
    fn event(self: &Arc<Self>) -> EventGuard {
        // A single capture thread owns events. BUSY prevents a new publisher;
        // the epoch makes a completed intervening event visible (no ready ABA).
        self.latch.fetch_or(BUSY, Ordering::AcqRel);
        let old = self.latch.fetch_add(EVENT_STEP, Ordering::AcqRel);
        if old > u64::MAX - EVENT_STEP {
            self.stop.store(true, Ordering::Release);
            self.disarm();
        }
        EventGuard(self.clone())
    }
    fn admission(&self, now: u64) -> Option<(AdmissionKey, u64, u32)> {
        if self.stop.load(Ordering::Acquire)
            || self.deadline.load(Ordering::Acquire) <= now
            || self.admission_deadline.load(Ordering::Acquire) <= now
        {
            return None;
        }
        let state = self.latch.load(Ordering::Acquire);
        if state & (PUBLISHING | ARMED) != ARMED {
            return None;
        }
        let words = std::array::from_fn(|index| self.admission[index].load(Ordering::Relaxed));
        if self.latch.load(Ordering::Acquire) != state {
            return None;
        }
        Some((
            AdmissionKey(words),
            state,
            self.dpi.load(Ordering::Relaxed) as u32,
        ))
    }
    #[cfg(test)]
    fn publish(&self, key: AdmissionKey, now: u64) -> Result<(), CaptureError> {
        self.publish_with(key, now, || {})
    }
    #[cfg(test)]
    fn publish_with(
        &self,
        key: AdmissionKey,
        now: u64,
        interleave: impl FnOnce(),
    ) -> Result<(), CaptureError> {
        self.publish_until(key, now, now.saturating_add(5000), interleave)
    }
    #[cfg(test)]
    fn publish_until(
        &self,
        key: AdmissionKey,
        now: u64,
        until: u64,
        interleave: impl FnOnce(),
    ) -> Result<(), CaptureError> {
        self.publish_frame(key, now, until, 96, interleave)
    }
    fn publish_frame(
        &self,
        key: AdmissionKey,
        now: u64,
        until: u64,
        dpi: u32,
        interleave: impl FnOnce(),
    ) -> Result<(), CaptureError> {
        if self.stop.load(Ordering::Acquire) || self.lost.load(Ordering::Acquire) {
            return Err(CaptureError::Stopped);
        }
        let old = self.latch.load(Ordering::Acquire);
        if old & (BUSY | PUBLISHING | RECOVERING) != 0 {
            return Err(CaptureError::Held);
        }
        let publishing = (old & !ARMED) | PUBLISHING;
        self.latch
            .compare_exchange(old, publishing, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| CaptureError::Held)?;
        if self.physical_held.load(Ordering::Acquire) != 0 || self.debt.load(Ordering::Acquire) {
            self.latch.fetch_and(!PUBLISHING, Ordering::AcqRel);
            return Err(CaptureError::Held);
        }
        for (slot, word) in self.admission.iter().zip(key.0) {
            slot.store(word, Ordering::Relaxed);
        }
        self.deadline
            .store(now.saturating_add(WATCHDOG_MS), Ordering::Release);
        self.admission_deadline.store(until, Ordering::Release);
        self.dpi.store(u64::from(dpi), Ordering::Relaxed);
        interleave();
        // Any callback during publication changes the epoch, even if it ended.
        // No completed callback can let this old publish reopen the one-use gate.
        if self.stop.load(Ordering::Acquire)
            || self
                .latch
                .compare_exchange(
                    publishing,
                    (old & !ARMED) | ARMED,
                    Ordering::Release,
                    Ordering::Acquire,
                )
                .is_err()
        {
            self.latch
                .fetch_and(!(PUBLISHING | ARMED), Ordering::AcqRel);
            return Err(CaptureError::Held);
        }
        Ok(())
    }
}
struct EventGuard(Arc<Shared>);
impl Drop for EventGuard {
    fn drop(&mut self) {
        self.0.latch.fetch_and(!BUSY, Ordering::Release);
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureError {
    Held,
    Stopped,
    OwnershipLost,
}
/// A single worker publishes native/rule-verified admission frames. arm is not
/// consent and cannot be built from a rule enabled flag alone.
pub struct GestureControl {
    shared: Arc<Shared>,
    source: ActionTriggerSource,
    _single: PhantomData<Cell<()>>,
}
fn admission_expiry(expires: Instant, clock_before_sample: u64, sampled: Instant) -> Option<u64> {
    let remaining = expires.saturating_duration_since(sampled).as_millis() as u64;
    (remaining != 0).then(|| clock_before_sample.saturating_add(remaining))
}
impl GestureControl {
    pub fn arm(&mut self, frame: CaptureAdmission) -> Result<(), CaptureError> {
        if !frame.is_current(&self.source) {
            return Err(CaptureError::Stopped);
        }
        // Sample the relative clock FIRST. A pause before the later Instant
        // sample can only shorten admission; it must never renew its expiry.
        let now = now_ms();
        let until =
            admission_expiry(frame.expires(), now, Instant::now()).ok_or(CaptureError::Stopped)?;
        self.shared
            .publish_frame(frame.key(), now, until, frame.dpi(), || {})
    }
    pub fn heartbeat(&mut self) {
        if !self.shared.stop.load(Ordering::Acquire) {
            self.shared
                .deadline
                .store(now_ms().saturating_add(WATCHDOG_MS), Ordering::Release);
        }
    }
    /// Call only after that stroke's action/recovery was submitted or cancelled.
    pub fn retire(&mut self, sequence: u64) {
        self.shared.retired.fetch_max(sequence, Ordering::Release);
    }
    pub fn stop(&self) {
        self.shared.recovery.cancel();
        self.shared.disarm();
        self.shared.stop.store(true, Ordering::Release);
        self.source.invalidate();
    }
    pub fn has_right_debt(&self) -> bool {
        self.shared.debt.load(Ordering::Acquire)
    }
    pub fn ownership_lost(&self) -> bool {
        self.shared.lost.load(Ordering::Acquire)
    }
    pub fn edges_blocked(&self) -> bool {
        self.shared.edges.load(Ordering::Acquire)
    }
    pub fn take_edge_rearm(&mut self) -> bool {
        self.shared.rearm.swap(false, Ordering::AcqRel)
    }
    pub fn cancelled_through(&self) -> u64 {
        self.shared.cancelled.load(Ordering::Acquire)
    }
}
impl Drop for GestureControl {
    fn drop(&mut self) {
        self.stop();
    }
}
pub struct GestureReceiver {
    queue: Consumer<GestureEvent>,
    shared: Arc<Shared>,
}
impl GestureReceiver {
    pub fn try_recv(&mut self) -> Option<GestureEvent> {
        for _ in 0..CAPACITY {
            let event = self.queue.try_pop()?;
            if event.sequence() > self.shared.cancelled.load(Ordering::Acquire) {
                return Some(event);
            }
        }
        None
    }
    pub fn cancelled_through(&self) -> u64 {
        self.shared.cancelled.load(Ordering::Acquire)
    }
    pub fn producer_alive(&self) -> bool {
        self.queue.is_producer_alive()
    }
}

trait TriggerProducer: Send {
    type Trigger: Send;
    fn mark(&self) -> Option<(Self::Trigger, AdmissionKey, Instant)>;
    fn current(&self, key: AdmissionKey) -> bool;
    fn invalidate(&self);
    fn emergency(&self);
    fn revoked(&self) -> bool;
}
impl TriggerProducer for ActionTriggerSource {
    type Trigger = ActionTrigger;
    fn mark(&self) -> Option<(ActionTrigger, AdmissionKey, Instant)> {
        let trigger = ActionTriggerSource::mark(self).ok()?;
        let key = trigger.frame_key();
        let at = trigger.started();
        Some((trigger, key, at))
    }
    fn current(&self, key: AdmissionKey) -> bool {
        self.matches_admission(key)
    }
    fn invalidate(&self) {
        ActionTriggerSource::invalidate(self);
    }
    fn emergency(&self) {
        self.emergency_revoke();
    }
    fn revoked(&self) -> bool {
        self.consent_revoked()
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Idle,
    BypassHeld,
    Candidate,
    CancelledHeld,
}
/// The only mutable owner is the existing capture thread; notifications never
/// produce SPSC messages. No recognition/OS queries or dynamic storage here.
struct Broker<S: TriggerProducer> {
    source: S,
    queue: Producer<Event<S::Trigger>>,
    shared: Arc<Shared>,
    phase: Phase,
    sequence: u64,
    key: Option<AdmissionKey>,
    started: Option<Instant>,
    first: Option<RawPoint>,
    max_distance_sq: f64,
    dpi: u32,
    keys: [u64; 1024],
    key_count: u64,
    buttons: u8,
}
impl<S: TriggerProducer> Broker<S> {
    fn initialize(
        &mut self,
        keys: impl IntoIterator<Item = u16>,
        buttons: impl IntoIterator<Item = u8>,
    ) {
        for key in keys {
            self.set_key(key, true);
        }
        for button in buttons {
            self.buttons |= if (1..=5).contains(&button) {
                1 << (button - 1)
            } else {
                0
            };
        }
        self.update_held();
        if self.buttons & 2 != 0 {
            self.phase = Phase::BypassHeld;
        }
    }
    fn tagged_right(&mut self, tag: usize, down: Option<bool>, point: RawPoint) -> Option<bool> {
        if !self.shared.recovery.matches_tag(tag) {
            return None;
        }
        let eligible = self.shared.recovery.point_matches(point)
            && !self.shared.stop.load(Ordering::Acquire)
            && !self.shared.lost.load(Ordering::Acquire)
            && self.shared.deadline.load(Ordering::Acquire) > now_ms()
            && self.shared.recovery.deadline() > now_ms()
            && self.held() == 0
            && !self.shared.debt.load(Ordering::Acquire)
            && self.shared.cancelled.load(Ordering::Acquire) < self.shared.recovery.sequence()
            && self.source.current(self.shared.recovery.key());
        let result = self.shared.recovery.tagged(tag, down, eligible);
        if self.shared.recovery.faulted() {
            self.shared.lost.store(true, Ordering::Release);
            self.shared.stop.store(true, Ordering::Release);
            self.shared.recovery.uncertain();
            self.source.invalidate();
        }
        result
    }
    fn foreign_input(&mut self, right: bool, motion: bool) -> bool {
        let owns = self.shared.recovery.owns_down();
        if right && owns {
            self.shared.lost.store(true, Ordering::Release);
            self.shared.stop.store(true, Ordering::Release);
            self.shared.recovery.uncertain();
            self.cancel();
            return false;
        }
        self.other();
        motion && owns && !self.shared.lost.load(Ordering::Acquire)
    }
    fn cleanup_request(&mut self) -> Option<RecoveryCleanup> {
        if let Some((sequence, _)) = self.shared.recovery.finish() {
            self.shared.cancelled.fetch_max(sequence, Ordering::AcqRel);
            self.shared.retired.fetch_max(sequence, Ordering::AcqRel);
            self.shared.latch.fetch_and(!RECOVERING, Ordering::AcqRel);
            self.shared.rearm.store(true, Ordering::Release);
            self.source.invalidate();
        }
        if self.shared.lost.load(Ordering::Acquire) {
            None
        } else {
            self.shared.recovery.cleanup_request()
        }
    }
    fn held(&self) -> u64 {
        self.key_count + u64::from(self.buttons.count_ones())
    }
    fn update_held(&self) {
        self.shared
            .physical_held
            .store(self.held(), Ordering::Release);
    }
    fn set_key(&mut self, key: u16, down: bool) {
        let index = usize::from(key) / 64;
        let mask = 1u64 << (key % 64);
        let old = self.keys[index] & mask != 0;
        if down && !old {
            self.keys[index] |= mask;
            self.key_count += 1;
        } else if !down && old {
            self.keys[index] &= !mask;
            self.key_count -= 1;
        }
        self.update_held();
    }
    fn contains(&self, key: u16) -> bool {
        self.keys[usize::from(key) / 64] & (1u64 << (key % 64)) != 0
    }
    fn cancel(&mut self) {
        self.shared.recovery.cancel();
        self.shared.disarm();
        if self.sequence > 0 {
            self.shared
                .cancelled
                .store(self.sequence, Ordering::Release);
        }
        self.shared.edges.store(false, Ordering::Release);
        if self.phase == Phase::Candidate {
            self.phase = Phase::CancelledHeld;
            self.shared.rearm.store(true, Ordering::Release);
        }
        self.source.invalidate();
    }
    fn other(&mut self) {
        self.shared.recovery.cancel();
        self.shared.disarm();
        if self.phase == Phase::Candidate
            || self.sequence > self.shared.retired.load(Ordering::Acquire)
        {
            self.cancel();
        }
    }
    fn key(&mut self, key: u16, down: bool) {
        let _event = self.shared.event();
        self.set_key(key, down);
        let emergency = down
            && key == 0x29
            && (self.contains(0xe0) || self.contains(0xe4))
            && (self.contains(0xe2) || self.contains(0xe6));
        if emergency {
            self.shared.recovery.cancel();
            self.source.emergency();
            self.shared.stop.store(true, Ordering::Release);
            self.cancel();
        } else {
            self.other();
        }
    }
    fn tick(&mut self, now: Instant, clock: u64) {
        if self.shared.stop.load(Ordering::Acquire)
            || !self.queue.is_consumer_alive()
            || self.source.revoked()
            || self.shared.deadline.load(Ordering::Acquire) <= clock
        {
            self.shared.stop.store(true, Ordering::Release);
            self.cancel();
        }
        if self.phase == Phase::Candidate
            && (self.shared.stop.load(Ordering::Acquire)
                || self.shared.deadline.load(Ordering::Acquire) <= clock
                || self.key.is_none_or(|key| !self.source.current(key))
                || self
                    .started
                    .is_none_or(|start| now < start || now.duration_since(start) >= MAX_HOLD))
        {
            self.cancel();
        }
    }
    fn right(
        &mut self,
        down: bool,
        point: RawPoint,
        now: Instant,
        clock: u64,
        remote: bool,
    ) -> bool {
        let _event = self.shared.event();
        self.tick(now, clock);
        if down {
            self.buttons |= 2;
        } else {
            self.buttons &= !2;
        }
        self.update_held();
        // A physical down arriving before our injected down is passed and then
        // makes that injected down ineligible. After an allowed owned down the
        // overlapping physical pair is suppressed, retaining its owed up.
        if down
            && self.phase == Phase::Idle
            && !self.shared.lost.load(Ordering::Acquire)
            && self.shared.recovery.owns_down()
        {
            self.cancel();
            self.phase = Phase::CancelledHeld;
            self.shared.debt.store(true, Ordering::Release);
            return true;
        }
        // Pair ownership is resolved before eligibility, stop or admission.
        match self.phase {
            Phase::BypassHeld => {
                if !down {
                    self.phase = Phase::Idle;
                }
                return false;
            }
            Phase::CancelledHeld => {
                if !down {
                    self.phase = Phase::Idle;
                    self.shared.debt.store(false, Ordering::Release);
                }
                return true;
            }
            Phase::Candidate => {
                if down {
                    if !point.valid() || remote {
                        self.cancel();
                    } else {
                        self.measure(point);
                    }
                    return true;
                }
                let sequence = self.sequence;
                self.measure(point);
                let receipt = RecoveryReceipt {
                    shared: self.shared.clone(),
                    sequence,
                    key: self.key.expect("candidate frame"),
                    started: self.started.expect("candidate clock"),
                    ended: now,
                    point,
                    max_distance_sq: self.max_distance_sq,
                    dpi: self.dpi,
                };
                if !point.valid()
                    || remote
                    || self
                        .queue
                        .try_push(Event::End {
                            sequence,
                            point,
                            at: now,
                            recovery: receipt,
                        })
                        .is_err()
                {
                    self.cancel();
                }
                self.phase = Phase::Idle;
                self.key = None;
                self.started = None;
                self.shared.debt.store(false, Ordering::Release);
                self.shared.edges.store(false, Ordering::Release);
                self.shared.rearm.store(true, Ordering::Release);
                return true;
            }
            Phase::Idle => {}
        }
        if !down {
            return false;
        }
        if self.sequence > self.shared.retired.load(Ordering::Acquire) {
            self.cancel();
            self.phase = Phase::BypassHeld;
            return false;
        }
        let admission = self.shared.admission(clock);
        if remote
            || self.key_count != 0
            || self.buttons != 2
            || !point.valid()
            || !self.queue.is_consumer_alive()
        {
            self.phase = Phase::BypassHeld;
            return false;
        }
        let Some((admission, state, dpi)) = admission else {
            self.phase = Phase::BypassHeld;
            return false;
        };
        if self
            .shared
            .latch
            .compare_exchange(state, state & !ARMED, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            self.phase = Phase::BypassHeld;
            return false;
        }
        let Some((trigger, key, started)) = self.source.mark() else {
            self.phase = Phase::BypassHeld;
            return false;
        };
        if key != admission || !self.source.current(key) {
            self.phase = Phase::BypassHeld;
            return false;
        }
        let Some(sequence) = self.sequence.checked_add(1) else {
            self.shared.stop.store(true, Ordering::Release);
            self.phase = Phase::BypassHeld;
            return false;
        };
        if self
            .queue
            .try_push(Event::Begin {
                sequence,
                trigger,
                point,
                at: started,
            })
            .is_err()
        {
            self.source.invalidate();
            self.phase = Phase::BypassHeld;
            return false;
        }
        self.first = Some(point);
        self.max_distance_sq = 0.0;
        self.dpi = dpi;
        self.sequence = sequence;
        self.key = Some(key);
        self.started = Some(started);
        self.phase = Phase::Candidate;
        self.shared.debt.store(true, Ordering::Release);
        self.shared.edges.store(true, Ordering::Release);
        true
    }
    fn measure(&mut self, point: RawPoint) {
        if let Some(first) = self.first {
            let dx = point.x - first.x;
            let dy = point.y - first.y;
            self.max_distance_sq = self.max_distance_sq.max(dx * dx + dy * dy);
        }
    }
    fn motion(&mut self, point: RawPoint, now: Instant, clock: u64) -> bool {
        let _event = self.shared.event();
        self.tick(now, clock);
        if self.phase == Phase::Idle && self.sequence > self.shared.retired.load(Ordering::Acquire)
        {
            self.other();
        }
        let block = self.shared.recovery.owns_down() && !self.shared.lost.load(Ordering::Acquire);
        if block {
            self.shared.recovery.cancel();
        }
        if self.phase == Phase::Candidate {
            self.measure(point);
        }
        if self.phase == Phase::Candidate
            && (!point.valid()
                || self
                    .queue
                    .try_push(Event::Move {
                        sequence: self.sequence,
                        point,
                        at: now,
                    })
                    .is_err())
        {
            self.cancel();
        }
        block
    }
    fn button(&mut self, button: u8, down: bool) {
        let _event = self.shared.event();
        let bit = if (1..=5).contains(&button) {
            1 << (button - 1)
        } else {
            0
        };
        if down {
            self.buttons |= bit;
        } else {
            self.buttons &= !bit;
        }
        self.update_held();
        self.other();
    }
    fn stop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        self.cancel();
    }
    fn lost(&mut self) {
        self.shared.recovery.uncertain();
        self.shared.lost.store(true, Ordering::Release);
        self.shared.stop.store(true, Ordering::Release);
        self.cancel();
        self.shared.debt.store(false, Ordering::Release);
    }
    fn keep_running(&mut self, stop: bool) -> bool {
        if stop {
            self.stop();
        }
        self.tick(Instant::now(), now_ms());
        if self.shared.lost.load(Ordering::Acquire) {
            self.shared.debt.load(Ordering::Acquire) || !self.shared.recovery.quiescent()
        } else {
            self.shared.debt.load(Ordering::Acquire)
                || self.shared.recovery.is_active()
                || !self.shared.stop.load(Ordering::Acquire)
        }
    }
}

#[cfg(windows)]
pub(crate) struct CallbackBroker(Broker<ActionTriggerSource>);
#[cfg(windows)]
impl CallbackBroker {
    pub(crate) fn initialize(
        &mut self,
        keys: impl IntoIterator<Item = u16>,
        buttons: impl IntoIterator<Item = u8>,
    ) {
        self.0.initialize(keys, buttons);
    }
    pub(crate) fn tagged_right(
        &mut self,
        tag: usize,
        down: Option<bool>,
        point: RawPoint,
    ) -> Option<bool> {
        self.0.tagged_right(tag, down, point)
    }
    pub(crate) fn foreign_input(&mut self, right: bool, motion: bool) -> bool {
        self.0.foreign_input(right, motion)
    }
    pub(crate) fn cleanup_request(&mut self) -> Option<RecoveryCleanup> {
        self.0.cleanup_request()
    }
    pub(crate) fn cleanup_result(&mut self, request: RecoveryCleanup, count: u32) {
        self.0.shared.recovery.cleanup_submitted(request, count);
    }
    pub(crate) fn clear_startup_key(&mut self, token: u16) {
        self.0.set_key(token, false);
    }
    #[cfg(windows)]
    pub(crate) fn key(&mut self, key: u16, down: bool) {
        self.0.key(key, down);
    }
    #[cfg(windows)]
    pub(crate) fn right(&mut self, down: bool, point: RawPoint, remote: bool) -> bool {
        self.0.right(down, point, Instant::now(), now_ms(), remote)
    }
    #[cfg(windows)]
    pub(crate) fn button(&mut self, button: u8, down: bool) {
        self.0.button(button, down);
    }
    #[cfg(windows)]
    pub(crate) fn motion(&mut self, point: RawPoint) -> bool {
        self.0.motion(point, Instant::now(), now_ms())
    }
    #[cfg(windows)]
    pub(crate) fn other(&mut self) {
        let _event = self.0.shared.event();
        self.0.other();
    }
    #[cfg(windows)]
    pub(crate) fn lost(&mut self) {
        self.0.lost();
    }
    #[cfg(windows)]
    pub(crate) fn keep_running(&mut self, stop: bool) -> bool {
        self.0.keep_running(stop)
    }
}
#[cfg(windows)]
pub(crate) fn channel(
    source: ActionTriggerSource,
) -> (CallbackBroker, GestureReceiver, GestureControl) {
    let (producer, consumer) = gesture_queue::channel(CAPACITY).expect("fixed valid capacity");
    let shared = Arc::new(Shared::new());
    let broker = Broker {
        source: source.clone(),
        queue: producer,
        shared: shared.clone(),
        phase: Phase::Idle,
        sequence: 0,
        key: None,
        started: None,
        first: None,
        max_distance_sq: 0.0,
        dpi: 96,
        keys: [0; 1024],
        key_count: 0,
        buttons: 0,
    };
    (
        CallbackBroker(broker),
        GestureReceiver {
            queue: consumer,
            shared: shared.clone(),
        },
        GestureControl {
            shared,
            source,
            _single: PhantomData,
        },
    )
}

/// Windows-only library entry point. No product binary calls this. The supplied
/// source must already have independent local action consent and a live native
/// context. This does not recognize gestures, replay clicks or execute actions.
#[cfg(windows)]
pub struct GestureCapture {
    control: GestureControl,
    session: crate::CaptureSession,
}
#[cfg(windows)]
impl GestureCapture {
    pub fn control(&mut self) -> &mut GestureControl {
        &mut self.control
    }
    /// Cancels immediately but keeps the original hook alive to drain an owed
    /// physical right-up. A timeout is an error; it never proves debt was paid.
    pub fn shutdown(&mut self) -> anyhow::Result<()> {
        self.control.stop();
        self.session.shutdown()?;
        anyhow::ensure!(
            !self.control.ownership_lost(),
            "本机手势捕获失去输入归属，不能确认右键已恢复"
        );
        anyhow::ensure!(
            !self.control.has_right_debt(),
            "仍在等待被抑制右键的物理抬起"
        );
        Ok(())
    }
}
#[cfg(windows)]
impl Drop for GestureCapture {
    fn drop(&mut self) {
        self.control.stop();
    }
}
#[cfg(windows)]
pub fn capture(source: ActionTriggerSource) -> anyhow::Result<(GestureCapture, GestureReceiver)> {
    // Validate before any native installation, outside callbacks. An admission
    // frame is still required separately after application rule/exclusion checks.
    anyhow::ensure!(
        source.mark().is_ok(),
        "本机手势需要独立同意和有效的原生上下文"
    );
    let (broker, receiver, control) = channel(source);
    let gate = crate::CaptureGate::default();
    let (tx, _rx) = tokio::sync::mpsc::channel(1);
    let thread = crate::native::capture(
        tx,
        gate.clone(),
        peerglide_protocol::InputCapabilities::default(),
        Some(broker),
    )?;
    let session = crate::CaptureSession {
        gate,
        _thread: thread,
        return_position: std::sync::Mutex::new(None),
    };
    Ok((GestureCapture { control, session }, receiver))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Clone)]
    struct Source {
        epoch: Arc<AtomicU64>,
        revoked: Arc<AtomicBool>,
    }
    impl TriggerProducer for Source {
        type Trigger = u64;
        fn mark(&self) -> Option<(u64, AdmissionKey, Instant)> {
            if self.revoked() {
                None
            } else {
                let key = self.frame();
                Some((key.0[0], key, Instant::now()))
            }
        }
        fn current(&self, key: AdmissionKey) -> bool {
            !self.revoked() && self.frame() == key
        }
        fn invalidate(&self) {
            self.epoch.fetch_add(1, Ordering::AcqRel);
        }
        fn emergency(&self) {
            self.revoked.store(true, Ordering::Release);
            self.invalidate();
        }
        fn revoked(&self) -> bool {
            self.revoked.load(Ordering::Acquire)
        }
    }
    impl Source {
        fn frame(&self) -> AdmissionKey {
            let mut key = [0; ADMISSION_WORDS];
            key[0] = self.epoch.load(Ordering::Acquire);
            AdmissionKey(key)
        }
    }
    fn fixture(capacity: usize) -> (Broker<Source>, Consumer<Event<u64>>) {
        let (queue, consumer) = gesture_queue::channel(capacity).unwrap();
        let source = Source {
            epoch: Arc::new(AtomicU64::new(1)),
            revoked: Arc::new(AtomicBool::new(false)),
        };
        let shared = Arc::new(Shared::new());
        let broker = Broker {
            source,
            queue,
            shared,
            phase: Phase::Idle,
            sequence: 0,
            key: None,
            started: None,
            first: None,
            max_distance_sq: 0.0,
            dpi: 96,
            keys: [0; 1024],
            key_count: 0,
            buttons: 0,
        };
        (broker, consumer)
    }
    fn point() -> RawPoint {
        RawPoint {
            x: 10.,
            y: 20.,
            units: CoordinateUnits::WindowsPhysicalPixels,
        }
    }
    fn arm(b: &Broker<Source>) {
        b.shared.publish(b.source.frame(), now_ms()).unwrap();
    }
    fn right(b: &mut Broker<Source>, down: bool) -> bool {
        b.right(down, point(), Instant::now(), now_ms(), false)
    }
    fn completed_receipt(b: &mut Broker<Source>, rx: &mut Consumer<Event<u64>>) -> RecoveryReceipt {
        arm(b);
        assert!(right(b, true));
        assert!(right(b, false));
        loop {
            if let Event::End { recovery, .. } = rx.try_pop().unwrap() {
                return recovery;
            }
        }
    }
    fn begin_test_recovery(b: &Broker<Source>, receipt: &RecoveryReceipt) {
        b.shared.latch.fetch_or(RECOVERING, Ordering::AcqRel);
        assert!(b.shared.recovery.begin(
            receipt.sequence,
            receipt.key,
            now_ms() + 1000,
            (11, 12),
            (10, 20)
        ));
    }
    #[test]
    fn sealed_receipt_preserves_full_geometry_start_and_original_expiry() {
        let (mut b, mut rx) = fixture(8);
        let receipt = completed_receipt(&mut b, &mut rx);
        assert!(Arc::ptr_eq(&receipt.shared, &b.shared));
        assert!(receipt.origin().0 == b.source.frame());
        assert_eq!(receipt.point(), (10, 20, 96));
        let original = receipt.ended + Duration::from_millis(100);
        assert_eq!(receipt.deadline(original), Some(original));
        assert_eq!(
            receipt.deadline(original + Duration::from_secs(1)),
            Some(receipt.ended + Duration::from_millis(250))
        );
        let (mut b, mut rx) = fixture(8);
        arm(&b);
        assert!(right(&mut b, true));
        b.motion(RawPoint { x: 50., ..point() }, Instant::now(), now_ms());
        assert!(right(&mut b, false));
        let receipt = loop {
            if let Event::End { recovery, .. } = rx.try_pop().unwrap() {
                break recovery;
            }
        };
        assert!(receipt
            .deadline(Instant::now() + Duration::from_secs(1))
            .is_none());
        let (mut b, mut rx) = fixture(8);
        arm(&b);
        assert!(right(&mut b, true));
        let far = RawPoint {
            x: 50.,
            ..point()
        };
        assert!(b.right(true, far, Instant::now(), now_ms(), false));
        assert!(right(&mut b, false));
        let receipt = loop {
            if let Event::End { recovery, .. } = rx.try_pop().unwrap() {
                break recovery;
            }
        };
        assert!(receipt
            .deadline(Instant::now() + Duration::from_secs(1))
            .is_none());
    }
    #[test]
    fn physical_down_before_injected_down_passes_its_pair_and_blocks_recovery() {
        let (mut b, mut rx) = fixture(8);
        let receipt = completed_receipt(&mut b, &mut rx);
        begin_test_recovery(&b, &receipt);
        assert!(!right(&mut b, true));
        assert_eq!(b.tagged_right(11, Some(true), point()), Some(true));
        assert_eq!(b.tagged_right(12, Some(false), point()), Some(true));
        b.shared.recovery.submitted(2);
        assert!(!right(&mut b, false));
        assert!(b.cleanup_request().is_none());
        assert!(!b.shared.recovery.is_active());
        assert_eq!(
            b.shared.recovery.completed(1),
            Some(RecoveryOutcome::Cancelled)
        );
    }
    #[test]
    fn physical_down_after_allowed_owned_down_retains_its_suppressed_up() {
        let (mut b, mut rx) = fixture(8);
        let receipt = completed_receipt(&mut b, &mut rx);
        begin_test_recovery(&b, &receipt);
        assert_eq!(b.tagged_right(11, Some(true), point()), Some(false));
        assert!(right(&mut b, true));
        assert!(b.shared.debt.load(Ordering::Acquire));
        assert!(b.motion(point(), Instant::now(), now_ms()));
        b.shared.recovery.submitted(1);
        let request = b.cleanup_request().unwrap();
        assert_eq!(b.tagged_right(12, Some(false), point()), Some(false));
        b.shared.recovery.cleanup_submitted(request, 1);
        b.cleanup_request();
        assert!(!b.shared.recovery.is_active());
        assert!(right(&mut b, false));
        assert!(!b.shared.debt.load(Ordering::Acquire));
        assert!(!b.motion(point(), Instant::now(), now_ms()));
        assert_eq!(
            b.shared.recovery.completed(1),
            Some(RecoveryOutcome::UnknownOutcome)
        );
    }
    #[test]
    fn wrong_owned_point_or_foreign_move_cancels_before_click() {
        for foreign in [false, true] {
            let (mut b, mut rx) = fixture(8);
            let receipt = completed_receipt(&mut b, &mut rx);
            begin_test_recovery(&b, &receipt);
            let mut at = point();
            if foreign {
                assert!(!b.foreign_input(false, true));
            } else {
                at.x += 1.0;
            }
            assert_eq!(b.tagged_right(11, Some(true), at), Some(true));
            assert_eq!(b.tagged_right(12, Some(false), point()), Some(true));
            b.shared.recovery.submitted(2);
            b.cleanup_request();
            assert_eq!(
                b.shared.recovery.completed(1),
                Some(RecoveryOutcome::Cancelled)
            );
        }
    }
    #[test]
    fn foreign_motion_is_frozen_during_owned_down_and_foreign_right_is_uncertain() {
        let (mut b, mut rx) = fixture(8);
        let receipt = completed_receipt(&mut b, &mut rx);
        begin_test_recovery(&b, &receipt);
        assert_eq!(b.tagged_right(11, Some(true), point()), Some(false));
        b.shared.recovery.submitted(1);
        assert!(b.foreign_input(false, true));
        assert!(b.shared.recovery.owns_down());
        assert!(!b.foreign_input(true, false));
        assert!(b.shared.lost.load(Ordering::Acquire));
        assert!(b.cleanup_request().is_none());
        assert!(!b.keep_running(false));
        assert!(b.shared.recovery.is_active());
    }
    #[test]
    fn already_queued_cleanup_is_blocked_after_foreign_right_takes_ownership() {
        let (mut b, mut rx) = fixture(8);
        let receipt = completed_receipt(&mut b, &mut rx);
        begin_test_recovery(&b, &receipt);
        assert_eq!(b.tagged_right(11, Some(true), point()), Some(false));
        b.shared.recovery.submitted(1);
        let request = b.cleanup_request().unwrap();
        b.shared.recovery.cleanup_submitted(request, 1);
        assert!(!b.foreign_input(true, false));
        assert!(b.keep_running(false));
        assert_eq!(b.tagged_right(12, Some(false), point()), Some(true));
        assert!(!b.keep_running(false));
        assert!(b.shared.recovery.owns_down());
        assert!(b.shared.lost.load(Ordering::Acquire));
    }
    #[test]
    fn owned_receipt_and_cleanup_short_paths_allocate_nothing() {
        let (mut b, mut rx) = fixture(8);
        let receipt = completed_receipt(&mut b, &mut rx);
        begin_test_recovery(&b, &receipt);
        assert_eq!(
            crate::allocation_checks::count(|| {
                assert_eq!(b.tagged_right(11, Some(true), point()), Some(false));
                b.shared.recovery.submitted(1);
                assert!(b.motion(point(), Instant::now(), now_ms()));
                let request = b.cleanup_request().unwrap();
                assert_eq!(b.tagged_right(12, Some(false), point()), Some(false));
                b.shared.recovery.cleanup_submitted(request, 1);
                assert!(b.cleanup_request().is_none());
            }),
            0
        );
    }
    #[test]
    fn stop_waits_for_original_batch_receipts_and_lost_never_blindly_cleans() {
        let (mut b, mut rx) = fixture(8);
        let receipt = completed_receipt(&mut b, &mut rx);
        begin_test_recovery(&b, &receipt);
        b.shared.recovery.submitted(2);
        assert!(b.keep_running(true));
        assert!(b.cleanup_request().is_none());
        assert_eq!(b.tagged_right(11, Some(true), point()), Some(true));
        assert_eq!(b.tagged_right(12, Some(false), point()), Some(true));
        b.cleanup_request();
        assert!(!b.keep_running(true));
        let (mut b, mut rx) = fixture(8);
        let receipt = completed_receipt(&mut b, &mut rx);
        begin_test_recovery(&b, &receipt);
        b.tagged_right(11, Some(true), point());
        b.shared.recovery.submitted(1);
        b.lost();
        assert!(b.cleanup_request().is_none());
        assert!(b.shared.recovery.owns_down());
        assert!(b.shared.lost.load(Ordering::Acquire));
    }
    #[test]
    fn callback_during_publication_cannot_reopen_a_held_or_already_completed_pair() {
        let (mut b, _rx) = fixture(8);
        arm(&b);
        let shared = b.shared.clone();
        let key = b.source.frame();
        assert_eq!(
            shared.publish_with(key, now_ms(), || {
                assert!(!right(&mut b, true));
            }),
            Err(CaptureError::Held)
        );
        assert!(!shared.is_armed());
        assert!(!right(&mut b, false));
        assert_eq!(
            shared.publish_with(b.source.frame(), now_ms(), || {
                assert!(!right(&mut b, true));
                assert!(!right(&mut b, false));
            }),
            Err(CaptureError::Held)
        );
        assert!(!shared.is_armed());
        arm(&b);
        assert!(right(&mut b, true));
        assert_eq!(
            shared.publish(b.source.frame(), now_ms()),
            Err(CaptureError::Held)
        );
        assert!(!shared.is_armed());
        assert!(right(&mut b, false));
    }
    #[test]
    fn delay_between_clock_samples_cannot_extend_original_admission_expiry() {
        let start = Instant::now();
        let expires = start + Duration::from_secs(5);
        assert_eq!(admission_expiry(expires, 100, start), Some(5100));
        let later = start + Duration::from_secs(2);
        assert_eq!(admission_expiry(expires, 100, later), Some(3100));
        assert_eq!(admission_expiry(expires, 100, expires), None);
    }
    #[test]
    fn original_admission_expiry_is_not_extended_by_a_live_watchdog() {
        let (mut b, _rx) = fixture(8);
        arm(&b);
        b.shared.admission_deadline.store(0, Ordering::Release);
        b.shared.deadline.store(u64::MAX, Ordering::Release);
        assert!(!right(&mut b, true));
        assert!(!right(&mut b, false));
        assert!(!b.shared.stop.load(Ordering::Acquire));
    }
    #[test]
    fn event_epoch_exhaustion_invalidates_completed_work_before_exit() {
        let (mut b, _rx) = fixture(8);
        arm(&b);
        assert!(right(&mut b, true));
        assert!(right(&mut b, false));
        let key = b.source.frame();
        b.shared.latch.store(!15u64, Ordering::Release);
        assert!(!right(&mut b, false));
        assert!(!b.keep_running(false));
        assert!(!b.source.current(key));
        assert_eq!(b.shared.cancelled.load(Ordering::Acquire), 1);
    }
    #[test]
    fn short_path_performs_no_heap_allocation_after_construction() {
        let (mut b, mut rx) = fixture(8);
        arm(&b);
        assert_eq!(
            crate::allocation_checks::count(|| {
                assert!(right(&mut b, true));
                b.motion(point(), Instant::now(), now_ms());
                assert!(right(&mut b, false));
                b.key(0x04, true);
                b.key(0x04, false);
                while rx.try_pop().is_some() {}
                b.shared.retired.store(1, Ordering::Release);
                arm(&b);
                assert!(right(&mut b, true));
                b.stop();
                assert!(right(&mut b, false));
            }),
            0
        );
    }
    #[test]
    fn startup_held_right_and_extra_buttons_cannot_straddle_admission() {
        let (mut b, _rx) = fixture(8);
        b.initialize([0x4041], 1..=5);
        assert_eq!(b.held(), 6);
        assert_eq!(
            b.shared.publish(b.source.frame(), now_ms()),
            Err(CaptureError::Held)
        );
        assert!(!right(&mut b, true));
        assert!(!right(&mut b, false));
        b.set_key(0x4041, false);
        for button in [1, 3, 4, 5] {
            b.button(button, false);
        }
        assert_eq!(b.held(), 0);
        arm(&b);
        assert!(right(&mut b, true));
        assert!(right(&mut b, false));
    }
    #[test]
    fn ordinary_pairs_pass_and_cannot_be_captured_halfway() {
        let (mut b, _rx) = fixture(8);
        assert!(!right(&mut b, true));
        assert_eq!(
            b.shared.publish(b.source.frame(), now_ms()),
            Err(CaptureError::Held)
        );
        // Even an incorrectly pre-published frame cannot steal a passed down.
        b.shared.latch.fetch_or(ARMED, Ordering::Release);
        assert!(!right(&mut b, false));
        assert!(!b.shared.debt.load(Ordering::Acquire));
        b.set_key(0xe0, true);
        assert_eq!(b.held(), 1);
        b.set_key(0xe0, true);
        assert_eq!(b.held(), 1);
        b.set_key(0xe0, false);
        assert_eq!(b.held(), 0);
    }
    #[test]
    fn successful_stroke_is_fifo_one_use_and_keeps_pending_target_until_retired() {
        let (mut b, mut rx) = fixture(8);
        arm(&b);
        assert!(right(&mut b, true));
        assert!(right(&mut b, true));
        assert!(b.shared.edges.load(Ordering::Acquire));
        b.motion(point(), Instant::now(), now_ms());
        assert!(right(&mut b, false));
        assert!(!b.shared.debt.load(Ordering::Acquire));
        assert!(matches!(
            rx.try_pop(),
            Some(Event::Begin { sequence: 1, .. })
        ));
        assert!(matches!(
            rx.try_pop(),
            Some(Event::Move { sequence: 1, .. })
        ));
        assert!(matches!(rx.try_pop(), Some(Event::End { sequence: 1, .. })));
        assert!(rx.try_pop().is_none());
        assert!(!b.shared.is_armed());
        // A new physical click invalidates old queued action/recovery context.
        let key = b.source.frame();
        assert!(!right(&mut b, true));
        assert!(!b.source.current(key));
        assert_eq!(b.shared.cancelled.load(Ordering::Acquire), 1);
        assert!(!right(&mut b, false));
        b.shared.retired.store(1, Ordering::Release);
        arm(&b);
        assert!(right(&mut b, true));
        assert!(right(&mut b, false));
    }
    #[test]
    fn stale_frame_modifier_remote_and_invalid_coordinates_never_start_suppression() {
        for kind in 0..5 {
            let (mut b, _rx) = fixture(8);
            arm(&b);
            let mut p = point();
            let mut remote = false;
            match kind {
                0 => b.source.invalidate(),
                1 => b.set_key(0xe2, true),
                2 => remote = true,
                3 => p.x = f64::NAN,
                _ => b.button(1, true),
            }
            assert!(!b.right(true, p, Instant::now(), now_ms(), remote));
            assert!(!right(&mut b, false));
            assert!(!b.shared.debt.load(Ordering::Acquire));
        }
    }
    #[test]
    fn cancellation_never_leaks_an_up_for_a_suppressed_down() {
        for reason in 0..8 {
            let (mut b, _rx) = fixture(8);
            arm(&b);
            assert!(right(&mut b, true));
            match reason {
                0 => b.source.invalidate(),
                1 => b.shared.stop.store(true, Ordering::Release),
                2 => b.shared.deadline.store(0, Ordering::Release),
                3 => b.started = Some(Instant::now() - MAX_HOLD),
                4 => b.key(0xe0, true),
                5 => b.button(1, true),
                6 => b.other(),
                _ => b.source.revoked.store(true, Ordering::Release),
            }
            b.tick(Instant::now(), now_ms());
            assert!(b.shared.debt.load(Ordering::Acquire));
            assert!(right(&mut b, true));
            assert!(right(&mut b, false));
            assert!(!b.shared.debt.load(Ordering::Acquire));
            assert_eq!(b.shared.cancelled.load(Ordering::Acquire), 1);
            assert!(!right(&mut b, false));
        }
    }
    #[test]
    fn queue_full_at_begin_passes_pair_but_full_after_begin_drains_debt() {
        let (mut b, mut rx) = fixture(1);
        arm(&b);
        // Fill without a held down: refusal to enqueue Begin must pass both.
        assert!(b
            .queue
            .try_push(Event::Move {
                sequence: 0,
                point: point(),
                at: Instant::now()
            })
            .is_ok());
        assert!(!right(&mut b, true));
        assert!(!right(&mut b, false));
        rx.try_pop();
        arm(&b);
        assert!(right(&mut b, true));
        b.motion(point(), Instant::now(), now_ms());
        assert_eq!(b.shared.cancelled.load(Ordering::Acquire), 1);
        assert!(right(&mut b, false));
        assert!(!b.shared.debt.load(Ordering::Acquire));
        let (mut b, _rx) = fixture(1);
        arm(&b);
        assert!(right(&mut b, true));
        assert!(right(&mut b, false));
        assert_eq!(b.shared.cancelled.load(Ordering::Acquire), 1);
    }
    #[test]
    fn stop_disconnect_escape_and_lost_source_do_not_claim_recovered_clicks() {
        let (mut b, rx) = fixture(8);
        arm(&b);
        assert!(right(&mut b, true));
        drop(rx);
        assert!(b.keep_running(false));
        assert!(b.shared.debt.load(Ordering::Acquire));
        assert!(right(&mut b, false));
        assert!(!b.keep_running(false));
        let (mut b, _rx) = fixture(8);
        arm(&b);
        assert!(right(&mut b, true));
        assert!(b.keep_running(true));
        assert!(right(&mut b, false));
        assert!(!b.keep_running(true));
        let (mut b, _rx) = fixture(8);
        arm(&b);
        assert!(right(&mut b, true));
        b.key(0xe0, true);
        b.key(0xe2, true);
        b.key(0x29, true);
        assert!(b.source.revoked());
        assert!(right(&mut b, false));
        assert!(!b.keep_running(false));
        let (mut b, _rx) = fixture(8);
        arm(&b);
        assert!(right(&mut b, true));
        b.lost();
        assert!(b.shared.lost.load(Ordering::Acquire));
        assert!(!b.keep_running(false));
        assert_eq!(
            b.shared.publish(b.source.frame(), now_ms()),
            Err(CaptureError::Stopped)
        );
    }
    #[test]
    fn idle_watchdog_is_terminal_and_pending_activity_invalidates_completed_stroke() {
        let (mut b, _rx) = fixture(8);
        arm(&b);
        b.tick(Instant::now(), u64::MAX);
        assert!(!b.keep_running(false));
        assert!(!right(&mut b, true));
        assert!(!right(&mut b, false));
        let (mut b, _rx) = fixture(8);
        arm(&b);
        assert!(right(&mut b, true));
        assert!(right(&mut b, false));
        b.key(0x04, true);
        assert_eq!(b.shared.cancelled.load(Ordering::Acquire), 1);
        assert!(b.shared.rearm.load(Ordering::Acquire));
    }
}
