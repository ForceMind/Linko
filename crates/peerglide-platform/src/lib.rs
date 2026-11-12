//! Native adapters. Capture is disabled automatically when the network watchdog expires.
use anyhow::Result;
use peerglide_protocol::{Input, InputCapabilities};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, OnceLock,
};
use std::time::Instant;
pub mod application_menu;
#[cfg(any(windows, target_os = "macos"))]
mod audio_output;
mod capture_events;
#[cfg(any(windows, target_os = "macos", test))]
mod capture_thread;
#[cfg(any(windows, target_os = "macos"))]
pub mod clipboard;
pub mod context;
pub mod display_layout;
pub mod display_routes;
#[cfg(any(windows, target_os = "macos", test))]
mod held_inputs;
#[cfg(any(windows, target_os = "macos"))]
mod hook_state;
#[cfg(any(windows, test))]
mod inject_thread;
pub mod input_diagnostics;
#[cfg(any(windows, target_os = "macos"))]
mod pair_ledger;
mod receive_driver;
#[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
pub mod system_audio_capture;
pub mod tray;
#[cfg(target_os = "linux")]
pub mod x11_activity;
#[cfg(target_os = "linux")]
pub mod x11_inject;
pub use capture_events::{CaptureEventSender, CaptureEvents};
pub use receive_driver::{
    ReceiveAck, ReceiveControl, ReceiveDriver, ReceiveError, ReceivePoint, ReceiveSnapshot,
    COMMAND_TIMEOUT,
};
#[cfg(any(windows, test))]
pub mod gesture_capture;
mod gesture_queue;
#[cfg(windows)]
pub mod gesture_worker;
#[cfg(any(windows, test))]
mod gesture_worker_core;
#[cfg(any(target_os = "macos", test))]
mod source_cursor;
#[cfg(any(windows, target_os = "macos"))]
use capture_thread::CaptureThread;
#[cfg(any(windows, target_os = "macos"))]
use hook_state::HookState;
#[cfg(any(windows, target_os = "macos"))]
use pair_ledger::{PairLedger, PairOwner};

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub mod macos_audio;
#[cfg(target_os = "macos")]
pub mod macos_native_drop;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub mod windows_audio;

#[cfg(windows)]
mod windows_display;
#[cfg(all(windows, target_env = "msvc"))]
pub mod windows_native_drop;
#[cfg(target_os = "macos")]
use macos as native;
#[cfg(windows)]
use windows as native;

/// Snapshot public display geometry outside the input capture callback.
#[cfg(target_os = "macos")]
pub fn read_display_layout() -> Result<display_layout::Layout> {
    native::read_display_layout()
}

/// Snapshot public display geometry outside the input capture callback.
#[cfg(windows)]
pub fn read_display_layout() -> Result<display_layout::Layout> {
    windows_display::read_display_layout()
}

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

/// Local-only commit result; never a network grant or OS/application receipt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureCommit {
    Committed(CaptureEpoch),
    Retry,
    Cancelled,
}
/// One pending source request. Its admission generation lives in the same
/// atomic word as callback/held/mode state. Never serialized as peer authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CaptureCommitToken {
    epoch: CaptureEpoch,
    request: u64,
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
const COMMIT_ARMED: u64 = 1 << 58;
const COUNT_BITS: u32 = 17;
const COUNT: u64 = (1 << COUNT_BITS) - 1;
// Keep route epochs/pair ownership separate from pending-request admission.
// Both bounded generations share the callback/mode CAS; neither ever wraps.
const GENERATION_BITS: u32 = 21;
const GENERATION_MAX: u64 = (1 << GENERATION_BITS) - 1;
const GENERATION: u64 = GENERATION_MAX << COUNT_BITS;
const REQUEST_SHIFT: u32 = COUNT_BITS + GENERATION_BITS;
const REQUEST_MAX: u64 = (1 << (58 - REQUEST_SHIFT)) - 1;
const REQUEST: u64 = REQUEST_MAX << REQUEST_SHIFT;
static NEXT_CAPTURE: AtomicU64 = AtomicU64::new(1);
/// The one local display snapshot selected for a worker, including reconnects.
pub struct ReceiveDisplay {
    #[cfg(any(windows, target_os = "macos"))]
    layout: display_layout::Layout,
    #[cfg(any(windows, target_os = "macos"))]
    target: display_layout::PlacementTarget,
}

