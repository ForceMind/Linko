//! Worker-owned focus invalidation and bounded, consuming local context bindings.
//! Native callbacks only set a coalesced atomic bit. No execution grant or input hook.
use super::{native, observe_foreground, ContextError, FocusStatus, Foreground};
use peerglide_actions::Context;
use std::{
    marker::PhantomData,
    rc::Rc,
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

pub const MAX_BINDINGS: usize = 128;
pub const MAX_LIFETIME: Duration = peerglide_actions::MAX_ACTION_LIFETIME;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WatchError {
    Context(ContextError),
    Unsupported,
    NotReady,
    Busy,
    WrongThread,
    NativeUnavailable,
    CleanupFailed,
    Backlog,
    Closed,
    Entropy,
    Expired,
    Stale,
    UnknownTicket,
    Capacity,
    Lifetime,
    CounterExhausted,
    Clock,
}
impl WatchError {
    pub fn code(self) -> &'static str {
        match self {
            Self::Context(error) => error.code(),
            Self::Unsupported => "unsupported",
            Self::NotReady => "not_ready",
            Self::Busy => "busy",
            Self::WrongThread => "wrong_thread",
            Self::NativeUnavailable => "native_unavailable",
            Self::CleanupFailed => "cleanup_failed",
            Self::Backlog => "backlog",
            Self::Closed => "closed",
            Self::Entropy => "entropy_unavailable",
            Self::Expired => "expired",
            Self::Stale => "stale",
            Self::UnknownTicket => "unknown_ticket",
            Self::Capacity => "capacity",
            Self::Lifetime => "invalid_lifetime",
            Self::CounterExhausted => "counter_exhausted",
            Self::Clock => "clock_regressed",
        }
    }
}
impl From<ContextError> for WatchError {
    fn from(error: ContextError) -> Self {
        Self::Context(error)
    }
}

/// Callbacks only call notify(). A single worker owns acknowledgement and epoch
/// changes. Coalescing several notifications still invalidates every older lease.
const DIRTY: u64 = 1 << 63;
const CLOSED: u64 = 1 << 62;
const READY: u64 = 1 << 61;
const EPOCH: u64 = READY - 1;
pub(crate) struct Signal {
    state: AtomicU64,
}
impl Signal {
    fn new() -> Self {
        Self {
            state: AtomicU64::new(DIRTY | 1),
        }
    }
    #[cfg(all(test, windows))]
    pub(crate) fn new_for_test() -> Self {
        Self::new()
    }
    #[cfg(all(test, windows))]
    pub(crate) fn closed_for_test(&self) -> bool {
        self.state.load(Ordering::Acquire) & CLOSED != 0
    }
    pub(crate) fn notify(&self) {
        self.state.fetch_or(DIRTY, Ordering::Release);
    }
    pub(crate) fn close(&self) {
        self.state.fetch_or(CLOSED | DIRTY, Ordering::Release);
    }
    fn epoch(&self) -> u64 {
        self.state.load(Ordering::Acquire) & EPOCH
    }
    fn check_state(state: u64, epoch: u64) -> Result<(), WatchError> {
        if state & CLOSED != 0 {
            return Err(WatchError::Closed);
        }
        if state & DIRTY != 0 || state & EPOCH != epoch {
            return Err(WatchError::Stale);
        }
        if state & READY == 0 {
            return Err(WatchError::NotReady);
        }
        Ok(())
    }
    fn check(&self, epoch: u64) -> Result<(), WatchError> {
        Self::check_state(self.state.load(Ordering::Acquire), epoch)
    }
}

