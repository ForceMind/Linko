//! Native adapters. Capture is disabled automatically when the network watchdog expires.
use anyhow::Result;
use peerglide_protocol::{Input, InputCapabilities};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, OnceLock,
};
use std::time::Instant;
use tokio::sync::mpsc;
#[cfg(any(windows, target_os = "macos", test))]
mod capture_thread;
pub mod context;
mod receive_driver;
pub use receive_driver::{ReceiveAck, ReceiveControl, ReceiveDriver, ReceiveError, ReceivePoint, ReceiveSnapshot, COMMAND_TIMEOUT};
#[cfg(any(windows, test))]
pub mod gesture_capture;
#[cfg(any(windows, test))]
mod gesture_queue;
#[cfg(any(windows, target_os = "macos"))]
use capture_thread::CaptureThread;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;
#[cfg(target_os = "macos")]
use macos as native;
#[cfg(windows)]
use windows as native;

pub fn now_ms() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_millis() as u64
}
/// Process-local capture generation, never network authentication or a lease ID.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CaptureEpoch {
    instance: u64,
    generation: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureRoute {
    Local(CaptureEpoch),
    Remote(CaptureEpoch),
    /// Protected receiver admission; never an outgoing input route.
    Receiving(CaptureEpoch),
}
#[cfg(any(windows, target_os = "macos"))]
impl CaptureRoute {
    fn epoch(self) -> CaptureEpoch {
        match self {
            Self::Local(epoch) | Self::Remote(epoch) | Self::Receiving(epoch) => epoch,
        }
    }
    fn remote(self) -> bool {
        matches!(self, Self::Remote(_))
    }
}
#[derive(Clone, Copy)]
pub enum Captured {
    Position {
        x: f64,
        y: f64,
        width: f64,
        height: f64,
        route: CaptureRoute,
    },
    Motion {
        dx: f64,
        dy: f64,
        route: CaptureRoute,
    },
    Input {
        event: Input,
        route: CaptureRoute,
    },
    Emergency,
    UnsupportedInput,
    Toggle {
        route: CaptureRoute,
    },
}
const ACTIVE: u64 = 1 << 63;
const IN_CALLBACK: u64 = 1 << 62;
const STOPPED: u64 = 1 << 61;
const RECEIVING: u64 = 1 << 60;
const ADMISSIONS_CLOSED: u64 = 1 << 59;
const COUNT_BITS: u32 = 17;
const COUNT: u64 = (1 << COUNT_BITS) - 1;
const GENERATION_MAX: u64 = (1 << (59 - COUNT_BITS)) - 1;
const GENERATION: u64 = GENERATION_MAX << COUNT_BITS;
static NEXT_CAPTURE: AtomicU64 = AtomicU64::new(1);
struct GateState {
    instance: u64,
    deadline: AtomicU64,
    control: AtomicU64,
    healthy: std::sync::atomic::AtomicBool,
    receive: OnceLock<ReceiveControl>,
    return_pending: std::sync::atomic::AtomicBool,
}
#[derive(Clone)]
pub struct CaptureGate(Arc<GateState>);
impl Default for CaptureGate {
    fn default() -> Self {
        // Construction is outside callbacks. No ID is ever reused after wrap.
        let mut observed = NEXT_CAPTURE.load(Ordering::Relaxed);
        let instance = loop {
            let Some(next) = observed.checked_add(1) else {
                break 0;
            };
            match NEXT_CAPTURE.compare_exchange_weak(
                observed,
                next,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break observed,
                Err(current) => observed = current,
            }
        };
        Self(Arc::new(GateState {
            instance,
            deadline: AtomicU64::new(0),
            control: AtomicU64::new(if instance == 0 { STOPPED } else { 0 }),
            healthy: std::sync::atomic::AtomicBool::new(instance != 0),
            receive: OnceLock::new(),
            return_pending: std::sync::atomic::AtomicBool::new(false),
        }))
    }
}
impl CaptureGate {
    fn epoch_from(&self, word: u64) -> CaptureEpoch {
        CaptureEpoch {
            instance: self.0.instance,
            generation: (word & GENERATION) >> COUNT_BITS,
        }
    }
    fn route_from(&self, word: u64) -> CaptureRoute {
        let epoch = self.epoch_from(word);
        if word & RECEIVING != 0 {
            CaptureRoute::Receiving(epoch)
        } else if word & ACTIVE != 0 {
            CaptureRoute::Remote(epoch)
        } else {
            CaptureRoute::Local(epoch)
        }
    }
    fn publish(&self, remote: bool) -> Option<CaptureEpoch> {
        self.publish_mode(if remote { ACTIVE } else { 0 })
    }
    fn publish_mode(&self, mode: u64) -> Option<CaptureEpoch> {
        let old = self.0.control.load(Ordering::Acquire);
        if old & (ACTIVE | RECEIVING | IN_CALLBACK | STOPPED | ADMISSIONS_CLOSED | COUNT) != 0 {
            return None;
        }
        let generation = (old & GENERATION) >> COUNT_BITS;
        let Some(next) = generation
            .checked_add(1)
            .filter(|next| *next <= GENERATION_MAX)
        else {
            self.source_lost();
            return None;
        };
        self.0.deadline.store(
            if mode == ACTIVE {
                now_ms().saturating_add(1000)
            } else {
                0
            },
            Ordering::Release,
        );
        let new = (next << COUNT_BITS) | mode;
        self.0
            .control
            .compare_exchange(old, new, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| self.epoch_from(new))
    }
    pub fn try_activate(&self) -> bool {
        self.publish(true).is_some()
    }
    /// Atomically fence a local/receiving phase only with no physical holds and
    /// no callback in progress. This is not a receive permission by itself.
    pub fn try_local_epoch(&self) -> Option<CaptureEpoch> {
        self.publish(false)
    }
    pub fn route(&self) -> Option<CaptureRoute> {
        self.active();
        let word = self.0.control.load(Ordering::Acquire);
        (word & STOPPED == 0).then(|| self.route_from(word))
    }
    pub fn is_current(&self, route: CaptureRoute) -> bool {
        self.route() == Some(route)
    }
    pub fn refresh(&self) {
        if self.active() {
            self.0
                .deadline
                .store(now_ms().saturating_add(1000), Ordering::Release);
        }
    }
    fn receiving_epoch(&self) -> Option<CaptureEpoch> {
        let word = self.0.control.load(Ordering::Acquire);
        (word & (RECEIVING | STOPPED) == RECEIVING).then(|| self.epoch_from(word))
    }
    fn request_takeover(&self, route: CaptureRoute) {
        if let CaptureRoute::Receiving(epoch) = route {
            if let Some(control) = self.0.receive.get() { control.request_recovery(epoch); }
        }
    }
    pub fn disable(&self) {
        self.0.deadline.store(0, Ordering::Release);
        // Mode and generation change together. A queued Local Toggle is invalid
        // after disable too, not only events that used to be Remote.
        let mut old = self.0.control.load(Ordering::Acquire);
        loop {
            if old & (RECEIVING | STOPPED) == RECEIVING {
                self.request_takeover(self.route_from(old));
                return; // Only Native-owned cleanup may remove receiver protection.
            }
            let generation = (old & GENERATION) >> COUNT_BITS;
            let new = if old & STOPPED != 0 || generation == GENERATION_MAX {
                (old & !(ACTIVE | RECEIVING)) | STOPPED
            } else {
                (old & !(ACTIVE | RECEIVING | GENERATION)) | ((generation + 1) << COUNT_BITS)
            };
            match self.0.control.compare_exchange_weak(
                old,
                new,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    if new & STOPPED != 0 {
                        self.0.healthy.store(false, Ordering::Release);
                    }
                    break;
                }
                Err(actual) => old = actual,
            }
        }
    }
    #[cfg(any(windows, target_os = "macos"))]
    fn enter_receiving(&self) -> Option<CaptureEpoch> {
        if self.0.return_pending.load(Ordering::Acquire) { return None; }
        self.publish_mode(RECEIVING)
    }
    #[cfg(any(windows, target_os = "macos"))]
    fn leave_receiving(&self, epoch: CaptureEpoch, force: bool) -> bool {
        let old = self.0.control.load(Ordering::Acquire);
        if old & (RECEIVING | STOPPED | IN_CALLBACK) != RECEIVING || self.epoch_from(old) != epoch {
            return false;
        }
        let generation = epoch.generation;
        if generation == GENERATION_MAX { self.source_lost(); return false; }
        let new = (old & !(RECEIVING | GENERATION)) | ((generation + 1) << COUNT_BITS)
            | if force { ADMISSIONS_CLOSED } else { 0 };
        self.0.control.compare_exchange(old, new, Ordering::AcqRel, Ordering::Acquire).is_ok()
    }
    pub fn active(&self) -> bool {
        let word = self.0.control.load(Ordering::Acquire);
        if word & ACTIVE == 0 || word & STOPPED != 0 {
            return false;
        }
        if self.0.deadline.load(Ordering::Acquire) <= now_ms() {
            self.disable();
            return false;
        }
        true
    }
    pub fn idle(&self) -> bool {
        self.0.control.load(Ordering::Acquire) & COUNT == 0
    }
    fn source_lost(&self) {
        self.0.control.fetch_or(STOPPED, Ordering::AcqRel);
        self.0.healthy.store(false, Ordering::Release);
        self.disable();
    }
    #[cfg(any(windows, target_os = "macos", test))]
    fn begin_event(&self) -> CallbackGuard {
        let word = self.0.control.fetch_or(IN_CALLBACK, Ordering::AcqRel);
        let route = if word & STOPPED != 0 {
            CaptureRoute::Local(self.epoch_from(word))
        } else if word & ACTIVE != 0 && self.0.deadline.load(Ordering::Acquire) <= now_ms() {
            self.disable();
            self.route_from(self.0.control.load(Ordering::Acquire) & !ACTIVE)
        } else {
            self.route_from(word)
        };
        CallbackGuard {
            gate: self.clone(),
            route,
        }
    }
    #[cfg(any(windows, target_os = "macos", test))]
    fn set_held(&self, count: u64) {
        // u16 key identities plus u8 button identities total at most 65792.
        debug_assert!(count <= COUNT);
        let mut old = self.0.control.load(Ordering::Acquire);
        loop {
            let new = (old & !COUNT) | (count & COUNT);
            match self.0.control.compare_exchange_weak(
                old,
                new,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(actual) => old = actual,
            }
        }
    }
    #[cfg(any(windows, all(test, target_os = "macos")))]
    fn snapshot(&self) -> CaptureRoute {
        self.active();
        self.route_from(self.0.control.load(Ordering::Acquire))
    }
}
#[cfg(any(windows, target_os = "macos", test))]
struct CallbackGuard {
    gate: CaptureGate,
    route: CaptureRoute,
}
#[cfg(any(windows, target_os = "macos", test))]
impl Drop for CallbackGuard {
    fn drop(&mut self) {
        self.gate
            .0
            .control
            .fetch_and(!IN_CALLBACK, Ordering::AcqRel);
    }
}
impl Drop for CaptureSession {
    fn drop(&mut self) {
        if self.shutdown().is_err() {
            eprintln!("Native capture shutdown incomplete; new capture remains blocked until the old thread exits.");
        }
    }
}
pub struct CaptureSession {
    pub gate: CaptureGate,
    #[cfg(any(windows, target_os = "macos"))]
    _thread: CaptureThread,
    return_position: std::sync::Mutex<Option<(f64, f64)>>,
    receive_driver: Option<ReceiveDriver>,
    receive_control: Option<ReceiveControl>,
}
impl CaptureSession {
    pub fn take_receive_driver(&mut self) -> Result<Option<ReceiveDriver>> {
        Ok(self.receive_driver.take())
    }
    pub fn shutdown(&mut self) -> Result<()> {
        anyhow::ensure!(
            self.shutdown_wait(std::time::Duration::from_millis(500))?,
            "native capture shutdown timed out; original hook still owns suppressed releases"
        );
        Ok(())
    }
    /// Poll the original owner without waiting on join. Keep this session and
    /// its process alive while false/pending; a process exit would kill the hook.
    pub fn shutdown_poll(&mut self) -> Result<bool> {
        self.shutdown_wait(std::time::Duration::ZERO)
    }
    pub fn shutdown_pending(&self) -> bool {
        #[cfg(any(windows, target_os = "macos"))]
        {
            self._thread.pending()
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        {
            false
        }
    }
    fn shutdown_wait(&mut self, timeout: std::time::Duration) -> Result<bool> {
        if let Some(control) = &self.receive_control { control.request_stop(); control.wake_loop(); }
        let restored = self.restore_checked();
        #[cfg(any(windows, target_os = "macos"))]
        let stopped = self._thread.shutdown(timeout);
        #[cfg(not(any(windows, target_os = "macos")))]
        let _ = timeout;
        restored?;
        anyhow::ensure!(
            self.gate.0.healthy.load(Ordering::Acquire),
            "native capture source lost; input recovery cannot be confirmed"
        );
        #[cfg(any(windows, target_os = "macos"))]
        if !stopped {
            if self._thread.pending() {
                return Ok(false);
            }
            anyhow::bail!("native capture thread or reservation release failed");
        }
        if let Some(control) = &self.receive_control {
            anyhow::ensure!(control.outcome() == ReceiveAck::Retired,
                "native receiver cleanup remains unconfirmed");
        }
        Ok(true)
    }
    pub fn begin(&self, return_x: f64, return_y: f64) -> Result<bool> {
        Ok(self.begin_routed(return_x, return_y)?.is_some())
    }
    pub fn begin_routed(&self, return_x: f64, return_y: f64) -> Result<Option<CaptureEpoch>> {
        if self.receive_control.as_ref().is_some_and(|control| control.outcome() != ReceiveAck::Retired) {
            return Ok(None);
        }
        if self.gate.0.return_pending.swap(true, Ordering::AcqRel) { return Ok(None); }
        let Some(epoch) = self.gate.publish(true) else {
            self.gate.0.return_pending.store(false, Ordering::Release);
            return Ok(None);
        };
        let result = (|| {
            *self
                .return_position
                .lock()
                .map_err(|_| anyhow::anyhow!("capture position lock poisoned"))? =
                Some((return_x, return_y));
            pointer(0.5, 0.5)?;
            Ok(Some(epoch))
        })();
        if result.is_err() {
            self.gate.disable();
        }
        result
    }
    pub fn restore_checked(&self) -> Result<()> {
        self.gate.disable();
        anyhow::ensure!(self.gate.0.healthy.load(Ordering::Acquire),
            "native capture source lost; no further pointer request is permitted");
        // A receiver's pointer/owned ledger belongs exclusively to its Native
        // loop. A source return cursor must never race that serialized owner.
        if self.gate.receiving_epoch().is_some() { return Ok(()); }
        let mut position = self
            .return_position
            .lock()
            .map_err(|_| anyhow::anyhow!("capture position lock poisoned"))?;
        if let Some((x, y)) = *position {
            pointer(x, y)?;
            *position = None;
        }
        self.gate.0.return_pending.store(false, Ordering::Release);
        Ok(())
    }
    pub fn restore(&self) {
        let _ = self.restore_checked();
    }
}
#[cfg(any(windows, target_os = "macos"))]
struct NativeReceiveBackend { gate: CaptureGate, observation: SourceObservation }
#[cfg(any(windows, target_os = "macos"))]
impl NativeReceiveBackend {
    fn source_available(&self) -> bool {
        if !self.observation.fresh_at(now_ms(), std::time::SystemTime::now()) {
            self.gate.source_lost();
            return false;
        }
        self.gate.0.healthy.load(Ordering::Acquire)
    }
}
#[cfg(any(windows, target_os = "macos"))]
impl receive_driver::ReceiveBackend for NativeReceiveBackend {
    fn enter_receiving(&mut self) -> std::result::Result<CaptureEpoch, peerglide_actions::execution::NativeFailure> {
        use peerglide_actions::execution::NativeFailure;
        if !self.source_available() { return Err(NativeFailure::PermissionLost); }
        // Failure must leave protection unpublished; publish_mode is one CAS.
        self.gate.enter_receiving().ok_or(NativeFailure::BeforeSubmission)
    }
    fn is_current(&self, epoch: CaptureEpoch) -> bool { self.gate.receiving_epoch() == Some(epoch) }
    fn source_alive(&self) -> bool { self.source_available() }
    fn consumed_modifiers_held(&self) -> bool { native::suppressed_modifiers_held() }
    fn pointer(&mut self, point: ReceivePoint) -> std::result::Result<(), peerglide_actions::execution::NativeFailure> {
        if !self.source_available() { return Err(peerglide_actions::execution::NativeFailure::PermissionLost); }
        native::receive_pointer(point.x, point.y)
    }
    fn input(&mut self, event: Input) -> std::result::Result<(), peerglide_actions::execution::NativeFailure> {
        if !self.source_available() { return Err(peerglide_actions::execution::NativeFailure::PermissionLost); }
        native::receive_inject(event)
    }
    fn leave_receiving(&mut self, epoch: CaptureEpoch) -> std::result::Result<(), peerglide_actions::execution::NativeFailure> {
        if !self.source_available() { return Err(peerglide_actions::execution::NativeFailure::PermissionLost); }
        self.gate.leave_receiving(epoch, false).then_some(())
            .ok_or(peerglide_actions::execution::NativeFailure::TargetLost)
    }
    fn force_return(&mut self, epoch: CaptureEpoch) -> std::result::Result<(), peerglide_actions::execution::NativeFailure> {
        if !self.source_available() { return Err(peerglide_actions::execution::NativeFailure::PermissionLost); }
        self.gate.leave_receiving(epoch, true).then_some(())
            .ok_or(peerglide_actions::execution::NativeFailure::TargetLost)
    }
}
/// The loop's last observation, never a timestamp refreshed by an SDK return.
/// Both clocks are checked: wall time includes sleep on hosts whose monotonic
/// clock pauses; backward/overflowing observations cannot confirm source health.
#[cfg(any(windows, target_os = "macos", test))]
#[derive(Clone, Copy)]
struct SourceObservation { monotonic: u64, wall: std::time::SystemTime }
#[cfg(any(windows, target_os = "macos", test))]
impl SourceObservation {
    fn at(monotonic: u64, wall: std::time::SystemTime) -> Self { Self { monotonic, wall } }
    fn fresh_at(self, monotonic: u64, wall: std::time::SystemTime) -> bool {
        monotonic.checked_sub(self.monotonic).is_some_and(|elapsed| elapsed <= 1500)
            && wall.duration_since(self.wall).is_ok_and(|elapsed| elapsed <= std::time::Duration::from_millis(1500))
    }
}
/// Read-only sampling occurs on the Native loop, never under STATE/callback.
/// The very same pre-query observation also guards every effect and cleanup.
#[cfg(any(windows, target_os = "macos"))]
struct SourceHealth { checked: u64, observation: SourceObservation }
#[cfg(any(windows, target_os = "macos"))]
impl SourceHealth {
    fn new() -> Self { Self { checked: u64::MAX, observation: SourceObservation::at(now_ms(), std::time::SystemTime::now()) } }
    fn sample(&mut self, backend: &mut NativeReceiveBackend, debt: bool) {
        let now = now_ms();
        let wall = std::time::SystemTime::now();
        if debt && !self.observation.fresh_at(now, wall) { backend.gate.source_lost(); return; }
        // Stamp before the health query. A slow query or later SDK call cannot
        // advance this baseline and hide a gap before subsequent owned Up.
        self.observation = SourceObservation::at(now, wall);
        backend.observation = self.observation;
        if self.checked == u64::MAX || now.saturating_sub(self.checked) >= 100 {
            self.checked = now;
            if !context::capture_source_available() { backend.gate.source_lost(); }
        }
    }
}
/// Compiled backend support, independent of OS permissions and peer grants.
pub fn input_capabilities() -> InputCapabilities {
    #[cfg(any(windows, target_os = "macos"))]
    {
        InputCapabilities::new(&native::key_usages(), true, 7, true)
            .expect("static backend capabilities")
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        InputCapabilities::default()
    }
}
pub fn capture(tx: mpsc::Sender<Captured>, target: InputCapabilities) -> Result<CaptureSession> {
    let gate = CaptureGate::default();
    #[cfg(any(windows, target_os = "macos"))]
    {
        let wake = Arc::new(native::ReceiveWake::default());
        let wake_callback = wake.clone();
        let (driver, engine) = receive_driver::channel(128, Arc::new(move || wake_callback.wake()))?;
        let control = driver.control();
        let _ = gate.0.receive.set(control.clone());
        let thread = native::capture_receiving(tx, gate.clone(), target, engine, wake)?;
        Ok(CaptureSession {
            gate,
            _thread: thread,
            return_position: std::sync::Mutex::new(None),
            receive_driver: Some(driver),
            receive_control: Some(control),
        })
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = (tx, gate, target);
        anyhow::bail!("native input currently requires Windows or macOS; use probe on Linux")
    }
}
pub fn inject(event: Input) -> Result<()> {
    #[cfg(any(windows, target_os = "macos"))]
    {
        native::inject(event)
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = event;
        anyhow::bail!("native injection unavailable")
    }
}
pub fn pointer(x: f64, y: f64) -> Result<()> {
    #[cfg(any(windows, target_os = "macos"))]
    {
        native::pointer(x, y)
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = (x, y);
        anyhow::bail!("native injection unavailable")
    }
}
#[cfg(any(windows, target_os = "macos"))]
struct HookState {
    pairs: PairLedger,
    stopping: bool,
    toggle_route: Option<CaptureRoute>,
    #[cfg(windows)]
    local_gestures: Option<gesture_capture::CallbackBroker>,
    tx: mpsc::Sender<Captured>,
    gate: CaptureGate,
    keys: std::collections::BTreeSet<u16>,
    buttons: std::collections::BTreeSet<u8>,
    toggle_armed: bool,
    target: InputCapabilities,
}
#[cfg(any(windows, target_os = "macos"))]
#[derive(Clone, Copy, Default)]
enum PairOwner {
    #[default]
    Empty,
    Local(CaptureEpoch),
    Remote(CaptureEpoch),
    Consumed(CaptureRoute),
    // Protected pairs are fixed-table holds, never added to the legacy sets.
    Protected(CaptureRoute),
}
#[cfg(any(windows, target_os = "macos"))]
impl PairOwner {
    fn route(self, fallback: CaptureRoute) -> CaptureRoute {
        match self {
            Self::Local(e) => CaptureRoute::Local(e),
            Self::Remote(e) => CaptureRoute::Remote(e),
            Self::Consumed(r) | Self::Protected(r) => r,
            Self::Empty => CaptureRoute::Local(fallback.epoch()),
        }
    }
    fn suppressed(self) -> bool {
        matches!(self, Self::Remote(_) | Self::Consumed(_) | Self::Protected(_))
    }
}
#[cfg(any(windows, target_os = "macos"))]
struct PairLedger {
    keys: [PairOwner; 256],
    buttons: [PairOwner; 256],
    unknown: [u64; 1024],
    extra_held: usize,
    force_armed: bool,
    force_pending: bool,
    debt: usize,
}
#[cfg(any(windows, target_os = "macos"))]
impl Default for PairLedger {
    fn default() -> Self {
        Self {
            keys: [PairOwner::Empty; 256],
            buttons: [PairOwner::Empty; 256],
            unknown: [0; 1024],
            extra_held: 0,
            force_armed: false,
            force_pending: false,
            debt: 0,
        }
    }
}
#[cfg(any(windows, target_os = "macos"))]
impl PairLedger {
    fn replace(&mut self, key: bool, index: usize, owner: PairOwner) {
        let slot = if key {
            &mut self.keys[index]
        } else {
            &mut self.buttons[index]
        };
        self.extra_held = self.extra_held - usize::from(matches!(slot, PairOwner::Protected(_)))
            + usize::from(matches!(owner, PairOwner::Protected(_)));
        self.debt = self.debt - usize::from(slot.suppressed()) + usize::from(owner.suppressed());
        *slot = owner;
    }
    fn mouse_debt(&self) -> bool {
        self.buttons.iter().any(|owner| owner.suppressed())
    }
}
#[cfg(any(windows, target_os = "macos"))]
impl HookState {
    fn keep_running(&mut self, stop: bool) -> bool {
        if !self.gate.0.healthy.load(Ordering::Acquire) {
            return false;
        }
        if stop && !self.stopping {
            self.stopping = true;
            self.toggle_armed = false;
            self.toggle_route = None;
            self.gate.disable();
        }
        !self.stopping || self.pairs.debt != 0
    }
    fn source_lost(&mut self) {
        self.gate.source_lost();
    }
    fn suppressed_modifiers(&self) -> bool {
        self.pairs.keys[0xe0..=0xe7].iter().any(|owner| owner.suppressed())
            // Unknown keys cannot safely be proved non-modifiers.
            || self.pairs.unknown.iter().any(|word| *word != 0)
    }
    fn physical_key_held(&self, usage: u16) -> bool {
        self.keys.contains(&usage) || (usize::from(usage) < self.pairs.keys.len()
            && matches!(self.pairs.keys[usize::from(usage)], PairOwner::Protected(_)))
    }
    fn finish_force_chord(&mut self) {
        if self.pairs.force_pending && !self.suppressed_modifiers()
            && !self.physical_key_held(0x29) {
            self.pairs.force_pending = false;
            self.pairs.force_armed = false;
            if let (Some(epoch), Some(control)) = (self.gate.receiving_epoch(), self.gate.0.receive.get()) {
                // The second, fresh complete escape chord authorizes dropping
                // new-input protection only. Native owned debt stays Unknown.
                control.request_force_return(epoch);
            }
        }
    }
    fn protect_pair(&mut self, key: bool, index: usize, down: bool, route: CaptureRoute) -> Option<bool> {
        let old = if key { self.pairs.keys[index] } else { self.pairs.buttons[index] };
        if matches!(old, PairOwner::Protected(_)) {
            if !down {
                self.pairs.replace(key, index, PairOwner::Empty);
                self.update_held();
                self.finish_force_chord();
            }
            return Some(true);
        }
        let held = if key { self.keys.contains(&(index as u16)) } else { self.buttons.contains(&(index as u8)) };
        let protected = matches!(route, CaptureRoute::Receiving(_)) || (!route.remote() && self.suppressed_modifiers());
        if !down || !matches!(old, PairOwner::Empty) || held || !protected { return None; }
        if key && !self.suppressed_modifiers() {
            self.pairs.force_armed = self.gate.0.receive.get()
                .is_some_and(|control| control.outcome() == ReceiveAck::RecoveryRequired);
        }
        self.pairs.replace(key, index, PairOwner::Protected(route));
        self.update_held();
        self.gate.request_takeover(route);
        if key && index == 0x29
            && (self.physical_key_held(0xe0) || self.physical_key_held(0xe4))
            && (self.physical_key_held(0xe2) || self.physical_key_held(0xe6)) {
            self.pairs.force_pending = self.pairs.force_armed;
            self.gate.disable();
            self.emit(Captured::Emergency);
        }
        Some(true)
    }
    fn pair_input(
        &mut self,
        event: Input,
        key: bool,
        index: usize,
        down: bool,
        route: CaptureRoute,
    ) -> bool {
        let old = if key {
            self.pairs.keys[index]
        } else {
            self.pairs.buttons[index]
        };
        let empty = matches!(old, PairOwner::Empty);
        let owner = if empty && down {
            if route.remote() {
                PairOwner::Remote(route.epoch())
            } else {
                PairOwner::Local(route.epoch())
            }
        } else if empty {
            PairOwner::Local(route.epoch())
        } else {
            old
        };
        let sent = self.emit(Captured::Input {
            event,
            route: owner.route(route),
        });
        let owner = if empty && down && owner.suppressed() && !sent {
            if self.suppressed_modifiers() { PairOwner::Consumed(route) }
            else { PairOwner::Local(route.epoch()) }
        } else {
            owner
        };
        let suppress = owner.suppressed();
        self.pairs
            .replace(key, index, if down { owner } else { PairOwner::Empty });
        suppress
    }
    #[cfg(test)]
    fn button(&mut self, button: u8, down: bool) -> bool {
        self.button_routed(button, down, self.gate.snapshot())
    }
    fn button_routed(&mut self, button: u8, down: bool, route: CaptureRoute) -> bool {
        if let Some(block) = self.protect_pair(false, usize::from(button), down, route) { return block; }
        if down {
            self.buttons.insert(button);
        } else {
            self.buttons.remove(&button);
        }
        self.update_held();
        if usize::from(button) >= self.pairs.buttons.len() {
            self.gate.disable();
            self.emit(Captured::UnsupportedInput);
            return false;
        }
        if self.reject_target(Input::Button { button, down }) {
            return self.rejected_pair(false, usize::from(button), down, route);
        }
        self.pair_input(
            Input::Button { button, down },
            false,
            usize::from(button),
            down,
            route,
        )
    }
    fn rejected_pair(&mut self, key: bool, index: usize, down: bool, route: CaptureRoute) -> bool {
        let old = if key {
            self.pairs.keys[index]
        } else {
            self.pairs.buttons[index]
        };
        let owner = if down && matches!(old, PairOwner::Empty) {
            if self.suppressed_modifiers() { PairOwner::Consumed(route) }
            else { PairOwner::Local(route.epoch()) }
        } else {
            old
        };
        self.pairs
            .replace(key, index, if down { owner } else { PairOwner::Empty });
        owner.suppressed()
    }
    fn unsupported_button(&mut self, button: u8, down: bool, route: CaptureRoute) -> bool {
        if let Some(block) = self.protect_pair(false, usize::from(button), down, route) { return block; }
        if down {
            self.buttons.insert(button);
        } else {
            self.buttons.remove(&button);
        }
        self.update_held();
        self.gate.disable();
        self.toggle_armed = false;
        self.toggle_route = None;
        self.emit(Captured::UnsupportedInput);
        if usize::from(button) < self.pairs.buttons.len() {
            self.rejected_pair(false, usize::from(button), down, route)
        } else {
            false
        }
    }
    fn reject_target(&mut self, event: Input) -> bool {
        if self.gate.active() && !self.target.supports(event) {
            self.gate.disable();
            self.toggle_armed = false;
            self.toggle_route = None;
            self.emit(Captured::UnsupportedInput);
            true
        } else {
            false
        }
    }
    #[cfg(test)]
    fn scroll(&mut self, x: i16, y: i16) -> bool {
        self.scroll_routed(x, y, self.gate.snapshot())
    }
    fn scroll_routed(&mut self, x: i16, y: i16, route: CaptureRoute) -> bool {
        if matches!(route, CaptureRoute::Receiving(_)) || (!route.remote() && self.suppressed_modifiers()) {
            self.gate.request_takeover(route);
            return true;
        }
        if self.reject_target(Input::Scroll { x, y }) {
            return false;
        }
        self.emit(Captured::Input {
            event: Input::Scroll { x, y },
            route,
        }) && route.remote()
    }
    fn movement(&mut self, event: Captured, route: CaptureRoute) -> bool {
        if matches!(route, CaptureRoute::Receiving(_)) {
            self.gate.request_takeover(route);
            return true;
        }
        let debt = self.pairs.mouse_debt();
        if !route.remote() && debt {
            return true;
        }
        let sent = self.emit(event);
        route.remote() && (sent || debt)
    }
    fn update_held(&self) {
        self.gate
            .set_held((self.keys.len() + self.buttons.len() + self.pairs.extra_held) as u64);
    }
    fn emit(&self, event: Captured) -> bool {
        if self.tx.try_send(event).is_err() {
            self.gate.disable();
            false
        } else {
            true
        }
    }
    fn unsupported_key(&mut self, code: u16, down: bool) -> bool {
        #[cfg(windows)]
        if let Some(broker) = self.local_gestures.as_mut() {
            broker.key(0x8000 | (code & 0x7fff), down);
            return false;
        }
        let token = 0x8000 | (code & 0x7fff);
        let route = self.gate.route_from(self.gate.0.control.load(Ordering::Acquire));
        let word = usize::from(token) / 64;
        let bit = 1u64 << (token % 64);
        let consumed = self.pairs.unknown[word] & bit != 0;
        if consumed || (down && !self.keys.contains(&token)
            && (matches!(route, CaptureRoute::Receiving(_)) || (!route.remote() && self.suppressed_modifiers()))) {
            if down && !consumed {
                self.pairs.unknown[word] |= bit;
                self.pairs.debt += 1;
                self.pairs.extra_held += 1;
            } else if !down && consumed {
                self.pairs.unknown[word] &= !bit;
                self.pairs.debt -= 1;
                self.pairs.extra_held -= 1;
            }
            self.update_held();
            self.gate.request_takeover(route);
            self.finish_force_chord();
            return true;
        }
        if down {
            self.keys.insert(token);
        } else {
            self.keys.remove(&token);
        }
        self.update_held();
        self.gate.disable();
        self.toggle_armed = false;
        self.toggle_route = None;
        self.emit(Captured::UnsupportedInput);
        false
    }
    #[cfg(any(windows, test))]
    fn key(&mut self, usage: u16, down: bool) -> bool {
        self.key_routed(usage, down, self.gate.snapshot())
    }
    fn key_routed(&mut self, usage: u16, down: bool, route: CaptureRoute) -> bool {
        #[cfg(windows)]
        if let Some(broker) = self.local_gestures.as_mut() {
            broker.key(usage, down);
            return false;
        }
        if usize::from(usage) >= self.pairs.keys.len() {
            return self.unsupported_key(usage, down);
        }
        if let Some(block) = self.protect_pair(true, usize::from(usage), down, route) { return block; }
        let fresh = if down {
            self.keys.insert(usage)
        } else {
            self.keys.remove(&usage);
            false
        };
        self.update_held();
        let ctrl = self.keys.contains(&0xe0) || self.keys.contains(&0xe4);
        let alt = self.keys.contains(&0xe2) || self.keys.contains(&0xe6);
        let old = self.pairs.keys[usize::from(usage)];
        if ctrl && alt && down && usage == 0x29 {
            self.gate.disable();
            self.toggle_armed = false;
            self.toggle_route = None;
            self.emit(Captured::Emergency);
            if matches!(old, PairOwner::Empty) {
                self.pairs
                    .replace(true, usize::from(usage), PairOwner::Consumed(route));
                return true;
            }
            return old.suppressed();
        }
        if ctrl
            && alt
            && fresh
            && usage == 0x4f
            && self.target.motion()
            && (route.remote() || self.pairs.debt == 0)
            && !self.stopping
        {
            self.toggle_armed = true;
            self.toggle_route = Some(route);
            self.pairs
                .replace(true, usize::from(usage), PairOwner::Consumed(route));
        }
        let owner = self.pairs.keys[usize::from(usage)];
        let suppress = if matches!(owner, PairOwner::Consumed(_)) {
            if !down {
                self.pairs
                    .replace(true, usize::from(usage), PairOwner::Empty);
            }
            true
        } else if self.reject_target(Input::Key { usage, down }) {
            self.rejected_pair(true, usize::from(usage), down, route)
        } else {
            self.pair_input(
                Input::Key { usage, down },
                true,
                usize::from(usage),
                down,
                route,
            )
        };
        if self.toggle_armed && self.keys.is_empty() {
            self.toggle_armed = false;
            if let Some(route) = self.toggle_route.take() {
                self.emit(Captured::Toggle { route });
            }
        }
        suppress
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn source_observation_checks_wall_and_monotonic_without_refreshing_after_a_slow_call() {
        use std::time::{Duration, SystemTime};
        let wall=SystemTime::UNIX_EPOCH+Duration::from_secs(10);
        let observed=SourceObservation::at(100,wall);
        assert!(observed.fresh_at(1600,wall+Duration::from_millis(1500)));
        assert!(!observed.fresh_at(1601,wall+Duration::from_millis(10)));
        assert!(!observed.fresh_at(110,wall+Duration::from_millis(1501)));
        assert!(!observed.fresh_at(99,wall));
        assert!(!observed.fresh_at(100,wall-Duration::from_millis(1)));
        assert!(!observed.fresh_at(u64::MAX,wall));
        assert!(!observed.fresh_at(1601,wall+Duration::from_millis(1501)),
            "checking a completed SDK call never advances the original observation");
    }
    #[test]
    fn receiving_is_a_distinct_atomic_mode_and_disable_cannot_remove_protection() {
        let gate = CaptureGate::default();
        let before = gate.route().unwrap();
        let epoch = gate.publish_mode(RECEIVING).unwrap();
        assert_eq!(gate.route(), Some(CaptureRoute::Receiving(epoch)));
        assert!(!gate.is_current(before));
        assert!(!gate.active());
        assert!(!gate.try_activate());
        assert!(gate.try_local_epoch().is_none());
        gate.disable();
        assert_eq!(gate.receiving_epoch(), Some(epoch));
        gate.source_lost();
        assert!(gate.route().is_none());
    }
    #[test]
    fn epochs_do_not_promote_local_or_retired_remote_events_across_phases_or_captures() {
        let gate = CaptureGate::default();
        let local = gate.route().unwrap();
        assert!(matches!(local, CaptureRoute::Local(_)));
        let epoch = gate.publish(true).unwrap();
        assert!(!gate.is_current(local));
        assert!(gate.is_current(CaptureRoute::Remote(epoch)));
        gate.disable();
        assert!(!gate.is_current(CaptureRoute::Remote(epoch)));
        let after = gate.route().unwrap();
        let receive = gate.try_local_epoch().unwrap();
        assert!(!gate.is_current(after));
        assert!(gate.is_current(CaptureRoute::Local(receive)));
        let next = gate.publish(true).unwrap();
        assert_ne!(epoch, next);
        assert!(!gate.is_current(CaptureRoute::Local(receive)));
        let another = CaptureGate::default();
        let another_epoch = another.publish(true).unwrap();
        assert_ne!(next, another_epoch);
        assert!(!another.is_current(CaptureRoute::Remote(next)));
    }
    #[test]
    fn callback_route_is_frozen_and_activation_or_local_fencing_cannot_split_it() {
        let gate = CaptureGate::default();
        let before = gate.route().unwrap();
        {
            let callback = gate.begin_event();
            assert_eq!(callback.route, before);
            assert!(!gate.try_activate());
            assert!(gate.try_local_epoch().is_none());
            gate.disable();
            assert_eq!(callback.route, before);
            assert!(!gate.is_current(callback.route));
        }
        let epoch = gate.publish(true).unwrap();
        let callback = gate.begin_event();
        gate.disable();
        assert_eq!(callback.route, CaptureRoute::Remote(epoch));
        assert!(!gate.is_current(callback.route));
    }
    #[test]
    fn epoch_exhaustion_is_permanently_closed_instead_of_reusing_an_old_route() {
        let gate = CaptureGate::default();
        gate.0.control.store(GENERATION, Ordering::Release);
        assert!(!gate.try_activate());
        assert!(gate.route().is_none());
        assert!(gate.try_local_epoch().is_none());
        gate.disable();
        assert!(gate.route().is_none());
        assert!(!gate.0.healthy.load(Ordering::Acquire));
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    #[test]
    fn checked_restore_reports_failure_and_keeps_the_return_position() {
        let session = CaptureSession {
            gate: CaptureGate::default(),
            return_position: std::sync::Mutex::new(Some((0.2, 0.3))),
            receive_driver: None,
            receive_control: None,
        };
        assert!(session.gate.try_activate());
        assert!(session.restore_checked().is_err());
        assert!(!session.gate.active());
        assert_eq!(*session.return_position.lock().unwrap(), Some((0.2, 0.3)));
    }
    #[cfg(any(windows, target_os = "macos"))]
    #[test]
    fn shutdown_still_reaps_thread_after_restore_lock_failure() {
        let position = std::sync::Mutex::new(None);
        let _ = std::panic::catch_unwind(|| {
            let _guard = position.lock().unwrap();
            panic!("test poison");
        });
        let mut session = CaptureSession {
            gate: CaptureGate::default(),
            _thread: CaptureThread::spawn(|stop| {
                while !stop.load(Ordering::Acquire) {
                    std::thread::yield_now();
                }
            }),
            return_position: position,
            receive_driver: None,
            receive_control: None,
        };
        assert!(session.begin_routed(0.2, 0.3).is_err());
        assert!(
            !session.gate.active(),
            "failed activation must not leave suppression enabled"
        );
        assert!(session.shutdown().is_err());
        assert!(session._thread.shutdown(std::time::Duration::from_secs(1)));
        assert!(!session.shutdown_pending());
        assert!(session.shutdown_poll().is_err());
    }
    #[test]
    fn compiled_backend_capabilities_are_valid_and_do_not_claim_linux_input() {
        let caps = input_capabilities();
        assert!(caps.valid());
        assert_eq!(caps.motion(), cfg!(any(windows, target_os = "macos")));
        #[cfg(any(windows, target_os = "macos"))]
        for usage in native::key_usages() {
            assert!(caps.supports(Input::Key { usage, down: true }));
            assert!(caps.supports(Input::Key { usage, down: false }));
        }
    }
    #[test]
    fn watchdog_is_fail_open_and_explicitly_releasable() {
        let gate = CaptureGate::default();
        assert!(!gate.active());
        assert!(gate.try_activate());
        assert!(gate.active());
        gate.disable();
        assert!(!gate.active());
        gate.0.deadline.store(now_ms(), Ordering::Release);
        assert!(!gate.active());
        gate.set_held(1);
        assert!(!gate.idle());
    }
}

#[cfg(all(test, any(windows, target_os = "macos")))]
mod hook_tests {
    use super::*;
    fn state(capacity: usize) -> (HookState, mpsc::Receiver<Captured>) {
        let (tx, rx) = mpsc::channel(capacity);
        (
            HookState {
                pairs: PairLedger::default(),
                stopping: false,
                toggle_route: None,
                #[cfg(windows)]
                local_gestures: None,
                tx,
                gate: CaptureGate::default(),
                keys: Default::default(),
                buttons: Default::default(),
                toggle_armed: false,
                target: InputCapabilities::new(
                    &(4..=0x73).chain(0xe0..=0xe7).collect::<Vec<_>>(),
                    true,
                    7,
                    true,
                )
                .unwrap(),
            },
            rx,
        )
    }
    fn activate(s: &HookState) -> CaptureEpoch {
        let epoch = s.gate.publish(true).unwrap();
        s.gate.0.deadline.store(u64::MAX, Ordering::Release);
        epoch
    }
    #[test]
    fn receiving_consumes_fixed_pairs_without_queueing_and_keeps_ups_after_mode_retirement() {
        let (mut s, mut rx) = state(1);
        let epoch = s.gate.enter_receiving().unwrap();
        let route = CaptureRoute::Receiving(epoch);
        assert!(s.key_routed(4, true, route));
        assert!(s.key_routed(4, true, route));
        assert!(s.unsupported_button(4, true, route));
        assert!(s.unsupported_key(0x100, true));
        assert_eq!(s.pairs.debt, 3);
        assert!(!s.gate.idle());
        assert!(s.keys.is_empty() && s.buttons.is_empty(), "protected Down never allocates legacy set entries");
        assert!(rx.try_recv().is_err(), "first takeover is atomic, not a queued input");
        // Exercise retained ownership even after a force/retirement transition.
        assert!(s.gate.leave_receiving(epoch, true));
        assert!(s.key_routed(4, false, route));
        assert!(s.unsupported_button(4, false, route));
        assert!(s.unsupported_key(0x100, false));
        assert_eq!(s.pairs.debt, 0);
        assert!(s.gate.idle());
        assert!(rx.try_recv().is_err());
    }
    #[test]
    fn local_passed_hold_prevents_receiver_admission_and_its_up_stays_local() {
        let (mut s, mut rx) = state(8);
        assert!(!s.key(4, true));
        assert!(s.gate.enter_receiving().is_none());
        assert!(!s.key(4, false));
        assert_eq!(s.pairs.debt, 0);
        let epoch = s.gate.enter_receiving().unwrap();
        assert!(s.key_routed(4, true, CaptureRoute::Receiving(epoch)));
        for _ in 0..2 { assert!(matches!(rx.try_recv().unwrap(), Captured::Input {route:CaptureRoute::Local(_),..})); }
        assert!(rx.try_recv().is_err());
    }
    #[test]
    fn old_remote_modifiers_protect_fresh_local_combinations_without_freezing_normal_local_input() {
        let (mut s, mut rx) = state(16);
        activate(&s);
        assert!(s.key(0xe0, true));
        s.gate.disable();
        assert!(s.key(6, true));
        assert!(s.button(1, true));
        assert!(s.scroll(0,1));
        assert!(s.unsupported_key(0x100,true));
        assert!(s.key(0xe0, false));
        // Unknown protected keys conservatively keep the modifier barrier.
        assert!(s.unsupported_key(0x100,false));
        assert!(!s.suppressed_modifiers());
        assert!(!s.key(7,true));
        assert!(!s.key(7,false));
        assert!(s.key(6,false));
        assert!(s.button(1,false));
        assert_eq!(s.pairs.debt,0);
        let events:Vec<_>=std::iter::from_fn(||rx.try_recv().ok()).collect();
        assert_eq!(events.len(),4,"only original Ctrl pair and later plain local key reach the worker");
    }
    #[test]
    fn remote_modifier_combinations_still_forward_but_queue_failure_cannot_fail_open_a_new_combo() {
        let (mut s, mut rx) = state(1);
        activate(&s);
        assert!(s.key(0xe0,true));
        assert!(matches!(rx.try_recv().unwrap(),Captured::Input{event:Input::Key{usage:0xe0,down:true},..}));
        assert!(s.key(6,true));
        assert!(matches!(rx.try_recv().unwrap(),Captured::Input{event:Input::Key{usage:6,down:true},..}));
        assert!(s.key(6,false)); // fills the queue
        assert!(s.key(7,true),"Ctrl held remotely: failed fresh Down must be consumed, never local");
        assert!(!s.gate.active());
        assert!(s.key(7,false));
        assert!(s.key(0xe0,false));
        assert_eq!(s.pairs.debt,0);
    }
    #[test]
    fn stopping_keeps_protected_side_button_and_key_pair_debt_until_observed_up() {
        let (mut s, _rx) = state(1);
        let epoch=s.gate.enter_receiving().unwrap();
        let route=CaptureRoute::Receiving(epoch);
        assert!(s.unsupported_button(32,true,route));
        assert!(s.key_routed(0xe2,true,route));
        assert!(s.keep_running(true));
        assert!(s.gate.leave_receiving(epoch,true));
        assert!(s.unsupported_button(32,false,route));
        assert!(s.keep_running(true));
        assert!(s.key_routed(0xe2,false,route));
        assert!(!s.keep_running(true));
    }
    #[test]
    fn key_repeat_and_up_keep_the_remote_down_owner_after_disable_and_queue_pressure() {
        let (mut s, mut rx) = state(1);
        let epoch = activate(&s);
        assert!(s.key(4, true));
        s.gate.disable();
        assert!(
            s.key(4, true),
            "an already suppressed repeat cannot fail open"
        );
        assert!(
            s.key(4, false),
            "the matching up must not leak locally even when full"
        );
        assert_eq!(s.pairs.debt, 0);
        assert!(
            matches!(rx.try_recv().unwrap(),Captured::Input{event:Input::Key{usage:4,down:true},route:CaptureRoute::Remote(actual)} if actual==epoch)
        );
        assert!(rx.try_recv().is_err());
        assert!(s.gate.idle());
    }
    #[test]
    fn local_down_and_a_failed_remote_down_both_keep_their_up_local() {
        let (mut s, mut rx) = state(8);
        let local = s.gate.route().unwrap();
        assert!(!s.key(4, true));
        assert!(s.gate.try_local_epoch().is_none());
        assert!(!s.gate.try_activate());
        s.gate.disable();
        assert!(!s.key(4, false));
        for _ in 0..2 {
            assert!(matches!(rx.try_recv().unwrap(),Captured::Input{route,..} if route==local));
        }
        let (mut s, mut rx) = state(1);
        activate(&s);
        s.emit(Captured::Emergency);
        assert!(
            !s.key(4, true),
            "a fresh down not enqueued remotely must pass locally"
        );
        assert_eq!(s.pairs.debt, 0);
        rx.try_recv().unwrap();
        assert!(!s.key(4, false));
        assert!(matches!(
            rx.try_recv().unwrap(),
            Captured::Input {
                event: Input::Key { down: false, .. },
                route: CaptureRoute::Local(_)
            }
        ));
    }
    #[test]
    fn all_basic_buttons_keep_pair_debt_until_the_original_up_and_old_up_never_matches_new_epoch() {
        for button in 1..=3 {
            let (mut s, mut rx) = state(8);
            let epoch = activate(&s);
            assert!(s.button(button, true));
            s.gate.disable();
            assert!(s.button(button, true));
            assert!(s.pairs.mouse_debt());
            assert!(s.button(button, false));
            assert_eq!(s.pairs.debt, 0);
            let new = s.gate.publish(true).unwrap();
            assert_ne!(epoch, new);
            for _ in 0..3 {
                let event = rx.try_recv().unwrap();
                assert!(
                    matches!(event,Captured::Input{route:CaptureRoute::Remote(old),..} if old==epoch)
                );
                assert!(!s.gate.is_current(CaptureRoute::Remote(epoch)));
            }
        }
    }
    #[test]
    fn delivery_does_not_switch_to_local_between_callback_snapshot_and_enqueue() {
        let (mut s, mut rx) = state(8);
        let epoch = activate(&s);
        let callback = s.gate.begin_event();
        s.gate.disable();
        assert!(s.key_routed(4, true, callback.route));
        assert!(
            matches!(rx.try_recv().unwrap(),Captured::Input{route:CaptureRoute::Remote(old),..} if old==epoch)
        );
        drop(callback);
        assert!(s.key(4, false));
        assert_eq!(s.pairs.debt, 0);
    }
    #[test]
    fn an_unmatched_up_is_never_invented_as_remote_and_unsupported_buttons_do_not_alias_middle() {
        let (mut s, mut rx) = state(8);
        activate(&s);
        assert!(!s.key(4, false));
        assert!(matches!(
            rx.try_recv().unwrap(),
            Captured::Input {
                event: Input::Key { down: false, .. },
                route: CaptureRoute::Local(_)
            }
        ));
        assert!(s.button(3, true));
        assert!(!s.unsupported_button(4, true, s.gate.snapshot()));
        assert!(!s.unsupported_button(4, false, s.gate.snapshot()));
        assert_eq!(
            s.pairs.debt, 1,
            "the real middle-button owner must survive a side-button pair"
        );
        assert!(s.button(3, false));
        assert_eq!(s.pairs.debt, 0);
    }
    #[test]
    fn motion_and_scroll_use_frozen_routes_and_local_motion_does_not_leak_an_owned_drag() {
        let (mut s, mut rx) = state(8);
        let epoch = activate(&s);
        let route = CaptureRoute::Remote(epoch);
        assert!(s.movement(
            Captured::Motion {
                dx: 1.0,
                dy: 2.0,
                route
            },
            route
        ));
        assert!(s.scroll_routed(0, 1, route));
        assert!(s.button(1, true));
        s.gate.disable();
        let local = s.gate.route().unwrap();
        assert!(s.movement(
            Captured::Position {
                x: 1.0,
                y: 2.0,
                width: 100.0,
                height: 100.0,
                route: local
            },
            local
        ));
        assert!(s.button(1, false));
        let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert_eq!(events.len(), 4);
        assert!(!events
            .iter()
            .any(|event| matches!(event, Captured::Position { .. })));
    }
    #[test]
    fn normal_shutdown_poll_retains_the_original_hook_until_suppressed_up_is_drained() {
        let (mut s, _events) = state(8);
        activate(&s);
        assert!(s.key(4, true));
        let gate = s.gate.clone();
        let blocked = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let observed = blocked.clone();
        let (up, release) = std::sync::mpsc::channel();
        let thread = CaptureThread::spawn(move |stop| loop {
            if release.try_recv().is_ok() {
                observed.store(s.key(4, false), Ordering::Release);
            }
            if !s.keep_running(stop.load(Ordering::Acquire)) {
                break;
            }
            std::thread::yield_now();
        });
        let mut session = CaptureSession {
            gate,
            _thread: thread,
            return_position: std::sync::Mutex::new(None),
            receive_driver: None,
            receive_control: None,
        };
        struct Rescue(Option<std::sync::mpsc::Sender<()>>);
        impl Drop for Rescue {
            fn drop(&mut self) {
                if let Some(up) = self.0.take() {
                    let _ = up.send(());
                }
            }
        }
        let mut rescue = Rescue(Some(up));
        assert!(!session.shutdown_poll().unwrap());
        assert!(session.shutdown_pending());
        rescue.0.take().unwrap().send(()).unwrap();
        assert!(session._thread.shutdown(std::time::Duration::from_secs(1)));
        assert!(session.shutdown_poll().unwrap());
        assert!(!session.shutdown_pending());
        assert!(blocked.load(Ordering::Acquire));
    }
    #[test]
    fn lost_source_finishes_as_error_instead_of_claiming_recovery_or_waiting_forever() {
        let (mut s, _events) = state(8);
        activate(&s);
        assert!(s.button(1, true));
        s.source_lost();
        assert!(!s.keep_running(true));
        assert!(s.gate.route().is_none());
        let mut session = CaptureSession {
            gate: s.gate.clone(),
            _thread: CaptureThread::spawn(|_| ()),
            return_position: std::sync::Mutex::new(None),
            receive_driver: None,
            receive_control: None,
        };
        assert!(session._thread.shutdown(std::time::Duration::from_secs(1)));
        assert!(session.shutdown_poll().is_err());
        assert!(!session.shutdown_pending());
    }
    #[test]
    fn target_key_rejection_is_local_and_does_not_swallow_the_press() {
        let (mut s, mut rx) = state(20);
        s.target = InputCapabilities::new(&[], true, 7, true).unwrap();
        assert!(s.gate.try_activate());
        assert!(!s.key(4, true));
        assert!(!s.gate.active());
        assert!(matches!(rx.try_recv().unwrap(), Captured::UnsupportedInput));
        assert!(!s.gate.try_activate());
        assert!(!s.key(4, false));
        assert!(s.gate.idle());
    }
    #[test]
    fn target_button_and_scroll_rejections_fail_open() {
        let (mut s, mut rx) = state(20);
        s.target = InputCapabilities::new(&[], true, 0, false).unwrap();
        assert!(s.gate.try_activate());
        s.scroll(0, 1);
        assert!(!s.gate.active());
        assert!(matches!(rx.try_recv().unwrap(), Captured::UnsupportedInput));
        assert!(s.gate.try_activate());
        s.button(1, true);
        assert!(!s.gate.active());
        assert!(matches!(rx.try_recv().unwrap(), Captured::UnsupportedInput));
        s.button(1, false);
        assert!(s.gate.idle());
    }
    #[test]
    fn unavailable_target_does_not_reserve_the_switch_arrow() {
        let (mut s, mut rx) = state(20);
        s.target = InputCapabilities::default();
        for (usage, down) in [
            (0xe0, true),
            (0xe2, true),
            (0x4f, true),
            (0x4f, false),
            (0xe0, false),
            (0xe2, false),
        ] {
            assert!(!s.key(usage, down));
        }
        while let Ok(event) = rx.try_recv() {
            assert!(!matches!(event, Captured::Toggle { .. }));
        }
    }
    #[test]
    fn held_switch_chord_cannot_resume_control_on_autorepeat() {
        let (mut s, mut rx) = state(20);
        s.keys.extend([0xe0, 0xe2, 0x4f]);
        s.update_held();
        assert!(!s.key(0x4f, true));
        for usage in [0x4f, 0xe0, 0xe2] {
            s.key(usage, false);
        }
        while let Ok(event) = rx.try_recv() {
            assert!(!matches!(event, Captured::Toggle { .. }));
        }
        assert!(s.gate.idle());
    }
    #[test]
    fn hotkey_waits_for_release_and_never_forwards_arrow() {
        let (mut s, mut rx) = state(20);
        s.key(0xe0, true);
        s.key(0xe2, true);
        assert!(s.key(0x4f, true));
        assert!(!s.gate.idle());
        assert!(s.key(0x4f, false));
        s.key(0xe0, false);
        s.key(0xe2, false);
        assert!(s.gate.idle());
        let mut toggles = 0;
        while let Ok(e) = rx.try_recv() {
            match e {
                Captured::Toggle { .. } => toggles += 1,
                Captured::Input {
                    event: Input::Key { usage: 0x4f, .. },
                    ..
                } => panic!("switch arrow leaked"),
                _ => (),
            }
        }
        assert_eq!(toggles, 1);
    }
    #[test]
    fn unsupported_key_cancels_control_and_blocks_acquisition_until_release() {
        let (mut s, mut rx) = state(20);
        assert!(s.gate.try_activate());
        assert!(!s.unsupported_key(0x100, true));
        assert!(!s.gate.active());
        assert!(!s.gate.try_activate());
        assert!(!s.unsupported_key(0x100, true)); // autorepeat is not a second hold
        assert!(!s.unsupported_key(0x100, false));
        assert!(s.gate.try_activate());
        while let Ok(event) = rx.try_recv() {
            assert!(matches!(event, Captured::UnsupportedInput));
        }
    }
    #[test]
    fn unsupported_key_cancels_an_armed_switch_and_fails_open_when_queue_full() {
        let (mut s, _rx) = state(1);
        s.toggle_armed = true;
        assert!(s.gate.try_activate());
        s.unsupported_key(0x100, true);
        s.unsupported_key(0x100, false);
        assert!(!s.toggle_armed);
        assert!(!s.gate.active());
        assert!(s.gate.idle());
    }
    #[test]
    fn queue_overflow_disables_capture() {
        let (s, _rx) = state(1);
        assert!(s.gate.try_activate());
        s.emit(Captured::Emergency);
        s.emit(Captured::Emergency);
        assert!(!s.gate.active());
    }
}

#[cfg(test)]
mod handoff_tests {
    use super::*;
    #[test]
    fn activation_cannot_split_a_local_callback_or_held_key() {
        let gate = CaptureGate::default();
        {
            let _callback = gate.begin_event();
            assert!(!gate.try_activate());
            gate.set_held(1);
        }
        assert!(!gate.try_activate());
        {
            let _callback = gate.begin_event();
            gate.set_held(0);
            assert!(!gate.try_activate());
        }
        assert!(gate.try_activate());
        gate.0.deadline.store(now_ms(), Ordering::Release);
        assert!(!gate.active());
        gate.refresh();
        assert!(!gate.active());
    }
}

#[cfg(test)]
mod allocation_checks {
    use std::{
        alloc::{GlobalAlloc, Layout, System},
        cell::Cell,
    };
    struct Counting;
    thread_local! {static COUNT:Cell<Option<usize>>=const {Cell::new(None)};}
    #[global_allocator]
    static ALLOCATOR: Counting = Counting;
    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            COUNT.with(|count| {
                if let Some(n) = count.get() {
                    count.set(Some(n + 1));
                }
            });
            System.alloc(layout)
        }
        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            System.dealloc(ptr, layout);
        }
        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            COUNT.with(|count| {
                if let Some(n) = count.get() {
                    count.set(Some(n + 1));
                }
            });
            System.realloc(ptr, layout, new_size)
        }
    }
    pub(crate) fn count(f: impl FnOnce()) -> usize {
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                COUNT.with(|count| count.set(None));
            }
        }
        COUNT.with(|count| {
            assert!(count.get().is_none());
            count.set(Some(0));
        });
        let _reset = Reset;
        f();
        COUNT.with(|count| count.get().unwrap())
    }
}