impl ReceiveDisplay {
    pub fn select(origin: (f64, f64)) -> Result<Self> {
        #[cfg(any(windows, target_os = "macos"))]
        {
            let layout = read_display_layout()?;
            let target = layout
                .placement_at_origin(origin.0, origin.1)
                .ok_or_else(|| {
                    anyhow::anyhow!("requested local display origin is missing or ambiguous")
                })?;
            Ok(Self { layout, target })
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        {
            let _ = origin;
            anyhow::bail!("local receive display requires Windows or macOS")
        }
    }
}

struct GateState {
    instance: u64,
    deadline: AtomicU64,
    control: Arc<AtomicU64>,
    healthy: std::sync::atomic::AtomicBool,
    receive: OnceLock<ReceiveControl>,
    #[cfg(any(windows, target_os = "macos"))]
    receive_display: OnceLock<Arc<ReceiveDisplay>>,
    return_pending: std::sync::atomic::AtomicBool,
    diagnostics: OnceLock<Arc<input_diagnostics::InputDiagnostics>>,
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
            control: Arc::new(AtomicU64::new(if instance == 0 { STOPPED } else { 0 })),
            healthy: std::sync::atomic::AtomicBool::new(instance != 0),
            receive: OnceLock::new(),
            #[cfg(any(windows, target_os = "macos"))]
            receive_display: OnceLock::new(),
            return_pending: std::sync::atomic::AtomicBool::new(false),
            diagnostics: OnceLock::new(),
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
        let new = (old & REQUEST) | (next << COUNT_BITS) | mode;
        self.0
            .control
            .compare_exchange(old, new, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| self.epoch_from(new))
    }
    pub fn try_activate(&self) -> bool {
        self.publish(true).is_some()
    }
    pub fn prepare_commit(&self, expected: CaptureEpoch) -> Option<CaptureCommitToken> {
        let word = self.0.control.load(Ordering::Acquire);
        if !self.0.healthy.load(Ordering::Acquire)
            || self.0.return_pending.load(Ordering::Acquire)
            || self.epoch_from(word) != expected
            || word & (ACTIVE | RECEIVING | STOPPED | ADMISSIONS_CLOSED | COUNT) != 0
        {
            return None;
        }
        let Some(request) = ((word & REQUEST) >> REQUEST_SHIFT)
            .checked_add(1)
            .filter(|request| *request <= REQUEST_MAX)
        else {
            self.source_lost();
            return None;
        };
        // A movement callback can still be exiting when its event is consumed.
        // Arming leaves its frozen route/pair epoch unchanged. A physical Down
        // atomically clears ARMED with COUNT, even if it completes before commit.
        let armed = (word & !REQUEST) | (request << REQUEST_SHIFT) | COMMIT_ARMED;
        self.0
            .control
            .compare_exchange(word, armed, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| CaptureCommitToken {
                epoch: expected,
                request,
            })
    }
    pub fn commit_pending(&self, token: CaptureCommitToken) -> bool {
        let word = self.0.control.load(Ordering::Acquire);
        !self.0.return_pending.load(Ordering::Acquire)
            && !matches!(
                self.commit_blocker(token, u64::MAX, 0, word),
                Some(CaptureCommit::Cancelled)
            )
    }
    fn commit_blocker(
        &self,
        token: CaptureCommitToken,
        deadline: u64,
        now: u64,
        word: u64,
    ) -> Option<CaptureCommit> {
        if now >= deadline
            || !self.0.healthy.load(Ordering::Acquire)
            || self.epoch_from(word) != token.epoch
            || (word & REQUEST) >> REQUEST_SHIFT != token.request
            || word & COMMIT_ARMED == 0
            || word & (ACTIVE | RECEIVING | STOPPED | ADMISSIONS_CLOSED | COUNT) != 0
        {
            Some(CaptureCommit::Cancelled)
        } else if word & IN_CALLBACK != 0 {
            Some(CaptureCommit::Retry)
        } else {
            None
        }
    }
    fn commit_from(&self, token: CaptureCommitToken, deadline: u64, now: u64) -> CaptureCommit {
        let word = self.0.control.load(Ordering::Acquire);
        self.commit_from_word(token, deadline, now, word, || {})
    }
    /// Pure read for source cursor reconciliation outside callbacks. An expired
    /// watchdog deadline already means Local, even before an event disables it.
    #[cfg(any(target_os = "macos", test))]
    fn remote_live(&self) -> Option<CaptureEpoch> {
        let word = self.0.control.load(Ordering::Acquire);
        let live = self.0.healthy.load(Ordering::Acquire)
            && word & (ACTIVE | RECEIVING | STOPPED | ADMISSIONS_CLOSED) == ACTIVE
            && self.0.deadline.load(Ordering::Acquire) > now_ms();
        live.then(|| self.epoch_from(word))
    }
    fn source_commit_current(&self, epoch: CaptureEpoch) -> bool {
        let word = self.0.control.load(Ordering::Acquire);
        self.0.healthy.load(Ordering::Acquire)
            && word & (ACTIVE | RECEIVING | STOPPED | ADMISSIONS_CLOSED) == ACTIVE
            && self.epoch_from(word) == epoch
    }
    fn commit_from_word(
        &self,
        token: CaptureCommitToken,
        deadline: u64,
        now: u64,
        word: u64,
        before_cas: impl FnOnce(),
    ) -> CaptureCommit {
        if let Some(blocked) = self.commit_blocker(token, deadline, now, word) {
            return blocked;
        }
        let Some(next) = token
            .epoch
            .generation
            .checked_add(1)
            .filter(|next| *next <= GENERATION_MAX)
        else {
            self.source_lost();
            return CaptureCommit::Cancelled;
        };
        // A scheduler pause may cross the original request deadline after this
        // check but before the CAS. Callbacks must fail open even before the
        // async caller gets its fresh readback and permits normal lease renewal.
        self.0
            .deadline
            .store(now.saturating_add(1000).min(deadline), Ordering::Release);
        let committed = (word & REQUEST) | (next << COUNT_BITS) | ACTIVE;
        before_cas();
        match self
            .0
            .control
            .compare_exchange(word, committed, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => CaptureCommit::Committed(self.epoch_from(committed)),
            Err(actual) => self
                .commit_blocker(token, deadline, now, actual)
                .unwrap_or(CaptureCommit::Retry),
        }
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
            if let Some(control) = self.0.receive.get() {
                control.request_recovery(epoch);
            }
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
                (old & !(ACTIVE | RECEIVING | COMMIT_ARMED)) | STOPPED
            } else {
                (old & !(ACTIVE | RECEIVING | GENERATION | COMMIT_ARMED))
                    | ((generation + 1) << COUNT_BITS)
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
        if self.0.return_pending.load(Ordering::Acquire) {
            return None;
        }
        self.publish_mode(RECEIVING)
    }
    #[cfg(any(windows, target_os = "macos"))]
    fn leave_receiving(&self, epoch: CaptureEpoch, force: bool) -> bool {
        let old = self.0.control.load(Ordering::Acquire);
        if old & (RECEIVING | STOPPED | IN_CALLBACK) != RECEIVING || self.epoch_from(old) != epoch {
            return false;
        }
        let generation = epoch.generation;
        if generation == GENERATION_MAX {
            self.source_lost();
            return false;
        }
        let new = (old & !(RECEIVING | GENERATION | COMMIT_ARMED))
            | ((generation + 1) << COUNT_BITS)
            | if force { ADMISSIONS_CLOSED } else { 0 };
        self.0
            .control
            .compare_exchange(old, new, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
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
    /// Retire queued local events when applying a new edge. The same mode /
    /// callback / physical-hold CAS used by capture admission remains in force.
    pub fn reconfigure_local(&self) -> bool {
        self.0.healthy.load(Ordering::Acquire)
            && !self.0.return_pending.load(Ordering::Acquire)
            && self.publish(false).is_some()
    }
    fn finish_outgoing_if_idle(&self, epoch: CaptureEpoch) -> bool {
        let word = self.0.control.load(Ordering::Acquire);
        self.finish_outgoing_from(epoch, word, || {})
    }
    fn finish_outgoing_from(
        &self,
        epoch: CaptureEpoch,
        word: u64,
        before_publish: impl FnOnce(),
    ) -> bool {
        // Ordinary edge return must not terminate a drag or a held modifier.
        // Pair ownership, callback entry and route retirement share this CAS.
        if !self.0.healthy.load(Ordering::Acquire)
            || self.epoch_from(word) != epoch
            || word & (ACTIVE | RECEIVING | IN_CALLBACK | STOPPED | ADMISSIONS_CLOSED | COUNT)
                != ACTIVE
        {
            return false;
        }
        let Some(next) = epoch
            .generation
            .checked_add(1)
            .filter(|next| *next <= GENERATION_MAX)
        else {
            self.source_lost();
            return false;
        };
        let local = (word & !(ACTIVE | GENERATION | COMMIT_ARMED)) | (next << COUNT_BITS);
        before_publish();
        if self
            .0
            .control
            .compare_exchange(word, local, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return false;
        }
        self.0.deadline.store(0, Ordering::Release);
        true
    }
    fn source_lost(&self) {
        self.0.control.fetch_or(STOPPED, Ordering::AcqRel);
        self.0.healthy.store(false, Ordering::Release);
        self.disable();
    }
    #[cfg(any(windows, target_os = "macos", test))]
    fn begin_event(&self) -> CallbackGuard {
        self.begin_event_with_clock(now_ms)
    }
    #[cfg(any(windows, target_os = "macos", test))]
    fn begin_event_with_clock(&self, now: impl FnOnce() -> u64) -> CallbackGuard {
        let word = self.0.control.fetch_or(IN_CALLBACK, Ordering::AcqRel);
        let route = if word & STOPPED != 0 {
            CaptureRoute::Local(self.epoch_from(word))
        } else if word & ACTIVE != 0 && self.0.deadline.load(Ordering::Acquire) <= now() {
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
            let mut new = (old & !COUNT) | (count & COUNT);
            if count != 0 && old & (ACTIVE | RECEIVING) == 0 {
                new &= !COMMIT_ARMED;
            }
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
        if let Some(control) = &self.receive_control {
            control.request_stop();
            control.wake_loop();
        }
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
            anyhow::ensure!(
                control.outcome() == ReceiveAck::Retired,
                "native receiver cleanup remains unconfirmed"
            );
        }
        Ok(true)
    }
    pub fn begin(&self, return_x: f64, return_y: f64) -> Result<bool> {
        Ok(self.begin_routed(return_x, return_y)?.is_some())
    }
    pub fn begin_routed(&self, return_x: f64, return_y: f64) -> Result<Option<CaptureEpoch>> {
        if self
            .receive_control
            .as_ref()
            .is_some_and(|control| control.outcome() != ReceiveAck::Retired)
        {
            return Ok(None);
        }
        if self.gate.0.return_pending.swap(true, Ordering::AcqRel) {
            return Ok(None);
        }
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
            // Windows derives relative movement from hook coordinates and
            // needs room beyond the edge. macOS reads native event deltas.
            #[cfg(windows)]
            pointer(0.5, 0.5)?;
            freeze_source_cursor(&self.gate, epoch);
            Ok(Some(epoch))
        })();
        if result.is_err() {
            self.gate.disable();
        }
        result
    }
    /// Commit only the original Local epoch and original request budget. Busy
    /// callbacks/CAS collisions are retryable; holds/protection/context loss are not.
    pub fn begin_routed_from(
        &self,
        token: CaptureCommitToken,
        deadline: u64,
        return_x: f64,
        return_y: f64,
    ) -> Result<CaptureCommit> {
        if self.receive_control.as_ref().is_some_and(|control| {
            control.stop_requested() || control.outcome() != ReceiveAck::Retired
        }) || self.return_pending_claim_failed()
        {
            return Ok(CaptureCommit::Cancelled);
        }
        let result = self.gate.commit_from(token, deadline, now_ms());
        let CaptureCommit::Committed(epoch) = result else {
            self.gate.0.return_pending.store(false, Ordering::Release);
            return Ok(result);
        };
        // SourceHealth's observed permission/source loss closes this same gate.
        // No extra OS permission query runs on the short asynchronous retry path.
        if now_ms() >= deadline
            || !self.gate.source_commit_current(epoch)
            || self.receive_control.as_ref().is_some_and(|control| {
                control.stop_requested() || control.outcome() != ReceiveAck::Retired
            })
        {
            self.gate.disable();
            self.gate.0.return_pending.store(false, Ordering::Release);
            return Ok(CaptureCommit::Cancelled);
        }
        let effect: Result<()> = (|| {
            *self
                .return_position
                .lock()
                .map_err(|_| anyhow::anyhow!("capture position lock poisoned"))? =
                Some((return_x, return_y));
            // Do not post an absolute move when taking over macOS's relative
            // motion stream. Its displacement can affect the next event.
            // macOS instead disconnects the cursor; event deltas stay valid.
            #[cfg(windows)]
            pointer(0.5, 0.5)?;
            freeze_source_cursor(&self.gate, epoch);
            Ok(())
        })();
        if effect.is_err() {
            self.gate.disable();
        }
        effect?;
        if now_ms() >= deadline
            || !self.gate.source_commit_current(epoch)
            || self.receive_control.as_ref().is_some_and(|control| {
                control.stop_requested() || control.outcome() != ReceiveAck::Retired
            })
        {
            self.restore_checked()?;
            return Ok(CaptureCommit::Cancelled);
        }
        Ok(CaptureCommit::Committed(epoch))
    }
    fn return_pending_claim_failed(&self) -> bool {
        self.gate
            .0
            .return_pending
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
    }
    /// Normal boundary return is conditional; stop/emergency still use the
    /// unconditional recovery path even when physical inputs remain held.
    pub fn restore_at_boundary(&self, epoch: CaptureEpoch) -> Result<bool> {
        // The successful outgoing commit already owns this restoration fence.
        // A refused return must retain it, not clear another phase's ownership.
        if !self.gate.0.return_pending.load(Ordering::Acquire)
            || !self.gate.finish_outgoing_if_idle(epoch)
        {
            return Ok(false);
        }
        self.restore_checked()?;
        Ok(true)
    }
    pub fn restore_checked(&self) -> Result<()> {
        self.gate.disable();
        // Reconnecting is recovery, not a pointer request: it also runs after
        // source loss and before the receiving early return.
        // A failed reconnect is retried by the capture loop while Local; it must
        // not turn an otherwise clean return into a failed session.
        if let Err(error) = release_source_cursor() {
            eprintln!("{error:#}");
        }
        anyhow::ensure!(
            self.gate.0.healthy.load(Ordering::Acquire),
            "native capture source lost; no further pointer request is permitted"
        );
        // A receiver's pointer/owned ledger belongs exclusively to its Native
        // loop. A source return cursor must never race that serialized owner.
        if self.gate.receiving_epoch().is_some() {
            return Ok(());
        }
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
/// Native loop only: tallies an SDK call that held the single native owner too long.
#[cfg(any(windows, target_os = "macos"))]
fn note_slow_sdk(gate: &CaptureGate, started: std::time::Instant, kind: input_diagnostics::Count) {
    if started.elapsed() >= std::time::Duration::from_millis(50) {
        if let Some(diagnostics) = gate.0.diagnostics.get() {
            diagnostics.increment(kind);
        }
    }
}
/// Native loop only: tallies a gap between loop turns long enough to delay commands.
#[cfg(any(windows, target_os = "macos"))]
fn note_loop_gap(gate: &CaptureGate, last: &mut std::time::Instant) {
    let now = std::time::Instant::now();
    if now.duration_since(*last) >= std::time::Duration::from_millis(200) {
        if let Some(diagnostics) = gate.0.diagnostics.get() {
            diagnostics.increment(input_diagnostics::Count::LoopStall);
        }
    }
    *last = now;
}
/// Native-owner SDK pointer request with its aggregate diagnostics. On Windows
/// receiving this runs on the injection thread, never inside a hook callback.
#[cfg(any(windows, target_os = "macos"))]
fn submit_pointer(
    gate: &CaptureGate,
    point: ReceivePoint,
) -> std::result::Result<(), peerglide_actions::execution::NativeFailure> {
    let diagnostics = gate.0.diagnostics.get();
    #[cfg(windows)]
    let before = diagnostics
        .filter(|d| d.sample_cursor(now_ms()))
        .map(|_| windows::diagnostic_cursor_position());
    let started = std::time::Instant::now();
    let result = if let Some(destination) = gate.0.receive_display.get() {
        destination
            .layout
            .place(destination.target, point)
            .ok_or(peerglide_actions::execution::NativeFailure::BeforeSubmission)
            .and_then(|placement| {
                native::receive_on_display(
                    destination.target.display(),
                    destination.target.desktop(),
                    placement,
                )
            })
    } else {
        native::receive_pointer(point.x, point.y)
    };
    note_slow_sdk(gate, started, input_diagnostics::Count::SlowPointer);
    if let Some(diagnostics) = diagnostics {
        use input_diagnostics::Count;
        diagnostics.increment(if result.is_ok() {
            Count::NativeSubmitted
        } else {
            Count::NativeFailed
        });
        #[cfg(windows)]
        if let Some(before) = before.filter(|_| result.is_ok()) {
            diagnostics.increment(match (before, windows::diagnostic_cursor_position()) {
                (Some(a), Some(b)) if a != b => Count::CursorMoved,
                (Some(_), Some(_)) => Count::CursorUnchanged,
                _ => Count::CursorUnknown,
            });
        }
    }
    result
}
#[cfg(any(windows, target_os = "macos"))]
fn submit_input(
    gate: &CaptureGate,
    event: Input,
) -> std::result::Result<(), peerglide_actions::execution::NativeFailure> {
    let started = std::time::Instant::now();
    let result = native::receive_inject(event);
    note_slow_sdk(gate, started, input_diagnostics::Count::SlowInput);
    result
}
/// Injection-thread side of the Windows receiver. Every item is rechecked
/// against the same source observation rule before its SDK call.
#[cfg(windows)]
struct ReceiveSink {
    gate: CaptureGate,
    observation: Arc<std::sync::Mutex<SourceObservation>>,
}
#[cfg(windows)]
impl inject_thread::Sink for ReceiveSink {
    fn start(&mut self) {
        native::prepare_inject_thread();
    }
    fn admit(&mut self, epoch: CaptureEpoch) -> bool {
        let observed = self.observation.lock();
        let observation = *observed.unwrap_or_else(std::sync::PoisonError::into_inner);
        observation.fresh_at(now_ms(), std::time::SystemTime::now())
            && self.gate.0.healthy.load(Ordering::Acquire)
            && self.gate.receiving_epoch() == Some(epoch)
    }
    fn submit(
        &mut self,
        item: inject_thread::Item,
    ) -> std::result::Result<(), peerglide_actions::execution::NativeFailure> {
        match item {
            inject_thread::Item::Pointer(point) => submit_pointer(&self.gate, point),
            inject_thread::Item::Input(event) => submit_input(&self.gate, event),
        }
    }
}
#[cfg(any(windows, target_os = "macos"))]
struct NativeReceiveBackend {
    gate: CaptureGate,
    observation: SourceObservation,
    /// Receiving capture only; owned and stopped by the Native loop.
    #[cfg(windows)]
    inject: Option<inject_thread::InjectThread>,
    #[cfg(windows)]
    published: Arc<std::sync::Mutex<SourceObservation>>,
}
#[cfg(any(windows, target_os = "macos"))]
impl NativeReceiveBackend {
    fn set_observation(&mut self, observation: SourceObservation) {
        self.observation = observation;
        #[cfg(windows)]
        {
            let published = self.published.lock();
            *published.unwrap_or_else(std::sync::PoisonError::into_inner) = observation;
        }
    }
    #[cfg(windows)]
    fn queue(
        &self,
        item: inject_thread::Item,
    ) -> Option<std::result::Result<(), peerglide_actions::execution::NativeFailure>> {
        use peerglide_actions::execution::NativeFailure;
        let inject = self.inject.as_ref()?;
        Some(match self.gate.receiving_epoch() {
            Some(epoch) => inject.push(epoch, item),
            None => Err(NativeFailure::BeforeSubmission),
        })
    }
    /// Discard unstarted work; no new SDK call starts afterwards. Non-blocking.
    #[cfg(windows)]
    fn request_inject_stop(&self) {
        if let Some(inject) = &self.inject {
            inject.request_stop();
        }
    }
    /// Bounded wait for the in-flight SDK call; false means unconfirmed.
    #[cfg(windows)]
    fn join_inject(&mut self, timeout: std::time::Duration) -> bool {
        self.inject.as_mut().is_none_or(|t| t.join(timeout))
    }
    fn source_available(&self) -> bool {
        if !self
            .observation
            .fresh_at(now_ms(), std::time::SystemTime::now())
        {
            self.gate.source_lost();
            return false;
        }
        self.gate.0.healthy.load(Ordering::Acquire)
    }
}
#[cfg(any(windows, target_os = "macos"))]
impl receive_driver::ReceiveBackend for NativeReceiveBackend {
    fn enter_receiving(
        &mut self,
    ) -> std::result::Result<CaptureEpoch, peerglide_actions::execution::NativeFailure> {
        use peerglide_actions::execution::NativeFailure;
        if !self.source_available() {
            return Err(NativeFailure::PermissionLost);
        }
        // Failure must leave protection unpublished; publish_mode is one CAS.
        self.gate
            .enter_receiving()
            .ok_or(NativeFailure::BeforeSubmission)
    }
    fn is_current(&self, epoch: CaptureEpoch) -> bool {
        self.gate.receiving_epoch() == Some(epoch)
    }
    fn source_alive(&self) -> bool {
        self.source_available()
    }
    fn consumed_modifiers_held(&self) -> bool {
        native::suppressed_modifiers_held()
    }
    fn pointer(
        &mut self,
        point: ReceivePoint,
    ) -> std::result::Result<(), peerglide_actions::execution::NativeFailure> {
        if !self.source_available() {
            return Err(peerglide_actions::execution::NativeFailure::PermissionLost);
        }
        #[cfg(windows)]
        if let Some(result) = self.queue(inject_thread::Item::Pointer(point)) {
            return result;
        }
        submit_pointer(&self.gate, point)
    }
    fn input(
        &mut self,
        event: Input,
    ) -> std::result::Result<(), peerglide_actions::execution::NativeFailure> {
        if !self.source_available() {
            return Err(peerglide_actions::execution::NativeFailure::PermissionLost);
        }
        #[cfg(windows)]
        if let Some(result) = self.queue(inject_thread::Item::Input(event)) {
            return result;
        }
        submit_input(&self.gate, event)
    }
    fn leave_receiving(
        &mut self,
        epoch: CaptureEpoch,
    ) -> std::result::Result<(), peerglide_actions::execution::NativeFailure> {
        if !self.source_available() {
            return Err(peerglide_actions::execution::NativeFailure::PermissionLost);
        }
        // Second check behind the Engine fence: never retire with queued effects.
        #[cfg(windows)]
        if self.inject.as_ref().is_some_and(|t| t.pending()) {
            return Err(peerglide_actions::execution::NativeFailure::UnknownOutcome);
        }
        self.gate
            .leave_receiving(epoch, false)
            .then_some(())
            .ok_or(peerglide_actions::execution::NativeFailure::TargetLost)
    }
    fn force_return(
        &mut self,
        epoch: CaptureEpoch,
    ) -> std::result::Result<(), peerglide_actions::execution::NativeFailure> {
        if !self.source_available() {
            return Err(peerglide_actions::execution::NativeFailure::PermissionLost);
        }
        // Forced return permits no further injection: drop unstarted work and
        // keep protection until any in-flight SDK call has returned.
        #[cfg(windows)]
        if self.inject.as_ref().is_some_and(|t| !t.discard()) {
            return Err(peerglide_actions::execution::NativeFailure::UnknownOutcome);
        }
        self.gate
            .leave_receiving(epoch, true)
            .then_some(())
            .ok_or(peerglide_actions::execution::NativeFailure::TargetLost)
    }
    fn note_guard_failure(&mut self, failure: receive_driver::GuardFailure) {
        use input_diagnostics::Count;
        use receive_driver::GuardFailure;
        if let Some(diagnostics) = self.gate.0.diagnostics.get() {
            diagnostics.increment(match failure {
                GuardFailure::Time => Count::GuardTime,
                GuardFailure::State => Count::GuardState,
                GuardFailure::Source => Count::GuardSource,
                GuardFailure::Epoch => Count::GuardEpoch,
                GuardFailure::Modifiers => Count::GuardModifiers,
            });
        }
    }
    #[cfg(windows)]
    fn effects_pending(&mut self) -> bool {
        self.inject.as_ref().is_some_and(|t| t.pending())
    }
    #[cfg(windows)]
    fn effects_failed(&mut self) -> bool {
        self.inject.as_ref().is_some_and(|t| t.take_failure())
    }
    #[cfg(windows)]
    fn take_failed_release(&mut self) -> Option<Input> {
        self.inject.as_ref()?.take_failed_release()
    }
}
/// The loop's last observation, never a timestamp refreshed by an SDK return.
/// Both clocks are checked: wall time includes sleep on hosts whose monotonic
/// clock pauses; backward/overflowing observations cannot confirm source health.
#[cfg(any(windows, target_os = "macos", test))]
#[derive(Clone, Copy)]
struct SourceObservation {
    monotonic: u64,
    wall: std::time::SystemTime,
}
#[cfg(any(windows, target_os = "macos", test))]
impl SourceObservation {
    fn at(monotonic: u64, wall: std::time::SystemTime) -> Self {
        Self { monotonic, wall }
    }
    fn fresh_at(self, monotonic: u64, wall: std::time::SystemTime) -> bool {
        monotonic
            .checked_sub(self.monotonic)
            .is_some_and(|elapsed| elapsed <= 1500)
            && wall
                .duration_since(self.wall)
                .is_ok_and(|elapsed| elapsed <= std::time::Duration::from_millis(1500))
    }
}
/// Read-only sampling occurs on the Native loop, never under STATE/callback.
/// The very same pre-query observation also guards every effect and cleanup.
#[cfg(any(windows, target_os = "macos"))]
struct SourceHealth {
    checked: u64,
    observation: SourceObservation,
}
#[cfg(any(windows, target_os = "macos"))]
impl SourceHealth {
    fn new() -> Self {
        Self {
            checked: u64::MAX,
            observation: SourceObservation::at(now_ms(), std::time::SystemTime::now()),
        }
    }
    fn sample(&mut self, backend: &mut NativeReceiveBackend, debt: bool) {
        let now = now_ms();
        let wall = std::time::SystemTime::now();
        if debt && !self.observation.fresh_at(now, wall) {
            backend.gate.source_lost();
            return;
        }
        // Stamp before the health query. A slow query or later SDK call cannot
        // advance this baseline and hide a gap before subsequent owned Up.
        self.observation = SourceObservation::at(now, wall);
        backend.set_observation(self.observation);
        if self.checked == u64::MAX || now.saturating_sub(self.checked) >= 100 {
            self.checked = now;
            if !context::capture_source_available() {
                backend.gate.source_lost();
            }
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
/// Preallocates the same 128 capture slots used by the desktop session.
pub fn capture_event_channel() -> (CaptureEventSender, CaptureEvents) {
    capture_events::channel(128).expect("fixed capture event capacity")
}

pub fn capture(tx: CaptureEventSender, target: InputCapabilities) -> Result<CaptureSession> {
    capture_with_diagnostics(tx, target, None)
}
pub fn capture_with_diagnostics(
    tx: CaptureEventSender,
    target: InputCapabilities,
    diagnostics: Option<Arc<input_diagnostics::InputDiagnostics>>,
) -> Result<CaptureSession> {
    capture_with_receive_display(tx, target, diagnostics, None)
}

/// Optional local display placement for the existing authenticated native owner.
pub fn capture_with_receive_display(
    tx: CaptureEventSender,
    target: InputCapabilities,
    diagnostics: Option<Arc<input_diagnostics::InputDiagnostics>>,
    receive_display: Option<Arc<ReceiveDisplay>>,
) -> Result<CaptureSession> {
    let gate = CaptureGate::default();
    if let Some(diagnostics) = diagnostics {
        let _ = gate.0.diagnostics.set(diagnostics);
    }
    #[cfg(any(windows, target_os = "macos"))]
    {
        if let Some(display) = receive_display {
            gate.0
                .receive_display
                .set(display)
                .map_err(|_| anyhow::anyhow!("local receive display was already selected"))?;
        }
        let wake = Arc::new(native::ReceiveWake::default());
        let wake_callback = wake.clone();
        let (driver, engine) =
            receive_driver::channel(128, Arc::new(move || wake_callback.wake()), Some(&gate))?;
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
        let _ = (tx, gate, target, receive_display);
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
/// Outside callbacks only. Windows keeps its existing parked cursor instead.
fn freeze_source_cursor(gate: &CaptureGate, epoch: CaptureEpoch) {
    #[cfg(target_os = "macos")]
    native::freeze_source_cursor(|| gate.remote_live() == Some(epoch));
    #[cfg(not(target_os = "macos"))]
    let _ = (gate, epoch);
}
fn release_source_cursor() -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        native::release_source_cursor()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn layout_change_retires_local_events_and_preserves_active_or_held_contexts() {
        let gate = CaptureGate::default();
        let previous = gate.route().unwrap();
        gate.set_held(1);
        assert!(!gate.reconfigure_local());
        assert_eq!(gate.route(), Some(previous));
        gate.set_held(0);
        let callback = gate.begin_event();
        assert!(!gate.reconfigure_local());
        drop(callback);
        assert!(gate.reconfigure_local());
        assert_ne!(gate.route(), Some(previous));
        assert!(gate.try_activate());
        assert!(!gate.reconfigure_local());
        gate.disable();
        gate.0.return_pending.store(true, Ordering::Release);
        assert!(!gate.reconfigure_local());
        gate.0.return_pending.store(false, Ordering::Release);
        gate.source_lost();
        assert!(!gate.reconfigure_local());
    }
    #[test]
    fn boundary_return_reuses_and_preserves_the_original_restoration_fence() {
        let session = CaptureSession {
            gate: CaptureGate::default(),
            #[cfg(any(windows, target_os = "macos"))]
            _thread: CaptureThread::spawn(|_| {}),
            // No OS pointer call or native hook is installed by this fixture.
            return_position: std::sync::Mutex::new(None),
            receive_driver: None,
            receive_control: None,
        };
        assert!(session.gate.try_activate());
        let epoch = session
            .gate
            .epoch_from(session.gate.0.control.load(Ordering::Acquire));
        session.gate.0.return_pending.store(true, Ordering::Release);
        session.gate.set_held(1);
        assert!(!session.restore_at_boundary(epoch).unwrap());
        assert!(session.gate.0.return_pending.load(Ordering::Acquire));
        assert_eq!(session.gate.route(), Some(CaptureRoute::Remote(epoch)));
        session.gate.set_held(0);
        assert!(session.restore_at_boundary(epoch).unwrap());
        assert!(!session.gate.0.return_pending.load(Ordering::Acquire));
        assert!(matches!(session.gate.route(), Some(CaptureRoute::Local(_))));
    }
    #[test]
    fn ordinary_return_preserves_holds_and_live_callbacks_until_idle() {
        let gate = CaptureGate::default();
        assert!(gate.try_activate());
        let epoch = gate.epoch_from(gate.0.control.load(Ordering::Acquire));
        gate.0.deadline.store(u64::MAX, Ordering::Release);
        for held in [1, 2, COUNT] {
            gate.set_held(held);
            let word = gate.0.control.load(Ordering::Acquire);
            assert!(!gate.finish_outgoing_if_idle(epoch));
            assert_eq!(gate.0.control.load(Ordering::Acquire), word);
            assert_eq!(gate.0.deadline.load(Ordering::Acquire), u64::MAX);
        }
        gate.set_held(0);
        let callback = gate.begin_event();
        assert!(!gate.finish_outgoing_if_idle(epoch));
        drop(callback);
        assert!(gate.finish_outgoing_if_idle(epoch));
        assert!(matches!(gate.route(), Some(CaptureRoute::Local(_))));
        assert!(!gate.finish_outgoing_if_idle(epoch));
    }
    #[test]
    fn down_racing_boundary_return_wins_without_retiring_its_remote_pair() {
        let gate = CaptureGate::default();
        assert!(gate.try_activate());
        let epoch = gate.epoch_from(gate.0.control.load(Ordering::Acquire));
        gate.0.deadline.store(u64::MAX, Ordering::Release);
        let word = gate.0.control.load(Ordering::Acquire);
        assert!(!gate.finish_outgoing_from(epoch, word, || {
            let callback = gate.begin_event();
            gate.set_held(1);
            drop(callback);
        }));
        assert_eq!(gate.route(), Some(CaptureRoute::Remote(epoch)));
        assert!(!gate.finish_outgoing_if_idle(epoch));
        // Emergency/stop must still restore even though normal return refused.
        gate.disable();
        assert!(matches!(gate.route(), Some(CaptureRoute::Local(_))));
        assert!(!gate.idle());
    }
    #[test]
    fn ordinary_return_cannot_retire_another_route_or_revive_a_closed_source() {
        let gate = CaptureGate::default();
        assert!(gate.try_activate());
        let old = gate.epoch_from(gate.0.control.load(Ordering::Acquire));
        gate.disable();
        assert!(gate.try_activate());
        let current = gate.epoch_from(gate.0.control.load(Ordering::Acquire));
        assert!(!gate.finish_outgoing_if_idle(old));
        let foreign = CaptureGate::default();
        assert!(!foreign.finish_outgoing_if_idle(current));
        let word = gate.0.control.load(Ordering::Acquire);
        assert!(!gate.finish_outgoing_from(current, word, || gate.source_lost()));
        assert!(gate.route().is_none());
    }
    fn local_commit_token(gate: &CaptureGate) -> CaptureCommitToken {
        let Some(CaptureRoute::Local(epoch)) = gate.route() else {
            panic!("expected local capture");
        };
        gate.prepare_commit(epoch).unwrap()
    }
    #[test]
    fn source_commit_retries_a_live_callback_without_renewing_the_request_budget() {
        let gate = CaptureGate::default();
        let token = local_commit_token(&gate);
        let callback = gate.begin_event();
        assert_eq!(gate.commit_from(token, 1000, 100), CaptureCommit::Retry);
        assert_eq!(gate.0.control.load(Ordering::Acquire) & ACTIVE, 0);
        drop(callback);
        let CaptureCommit::Committed(epoch) = gate.commit_from(token, 1000, 110) else {
            panic!("callback should have drained");
        };
        assert_eq!(
            gate.route_from(gate.0.control.load(Ordering::Acquire)),
            CaptureRoute::Remote(epoch)
        );

        let gate = CaptureGate::default();
        let token = local_commit_token(&gate);
        assert_eq!(
            gate.commit_from(token, 1000, 1000),
            CaptureCommit::Cancelled
        );
        assert_eq!(gate.0.control.load(Ordering::Acquire) & ACTIVE, 0);
    }
    #[test]
    fn source_commit_classifies_cas_callback_collision_and_physical_hold_aba() {
        let gate = CaptureGate::default();
        let token = local_commit_token(&gate);
        let word = gate.0.control.load(Ordering::Acquire);
        let mut callback = None;
        assert_eq!(
            gate.commit_from_word(token, 1000, 100, word, || {
                callback = Some(gate.begin_event());
            }),
            CaptureCommit::Retry
        );
        drop(callback);
        assert!(matches!(
            gate.commit_from(token, 1000, 110),
            CaptureCommit::Committed(_)
        ));

        let gate = CaptureGate::default();
        let token = local_commit_token(&gate);
        let word = gate.0.control.load(Ordering::Acquire);
        assert_eq!(
            gate.commit_from_word(token, 1000, 100, word, || {
                let callback = gate.begin_event();
                assert_eq!(callback.route, CaptureRoute::Local(token.epoch));
                gate.set_held(1);
                gate.set_held(0);
                assert_eq!(gate.0.control.load(Ordering::Acquire) & ACTIVE, 0);
            }),
            CaptureCommit::Cancelled
        );
        assert_eq!(
            gate.0.control.load(Ordering::Acquire) & ACTIVE,
            0,
            "completed hold must not make the old grant eligible again"
        );
        assert!(!gate.commit_pending(token));
        let callback = gate.begin_event();
        assert_eq!(callback.route, CaptureRoute::Local(token.epoch));
    }
    #[test]
    fn source_commit_cancels_holds_epoch_protection_return_and_source_loss() {
        for blocked in [COUNT, ACTIVE, RECEIVING, STOPPED, ADMISSIONS_CLOSED] {
            let gate = CaptureGate::default();
            let token = local_commit_token(&gate);
            gate.0.control.fetch_or(blocked, Ordering::AcqRel);
            assert_eq!(gate.commit_from(token, 1000, 100), CaptureCommit::Cancelled);
        }
        let gate = CaptureGate::default();
        let token = local_commit_token(&gate);
        gate.set_held(1);
        gate.set_held(0);
        assert_eq!(gate.commit_from(token, 1000, 100), CaptureCommit::Cancelled);
        let fresh = local_commit_token(&gate);
        assert_ne!(fresh, token);
        assert_eq!(gate.commit_from(token, 1000, 100), CaptureCommit::Cancelled);
        assert!(gate.commit_pending(fresh));
        gate.disable();
        assert_eq!(gate.commit_from(fresh, 1000, 100), CaptureCommit::Cancelled);

        let gate = CaptureGate::default();
        let token = local_commit_token(&gate);
        let another = CaptureGate::default();
        assert_eq!(
            another.commit_from(token, 1000, 100),
            CaptureCommit::Cancelled
        );
        gate.0.return_pending.store(true, Ordering::Release);
        assert!(!gate.commit_pending(token));
        assert!(gate.prepare_commit(token.epoch).is_none());
        gate.0.return_pending.store(false, Ordering::Release);
        gate.source_lost();
        assert_eq!(gate.commit_from(token, 1000, 100), CaptureCommit::Cancelled);

        let gate = CaptureGate::default();
        let token = local_commit_token(&gate);
        gate.0.control.fetch_or(REQUEST, Ordering::AcqRel);
        assert!(gate.prepare_commit(token.epoch).is_none());
        assert!(
            gate.route().is_none(),
            "request generation exhaustion never wraps"
        );
        assert_eq!(gate.commit_from(token, 1000, 100), CaptureCommit::Cancelled);
    }
    #[test]
    fn source_commit_stop_closes_the_cas_even_before_native_observes_stop() {
        let gate = CaptureGate::default();
        let (driver, _engine) = receive_driver::channel(1, Arc::new(|| {}), Some(&gate)).unwrap();
        let control = driver.control();
        let token = local_commit_token(&gate);
        let word = gate.0.control.load(Ordering::Acquire);
        assert_eq!(control.outcome(), ReceiveAck::Retired);
        assert!(!control.stop_requested());
        assert_eq!(
            gate.commit_from_word(token, 1000, 100, word, || {
                // The real managed-stop control path, after prechecks and before
                // the production publication CAS. No Engine pump is performed.
                control.request_stop();
                assert!(control.stop_requested());
                assert_eq!(control.outcome(), ReceiveAck::Retired);
                let callback = gate.begin_event();
                assert_eq!(callback.route, CaptureRoute::Local(token.epoch));
            }),
            CaptureCommit::Cancelled
        );
        let callback = gate.begin_event();
        assert_eq!(callback.route, CaptureRoute::Local(token.epoch));
        assert_eq!(gate.0.control.load(Ordering::Acquire) & ACTIVE, 0);
        assert!(!gate.commit_pending(token));
        assert!(gate.prepare_commit(token.epoch).is_none());
    }
    #[test]
    fn source_commit_paused_past_request_deadline_is_local_at_the_first_callback() {
        let gate = CaptureGate::default();
        let token = local_commit_token(&gate);
        let word = gate.0.control.load(Ordering::Acquire);
        let clock = std::cell::Cell::new(900);
        let CaptureCommit::Committed(epoch) =
            gate.commit_from_word(token, 1000, clock.get(), word, || clock.set(1001))
        else {
            panic!("controlled CAS window must complete before caller readback");
        };
        assert_eq!(gate.0.deadline.load(Ordering::Acquire), 1000);
        // This is the same callback admission path as Native, immediately after
        // the delayed CAS and before any async freshness check/restore can run.
        let callback = gate.begin_event_with_clock(|| clock.get());
        assert!(matches!(callback.route, CaptureRoute::Local(_)));
        assert_ne!(callback.route, CaptureRoute::Remote(epoch));
        assert_eq!(gate.0.control.load(Ordering::Acquire) & ACTIVE, 0);
    }
    #[test]
    fn source_commit_keeps_a_real_remote_down_after_its_linearization_point() {
        let gate = CaptureGate::default();
        let token = local_commit_token(&gate);
        let CaptureCommit::Committed(epoch) = gate.commit_from(token, 1000, 100) else {
            panic!("idle commit must succeed");
        };
        // Avoid a real clock in this controlled post-CAS callback window.
        gate.0.deadline.store(u64::MAX, Ordering::Release);
        let callback = gate.begin_event();
        assert_eq!(callback.route, CaptureRoute::Remote(epoch));
        gate.set_held(1);
        assert!(gate.source_commit_current(epoch));
        assert_eq!(callback.route, CaptureRoute::Remote(epoch));
        gate.set_held(0);
        assert!(gate.source_commit_current(epoch));
    }
    #[test]
    fn source_commit_cas_never_overwrites_a_new_owner_or_epoch() {
        for mode in [RECEIVING, ADMISSIONS_CLOSED, STOPPED, 0] {
            let gate = CaptureGate::default();
            let token = local_commit_token(&gate);
            let word = gate.0.control.load(Ordering::Acquire);
            assert_eq!(
                gate.commit_from_word(token, 1000, 100, word, || {
                    if mode == 0 {
                        gate.disable();
                    } else {
                        gate.0.control.fetch_or(mode, Ordering::AcqRel);
                    }
                }),
                CaptureCommit::Cancelled
            );
            assert_eq!(gate.0.control.load(Ordering::Acquire) & ACTIVE, 0);
        }
    }
    #[test]
    fn source_commit_old_mode_paths_clear_armed_and_preserve_request_generation() {
        for mode in [0, ACTIVE, RECEIVING] {
            let gate = CaptureGate::default();
            let token = local_commit_token(&gate);
            let epoch = gate.publish_mode(mode).unwrap();
            let word = gate.0.control.load(Ordering::Acquire);
            assert_eq!(word & COMMIT_ARMED, 0);
            assert_eq!((word & REQUEST) >> REQUEST_SHIFT, token.request);
            assert_eq!(gate.epoch_from(word), epoch);
            assert_ne!(epoch, token.epoch);
            assert_eq!(gate.commit_from(token, 1000, 100), CaptureCommit::Cancelled);
        }
        for local_fence in [false, true] {
            let gate = CaptureGate::default();
            let token = local_commit_token(&gate);
            if local_fence {
                assert!(gate.try_local_epoch().is_some());
            } else {
                gate.disable();
            }
            let word = gate.0.control.load(Ordering::Acquire);
            assert_eq!(word & COMMIT_ARMED, 0);
            assert_eq!((word & REQUEST) >> REQUEST_SHIFT, token.request);
            assert_ne!(gate.epoch_from(word), token.epoch);
            assert_eq!(gate.commit_from(token, 1000, 100), CaptureCommit::Cancelled);
        }
    }
    #[test]
    fn source_observation_checks_wall_and_monotonic_without_refreshing_after_a_slow_call() {
        use std::time::{Duration, SystemTime};
        let wall = SystemTime::UNIX_EPOCH + Duration::from_secs(10);
        let observed = SourceObservation::at(100, wall);
        assert!(observed.fresh_at(1600, wall + Duration::from_millis(1500)));
        assert!(!observed.fresh_at(1601, wall + Duration::from_millis(10)));
        assert!(!observed.fresh_at(110, wall + Duration::from_millis(1501)));
        assert!(!observed.fresh_at(99, wall));
        assert!(!observed.fresh_at(100, wall - Duration::from_millis(1)));
        assert!(!observed.fresh_at(u64::MAX, wall));
        assert!(
            !observed.fresh_at(1601, wall + Duration::from_millis(1501)),
            "checking a completed SDK call never advances the original observation"
        );
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
    #[test]
    fn source_cursor_freeze_follows_only_the_exact_live_remote_phase() {
        use crate::source_cursor::{CursorBackend, FreezeState};
        struct Requests(Vec<bool>);
        impl CursorBackend for Requests {
            fn associate(&mut self, connected: bool) -> bool {
                self.0.push(connected);
                true
            }
        }
        let gate = CaptureGate::default();
        let mut native = Requests(Vec::new());
        let mut cursor = FreezeState::new();
        assert_eq!(gate.remote_live(), None);
        // Stop/Emergency/fault disable, watchdog expiry and source loss (last: terminal).
        let endings: [fn(&CaptureGate); 3] = [
            CaptureGate::disable,
            |gate| gate.0.deadline.store(0, Ordering::Release),
            CaptureGate::source_lost,
        ];
        for end in endings {
            assert!(gate.try_activate());
            let epoch = gate.remote_live().unwrap();
            cursor.freeze(gate.remote_live() == Some(epoch), &mut native);
            cursor.freeze(gate.remote_live() == Some(epoch), &mut native);
            cursor.reconcile(gate.remote_live().is_some(), &mut native);
            assert_eq!(native.0, [false]);
            end(&gate);
            assert_eq!(gate.remote_live(), None);
            // Independent loop check and the later session restore both run.
            cursor.reconcile(gate.remote_live().is_some(), &mut native);
            cursor.reconcile(gate.remote_live().is_some(), &mut native);
            assert!(cursor.release(&mut native));
            assert_eq!(native.0, [false, true], "exactly one reconnect per phase");
            gate.disable();
            // A late handoff effect from the finished epoch never disconnects.
            cursor.freeze(gate.remote_live() == Some(epoch), &mut native);
            assert_eq!(native.0, [false, true]);
            native.0.clear();
        }
        assert!(!gate.try_activate(), "source loss is terminal");
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
    fn state(capacity: usize) -> (HookState, CaptureEvents) {
        let (tx, rx) = capture_events::channel(capacity).unwrap();
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
    fn boundary_return_keeps_remote_down_up_pairs_and_then_passes_local_input() {
        for input in [
            Input::Key {
                usage: 4,
                down: true,
            },
            Input::Key {
                usage: 0xe1,
                down: true,
            },
            Input::Button {
                button: 1,
                down: true,
            },
            Input::Button {
                button: 2,
                down: true,
            },
            Input::Button {
                button: 3,
                down: true,
            },
        ] {
            let (mut state, mut events) = state(8);
            let epoch = activate(&state);
            let gate = state.gate.clone();
            for down in [true, false] {
                {
                    let callback = gate.begin_event();
                    assert_eq!(callback.route, CaptureRoute::Remote(epoch));
                    assert!(match input {
                        Input::Key { usage, .. } => state.key_routed(usage, down, callback.route),
                        Input::Button { button, .. } =>
                            state.button_routed(button, down, callback.route),
                        _ => unreachable!(),
                    });
                    assert!(!gate.finish_outgoing_if_idle(epoch));
                }
                if down {
                    assert!(!gate.finish_outgoing_if_idle(epoch));
                }
                let Captured::Input { event, route } = events.try_recv().unwrap() else {
                    panic!("remote pair must stay queued for the same lease");
                };
                assert_eq!(route, CaptureRoute::Remote(epoch));
                assert!(match (input, event) {
                    (
                        Input::Key { usage, .. },
                        Input::Key {
                            usage: actual,
                            down: actual_down,
                        },
                    ) => usage == actual && down == actual_down,
                    (
                        Input::Button { button, .. },
                        Input::Button {
                            button: actual,
                            down: actual_down,
                        },
                    ) => button == actual && down == actual_down,
                    _ => false,
                });
            }
            assert_eq!(state.pairs.debt, 0);
            assert!(gate.finish_outgoing_if_idle(epoch));
            for down in [true, false] {
                let callback = gate.begin_event();
                assert!(matches!(callback.route, CaptureRoute::Local(_)));
                assert!(!state.key_routed(5, down, callback.route));
            }
            assert_eq!(state.pairs.debt, 0);
        }
    }
    #[test]
    fn pending_commit_aba_never_suppresses_a_local_down_or_its_matching_up() {
        let (mut state, mut events) = state(8);
        let gate = state.gate.clone();
        let Some(CaptureRoute::Local(local)) = gate.route() else {
            panic!("expected local route");
        };
        let token = gate.prepare_commit(local).unwrap();
        let word = gate.0.control.load(Ordering::Acquire);
        assert_eq!(
            gate.commit_from_word(token, 1000, 100, word, || {
                for down in [true, false] {
                    let callback = gate.begin_event();
                    assert_eq!(callback.route, CaptureRoute::Local(local));
                    assert!(!state.key_routed(4, down, callback.route));
                }
            }),
            CaptureCommit::Cancelled
        );
        for down in [true, false] {
            let callback = gate.begin_event();
            assert_eq!(callback.route, CaptureRoute::Local(local));
            assert!(!state.key_routed(5, down, callback.route));
        }
        for (usage, down) in [(4, true), (4, false), (5, true), (5, false)] {
            let Captured::Input { event, route } = events.try_recv().unwrap() else {
                panic!("expected the original physical pair");
            };
            assert!(event == Input::Key { usage, down });
            assert_eq!(route, CaptureRoute::Local(local));
        }
        assert_eq!(state.pairs.debt, 0);
    }
    #[test]
    fn committed_source_retains_a_remote_pair_arriving_before_session_readback() {
        let (mut state, mut events) = state(8);
        let gate = state.gate.clone();
        let Some(CaptureRoute::Local(local)) = gate.route() else {
            panic!("expected local route");
        };
        let token = gate.prepare_commit(local).unwrap();
        let CaptureCommit::Committed(epoch) = gate.commit_from(token, 1000, 100) else {
            panic!("idle commit must succeed");
        };
        gate.0.deadline.store(u64::MAX, Ordering::Release);
        for down in [true, false] {
            let callback = gate.begin_event();
            assert_eq!(callback.route, CaptureRoute::Remote(epoch));
            assert!(state.key_routed(4, down, callback.route));
            assert!(gate.source_commit_current(epoch));
        }
        for down in [true, false] {
            let Captured::Input { event, route } = events.try_recv().unwrap() else {
                panic!("expected retained remote pair");
            };
            assert!(event == Input::Key { usage: 4, down });
            assert_eq!(route, CaptureRoute::Remote(epoch));
        }
        assert_eq!(state.pairs.debt, 0);
    }
    #[test]
    fn commit_paused_past_deadline_passes_the_first_physical_down_and_up_locally() {
        let (mut state, mut events) = state(8);
        let gate = state.gate.clone();
        let Some(CaptureRoute::Local(local)) = gate.route() else {
            panic!("expected local route");
        };
        let token = gate.prepare_commit(local).unwrap();
        let word = gate.0.control.load(Ordering::Acquire);
        let clock = std::cell::Cell::new(900);
        let CaptureCommit::Committed(epoch) =
            gate.commit_from_word(token, 1000, clock.get(), word, || clock.set(1001))
        else {
            panic!("controlled CAS window must complete before caller readback");
        };
        for down in [true, false] {
            let callback = gate.begin_event_with_clock(|| clock.get());
            assert!(matches!(callback.route, CaptureRoute::Local(_)));
            assert_ne!(callback.route, CaptureRoute::Remote(epoch));
            assert!(!state.key_routed(4, down, callback.route));
        }
        for down in [true, false] {
            let Captured::Input { event, route } = events.try_recv().unwrap() else {
                panic!("expected local physical pair");
            };
            assert!(event == Input::Key { usage: 4, down });
            assert!(matches!(route, CaptureRoute::Local(_)));
            assert_ne!(route, CaptureRoute::Remote(epoch));
        }
        assert_eq!(state.pairs.debt, 0);
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
        assert!(s.unsupported_key(0x100, true));
        assert_eq!(s.pairs.unknown_debt, 1);
        assert_eq!(s.pairs.debt, 3);
        assert!(!s.gate.idle());
        assert!(
            s.keys.is_empty() && s.buttons.is_empty(),
            "protected Down never allocates legacy set entries"
        );
        assert!(
            rx.try_recv().is_err(),
            "first takeover is atomic, not a queued input"
        );
        // Exercise retained ownership even after a force/retirement transition.
        assert!(s.gate.leave_receiving(epoch, true));
        assert!(s.key_routed(4, false, route));
        assert!(s.unsupported_button(4, false, route));
        assert!(s.unsupported_key(0x100, false));
        assert_eq!(s.pairs.unknown_debt, 0);
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
        for _ in 0..2 {
            assert!(matches!(
                rx.try_recv().unwrap(),
                Captured::Input {
                    route: CaptureRoute::Local(_),
                    ..
                }
            ));
        }
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
        assert!(s.scroll(0, 1));
        assert!(s.unsupported_key(0x100, true));
        assert!(s.key(0xe0, false));
        // Unknown protected keys conservatively keep the modifier barrier.
        assert!(s.unsupported_key(0x100, false));
        assert!(!s.suppressed_modifiers());
        assert!(!s.key(7, true));
        assert!(!s.key(7, false));
        assert!(s.key(6, false));
        assert!(s.button(1, false));
        assert_eq!(s.pairs.debt, 0);
        let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert_eq!(
            events.len(),
            4,
            "only original Ctrl pair and later plain local key reach the worker"
        );
    }
    #[test]
    fn remote_modifier_combinations_still_forward_but_queue_failure_cannot_fail_open_a_new_combo() {
        let (mut s, mut rx) = state(1);
        activate(&s);
        assert!(s.key(0xe0, true));
        assert!(matches!(
            rx.try_recv().unwrap(),
            Captured::Input {
                event: Input::Key {
                    usage: 0xe0,
                    down: true
                },
                ..
            }
        ));
        assert!(s.key(6, true));
        assert!(matches!(
            rx.try_recv().unwrap(),
            Captured::Input {
                event: Input::Key {
                    usage: 6,
                    down: true
                },
                ..
            }
        ));
        assert!(s.key(6, false)); // fills the queue
        assert!(
            s.key(7, true),
            "Ctrl held remotely: failed fresh Down must be consumed, never local"
        );
        assert!(!s.gate.active());
        assert!(s.key(7, false));
        assert!(s.key(0xe0, false));
        assert_eq!(s.pairs.debt, 0);
    }
    #[test]
    fn stopping_keeps_protected_side_button_and_key_pair_debt_until_observed_up() {
        let (mut s, _rx) = state(1);
        let epoch = s.gate.enter_receiving().unwrap();
        let route = CaptureRoute::Receiving(epoch);
        assert!(s.unsupported_button(32, true, route));
        assert!(s.key_routed(0xe2, true, route));
        assert!(s.keep_running(true));
        assert!(s.gate.leave_receiving(epoch, true));
        assert!(s.unsupported_button(32, false, route));
        assert!(s.keep_running(true));
        assert!(s.key_routed(0xe2, false, route));
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
    fn mouse_debt_count_changes_only_on_button_ownership_transitions() {
        let (mut state, _) = state(8);
        let epoch = activate(&state);
        state.pairs.replace(false, 1, PairOwner::Remote(epoch));
        state.pairs.replace(false, 1, PairOwner::Remote(epoch));
        state.pairs.replace(true, 4, PairOwner::Remote(epoch));
        assert_eq!(state.pairs.mouse_debt, 1);
        assert_eq!(state.pairs.debt, 2);
        state.pairs.replace(false, 2, PairOwner::Remote(epoch));
        state.pairs.replace(false, 1, PairOwner::Empty);
        assert_eq!(state.pairs.mouse_debt, 1);
        state.pairs.replace(false, 2, PairOwner::Local(epoch));
        assert!(!state.pairs.mouse_debt());
        assert_eq!(state.pairs.debt, 1);
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
    fn zero_scroll_never_consumes_capture_queue_capacity() {
        let (mut state, mut events) = state(1);
        let diagnostics = Arc::new(input_diagnostics::InputDiagnostics::default());
        assert!(state.gate.0.diagnostics.set(diagnostics.clone()).is_ok());

        assert!(!state.scroll(0, 0));
        assert!(matches!(
            events.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ));

        let epoch = activate(&state);
        assert!(state.scroll(0, 1));
        assert!(state.scroll(0, 0));
        assert!(state.gate.is_current(CaptureRoute::Remote(epoch)));
        assert!(matches!(
            events.try_recv().unwrap(),
            Captured::Input {
                event: Input::Scroll { x: 0, y: 1 },
                route: CaptureRoute::Remote(queued),
            } if queued == epoch
        ));
        assert!(matches!(
            events.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ));
        assert!(diagnostics
            .snapshot(input_diagnostics::Phase::Sample)
            .to_string()
            .split_ascii_whitespace()
            .any(|field| field == "scr_zero=1"));
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
        let (mut s, _rx) = state(1);
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

// Software-only callback/Engine integration; backend effects never call OS input APIs.
#[cfg(all(test, any(windows, target_os = "macos")))]
mod fresh_escape_tests {
    use super::*;
    use crate::receive_driver::{channel, ReceiveBackend, ReceiveEngine};
    use peerglide_actions::execution::NativeFailure;
    use std::{
        future::{poll_fn, Future},
        task::Poll,
    };

    struct Rig {
        state: HookState,
        driver: ReceiveDriver,
        engine: ReceiveEngine,
        fail_up: bool,
        force_calls: usize,
        up_calls: usize,
        arm_ctrl_during_second_up: bool,
        _events: CaptureEvents,
    }
    struct FakeBackend<'a> {
        state: &'a mut HookState,
        fail_up: bool,
        force_calls: &'a mut usize,
        up_calls: &'a mut usize,
        arm_ctrl_during_second_up: bool,
    }
    impl ReceiveBackend for FakeBackend<'_> {
        fn enter_receiving(&mut self) -> Result<CaptureEpoch, NativeFailure> {
            self.state
                .gate
                .enter_receiving()
                .ok_or(NativeFailure::BeforeSubmission)
        }
        fn is_current(&self, epoch: CaptureEpoch) -> bool {
            self.state.gate.receiving_epoch() == Some(epoch)
        }
        fn source_alive(&self) -> bool {
            self.state.gate.0.healthy.load(Ordering::Acquire)
        }
        fn consumed_modifiers_held(&self) -> bool {
            self.state.suppressed_modifiers()
        }
        fn pointer(&mut self, _point: ReceivePoint) -> Result<(), NativeFailure> {
            Ok(())
        }
        fn input(&mut self, event: Input) -> Result<(), NativeFailure> {
            if matches!(
                event,
                Input::Key { down: false, .. } | Input::Button { down: false, .. }
            ) {
                *self.up_calls += 1;
                if self.arm_ctrl_during_second_up && *self.up_calls == 2 {
                    // The first failed cleanup already made outcome Required,
                    // but Engine is still DRAINING during this SDK retry.
                    let epoch = self.state.gate.receiving_epoch().unwrap();
                    let control = self.state.gate.0.receive.get().unwrap();
                    assert_eq!(control.outcome(), ReceiveAck::RecoveryRequired);
                    assert!(!control.can_force_return(epoch));
                    let callback = self.state.gate.begin_event();
                    assert!(self.state.key_routed(0xe0, true, callback.route));
                }
                if self.fail_up {
                    return Err(NativeFailure::PermissionLost);
                }
            }
            Ok(())
        }
        fn leave_receiving(&mut self, epoch: CaptureEpoch) -> Result<(), NativeFailure> {
            self.state
                .gate
                .leave_receiving(epoch, false)
                .then_some(())
                .ok_or(NativeFailure::TargetLost)
        }
        fn force_return(&mut self, epoch: CaptureEpoch) -> Result<(), NativeFailure> {
            assert!(!self.state.suppressed_modifiers());
            self.state
                .gate
                .leave_receiving(epoch, true)
                .then_some(())
                .ok_or(NativeFailure::TargetLost)?;
            *self.force_calls += 1;
            Ok(())
        }
    }
    impl Rig {
        fn new() -> Self {
            let (tx, events) = capture_events::channel(16).unwrap();
            let gate = CaptureGate::default();
            let (driver, engine) = channel(8, Arc::new(|| {}), Some(&gate)).unwrap();
            assert!(gate.0.receive.set(driver.control()).is_ok());
            Self {
                state: HookState {
                    pairs: PairLedger::default(),
                    stopping: false,
                    toggle_route: None,
                    #[cfg(windows)]
                    local_gestures: None,
                    tx,
                    gate,
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
                driver,
                engine,
                fail_up: false,
                force_calls: 0,
                up_calls: 0,
                arm_ctrl_during_second_up: false,
                _events: events,
            }
        }
        fn pump_parts(
            engine: &mut ReceiveEngine,
            state: &mut HookState,
            fail_up: bool,
            forces: &mut usize,
            ups: &mut usize,
            arm_during_retry: bool,
        ) {
            let mut backend = FakeBackend {
                state,
                fail_up,
                force_calls: forces,
                up_calls: ups,
                arm_ctrl_during_second_up: arm_during_retry,
            };
            for _ in 0..32 {
                if !engine.pump(&mut backend, now_ms(), false) {
                    return;
                }
            }
            panic!("bounded fake service did not quiesce");
        }
        fn pump(&mut self) {
            Self::pump_parts(
                &mut self.engine,
                &mut self.state,
                self.fail_up,
                &mut self.force_calls,
                &mut self.up_calls,
                self.arm_ctrl_during_second_up,
            );
        }
        async fn begin(&mut self) -> CaptureEpoch {
            let mut pending = Box::pin(self.driver.begin());
            poll_fn(|cx| {
                assert!(pending.as_mut().poll(cx).is_pending());
                Poll::Ready(())
            })
            .await;
            Self::pump_parts(
                &mut self.engine,
                &mut self.state,
                self.fail_up,
                &mut self.force_calls,
                &mut self.up_calls,
                false,
            );
            pending.await.unwrap()
        }
        async fn own_remote_ctrl(&mut self, epoch: CaptureEpoch) {
            let mut pending = Box::pin(self.driver.input(
                epoch,
                Input::Key {
                    usage: 0xe0,
                    down: true,
                },
                ReceivePoint { x: 0.5, y: 0.5 },
            ));
            poll_fn(|cx| {
                assert!(pending.as_mut().poll(cx).is_pending());
                Poll::Ready(())
            })
            .await;
            Self::pump_parts(
                &mut self.engine,
                &mut self.state,
                self.fail_up,
                &mut self.force_calls,
                &mut self.up_calls,
                false,
            );
            assert_eq!(pending.await.unwrap(), ReceiveAck::Submitted);
        }
        fn key(&mut self, usage: u16, down: bool) {
            let callback = self.state.gate.begin_event();
            assert!(
                self.state.key_routed(usage, down, callback.route),
                "protected trigger pair must stay consumed"
            );
        }
        fn side_button(&mut self, down: bool) {
            let callback = self.state.gate.begin_event();
            assert!(self.state.unsupported_button(4, down, callback.route));
        }
        async fn fail_after_active_ctrl_down() -> (Self, CaptureEpoch) {
            let mut rig = Self::new();
            let epoch = rig.begin().await;
            rig.own_remote_ctrl(epoch).await;
            assert_eq!(rig.driver.snapshot(epoch), ReceiveSnapshot::Active);
            assert!(!rig.driver.control().can_force_return(epoch));
            rig.key(0xe0, true);
            assert!(
                !rig.state.pairs.force_armed,
                "first chord began while ACTIVE"
            );
            rig.fail_up = true;
            rig.pump();
            assert_eq!(
                rig.driver.snapshot(epoch),
                ReceiveSnapshot::RecoveryRequired
            );
            assert!(rig.driver.control().can_force_return(epoch));
            assert!(
                !rig.engine.can_exit(),
                "failed SDK cleanup still protects new input"
            );
            assert_eq!(rig.force_calls, 0);
            (rig, epoch)
        }
        fn release_first_chord(&mut self) {
            self.key(0x29, false);
            self.key(0xe2, false);
            self.key(0xe0, false);
            self.pump();
            assert_eq!(self.force_calls, 0);
            assert!(!self.state.suppressed_modifiers());
        }
    }

    #[tokio::test]
    async fn active_first_chord_never_becomes_force_when_cleanup_fails_mid_chord_or_esc_repeats() {
        let (mut rig, epoch) = Rig::fail_after_active_ctrl_down().await;
        rig.key(0xe2, true);
        rig.key(0x29, true);
        rig.key(0x29, true); // repeat after FAILED does not create a fresh chord
        rig.pump();
        assert!(!rig.state.pairs.force_pending);
        assert_eq!(rig.force_calls, 0);
        rig.key(0x29, false);
        rig.key(0x29, true); // fresh Esc, but original modifiers never released
        rig.key(0x29, true);
        rig.pump();
        assert_eq!(rig.force_calls, 0);
        rig.release_first_chord();
        assert_eq!(
            rig.driver.snapshot(epoch),
            ReceiveSnapshot::RecoveryRequired
        );
        assert!(!rig.engine.can_exit());
    }

    #[tokio::test]
    async fn only_a_new_complete_failed_escape_forces_once_and_preserves_old_pair_ups() {
        let (mut rig, epoch) = Rig::fail_after_active_ctrl_down().await;
        rig.key(0xe2, true);
        rig.key(0x29, true);
        rig.release_first_chord();
        rig.side_button(true); // an unrelated already-consumed pair must survive force
        rig.key(0xe0, true);
        assert!(rig.state.pairs.force_armed);
        rig.key(0xe2, true);
        rig.key(0x29, true);
        rig.key(0x29, true);
        assert!(rig.state.pairs.force_pending);
        rig.pump();
        assert_eq!(rig.force_calls, 0, "Down/repeat alone cannot force");
        rig.key(0x29, false);
        rig.pump();
        assert_eq!(rig.force_calls, 0, "consumed Ctrl/Alt are still held");
        rig.key(0xe0, false);
        rig.pump();
        assert_eq!(rig.force_calls, 0, "consumed Alt is still held");
        rig.key(0xe2, false);
        assert!(!rig.state.pairs.force_pending);
        assert!(!rig.state.pairs.force_armed);
        rig.pump();
        assert_eq!(rig.force_calls, 1);
        rig.pump();
        assert_eq!(
            rig.force_calls, 1,
            "no duplicate force request after the completed chord"
        );
        let control = rig.state.gate.0.control.load(Ordering::Acquire);
        assert_ne!(control & ADMISSIONS_CLOSED, 0);
        assert_eq!(control & STOPPED, 0, "force is not a source-loss claim");
        assert!(rig.state.gate.0.healthy.load(Ordering::Acquire));
        assert!(rig.state.gate.receiving_epoch().is_none());
        assert!(!rig.state.gate.try_activate());
        assert!(rig.state.gate.enter_receiving().is_none());
        assert_eq!(
            rig.driver.snapshot(epoch),
            ReceiveSnapshot::RecoveryRequired
        );
        assert!(rig.engine.can_exit());
        assert!(!rig.driver.control().can_force_return(epoch));
        assert_eq!(rig.state.pairs.debt, 1);
        assert!(
            rig.state.keep_running(true),
            "old consumed side-button Up still needs the original hook"
        );
        rig.side_button(false);
        assert_eq!(rig.state.pairs.debt, 0);
        assert!(!rig.state.keep_running(true));
        assert!(rig.state.gate.0.healthy.load(Ordering::Acquire));
        assert_eq!(rig.force_calls, 1);
    }

    // Required during a DRAINING retry cannot authorize a fresh FAILED chord.
    #[tokio::test]
    async fn a_chord_started_during_sdk_cleanup_retry_is_not_a_fresh_failed_chord() {
        let mut rig = Rig::new();
        let epoch = rig.begin().await;
        rig.own_remote_ctrl(epoch).await;
        rig.fail_up = true;
        rig.arm_ctrl_during_second_up = true;
        rig.driver.control().request_recovery(epoch);
        rig.pump();
        assert_eq!(
            rig.driver.snapshot(epoch),
            ReceiveSnapshot::RecoveryRequired
        );
        assert!(
            !rig.state.pairs.force_armed,
            "Required while DRAINING is not FAILED permission to start a force chord"
        );
        rig.key(0xe2, true);
        rig.key(0x29, true);
        rig.release_first_chord();
        assert_eq!(rig.force_calls, 0);
    }
}