struct Slot(Arc<AtomicUsize>);
impl Slot {
    fn reserve(count: &Arc<AtomicUsize>) -> Result<Self, WatchError> {
        // Bounded worker-side CAS works on the minimum toolchain as well as
        // current stable; no deprecated update helper or callback retry loop.
        for _ in 0..16 {
            let value = count.load(Ordering::Acquire);
            if value >= MAX_BINDINGS {
                return Err(WatchError::Capacity);
            }
            if count
                .compare_exchange(value, value + 1, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return Ok(Self(count.clone()));
            }
        }
        Err(WatchError::Backlog)
    }
}
impl Drop for Slot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}
struct Entry<T> {
    context: Context,
    expires: Instant,
    target: T,
    _slot: Slot,
}
struct Store<T> {
    signal: Arc<Signal>,
    device: [u8; 32],
    live: Arc<AtomicUsize>,
    configuration: u64,
    sequence: u64,
    last_time: Instant,
    entries: Vec<Entry<T>>,
}
fn random_token() -> Result<[u8; 32], WatchError> {
    let mut bytes = [0; 32];
    getrandom::fill(&mut bytes).map_err(|_| WatchError::Entropy)?;
    Ok(bytes)
}
impl<T> Store<T> {
    fn new(signal: Arc<Signal>, now: Instant) -> Result<Self, WatchError> {
        Ok(Self {
            signal,
            device: random_token()?,
            live: Arc::new(AtomicUsize::new(0)),
            configuration: 1,
            sequence: 0,
            last_time: now,
            entries: Vec::new(),
        })
    }
    fn acknowledge(&mut self, now: Instant) -> Result<(), WatchError> {
        if now < self.last_time {
            self.close();
            return Err(WatchError::Clock);
        }
        self.last_time = now;
        for _ in 0..16 {
            let state = self.signal.state.load(Ordering::Acquire);
            if state & CLOSED != 0 {
                self.entries.clear();
                return Err(WatchError::Closed);
            }
            if state & DIRTY == 0 {
                self.entries.retain(|entry| entry.expires > now);
                return Ok(());
            }
            self.entries.clear();
            let epoch = state & EPOCH;
            if epoch == EPOCH {
                self.close();
                return Err(WatchError::CounterExhausted);
            }
            // One atomic transition: clear ready/dirty and advance the epoch.
            // A callback can never read a ready bit belonging to another epoch.
            let next = epoch + 1;
            if self
                .signal
                .state
                .compare_exchange(state, next, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return Ok(());
            }
        }
        self.signal.notify();
        Err(WatchError::Backlog)
    }
    fn finish_frame(&mut self, now: Instant) -> Result<(), WatchError> {
        let epoch = self.signal.epoch();
        self.acknowledge(now)?;
        if self.signal.epoch() != epoch {
            return Err(WatchError::Stale);
        }
        let state = self.signal.state.load(Ordering::Acquire);
        if state & (CLOSED | DIRTY) != 0 || state & EPOCH != epoch {
            return Err(WatchError::Stale);
        }
        self.signal
            .state
            .compare_exchange(state, state | READY, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| WatchError::Stale)?;
        self.signal.check(epoch)
    }
    fn close(&mut self) {
        self.signal.close();
        self.entries.clear();
    }
    fn configuration_changed(&mut self, now: Instant) -> Result<(), WatchError> {
        let Some(next) = self.configuration.checked_add(1) else {
            self.close();
            return Err(WatchError::CounterExhausted);
        };
        self.configuration = next;
        self.signal.notify();
        self.acknowledge(now)
    }
    fn insert(
        &mut self,
        target: T,
        expected_epoch: u64,
        started: Instant,
        expires: Instant,
        now: Instant,
    ) -> Result<FrozenContext, WatchError> {
        self.acknowledge(now)?;
        if expires <= started || expires.duration_since(started) > MAX_LIFETIME {
            return Err(WatchError::Lifetime);
        }
        if now >= expires {
            return Err(WatchError::Expired);
        }
        self.signal.check(expected_epoch)?;
        if self.entries.len() >= MAX_BINDINGS {
            return Err(WatchError::Capacity);
        }
        let Some(sequence) = self.sequence.checked_add(1) else {
            self.close();
            return Err(WatchError::CounterExhausted);
        };
        let slot = Slot::reserve(&self.live)?;
        let mut token = None;
        for _ in 0..4 {
            let candidate = random_token()?;
            if self
                .entries
                .iter()
                .all(|entry| entry.context.target != candidate)
            {
                token = Some(candidate);
                break;
            }
        }
        let Some(token) = token else {
            self.close();
            return Err(WatchError::Entropy);
        };
        self.signal.check(expected_epoch)?;
        self.sequence = sequence;
        let context = Context {
            device: self.device,
            target: token,
            focus_generation: expected_epoch,
            configuration_generation: self.configuration,
            trigger_sequence: sequence,
        };
        self.entries.push(Entry {
            context,
            expires,
            target,
            _slot: slot,
        });
        Ok(FrozenContext { context, expires })
    }
    fn cancel(&mut self, context: Context) -> Result<(), WatchError> {
        if context.device != self.device {
            return Err(WatchError::Stale);
        }
        // Cancellation needs no current permission/native query. Compare the
        // complete context so an older trigger cannot remove a newer binding.
        self.entries.retain(|entry| entry.context != context);
        Ok(())
    }
    fn take(&mut self, context: Context, now: Instant) -> Result<Entry<T>, WatchError> {
        self.acknowledge(now)?;
        if context.device != self.device || context.configuration_generation != self.configuration {
            return Err(WatchError::Stale);
        }
        self.signal.check(context.focus_generation)?;
        let index = self
            .entries
            .iter()
            .position(|entry| entry.context.target == context.target)
            .ok_or(WatchError::UnknownTicket)?;
        // Consume before any fallible native check. Conflicts cannot retain a live
        // side-effect opportunity, and failed/unknown outcomes never auto-replay.
        let entry = self.entries.swap_remove(index);
        if entry.context != context {
            return Err(WatchError::Stale);
        }
        if now >= entry.expires {
            return Err(WatchError::Expired);
        }
        Ok(entry)
    }
}

/// Clone once outside callbacks. mark() only reads atomics and the monotonic
/// clock, returning fixed-size data for the existing bounded capture queue.
#[derive(Clone)]
pub struct TriggerSource {
    signal: Arc<Signal>,
    device: [u8; 32],
}
pub struct TriggerMark {
    device: [u8; 32],
    epoch: u64,
    started: Instant,
}
impl TriggerMark {
    pub(in crate::context) fn frame(&self) -> ([u8; 32], u64) { (self.device, self.epoch) }
    #[cfg(any(windows, test))]
    pub(in crate::context) fn started(&self) -> Instant { self.started }
}
impl TriggerSource {
    #[cfg(any(windows, test))]
    pub(in crate::context) fn matches_frame(&self, device: [u8; 32], epoch: u64) -> bool {
        self.device == device && self.signal.check(epoch).is_ok()
    }
    pub fn mark(&self) -> Result<TriggerMark, WatchError> {
        self.mark_from_clock(Instant::now)
    }
    fn mark_from_clock(&self, clock: impl FnOnce() -> Instant) -> Result<TriggerMark, WatchError> {
        let state = self.signal.state.load(Ordering::Acquire);
        let epoch = state & EPOCH;
        Signal::check_state(state, epoch)?;
        let started = clock();
        if self.signal.state.load(Ordering::Acquire) != state {
            return Err(WatchError::Stale);
        }
        Ok(TriggerMark {
            device: self.device,
            epoch,
            started,
        })
    }
    /// Queue loss/emergency/configuration handoff must cancel old contexts.
    /// This operation is also allocation-free and safe in the native short path.
    pub fn invalidate(&self) {
        self.signal.notify();
    }
}

/// Opaque process-local context only. It grants no action/input/data permission.
/// No Debug/Serialize/Clone; Context fields must never be treated as remote auth.
pub struct FrozenContext {
    context: Context,
    expires: Instant,
}
impl FrozenContext {
    pub fn context(&self) -> Context {
        self.context
    }
    pub fn expires(&self) -> Instant {
        self.expires
    }
}

/// A consumed native target. Native execution must still revalidate policy,
/// capabilities and OS target immediately at submission; no action is provided here.
pub struct TargetLease {
    entry: Entry<Foreground>,
    signal: Arc<Signal>,
    _thread: PhantomData<Rc<()>>,
}
impl TargetLease {
    pub fn context(&self) -> Context {
        self.entry.context
    }
    pub fn application_id(&self) -> &str {
        self.entry.target.application_id()
    }
    pub(in crate::context) fn native_target(&self) -> &native::Target {
        &self.entry.target.target
    }
    pub fn recheck(&self, owner: &mut LocalContexts) -> Result<(), WatchError> {
        if !Arc::ptr_eq(&self.signal, &owner.signal) {
            return Err(WatchError::Stale);
        }
        owner.poll()?;
        self.signal.check(self.entry.context.focus_generation)?;
        if Instant::now() >= self.entry.expires {
            return Err(WatchError::Expired);
        }
        owner.check_target(&self.entry.target)?;
        if Instant::now() >= self.entry.expires {
            return Err(WatchError::Expired);
        }
        self.signal.check(self.entry.context.focus_generation)
    }
}

/// One native notification owner on its creating thread. Mac requires the main
/// thread; Windows pumps only this dedicated worker thread's message queue.
/// Call poll before capture/consume; shutdown closes all outstanding contexts.
pub struct LocalContexts {
    native: native::Watch,
    store: Store<Foreground>,
    signal: Arc<Signal>,
    _thread: PhantomData<Rc<()>>,
}
impl LocalContexts {
    pub fn new() -> Result<Self, WatchError> {
        let signal = Arc::new(Signal::new());
        let mut native = native::Watch::new(signal.clone())?;
        let store = match Store::new(signal.clone(), Instant::now()) {
            Ok(store) => store,
            Err(error) => {
                signal.close();
                native.shutdown()?;
                return Err(error);
            }
        };
        let mut contexts = Self {
            native,
            store,
            signal,
            _thread: PhantomData,
        };
        if let Err(error) = contexts.poll() {
            contexts.shutdown()?;
            return Err(error);
        }
        Ok(contexts)
    }
    pub fn poll(&mut self) -> Result<(), WatchError> {
        self.signal.state.fetch_and(!READY, Ordering::AcqRel);
        if self.signal.state.load(Ordering::Acquire) & CLOSED != 0 {
            self.store.close();
            return Err(WatchError::Closed);
        }
        if let Err(error) = self.native.pump() {
            if error == WatchError::CleanupFailed {
                self.store.close();
                return Err(error);
            }
            self.signal.notify();
            self.store.acknowledge(Instant::now())?;
            return Err(error);
        }
        self.store.acknowledge(Instant::now())?;
        if let Err(error) = self.native.refresh() {
            self.signal.notify();
            self.store.acknowledge(Instant::now())?;
            return Err(error);
        }
        // A notification or rebind DURING refresh cannot be acknowledged into
        // a supposedly-ready epoch. Retry a fresh native frame instead.
        self.store.finish_frame(Instant::now())
    }
    fn check_target(&self, target: &Foreground) -> Result<(), WatchError> {
        let result = (|| {
            if !self.native.accepts(&target.target)? || !target.matches_current()? {
                return Err(WatchError::Stale);
            }
            Ok(())
        })();
        result.inspect_err(|_| self.signal.notify())
    }
    pub fn epoch(&self) -> u64 {
        self.signal.epoch()
    }
    pub fn trigger_source(&self) -> TriggerSource {
        TriggerSource {
            signal: self.signal.clone(),
            device: self.store.device,
        }
    }
    pub fn configuration_changed(&mut self) -> Result<(), WatchError> {
        self.store.configuration_changed(Instant::now())
    }
    pub fn capture(
        &mut self,
        mark: TriggerMark,
        lifetime: Duration,
    ) -> Result<FrozenContext, WatchError> {
        let started = mark.started;
        if mark.device != self.store.device {
            return Err(WatchError::Stale);
        }
        if lifetime.is_zero() || lifetime > MAX_LIFETIME {
            return Err(WatchError::Lifetime);
        }
        let expires = started.checked_add(lifetime).ok_or(WatchError::Lifetime)?;
        self.poll()?;
        let epoch = mark.epoch;
        self.signal.check(epoch)?;
        let target = observe_foreground().inspect_err(|_| self.signal.notify())?;
        if target.focus() != FocusStatus::Available {
            self.signal.notify();
            return Err(WatchError::NotReady);
        }
        self.poll()?;
        if self.signal.epoch() != epoch {
            self.signal.notify();
            return Err(WatchError::Stale);
        }
        self.check_target(&target)?;
        self.signal.check(epoch)?;
        let frozen = self
            .store
            .insert(target, epoch, started, expires, Instant::now())?;
        let final_check = if Instant::now() >= expires {
            Err(WatchError::Expired)
        } else {
            self.signal.check(epoch)
        };
        if let Err(error) = final_check {
            self.store
                .entries
                .retain(|entry| entry.context.target != frozen.context.target);
            return Err(error);
        }
        Ok(frozen)
    }
    /// Cancel/reject a queued gesture without waiting for expiry. No native
    /// permission or focus is needed to release our own retained references.
    pub fn cancel(&mut self, frozen: FrozenContext) -> Result<(), WatchError> {
        self.store.cancel(frozen.context)
    }
    pub fn consume(&mut self, frozen: FrozenContext) -> Result<TargetLease, WatchError> {
        self.poll()?;
        let now = Instant::now();
        if now >= frozen.expires {
            return Err(WatchError::Expired);
        }
        let entry = self.store.take(frozen.context, now)?;
        self.check_target(&entry.target)?;
        let lease = TargetLease {
            entry,
            signal: self.signal.clone(),
            _thread: PhantomData,
        };
        lease.recheck(self)?;
        Ok(lease)
    }
    pub fn shutdown(&mut self) -> Result<(), WatchError> {
        self.store.close();
        self.native.shutdown()
    }
}
impl Drop for LocalContexts {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn make_store(now: Instant) -> Store<u8> {
        let mut store = Store::new(Arc::new(Signal::new()), now).unwrap();
        store.acknowledge(now).unwrap();
        store.finish_frame(now).unwrap();
        store
    }
    fn insert(store: &mut Store<u8>, now: Instant) -> FrozenContext {
        store.finish_frame(now).unwrap();
        let epoch = store.signal.epoch();
        store
            .insert(7, epoch, now, now + Duration::from_secs(1), now)
            .unwrap()
    }
    #[test]
    fn away_and_back_or_coalesced_notifications_never_revive_old_contexts() {
        let now = Instant::now();
        let mut store = make_store(now);
        let frozen = insert(&mut store, now);
        let old = frozen.context();
        let entry = store.take(old, now).unwrap();
        store.signal.notify();
        store.signal.notify();
        assert_eq!(
            store.signal.check(entry.context.focus_generation),
            Err(WatchError::Stale)
        );
        store.acknowledge(now).unwrap();
        assert_eq!(
            store.signal.check(entry.context.focus_generation),
            Err(WatchError::Stale)
        );
        let new = insert(&mut store, now);
        assert!(new.context().focus_generation > old.focus_generation);
        assert!(matches!(store.take(old, now), Err(WatchError::Stale)));
    }
    #[test]
    fn event_during_target_sampling_cannot_stamp_old_target_with_new_epoch() {
        let now = Instant::now();
        let mut store = make_store(now);
        let captured_epoch = store.signal.epoch();
        store.signal.notify();
        assert!(matches!(
            store.insert(7, captured_epoch, now, now + MAX_LIFETIME, now),
            Err(WatchError::Stale)
        ));
        assert!(store.entries.is_empty());
    }
    #[test]
    fn trigger_mark_is_fixed_size_and_queue_delay_never_resets_deadline() {
        let now = Instant::now();
        let mut store = make_store(now);
        let source = TriggerSource {
            signal: store.signal.clone(),
            device: store.device,
        };
        let mark = source.mark().unwrap();
        assert!(std::mem::size_of::<TriggerMark>() <= 64);
        let expires = mark.started + Duration::from_millis(50);
        assert!(matches!(
            store.insert(7, mark.epoch, mark.started, expires, expires),
            Err(WatchError::Expired)
        ));
        source.invalidate();
        assert!(matches!(source.mark(), Err(WatchError::Stale)));
    }
    #[test]
    fn tickets_consume_once_and_conflicting_live_token_is_not_replayed() {
        let now = Instant::now();
        let mut store = make_store(now);
        let context = insert(&mut store, now).context();
        assert_eq!(store.take(context, now).unwrap().target, 7);
        assert!(matches!(
            store.take(context, now),
            Err(WatchError::UnknownTicket)
        ));
        let mut conflict = insert(&mut store, now).context();
        let original = conflict;
        conflict.trigger_sequence += 1;
        assert!(matches!(store.take(conflict, now), Err(WatchError::Stale)));
        assert!(matches!(
            store.take(original, now),
            Err(WatchError::UnknownTicket)
        ));
    }
    #[test]
    fn configuration_shutdown_and_foreign_worker_contexts_do_not_recover() {
        let now = Instant::now();
        let mut first = make_store(now);
        let mut second = make_store(now);
        assert_ne!(first.device, second.device); // OS entropy used, not a quality proof.
        let old = insert(&mut first, now).context();
        assert!(matches!(second.take(old, now), Err(WatchError::Stale)));
        first.configuration_changed(now).unwrap();
        assert!(matches!(first.take(old, now), Err(WatchError::Stale)));
        first.close();
        assert!(matches!(first.take(old, now), Err(WatchError::Closed)));
    }
    #[test]
    fn bounded_live_bindings_cannot_evict_an_older_opportunity() {
        let now = Instant::now();
        let mut store = make_store(now);
        let first = insert(&mut store, now).context();
        for _ in 1..MAX_BINDINGS {
            insert(&mut store, now);
        }
        let epoch = store.signal.epoch();
        assert!(matches!(
            store.insert(7, epoch, now, now + MAX_LIFETIME, now),
            Err(WatchError::Capacity)
        ));
        assert_eq!(store.take(first, now).unwrap().target, 7);
        store.acknowledge(now + Duration::from_secs(2)).unwrap();
        assert!(store.entries.is_empty());
    }
    #[test]
    fn consumed_target_references_keep_the_same_resource_budget_until_drop() {
        let now = Instant::now();
        let mut store = make_store(now);
        let first = insert(&mut store, now).context();
        let held = store.take(first, now).unwrap();
        for _ in 1..MAX_BINDINGS {
            insert(&mut store, now);
        }
        let epoch = store.signal.epoch();
        assert!(matches!(
            store.insert(9, epoch, now, now + MAX_LIFETIME, now),
            Err(WatchError::Capacity)
        ));
        drop(held);
        assert!(store.insert(9, epoch, now, now + MAX_LIFETIME, now).is_ok());
        store.close();
        assert_eq!(store.live.load(Ordering::Acquire), 0);
    }
    #[test]
    fn explicit_cancellation_releases_only_its_own_queued_native_slot() {
        let now = Instant::now();
        let mut owner = make_store(now);
        let first = insert(&mut owner, now).context();
        let second = insert(&mut owner, now).context();
        owner.signal.notify(); // Cancellation still works while target is lost.
        owner.cancel(first).unwrap();
        assert_eq!(owner.live.load(Ordering::Acquire), 1);
        owner.cancel(first).unwrap();
        assert_eq!(owner.live.load(Ordering::Acquire), 1);
        assert_eq!(owner.entries[0].context, second);
        let foreign = make_store(now);
        assert_eq!(
            owner.cancel(Context {
                device: foreign.device,
                ..second
            }),
            Err(WatchError::Stale)
        );
        assert_eq!(owner.live.load(Ordering::Acquire), 1);
    }
    #[test]
    fn invalid_lifetimes_clock_regression_and_counter_exhaustion_fail_closed() {
        let now = Instant::now();
        let mut store = make_store(now);
        let epoch = store.signal.epoch();
        assert!(matches!(
            store.insert(7, epoch, now, now, now),
            Err(WatchError::Lifetime)
        ));
        assert!(matches!(
            store.insert(7, epoch, now, now + Duration::from_secs(6), now),
            Err(WatchError::Lifetime)
        ));
        store.acknowledge(now + Duration::from_secs(1)).unwrap();
        assert_eq!(store.acknowledge(now), Err(WatchError::Clock));
        assert!(store.signal.state.load(Ordering::Acquire) & CLOSED != 0);
        let mut overflowing = make_store(now);
        overflowing
            .signal
            .state
            .store(EPOCH | READY, Ordering::Release);
        overflowing.signal.notify();
        assert_eq!(
            overflowing.acknowledge(now),
            Err(WatchError::CounterExhausted)
        );
        assert!(overflowing.signal.state.load(Ordering::Acquire) & CLOSED != 0);
    }
    #[test]
    fn versioned_mark_rejects_ready_aba_across_a_complete_native_frame() {
        let now = Instant::now();
        let mut store = make_store(now);
        let source = TriggerSource {
            signal: store.signal.clone(),
            device: store.device,
        };
        let result = source.mark_from_clock(|| {
            source.invalidate();
            store.acknowledge(now).unwrap();
            store.finish_frame(now).unwrap();
            Instant::now()
        });
        assert!(matches!(result, Err(WatchError::Stale)));
        assert!(source.mark().is_ok());
    }
    #[test]
    fn callbacks_cannot_mark_a_new_epoch_until_a_stable_native_frame_finishes() {
        let now = Instant::now();
        let mut store = make_store(now);
        let source = TriggerSource {
            signal: store.signal.clone(),
            device: store.device,
        };
        let old = source.mark().unwrap();
        source.invalidate();
        store.acknowledge(now).unwrap();
        assert!(matches!(source.mark(), Err(WatchError::NotReady)));
        // A target rebind/notification DURING refresh is not a ready frame.
        source.invalidate();
        assert_eq!(store.finish_frame(now), Err(WatchError::Stale));
        assert!(matches!(source.mark(), Err(WatchError::NotReady)));
        store.finish_frame(now).unwrap();
        let current = source.mark().unwrap();
        assert!(current.epoch > old.epoch);
        assert!(matches!(
            store.insert(
                9,
                old.epoch,
                old.started,
                old.started + MAX_LIFETIME,
                current.started
            ),
            Err(WatchError::Stale)
        ));
        assert!(store.entries.is_empty());
        assert!(store
            .insert(
                9,
                current.epoch,
                current.started,
                current.started + MAX_LIFETIME,
                current.started
            )
            .is_ok());
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    #[test]
    fn native_watch_never_fabricates_support_on_linux() {
        assert!(matches!(LocalContexts::new(), Err(WatchError::Unsupported)));
    }
}
