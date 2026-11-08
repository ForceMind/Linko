//! Finite, epoch-bound KVM receive service for the existing Native loop.
//! No hooks, G1, wire commands, logging or OS calls are installed by this module.
//! Only Engine/Backend submit effects; callbacks use precloned Control atomics.
use crate::CaptureEpoch;
use peerglide_actions::execution::NativeFailure;
use peerglide_protocol::Input;
use std::{fmt, time::Duration};

pub const COMMAND_TIMEOUT: Duration = Duration::from_millis(250);
#[derive(Clone, Copy)]
pub struct ReceivePoint {
    pub x: f64,
    pub y: f64,
}
impl ReceivePoint {
    pub(crate) fn valid(self) -> bool {
        self.x.is_finite()
            && self.y.is_finite()
            && (0.0..=1.0).contains(&self.x)
            && (0.0..=1.0).contains(&self.y)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReceiveAck {
    /// SDK submission, or an accepted release request. Never an OS/app receipt.
    /// For release this does NOT mean Receiving protection was removed.
    Submitted,
    /// Owned cleanup requests completed, consumed modifiers are up, and the
    /// matching epoch's Receiving protection has actually been removed.
    Retired,
    RecoveryRequired,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReceiveSnapshot {
    Active,
    Draining,
    Retired,
    RecoveryRequired,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReceiveError {
    Invalid,
    Busy,
    QueueFull,
    Closed,
    Stale,
    Timeout,
    Native(NativeFailure),
}
impl fmt::Display for ReceiveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Native(failure) => write!(f, "native receive submission failed: {failure:?}"),
            other => write!(f, "native receive service: {other:?}"),
        }
    }
}
impl std::error::Error for ReceiveError {}

// The production Engine only exists where there is a Native loop. Tests use
// this same implementation with injected backends on every CI platform.
#[cfg(any(windows, target_os = "macos", test))]
mod implementation {
    use super::*;
    use crate::now_ms;
    use std::{
        cell::Cell,
        marker::PhantomData,
        sync::{
            atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering},
            mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
            Arc,
        },
    };
    use tokio::sync::oneshot;
    const COMMAND_MS: u64 = 250;
    const WATCHDOG_MS: u64 = 1000;
    const BUDGET: usize = 32;
    const RETRIES: u8 = 3;
    const IDLE: u8 = 0;
    const ACTIVE: u8 = 1;
    const DRAINING: u8 = 2;
    const FAILED: u8 = 3;
    const FORCED: u8 = 4;

    type Wake = Arc<dyn Fn() + Send + Sync>;

    struct Shared {
        phase: AtomicU8,
        instance: AtomicU64,
        generation: AtomicU64,
        admission_request: AtomicU64,
        retired_through: AtomicU64,
        recovery: AtomicU64,
        force: AtomicU64,
        cancelled_through: AtomicU64,
        stop: AtomicBool,
        required: AtomicBool,
    }
    impl Shared {
        fn new() -> Self {
            Self {
                phase: AtomicU8::new(IDLE),
                instance: AtomicU64::new(0),
                generation: AtomicU64::new(0),
                admission_request: AtomicU64::new(0),
                retired_through: AtomicU64::new(0),
                recovery: AtomicU64::new(0),
                force: AtomicU64::new(0),
                cancelled_through: AtomicU64::new(0),
                stop: AtomicBool::new(false),
                required: AtomicBool::new(false),
            }
        }
        fn scope(&self, epoch: CaptureEpoch) -> bool {
            epoch.instance != 0
                && epoch.generation != 0
                && self.instance.load(Ordering::Acquire) == epoch.instance
        }
        fn exact(&self, epoch: CaptureEpoch) -> bool {
            self.scope(epoch) && self.generation.load(Ordering::Acquire) == epoch.generation
        }
        fn snapshot(&self, epoch: CaptureEpoch) -> ReceiveSnapshot {
            if !self.scope(epoch) {
                return ReceiveSnapshot::RecoveryRequired;
            }
            if epoch.generation <= self.retired_through.load(Ordering::Acquire) {
                return ReceiveSnapshot::Retired;
            }
            if !self.exact(epoch) || self.required.load(Ordering::Acquire) {
                return ReceiveSnapshot::RecoveryRequired;
            }
            let phase = self.phase.load(Ordering::Acquire);
            match phase {
                ACTIVE
                    if !self.stop.load(Ordering::Acquire)
                        && self.recovery.load(Ordering::Acquire) < epoch.generation =>
                {
                    if self.phase.load(Ordering::Acquire) == phase && self.exact(epoch) {
                        ReceiveSnapshot::Active
                    } else {
                        ReceiveSnapshot::Draining
                    }
                }
                ACTIVE | DRAINING => ReceiveSnapshot::Draining,
                _ => ReceiveSnapshot::RecoveryRequired,
            }
        }
    }

    /// Obtain clones outside callbacks. request_* perform only atomics. wake_loop
    /// must be called outside callbacks; the callback already runs on the Native loop.
    #[derive(Clone)]
    pub struct ReceiveControl {
        shared: Arc<Shared>,
        wake: Wake,
    }
    impl ReceiveControl {
        pub fn snapshot(&self, epoch: CaptureEpoch) -> ReceiveSnapshot {
            self.shared.snapshot(epoch)
        }
        /// CaptureSession keeps a clone after the Driver is taken, so shutdown can
        /// retain a final failure even after the receiving mode/epoch is no longer
        /// present in the capture gate. No input contents are exposed.
        pub fn outcome(&self) -> ReceiveAck {
            if self.shared.required.load(Ordering::Acquire) {
                ReceiveAck::RecoveryRequired
            } else if self.shared.phase.load(Ordering::Acquire) == IDLE {
                ReceiveAck::Retired
            } else {
                ReceiveAck::Submitted
            }
        }
        pub fn request_recovery(&self, epoch: CaptureEpoch) {
            if self.shared.scope(epoch) {
                // Monotonic within this capture instance: an old callback/request
                // cannot overwrite a newer epoch's coalesced recovery notification.
                self.shared
                    .recovery
                    .fetch_max(epoch.generation, Ordering::AcqRel);
            }
        }
        pub fn request_stop(&self) {
            self.shared.stop.store(true, Ordering::Release);
        }
        pub fn stop_requested(&self) -> bool {
            self.shared.stop.load(Ordering::Acquire)
        }
        /// Only exact FAILED permits starting a fresh physical force chord.
        /// RecoveryRequired alone also describes an SDK retry still DRAINING.
        pub fn can_force_return(&self, epoch: CaptureEpoch) -> bool {
            self.shared.exact(epoch) && self.shared.phase.load(Ordering::Acquire) == FAILED
        }
        /// Native wiring must only call after failed recovery, release of consumed
        /// modifiers, and a new complete physical escape chord (never autorepeat).
        pub fn request_force_return(&self, epoch: CaptureEpoch) -> bool {
            if self.can_force_return(epoch) {
                self.shared
                    .force
                    .fetch_max(epoch.generation, Ordering::AcqRel);
                true
            } else {
                false
            }
        }
        pub fn wake_loop(&self) {
            (self.wake)();
        }
    }

    enum Kind {
        Begin,
        Pointer {
            epoch: CaptureEpoch,
            point: ReceivePoint,
        },
        Input {
            epoch: CaptureEpoch,
            event: Input,
            cursor: ReceivePoint,
        },
        Release {
            epoch: CaptureEpoch,
            cursor: Option<ReceivePoint>,
        },
        Heartbeat {
            epoch: CaptureEpoch,
            observed_at: u64,
        },
    }
    impl Kind {
        fn epoch(&self) -> Option<CaptureEpoch> {
            match self {
                Self::Begin => None,
                Self::Pointer { epoch, .. }
                | Self::Input { epoch, .. }
                | Self::Release { epoch, .. }
                | Self::Heartbeat { epoch, .. } => Some(*epoch),
            }
        }
    }
    enum Reply {
        Begun(CaptureEpoch),
        Ack(ReceiveAck),
    }
    struct Command {
        request: u64,
        expires: u64,
        kind: Kind,
        reply: Option<oneshot::Sender<Result<Reply, ReceiveError>>>,
    }
    #[derive(Clone, Copy)]
    struct Stamp {
        request: u64,
        expires: u64,
    }
    struct CancelRequest {
        control: ReceiveControl,
        request: u64,
        epoch: Option<CaptureEpoch>,
        armed: bool,
    }
    impl Drop for CancelRequest {
        fn drop(&mut self) {
            if self.armed {
                self.control
                    .shared
                    .cancelled_through
                    .fetch_max(self.request, Ordering::AcqRel);
                if let Some(epoch) = self.epoch {
                    self.control.request_recovery(epoch);
                } else {
                    // A canceled Begin does not yet know its epoch. Before
                    // publication, handle(Begin)'s post-check sees this high-water
                    // mark. After publication, target only this exact admission.
                    // The phase/readback excludes a next epoch's partial publish.
                    let shared = &self.control.shared;
                    let phase = shared.phase.load(Ordering::Acquire);
                    if phase == ACTIVE
                        && shared.admission_request.load(Ordering::Acquire) == self.request
                    {
                        let epoch = CaptureEpoch {
                            instance: shared.instance.load(Ordering::Acquire),
                            generation: shared.generation.load(Ordering::Acquire),
                        };
                        if shared.phase.load(Ordering::Acquire) == phase
                            && shared.admission_request.load(Ordering::Acquire) == self.request
                        {
                            self.control.request_recovery(epoch);
                        }
                    }
                }
                self.control.wake_loop();
            }
        }
    }

    /// One async session owner. Non-Clone and !Sync preserve a single command order.
    /// Construct/drop outside callbacks. Input contents are never logged/serialized.
    pub struct ReceiveDriver {
        sender: SyncSender<Command>,
        control: ReceiveControl,
        next_request: u64,
        _single: PhantomData<Cell<()>>,
    }
    impl ReceiveDriver {
        pub fn control(&self) -> ReceiveControl {
            self.control.clone()
        }
        pub fn snapshot(&self, epoch: CaptureEpoch) -> ReceiveSnapshot {
            self.control.snapshot(epoch)
        }
        pub fn is_current(&self, epoch: CaptureEpoch) -> bool {
            self.snapshot(epoch) == ReceiveSnapshot::Active
        }
        async fn command(&mut self, kind: Kind) -> Result<Reply, ReceiveError> {
            let request = self.next_request;
            let Some(next) = request.checked_add(1) else {
                self.control.request_stop();
                self.control.wake_loop();
                return Err(ReceiveError::Closed);
            };
            self.next_request = next;
            let epoch = kind.epoch();
            let mut cancel = CancelRequest {
                control: self.control.clone(),
                request,
                epoch,
                armed: true,
            };
            let (reply, result) = oneshot::channel();
            let command = Command {
                request,
                expires: now_ms().saturating_add(COMMAND_MS),
                kind,
                reply: Some(reply),
            };
            self.sender.try_send(command).map_err(|error| match error {
                TrySendError::Full(_) => ReceiveError::QueueFull,
                TrySendError::Disconnected(_) => ReceiveError::Closed,
            })?;
            self.control.wake_loop();
            match tokio::time::timeout(COMMAND_TIMEOUT, result).await {
                Ok(Ok(result)) => {
                    cancel.armed = false;
                    result
                }
                Ok(Err(_)) => Err(ReceiveError::Closed),
                Err(_) => Err(ReceiveError::Timeout),
            }
        }
        pub async fn begin(&mut self) -> Result<CaptureEpoch, ReceiveError> {
            match self.command(Kind::Begin).await? {
                Reply::Begun(epoch) => Ok(epoch),
                _ => Err(ReceiveError::Invalid),
            }
        }
        pub async fn pointer(
            &mut self,
            epoch: CaptureEpoch,
            point: ReceivePoint,
        ) -> Result<ReceiveAck, ReceiveError> {
            if !point.valid() {
                return Err(ReceiveError::Invalid);
            }
            self.submit(Kind::Pointer { epoch, point }).await
        }
        pub async fn input(
            &mut self,
            epoch: CaptureEpoch,
            event: Input,
            cursor: ReceivePoint,
        ) -> Result<ReceiveAck, ReceiveError> {
            if !event.valid() || !cursor.valid() {
                return Err(ReceiveError::Invalid);
            }
            self.submit(Kind::Input {
                epoch,
                event,
                cursor,
            })
            .await
        }
        pub async fn release(
            &mut self,
            epoch: CaptureEpoch,
            terminal_cursor: Option<ReceivePoint>,
        ) -> Result<ReceiveAck, ReceiveError> {
            if terminal_cursor.is_some_and(|point| !point.valid()) {
                return Err(ReceiveError::Invalid);
            }
            // Queue-full/cancel/timeout invokes CancelRequest's atomic recovery.
            // A normally queued release may still apply its terminal cursor before
            // retiring; requesting takeover first would discard that cursor.
            match self
                .submit(Kind::Release {
                    epoch,
                    cursor: terminal_cursor,
                })
                .await
            {
                // Stop may retire this epoch and close the Native inbox after
                // the caller observed it active. Closed alone is not cleanup:
                // both the epoch and the whole owner must now confirm retirement.
                Err(ReceiveError::Closed)
                    if self.snapshot(epoch) == ReceiveSnapshot::Retired
                        && self.control.outcome() == ReceiveAck::Retired =>
                {
                    Ok(ReceiveAck::Retired)
                }
                result => result,
            }
        }
        async fn submit(&mut self, kind: Kind) -> Result<ReceiveAck, ReceiveError> {
            match self.command(kind).await? {
                Reply::Ack(ack) => Ok(ack),
                _ => Err(ReceiveError::Invalid),
            }
        }
        pub fn heartbeat(&mut self, epoch: CaptureEpoch) -> bool {
            if !self.is_current(epoch) {
                return false;
            }
            let command = Command {
                request: 0,
                expires: 0,
                kind: Kind::Heartbeat {
                    epoch,
                    observed_at: now_ms(),
                },
                reply: None,
            };
            let sent = self.sender.try_send(command).is_ok();
            if !sent {
                self.control.request_recovery(epoch);
            }
            self.control.wake_loop();
            sent
        }
    }
    impl Drop for ReceiveDriver {
        fn drop(&mut self) {
            self.control.request_stop();
            self.control.wake_loop();
        }
    }

    /// Implementations must submit only this receiving scope's typed effects.
    /// All methods are called on the Native loop OUTSIDE every STATE RefCell borrow.
    /// Query methods are atomic/fixed-state reads. An Ok effect is an SDK submission
    /// request (including CGEventPost(void)), never proof of OS/application delivery.
    pub(crate) trait ReceiveBackend {
        /// Publish Receiving using this capture's unique instance and a strictly
        /// increasing generation, atomically with held/IN_CALLBACK admission.
        /// Err guarantees that no receiver protection has been published.
        fn enter_receiving(&mut self) -> Result<CaptureEpoch, NativeFailure>;
        fn is_current(&self, epoch: CaptureEpoch) -> bool;
        fn source_alive(&self) -> bool;
        /// Include every suppressed modifier domain (old source Remote as well as
        /// new receiver Consumed), not only the current Receiving pair table.
        fn consumed_modifiers_held(&self) -> bool;
        fn pointer(&mut self, point: ReceivePoint) -> Result<(), NativeFailure>;
        fn input(&mut self, event: Input) -> Result<(), NativeFailure>;
        fn leave_receiving(&mut self, epoch: CaptureEpoch) -> Result<(), NativeFailure>;
        /// Clear only protection/admissions, preserve old Consumed Up ownership,
        /// disable all further injections. No synthetic events may be emitted here.
        fn force_return(&mut self, epoch: CaptureEpoch) -> Result<(), NativeFailure>;
        fn now_ms(&self) -> u64 {
            now_ms()
        }
    }

    struct Ledger {
        keys: [u64; 4],
        buttons: u8,
        key_attempts: [u8; 256],
        button_attempts: [u8; 4],
    }
    impl Default for Ledger {
        fn default() -> Self {
            Self {
                keys: [0; 4],
                buttons: 0,
                key_attempts: [0; 256],
                button_attempts: [0; 4],
            }
        }
    }
    impl Ledger {
        fn held(&self, event: Input) -> bool {
            match event {
                Input::Key { usage, .. } => {
                    self.keys[usize::from(usage) / 64] & (1 << (usage % 64)) != 0
                }
                Input::Button { button, .. } => self.buttons & (1 << button) != 0,
                Input::Scroll { .. } => false,
            }
        }
        fn mark(&mut self, event: Input, held: bool) {
            match event {
                Input::Key { usage, .. } => {
                    let bit = 1 << (usage % 64);
                    let word = &mut self.keys[usize::from(usage) / 64];
                    if held {
                        *word |= bit;
                    } else {
                        *word &= !bit;
                        self.key_attempts[usize::from(usage)] = 0;
                    }
                }
                Input::Button { button, .. } => {
                    if held {
                        self.buttons |= 1 << button;
                    } else {
                        self.buttons &= !(1 << button);
                        self.button_attempts[usize::from(button)] = 0;
                    }
                }
                Input::Scroll { .. } => (),
            }
        }
        fn empty(&self) -> bool {
            self.keys.iter().all(|word| *word == 0) && self.buttons == 0
        }
        fn attempts(&self, event: Input) -> u8 {
            match event {
                Input::Key { usage, .. } => self.key_attempts[usize::from(usage)],
                Input::Button { button, .. } => self.button_attempts[usize::from(button)],
                Input::Scroll { .. } => 0,
            }
        }
        fn failed(&mut self, event: Input) {
            match event {
                Input::Key { usage, .. } => self.key_attempts[usize::from(usage)] += 1,
                Input::Button { button, .. } => self.button_attempts[usize::from(button)] += 1,
                Input::Scroll { .. } => (),
            }
        }
        fn next_up(&self) -> Option<Input> {
            // Release ordinary keys/buttons before modifiers. Never release-all or
            // invent an Up from the Core ledger or global/hardware key snapshots.
            (4..=0x73)
                .chain(0xe8..=0xff)
                .map(|usage| Input::Key { usage, down: false })
                .chain((1..=3).map(|button| Input::Button {
                    button,
                    down: false,
                }))
                .chain((0xe0..=0xe7).map(|usage| Input::Key { usage, down: false }))
                .find(|event| self.held(*event) && self.attempts(*event) < RETRIES)
        }
    }
    struct Active {
        epoch: CaptureEpoch,
        heartbeat_at: u64,
        ledger: Ledger,
        draining: bool,
        protected: bool,
        detached: bool,
    }
    pub(crate) struct ReceiveEngine {
        inbox: Receiver<Command>,
        control: ReceiveControl,
        active: Option<Active>,
        poisoned: bool,
    }

    /// Construct outside callbacks. Wake must be non-panicking and promptly wake
    /// the existing loop (PostThreadMessage/CFRunLoopWakeUp at native wiring time).
    pub(crate) fn channel(
        capacity: usize,
        wake: Wake,
    ) -> Result<(ReceiveDriver, ReceiveEngine), ReceiveError> {
        if !(1..=128).contains(&capacity) {
            return Err(ReceiveError::Invalid);
        }
        let (sender, inbox) = mpsc::sync_channel(capacity);
        let control = ReceiveControl {
            shared: Arc::new(Shared::new()),
            wake,
        };
        Ok((
            ReceiveDriver {
                sender,
                control: control.clone(),
                next_request: 1,
                _single: PhantomData,
            },
            ReceiveEngine {
                inbox,
                control,
                active: None,
                poisoned: false,
            },
        ))
    }
    impl ReceiveEngine {
        pub(crate) fn control(&self) -> ReceiveControl {
            self.control.clone()
        }
        pub(crate) fn can_exit(&self) -> bool {
            self.active
                .as_ref()
                .is_none_or(|active| !active.protected || active.detached)
        }
        pub(crate) fn outcome(&self) -> ReceiveAck {
            self.control.outcome()
        }
        fn fail(&mut self) {
            self.poisoned = true;
            self.control.shared.required.store(true, Ordering::Release);
        }
        fn retire(&mut self) {
            if matches!(
                self.control.shared.phase.load(Ordering::Acquire),
                FAILED | FORCED
            ) {
                return;
            }
            if let Some(active) = &mut self.active {
                active.draining = true;
                self.control.shared.phase.store(DRAINING, Ordering::Release);
            }
        }
        fn retire_epoch(&mut self, epoch: CaptureEpoch) {
            if self
                .active
                .as_ref()
                .is_some_and(|active| active.epoch == epoch)
            {
                self.retire();
            }
        }
        fn priority(&mut self, backend: &mut impl ReceiveBackend, now: u64) {
            let Some(active) = self.active.as_ref() else {
                return;
            };
            if active.detached || !active.protected {
                return;
            }
            let epoch = active.epoch;
            if !backend.source_alive() || !backend.is_current(epoch) {
                // Source loss may have allowed a fresh physical Down. Do not send
                // blind Up afterwards; preserve uncertainty and allow loop exit.
                self.fail();
                if let Some(active) = &mut self.active {
                    active.detached = true;
                }
                self.control.shared.phase.store(FAILED, Ordering::Release);
                return;
            }
            if self.control.shared.phase.load(Ordering::Acquire) == FAILED {
                if self.control.shared.force.load(Ordering::Acquire) >= epoch.generation
                    && !backend.consumed_modifiers_held()
                    && backend.force_return(epoch).is_ok()
                {
                    if let Some(active) = &mut self.active {
                        active.protected = false;
                    }
                    self.control.shared.stop.store(true, Ordering::Release);
                    self.control.shared.phase.store(FORCED, Ordering::Release);
                }
                return;
            }
            let should_retire = self.control.shared.stop.load(Ordering::Acquire)
                || self.control.shared.recovery.load(Ordering::Acquire) >= epoch.generation
                || now < active.heartbeat_at
                || now.saturating_sub(active.heartbeat_at) >= WATCHDOG_MS;
            if should_retire {
                self.retire();
            }
        }
        fn data_guard(
            &self,
            backend: &impl ReceiveBackend,
            epoch: CaptureEpoch,
            request: u64,
            expires: u64,
            now: u64,
        ) -> bool {
            now < expires
                && self
                    .control
                    .shared
                    .cancelled_through
                    .load(Ordering::Acquire)
                    < request
                && self.control.shared.snapshot(epoch) == ReceiveSnapshot::Active
                && backend.source_alive()
                && backend.is_current(epoch)
                && !backend.consumed_modifiers_held()
        }
        fn ack(&self, epoch: CaptureEpoch) -> ReceiveAck {
            match self.control.shared.snapshot(epoch) {
                ReceiveSnapshot::Retired => ReceiveAck::Retired,
                ReceiveSnapshot::RecoveryRequired => ReceiveAck::RecoveryRequired,
                _ => ReceiveAck::Submitted,
            }
        }
        fn handle(&mut self, backend: &mut impl ReceiveBackend, command: Command, now: u64) {
            let Command {
                request,
                expires,
                kind,
                reply,
            } = command;
            let result = match kind {
                Kind::Heartbeat { epoch, observed_at } => {
                    if self.control.shared.snapshot(epoch) == ReceiveSnapshot::Active {
                        if let Some(active) = &mut self.active {
                            if active.epoch == epoch
                                && observed_at >= active.heartbeat_at
                                && observed_at <= now
                            {
                                active.heartbeat_at = observed_at;
                            }
                        }
                    }
                    return;
                }
                Kind::Release { epoch, cursor } => {
                    // Ordinary cleanup must survive cancellation of its requester.
                    // A terminal cursor is only valid while still receiving; never
                    // move after a takeover/watchdog has already retired this epoch.
                    if let Some(active) = &self.active {
                        if active.epoch == epoch && !active.draining && !self.poisoned {
                            if let Some(point) = cursor {
                                if self.data_guard(backend, epoch, request, expires, now)
                                    && backend.pointer(point).is_err()
                                {
                                    self.fail();
                                }
                            }
                        }
                    }
                    self.retire_epoch(epoch);
                    Ok(Reply::Ack(self.ack(epoch)))
                }
                Kind::Begin => {
                    if self.poisoned || self.control.shared.stop.load(Ordering::Acquire) {
                        Err(ReceiveError::Closed)
                    } else if self.active.is_some() {
                        Err(ReceiveError::Busy)
                    } else if now >= expires
                        || self
                            .control
                            .shared
                            .cancelled_through
                            .load(Ordering::Acquire)
                            >= request
                        || reply.as_ref().is_none_or(|reply| reply.is_closed())
                    {
                        Err(ReceiveError::Timeout)
                    } else {
                        match backend.enter_receiving() {
                            Err(NativeFailure::BeforeSubmission) => Err(ReceiveError::Busy),
                            Err(error) => {
                                self.fail();
                                self.control.shared.phase.store(FAILED, Ordering::Release);
                                Err(ReceiveError::Native(error))
                            }
                            Ok(epoch) => {
                                let scope = self.control.shared.instance.load(Ordering::Acquire);
                                let previous =
                                    self.control.shared.generation.load(Ordering::Acquire);
                                if epoch.instance == 0
                                    || epoch.generation == 0
                                    || (scope != 0 && scope != epoch.instance)
                                    || epoch.generation <= previous
                                {
                                    let _ = backend.force_return(epoch);
                                    self.fail();
                                    self.control.shared.phase.store(FAILED, Ordering::Release);
                                    Err(ReceiveError::Invalid)
                                } else {
                                    self.control
                                        .shared
                                        .instance
                                        .store(epoch.instance, Ordering::Release);
                                    self.control
                                        .shared
                                        .generation
                                        .store(epoch.generation, Ordering::Release);
                                    self.control
                                        .shared
                                        .admission_request
                                        .store(request, Ordering::Release);
                                    self.control.shared.phase.store(ACTIVE, Ordering::Release);
                                    self.active = Some(Active {
                                        epoch,
                                        heartbeat_at: now,
                                        ledger: Ledger::default(),
                                        draining: false,
                                        protected: true,
                                        detached: false,
                                    });
                                    if self
                                        .control
                                        .shared
                                        .cancelled_through
                                        .load(Ordering::Acquire)
                                        >= request
                                        || backend.now_ms().max(now) >= expires
                                        || reply.as_ref().is_none_or(|reply| reply.is_closed())
                                        || self.control.shared.snapshot(epoch)
                                            != ReceiveSnapshot::Active
                                        || !backend.source_alive()
                                        || !backend.is_current(epoch)
                                    {
                                        self.retire();
                                        Err(ReceiveError::Timeout)
                                    } else {
                                        Ok(Reply::Begun(epoch))
                                    }
                                }
                            }
                        }
                    }
                }
                Kind::Pointer { epoch, point } => {
                    self.apply(backend, epoch, Stamp { request, expires }, now, point, None)
                }
                Kind::Input {
                    epoch,
                    event,
                    cursor,
                } => self.apply(
                    backend,
                    epoch,
                    Stamp { request, expires },
                    now,
                    cursor,
                    Some(event),
                ),
            };
            if let Some(reply) = reply {
                let _ = reply.send(result);
            }
        }
        fn apply(
            &mut self,
            backend: &mut impl ReceiveBackend,
            epoch: CaptureEpoch,
            stamp: Stamp,
            now: u64,
            cursor: ReceivePoint,
            event: Option<Input>,
        ) -> Result<Reply, ReceiveError> {
            let Stamp { request, expires } = stamp;
            if !cursor.valid() || event.is_some_and(|event| !event.valid()) {
                return Err(ReceiveError::Invalid);
            }
            if !self.data_guard(backend, epoch, request, expires, now) {
                self.retire_epoch(epoch);
                return Err(ReceiveError::Stale);
            }
            if event.is_some_and(|event| {
                matches!(
                    event,
                    Input::Key { down: false, .. } | Input::Button { down: false, .. }
                ) && self
                    .active
                    .as_ref()
                    .is_none_or(|active| !active.ledger.held(event))
            }) {
                return Err(ReceiveError::Invalid);
            }
            if let Err(error) = backend.pointer(cursor) {
                self.fail();
                self.retire();
                return Err(ReceiveError::Native(error));
            }
            if let Some(event) = event {
                if !self.data_guard(backend, epoch, request, expires, backend.now_ms().max(now)) {
                    self.retire_epoch(epoch);
                    return Err(ReceiveError::Stale);
                }
                let down = matches!(
                    event,
                    Input::Key { down: true, .. } | Input::Button { down: true, .. }
                );
                let previously_held = self
                    .active
                    .as_ref()
                    .is_some_and(|active| active.ledger.held(event));
                // The single Native owner records a possible Down before its SDK
                // attempt, covering partial failure or a lost/canceled async reply.
                if down {
                    if let Some(active) = &mut self.active {
                        active.ledger.mark(event, true);
                    }
                }
                if let Err(error) = backend.input(event) {
                    if down && !previously_held && error == NativeFailure::BeforeSubmission {
                        if let Some(active) = &mut self.active {
                            active.ledger.mark(event, false);
                        }
                    }
                    self.fail();
                    self.retire();
                    return Err(ReceiveError::Native(error));
                }
                if !down {
                    if let Some(active) = &mut self.active {
                        active.ledger.mark(event, false);
                    }
                }
            }
            Ok(Reply::Ack(ReceiveAck::Submitted))
        }
        fn cleanup(&mut self, backend: &mut impl ReceiveBackend) -> bool {
            if self.control.shared.phase.load(Ordering::Acquire) != DRAINING {
                return false;
            }
            for _ in 0..BUDGET {
                let Some(active) = &self.active else {
                    return false;
                };
                if !active.draining || !active.protected || active.detached {
                    return false;
                }
                let epoch = active.epoch;
                if !backend.source_alive() || !backend.is_current(epoch) {
                    let clock = backend.now_ms();
                    self.priority(backend, clock);
                    return false;
                }
                let Some(event) = active.ledger.next_up() else {
                    break;
                };
                let result = backend.input(event);
                if result.is_err() {
                    self.fail();
                }
                if let Some(active) = &mut self.active {
                    if result.is_ok() {
                        active.ledger.mark(event, false);
                    } else {
                        active.ledger.failed(event);
                    }
                }
            }
            let Some(active) = &self.active else {
                return false;
            };
            if !active.ledger.empty() {
                if active.ledger.next_up().is_none() {
                    self.fail();
                    self.control.shared.phase.store(FAILED, Ordering::Release);
                    return false;
                }
                return true;
            }
            if backend.consumed_modifiers_held() {
                return false;
            }
            let epoch = active.epoch;
            if backend.leave_receiving(epoch).is_err() {
                self.fail();
                self.control.shared.phase.store(FAILED, Ordering::Release);
                return false;
            }
            if self.poisoned {
                if let Some(active) = &mut self.active {
                    active.protected = false;
                }
                self.control.shared.phase.store(FAILED, Ordering::Release);
            } else {
                self.control
                    .shared
                    .retired_through
                    .store(epoch.generation, Ordering::Release);
                self.control.shared.phase.store(IDLE, Ordering::Release);
                self.active = None;
            }
            false
        }
        /// Returns true for immediately remaining bounded work. Native wiring must
        /// service again promptly and interleave its normal OS message/runloop pump.
        pub(crate) fn pump(
            &mut self,
            backend: &mut impl ReceiveBackend,
            now: u64,
            stop_requested: bool,
        ) -> bool {
            if stop_requested {
                self.control.request_stop();
            }
            self.priority(backend, now);
            let mut handled = 0;
            while handled < BUDGET {
                let clock = backend.now_ms().max(now);
                self.priority(backend, clock);
                match self.inbox.try_recv() {
                    Ok(command) => {
                        let clock = backend.now_ms().max(now);
                        self.handle(backend, command, clock);
                        handled += 1;
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        self.control.request_stop();
                        self.priority(backend, now);
                        break;
                    }
                }
            }
            let clock = backend.now_ms().max(now);
            self.priority(backend, clock);
            self.cleanup(backend) || handled == BUDGET
        }
    }
    impl Drop for ReceiveEngine {
        fn drop(&mut self) {
            if !self.can_exit() {
                self.control.shared.required.store(true, Ordering::Release);
                self.control.shared.phase.store(FAILED, Ordering::Release);
            }
            self.control.request_stop();
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::sync::atomic::AtomicUsize;

        // Fake identifiers and effects exercise only software invariants. They are
        // not native input, focus, permissions, physical-origin or authentication.
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        enum Effect {
            Enter,
            Pointer,
            Key(u16, bool),
            Button(u8, bool),
            Scroll,
            Leave,
            Force,
        }
        struct Fake {
            generation: u64,
            epoch: Option<CaptureEpoch>,
            enter_error: Option<NativeFailure>,
            alive: bool,
            observation: Option<crate::SourceObservation>,
            modifiers: bool,
            clock: u64,
            advance_pointer: u64,
            advance_input: u64,
            recover_in_pointer: Option<ReceiveControl>,
            fail_down: Option<NativeFailure>,
            fail_pointer: bool,
            up_failures_remaining: usize,
            fail_up: bool,
            fail_key_up: Option<u16>,
            effects: Vec<Effect>,
        }
        impl Fake {
            fn new() -> Self {
                Self {
                    generation: 0,
                    epoch: None,
                    enter_error: None,
                    alive: true,
                    observation: None,
                    modifiers: false,
                    clock: 0,
                    advance_pointer: 0,
                    advance_input: 0,
                    recover_in_pointer: None,
                    fail_down: None,
                    fail_pointer: false,
                    up_failures_remaining: 0,
                    fail_up: false,
                    fail_key_up: None,
                    effects: Vec::new(),
                }
            }
        }
        impl ReceiveBackend for Fake {
            fn enter_receiving(&mut self) -> Result<CaptureEpoch, NativeFailure> {
                if let Some(error) = self.enter_error {
                    return Err(error);
                }
                if self.epoch.is_some() {
                    return Err(NativeFailure::BeforeSubmission);
                }
                self.generation += 1;
                let epoch = CaptureEpoch {
                    instance: 7,
                    generation: self.generation,
                };
                self.epoch = Some(epoch);
                self.effects.push(Effect::Enter);
                Ok(epoch)
            }
            fn is_current(&self, epoch: CaptureEpoch) -> bool {
                self.epoch == Some(epoch)
            }
            fn source_alive(&self) -> bool {
                self.alive
                    && self.observation.is_none_or(|observation| {
                        observation.fresh_at(
                            self.clock,
                            std::time::SystemTime::UNIX_EPOCH + Duration::from_millis(self.clock),
                        )
                    })
            }
            fn consumed_modifiers_held(&self) -> bool {
                self.modifiers
            }
            fn pointer(&mut self, _point: ReceivePoint) -> Result<(), NativeFailure> {
                self.effects.push(Effect::Pointer);
                if self.fail_pointer {
                    return Err(NativeFailure::BeforeSubmission);
                }
                self.clock = self.clock.saturating_add(self.advance_pointer);
                if let (Some(control), Some(epoch)) = (&self.recover_in_pointer, self.epoch) {
                    control.request_recovery(epoch);
                }
                Ok(())
            }
            fn input(&mut self, event: Input) -> Result<(), NativeFailure> {
                self.clock = self.clock.saturating_add(self.advance_input);
                match event {
                    Input::Key { usage, down } => {
                        self.effects.push(Effect::Key(usage, down));
                        if down {
                            if let Some(error) = self.fail_down {
                                return Err(error);
                            }
                        } else if self.fail_up
                            || self.fail_key_up == Some(usage)
                            || self.up_failures_remaining != 0
                        {
                            self.up_failures_remaining =
                                self.up_failures_remaining.saturating_sub(1);
                            return Err(NativeFailure::PermissionLost);
                        }
                    }
                    Input::Button { button, down } => {
                        self.effects.push(Effect::Button(button, down));
                        if down {
                            if let Some(error) = self.fail_down {
                                return Err(error);
                            }
                        } else if self.fail_up || self.up_failures_remaining != 0 {
                            self.up_failures_remaining =
                                self.up_failures_remaining.saturating_sub(1);
                            return Err(NativeFailure::PermissionLost);
                        }
                    }
                    Input::Scroll { .. } => self.effects.push(Effect::Scroll),
                }
                Ok(())
            }
            fn leave_receiving(&mut self, epoch: CaptureEpoch) -> Result<(), NativeFailure> {
                assert_eq!(self.epoch, Some(epoch));
                assert!(!self.modifiers);
                self.effects.push(Effect::Leave);
                self.epoch = None;
                Ok(())
            }
            fn force_return(&mut self, epoch: CaptureEpoch) -> Result<(), NativeFailure> {
                assert_eq!(self.epoch, Some(epoch));
                assert!(!self.modifiers);
                self.effects.push(Effect::Force);
                self.epoch = None;
                Ok(())
            }
            fn now_ms(&self) -> u64 {
                self.clock
            }
        }
        fn setup(capacity: usize) -> (ReceiveDriver, ReceiveEngine, Fake, Arc<AtomicUsize>) {
            let wakes = Arc::new(AtomicUsize::new(0));
            let count = wakes.clone();
            let (driver, engine) = channel(
                capacity,
                Arc::new(move || {
                    count.fetch_add(1, Ordering::Relaxed);
                }),
            )
            .unwrap();
            (driver, engine, Fake::new(), wakes)
        }
        fn point() -> ReceivePoint {
            ReceivePoint { x: 0.5, y: 0.5 }
        }
        fn enqueue(
            driver: &mut ReceiveDriver,
            kind: Kind,
            now: u64,
        ) -> (u64, oneshot::Receiver<Result<Reply, ReceiveError>>) {
            let request = driver.next_request;
            driver.next_request += 1;
            let (reply, result) = oneshot::channel();
            assert!(driver
                .sender
                .try_send(Command {
                    request,
                    expires: now + COMMAND_MS,
                    kind,
                    reply: Some(reply)
                })
                .is_ok());
            (request, result)
        }
        fn pump(engine: &mut ReceiveEngine, backend: &mut Fake, now: u64) {
            backend.clock = now;
            for _ in 0..32 {
                if !engine.pump(backend, now, false) {
                    return;
                }
            }
            panic!("finite fake work failed to quiesce");
        }
        fn begin(
            driver: &mut ReceiveDriver,
            engine: &mut ReceiveEngine,
            backend: &mut Fake,
            now: u64,
        ) -> CaptureEpoch {
            let (_, mut reply) = enqueue(driver, Kind::Begin, now);
            pump(engine, backend, now);
            match reply.try_recv().unwrap().unwrap() {
                Reply::Begun(epoch) => epoch,
                _ => panic!("no epoch"),
            }
        }
        fn input(
            driver: &mut ReceiveDriver,
            engine: &mut ReceiveEngine,
            backend: &mut Fake,
            epoch: CaptureEpoch,
            event: Input,
            now: u64,
        ) -> Result<ReceiveAck, ReceiveError> {
            let (_, mut reply) = enqueue(
                driver,
                Kind::Input {
                    epoch,
                    event,
                    cursor: point(),
                },
                now,
            );
            pump(engine, backend, now);
            match reply.try_recv().unwrap()? {
                Reply::Ack(ack) => Ok(ack),
                _ => panic!("no submission reply"),
            }
        }
        fn release(
            driver: &mut ReceiveDriver,
            engine: &mut ReceiveEngine,
            backend: &mut Fake,
            epoch: CaptureEpoch,
            now: u64,
        ) -> ReceiveAck {
            let (_, mut reply) = enqueue(
                driver,
                Kind::Release {
                    epoch,
                    cursor: None,
                },
                now,
            );
            pump(engine, backend, now);
            match reply.try_recv().unwrap().unwrap() {
                Reply::Ack(ack) => ack,
                _ => panic!("no cleanup reply"),
            }
        }

        #[tokio::test]
        async fn closed_release_is_idempotent_only_after_the_observed_owner_retires() {
            let (mut driver, mut engine, mut backend, _) = setup(8);
            let epoch = begin(&mut driver, &mut engine, &mut backend, 0);
            assert_eq!(
                input(
                    &mut driver,
                    &mut engine,
                    &mut backend,
                    epoch,
                    Input::Key {
                        usage: 4,
                        down: true,
                    },
                    1,
                ),
                Ok(ReceiveAck::Submitted)
            );
            // The caller's Active observation precedes the Native stop/exit.
            assert_eq!(driver.snapshot(epoch), ReceiveSnapshot::Active);
            driver.control().request_stop();
            pump(&mut engine, &mut backend, 2);
            assert_eq!(driver.snapshot(epoch), ReceiveSnapshot::Retired);
            assert_eq!(driver.control().outcome(), ReceiveAck::Retired);
            assert!(engine.can_exit());
            assert!(backend.effects.contains(&Effect::Key(4, false)));
            assert!(backend.effects.contains(&Effect::Leave));
            let effects = backend.effects.clone();
            drop(engine);
            assert_eq!(
                driver.release(epoch, Some(point())).await,
                Ok(ReceiveAck::Retired)
            );
            assert_eq!(
                backend.effects, effects,
                "closed cleanup must not replay effects"
            );
            assert_eq!(
                driver
                    .release(
                        epoch,
                        Some(ReceivePoint {
                            x: f64::NAN,
                            y: 0.5,
                        }),
                    )
                    .await,
                Err(ReceiveError::Invalid)
            );
        }

        #[tokio::test]
        async fn closed_release_never_confirms_draining_failed_or_unknown_owners() {
            for scenario in [
                "draining",
                "failed",
                "source_lost",
                "unknown",
                "new_owner",
            ] {
                let (mut driver, mut engine, mut backend, _) = setup(8);
                let epoch = begin(&mut driver, &mut engine, &mut backend, 0);
                let mut released = epoch;
                match scenario {
                    "draining" => {
                        backend.modifiers = true;
                        driver.control().request_stop();
                        pump(&mut engine, &mut backend, 1);
                        assert_eq!(driver.snapshot(epoch), ReceiveSnapshot::Draining);
                    }
                    "failed" => {
                        engine.fail();
                        assert_eq!(driver.control().outcome(), ReceiveAck::RecoveryRequired);
                    }
                    "source_lost" => {
                        backend.alive = false;
                        pump(&mut engine, &mut backend, 1);
                        assert_eq!(driver.snapshot(epoch), ReceiveSnapshot::RecoveryRequired);
                    }
                    "unknown" => {
                        assert_eq!(
                            release(&mut driver, &mut engine, &mut backend, epoch, 1),
                            ReceiveAck::Retired
                        );
                        released.instance += 1;
                        assert_eq!(driver.snapshot(released), ReceiveSnapshot::RecoveryRequired);
                        assert_eq!(driver.control().outcome(), ReceiveAck::Retired);
                    }
                    "new_owner" => {
                        assert_eq!(
                            release(&mut driver, &mut engine, &mut backend, epoch, 1),
                            ReceiveAck::Retired
                        );
                        let current = begin(&mut driver, &mut engine, &mut backend, 2);
                        assert_eq!(driver.snapshot(current), ReceiveSnapshot::Active);
                        assert_eq!(driver.snapshot(epoch), ReceiveSnapshot::Retired);
                        assert_eq!(driver.control().outcome(), ReceiveAck::Submitted);
                    }
                    _ => unreachable!(),
                }
                drop(engine);
                let effects = backend.effects.clone();
                assert_eq!(driver.release(released, None).await, Err(ReceiveError::Closed));
                assert_eq!(backend.effects, effects);
            }
        }

        #[tokio::test]
        async fn retired_owner_does_not_hide_nonclosed_release_errors() {
            use std::{
                future::{poll_fn, Future},
                task::Poll,
            };
            for error in [
                ReceiveError::Timeout,
                ReceiveError::Stale,
                ReceiveError::QueueFull,
            ] {
                let (mut driver, mut engine, mut backend, _) = setup(8);
                let epoch = begin(&mut driver, &mut engine, &mut backend, 0);
                let control = driver.control();
                let mut pending = Box::pin(driver.release(epoch, None));
                poll_fn(|cx| {
                    assert!(matches!(pending.as_mut().poll(cx), Poll::Pending));
                    Poll::Ready(())
                })
                .await;
                let command = engine.inbox.try_recv().unwrap();
                control.request_stop();
                pump(&mut engine, &mut backend, 1);
                assert_eq!(control.snapshot(epoch), ReceiveSnapshot::Retired);
                assert_eq!(control.outcome(), ReceiveAck::Retired);
                assert!(command.reply.unwrap().send(Err(error)).is_ok());
                assert_eq!(pending.await, Err(error));
            }
        }

        #[test]
        fn takeover_cleans_only_owned_inputs_and_waits_for_suppressed_modifiers_before_local() {
            let (mut driver, mut engine, mut backend, _) = setup(8);
            let epoch = begin(&mut driver, &mut engine, &mut backend, 0);
            assert_eq!(
                input(
                    &mut driver,
                    &mut engine,
                    &mut backend,
                    epoch,
                    Input::Key {
                        usage: 0xe0,
                        down: true
                    },
                    1
                ),
                Ok(ReceiveAck::Submitted)
            );
            backend.modifiers = true;
            engine.control().request_recovery(epoch);
            pump(&mut engine, &mut backend, 2);
            assert_eq!(driver.snapshot(epoch), ReceiveSnapshot::Draining);
            assert!(!engine.can_exit());
            assert!(backend.effects.contains(&Effect::Key(0xe0, false)));
            assert!(
                !backend.effects.contains(&Effect::Key(6, true)),
                "the takeover C must not become an injected action"
            );
            assert!(!backend.effects.contains(&Effect::Leave));
            assert_eq!(
                release(&mut driver, &mut engine, &mut backend, epoch, 3),
                ReceiveAck::Submitted
            );
            backend.modifiers = false;
            pump(&mut engine, &mut backend, 4);
            assert_eq!(driver.snapshot(epoch), ReceiveSnapshot::Retired);
            assert!(engine.can_exit());
            let before = backend.effects.len();
            assert_eq!(
                release(&mut driver, &mut engine, &mut backend, epoch, 5),
                ReceiveAck::Retired
            );
            assert_eq!(
                backend.effects.len(),
                before,
                "idempotent cleanup must not replay Up"
            );
        }

        #[test]
        fn watchdog_stop_and_queued_old_heartbeat_cannot_resurrect_receiving() {
            let (mut driver, mut engine, mut backend, _) = setup(8);
            let epoch = begin(&mut driver, &mut engine, &mut backend, 0);
            input(
                &mut driver,
                &mut engine,
                &mut backend,
                epoch,
                Input::Button {
                    button: 1,
                    down: true,
                },
                1,
            )
            .unwrap();
            assert!(driver
                .sender
                .try_send(Command {
                    request: 0,
                    expires: 0,
                    kind: Kind::Heartbeat {
                        epoch,
                        observed_at: 999
                    },
                    reply: None
                })
                .is_ok());
            pump(&mut engine, &mut backend, 1000);
            assert_eq!(driver.snapshot(epoch), ReceiveSnapshot::Retired);
            assert!(backend.effects.contains(&Effect::Button(1, false)));
            let next = begin(&mut driver, &mut engine, &mut backend, 1001);
            input(
                &mut driver,
                &mut engine,
                &mut backend,
                next,
                Input::Key {
                    usage: 4,
                    down: true,
                },
                1002,
            )
            .unwrap();
            driver.control().request_stop();
            pump(&mut engine, &mut backend, 1003);
            assert_eq!(driver.snapshot(next), ReceiveSnapshot::Retired);
            assert!(backend.effects.contains(&Effect::Key(4, false)));
            assert!(engine.can_exit());
        }

        #[test]
        fn old_data_cleanup_and_cancellation_do_not_revoke_or_mutate_a_new_epoch() {
            let (mut driver, mut engine, mut backend, _) = setup(8);
            let old = begin(&mut driver, &mut engine, &mut backend, 0);
            release(&mut driver, &mut engine, &mut backend, old, 1);
            let current = begin(&mut driver, &mut engine, &mut backend, 2);
            let before = backend.effects.len();
            assert_eq!(
                input(
                    &mut driver,
                    &mut engine,
                    &mut backend,
                    old,
                    Input::Key {
                        usage: 4,
                        down: true
                    },
                    3
                ),
                Err(ReceiveError::Stale)
            );
            assert!(driver.is_current(current));
            assert_eq!(backend.effects.len(), before);
            let request = driver.next_request;
            driver.next_request += 1;
            drop(CancelRequest {
                control: driver.control(),
                request,
                epoch: Some(old),
                armed: true,
            });
            pump(&mut engine, &mut backend, 4);
            assert!(driver.is_current(current));
            assert_eq!(
                release(&mut driver, &mut engine, &mut backend, old, 5),
                ReceiveAck::Retired
            );
            assert!(driver.is_current(current));
        }

        #[test]
        fn cancellation_before_or_after_begin_publication_never_leaves_an_orphan_admission() {
            let (mut driver, mut engine, mut backend, _) = setup(8);
            let (request, mut reply) = enqueue(&mut driver, Kind::Begin, 0);
            drop(CancelRequest {
                control: driver.control(),
                request,
                epoch: None,
                armed: true,
            });
            pump(&mut engine, &mut backend, 1);
            assert!(matches!(
                reply.try_recv().unwrap(),
                Err(ReceiveError::Timeout)
            ));
            assert!(backend.effects.is_empty());
            let admission = driver.next_request;
            let epoch = begin(&mut driver, &mut engine, &mut backend, 2);
            drop(CancelRequest {
                control: driver.control(),
                request: admission,
                epoch: None,
                armed: true,
            });
            assert_eq!(driver.snapshot(epoch), ReceiveSnapshot::Draining);
            pump(&mut engine, &mut backend, 3);
            assert_eq!(driver.snapshot(epoch), ReceiveSnapshot::Retired);
        }

        #[test]
        fn takeover_or_deadline_between_cursor_and_down_prevents_the_late_down() {
            for deadline in [false, true] {
                let (mut driver, mut engine, mut backend, _) = setup(8);
                let epoch = begin(&mut driver, &mut engine, &mut backend, 0);
                if deadline {
                    backend.advance_pointer = 300;
                } else {
                    backend.recover_in_pointer = Some(engine.control());
                }
                assert_eq!(
                    input(
                        &mut driver,
                        &mut engine,
                        &mut backend,
                        epoch,
                        Input::Key {
                            usage: 4,
                            down: true
                        },
                        1
                    ),
                    Err(ReceiveError::Stale)
                );
                assert!(!backend.effects.contains(&Effect::Key(4, true)));
                assert_eq!(driver.snapshot(epoch), ReceiveSnapshot::Retired);
            }
        }

        #[test]
        fn any_suppressed_modifier_domain_blocks_new_combinations_before_leaving_receiving() {
            let (mut driver, mut engine, mut backend, _) = setup(8);
            let epoch = begin(&mut driver, &mut engine, &mut backend, 0);
            backend.modifiers = true;
            assert_eq!(
                input(
                    &mut driver,
                    &mut engine,
                    &mut backend,
                    epoch,
                    Input::Key {
                        usage: 6,
                        down: true
                    },
                    1
                ),
                Err(ReceiveError::Stale)
            );
            assert!(!backend.effects.contains(&Effect::Pointer));
            assert!(!backend.effects.contains(&Effect::Key(6, true)));
            assert_eq!(driver.snapshot(epoch), ReceiveSnapshot::Draining);
            backend.modifiers = false;
            pump(&mut engine, &mut backend, 2);
            assert_eq!(driver.snapshot(epoch), ReceiveSnapshot::Retired);
        }

        #[test]
        fn normal_terminal_cursor_is_ordered_before_owned_up_but_not_replayed_after_takeover() {
            let (mut driver, mut engine, mut backend, _) = setup(8);
            let epoch = begin(&mut driver, &mut engine, &mut backend, 0);
            input(
                &mut driver,
                &mut engine,
                &mut backend,
                epoch,
                Input::Button {
                    button: 1,
                    down: true,
                },
                1,
            )
            .unwrap();
            let before = backend.effects.len();
            let (_, mut reply) = enqueue(
                &mut driver,
                Kind::Release {
                    epoch,
                    cursor: Some(point()),
                },
                2,
            );
            pump(&mut engine, &mut backend, 2);
            assert!(matches!(reply.try_recv().unwrap(), Ok(Reply::Ack(_))));
            assert_eq!(
                &backend.effects[before..],
                [Effect::Pointer, Effect::Button(1, false), Effect::Leave]
            );
            let before = backend.effects.len();
            let (_, mut reply) = enqueue(
                &mut driver,
                Kind::Release {
                    epoch,
                    cursor: Some(point()),
                },
                3,
            );
            pump(&mut engine, &mut backend, 3);
            assert!(matches!(
                reply.try_recv().unwrap(),
                Ok(Reply::Ack(ReceiveAck::Retired))
            ));
            assert_eq!(backend.effects.len(), before);
        }

        #[test]
        fn unknown_down_and_failed_ups_remain_uncertain_force_only_clears_protection_without_injection(
        ) {
            let (mut driver, mut engine, mut backend, _) = setup(8);
            let epoch = begin(&mut driver, &mut engine, &mut backend, 0);
            backend.fail_down = Some(NativeFailure::UnknownOutcome);
            backend.fail_up = true;
            assert_eq!(
                input(
                    &mut driver,
                    &mut engine,
                    &mut backend,
                    epoch,
                    Input::Key {
                        usage: 0xe0,
                        down: true
                    },
                    1
                ),
                Err(ReceiveError::Native(NativeFailure::UnknownOutcome))
            );
            assert_eq!(
                backend
                    .effects
                    .iter()
                    .filter(|effect| **effect == Effect::Key(0xe0, false))
                    .count(),
                3
            );
            assert_eq!(driver.snapshot(epoch), ReceiveSnapshot::RecoveryRequired);
            assert!(!engine.can_exit());
            backend.modifiers = true;
            assert!(driver.control().request_force_return(epoch));
            pump(&mut engine, &mut backend, 2);
            assert!(!backend.effects.contains(&Effect::Force));
            backend.modifiers = false;
            pump(&mut engine, &mut backend, 3);
            assert!(backend.effects.contains(&Effect::Force));
            assert!(engine.can_exit());
            assert_eq!(driver.snapshot(epoch), ReceiveSnapshot::RecoveryRequired);
            assert_eq!(engine.outcome(), ReceiveAck::RecoveryRequired);
            let before = backend.effects.len();
            assert_eq!(
                release(&mut driver, &mut engine, &mut backend, epoch, 4),
                ReceiveAck::RecoveryRequired
            );
            assert_eq!(
                input(
                    &mut driver,
                    &mut engine,
                    &mut backend,
                    epoch,
                    Input::Key {
                        usage: 5,
                        down: true
                    },
                    5
                ),
                Err(ReceiveError::Stale)
            );
            assert_eq!(
                backend.effects.len(),
                before,
                "force-return must never inject further events"
            );
        }

        #[test]
        fn one_failed_release_does_not_prevent_other_owned_keys_buttons_or_modifiers_from_cleanup()
        {
            let (mut driver, mut engine, mut backend, _) = setup(16);
            let epoch = begin(&mut driver, &mut engine, &mut backend, 0);
            for event in [
                Input::Key {
                    usage: 4,
                    down: true,
                },
                Input::Key {
                    usage: 0xe0,
                    down: true,
                },
                Input::Button {
                    button: 1,
                    down: true,
                },
            ] {
                input(&mut driver, &mut engine, &mut backend, epoch, event, 1).unwrap();
            }
            backend.fail_key_up = Some(4);
            engine.control().request_recovery(epoch);
            pump(&mut engine, &mut backend, 2);
            assert_eq!(
                backend
                    .effects
                    .iter()
                    .filter(|effect| **effect == Effect::Key(4, false))
                    .count(),
                3
            );
            assert!(backend.effects.contains(&Effect::Key(0xe0, false)));
            assert!(backend.effects.contains(&Effect::Button(1, false)));
            assert_eq!(driver.snapshot(epoch), ReceiveSnapshot::RecoveryRequired);
            assert!(!engine.can_exit());
        }

        #[test]
        fn known_before_submission_failure_does_not_invent_an_owned_up_and_source_loss_never_blindly_cleans(
        ) {
            let (mut driver, mut engine, mut backend, _) = setup(8);
            let epoch = begin(&mut driver, &mut engine, &mut backend, 0);
            backend.fail_down = Some(NativeFailure::BeforeSubmission);
            assert!(input(
                &mut driver,
                &mut engine,
                &mut backend,
                epoch,
                Input::Key {
                    usage: 4,
                    down: true
                },
                1
            )
            .is_err());
            assert!(!backend.effects.contains(&Effect::Key(4, false)));
            let (mut driver, mut engine, mut backend, _) = setup(8);
            let epoch = begin(&mut driver, &mut engine, &mut backend, 0);
            input(
                &mut driver,
                &mut engine,
                &mut backend,
                epoch,
                Input::Key {
                    usage: 4,
                    down: true,
                },
                1,
            )
            .unwrap();
            backend.alive = false;
            pump(&mut engine, &mut backend, 2);
            assert!(!backend.effects.contains(&Effect::Key(4, false)));
            assert_eq!(driver.snapshot(epoch), ReceiveSnapshot::RecoveryRequired);
            assert!(
                engine.can_exit(),
                "a lost source cannot wait forever for unobservable Up"
            );
        }

        #[tokio::test]
        async fn queue_full_recovers_atomically_and_every_async_request_wakes_the_loop() {
            let (mut driver, mut engine, mut backend, wakes) = setup(1);
            let epoch = begin(&mut driver, &mut engine, &mut backend, 0);
            input(
                &mut driver,
                &mut engine,
                &mut backend,
                epoch,
                Input::Key {
                    usage: 0xe0,
                    down: true,
                },
                1,
            )
            .unwrap();
            assert!(driver.heartbeat(epoch));
            assert!(wakes.load(Ordering::Relaxed) > 0);
            assert_eq!(
                driver
                    .input(
                        epoch,
                        Input::Key {
                            usage: 6,
                            down: true
                        },
                        point()
                    )
                    .await,
                Err(ReceiveError::QueueFull)
            );
            assert_eq!(driver.snapshot(epoch), ReceiveSnapshot::Draining);
            pump(&mut engine, &mut backend, 2);
            assert!(backend.effects.contains(&Effect::Key(0xe0, false)));
            assert!(!backend.effects.contains(&Effect::Key(6, true)));
        }

        #[tokio::test]
        async fn bounded_timeout_cancels_late_begin_and_down_even_without_any_native_pump_progress()
        {
            let (mut driver, mut engine, mut backend, _) = setup(8);
            let result = tokio::time::timeout(Duration::from_secs(2), driver.begin())
                .await
                .unwrap();
            assert_eq!(result, Err(ReceiveError::Timeout));
            let clock = now_ms();
            pump(&mut engine, &mut backend, clock);
            assert!(backend.effects.is_empty());
            let epoch = begin(&mut driver, &mut engine, &mut backend, clock);
            let result = tokio::time::timeout(
                Duration::from_secs(2),
                driver.input(
                    epoch,
                    Input::Key {
                        usage: 4,
                        down: true,
                    },
                    point(),
                ),
            )
            .await
            .unwrap();
            assert_eq!(result, Err(ReceiveError::Timeout));
            assert_eq!(driver.snapshot(epoch), ReceiveSnapshot::Draining);
            let clock = now_ms();
            pump(&mut engine, &mut backend, clock);
            assert!(!backend.effects.contains(&Effect::Key(4, true)));
            assert_eq!(driver.snapshot(epoch), ReceiveSnapshot::Retired);
        }

        #[tokio::test]
        async fn dropping_an_actually_polled_begin_future_cancels_its_queued_admission() {
            use std::{
                future::{poll_fn, Future},
                task::Poll,
            };
            let (mut driver, mut engine, mut backend, _) = setup(8);
            let mut pending = Box::pin(driver.begin());
            poll_fn(|cx| {
                assert!(matches!(pending.as_mut().poll(cx), Poll::Pending));
                Poll::Ready(())
            })
            .await;
            drop(pending);
            let clock = now_ms();
            pump(&mut engine, &mut backend, clock);
            assert!(backend.effects.is_empty());
            assert!(engine.can_exit());
        }

        #[test]
        fn a_slow_pointer_cannot_hide_a_source_gap_before_owned_modifier_cleanup() {
            let (mut driver, mut engine, mut backend, _) = setup(8);
            backend.observation = Some(crate::SourceObservation::at(
                0,
                std::time::SystemTime::UNIX_EPOCH,
            ));
            let epoch = begin(&mut driver, &mut engine, &mut backend, 0);
            input(
                &mut driver,
                &mut engine,
                &mut backend,
                epoch,
                Input::Key {
                    usage: 0xe0,
                    down: true,
                },
                1,
            )
            .unwrap();
            backend.advance_pointer = 1501;
            let (_, mut reply) = enqueue(
                &mut driver,
                Kind::Pointer {
                    epoch,
                    point: point(),
                },
                2,
            );
            pump(&mut engine, &mut backend, 2);
            // The in-flight SDK request may already have completed; this check
            // promises only to reject subsequent effects, never to interrupt it.
            assert!(matches!(
                reply.try_recv().unwrap(),
                Ok(Reply::Ack(ReceiveAck::Submitted))
            ));
            assert_eq!(driver.snapshot(epoch), ReceiveSnapshot::RecoveryRequired);
            assert!(
                engine.can_exit(),
                "source loss detaches instead of waiting for an unobservable physical Up"
            );
            assert!(engine.active.as_ref().unwrap().ledger.held(Input::Key {
                usage: 0xe0,
                down: true
            }));
            assert!(
                !backend.effects.contains(&Effect::Key(0xe0, false)),
                "no blind owned Up after the 1.5s gap"
            );
        }
        #[test]
        fn cleanup_checks_the_same_observation_after_each_sdk_return_and_stops_before_modifier_up()
        {
            let (mut driver, mut engine, mut backend, _) = setup(8);
            backend.observation = Some(crate::SourceObservation::at(
                0,
                std::time::SystemTime::UNIX_EPOCH,
            ));
            let epoch = begin(&mut driver, &mut engine, &mut backend, 0);
            input(
                &mut driver,
                &mut engine,
                &mut backend,
                epoch,
                Input::Key {
                    usage: 0xe0,
                    down: true,
                },
                1,
            )
            .unwrap();
            input(
                &mut driver,
                &mut engine,
                &mut backend,
                epoch,
                Input::Key {
                    usage: 4,
                    down: true,
                },
                2,
            )
            .unwrap();
            backend.advance_input = 1501;
            driver.control().request_recovery(epoch);
            pump(&mut engine, &mut backend, 3);
            assert!(
                backend.effects.contains(&Effect::Key(4, false)),
                "the first cleanup SDK call was already in flight"
            );
            assert!(
                !backend.effects.contains(&Effect::Key(0xe0, false)),
                "the next owned modifier Up must not be submitted"
            );
            assert_eq!(driver.snapshot(epoch), ReceiveSnapshot::RecoveryRequired);
            assert!(engine.can_exit());
            assert!(engine.active.as_ref().unwrap().ledger.held(Input::Key {
                usage: 0xe0,
                down: true
            }));
        }
        #[test]
        fn begin_refusal_is_busy_only_before_publication_and_permission_failure_stays_required() {
            let (mut driver, mut engine, mut backend, _) = setup(8);
            backend.enter_error = Some(NativeFailure::BeforeSubmission);
            let (_, mut reply) = enqueue(&mut driver, Kind::Begin, 0);
            pump(&mut engine, &mut backend, 0);
            assert!(matches!(reply.try_recv().unwrap(), Err(ReceiveError::Busy)));
            assert!(backend.epoch.is_none());
            assert!(backend.effects.is_empty());
            assert_eq!(driver.control().outcome(), ReceiveAck::Retired);
            backend.enter_error = None;
            let epoch = begin(&mut driver, &mut engine, &mut backend, 1);
            assert!(
                driver.is_current(epoch),
                "ordinary physical-held refusal can be retried"
            );

            let (mut driver, mut engine, mut backend, _) = setup(8);
            backend.enter_error = Some(NativeFailure::PermissionLost);
            let (_, mut reply) = enqueue(&mut driver, Kind::Begin, 0);
            pump(&mut engine, &mut backend, 0);
            assert!(matches!(
                reply.try_recv().unwrap(),
                Err(ReceiveError::Native(NativeFailure::PermissionLost))
            ));
            assert!(backend.epoch.is_none());
            assert!(backend.effects.is_empty());
            assert_eq!(driver.control().outcome(), ReceiveAck::RecoveryRequired);
        }
        fn pair(down: bool, button: bool) -> Input {
            if button {
                Input::Button { button: 1, down }
            } else {
                Input::Key { usage: 4, down }
            }
        }
        fn up_count(backend: &Fake, button: bool) -> usize {
            let expected = if button {
                Effect::Button(1, false)
            } else {
                Effect::Key(4, false)
            };
            backend
                .effects
                .iter()
                .filter(|effect| **effect == expected)
                .count()
        }
        #[test]
        fn owned_normal_up_survives_cursor_failure_and_successful_cleanup_up_is_not_replayed() {
            for button in [false, true] {
                let (mut driver, mut engine, mut backend, _) = setup(8);
                let epoch = begin(&mut driver, &mut engine, &mut backend, 0);
                input(
                    &mut driver,
                    &mut engine,
                    &mut backend,
                    epoch,
                    pair(true, button),
                    1,
                )
                .unwrap();
                backend.fail_pointer = true;
                assert_eq!(
                    input(
                        &mut driver,
                        &mut engine,
                        &mut backend,
                        epoch,
                        pair(false, button),
                        2
                    ),
                    Err(ReceiveError::Native(NativeFailure::BeforeSubmission))
                );
                assert_eq!(
                    up_count(&backend, button),
                    1,
                    "failed cursor skipped normal Up, so Native cleanup still owns one Up"
                );
                assert!(!engine
                    .active
                    .as_ref()
                    .unwrap()
                    .ledger
                    .held(pair(false, button)));
                pump(&mut engine, &mut backend, 3);
                assert_eq!(up_count(&backend, button), 1);
                assert_eq!(
                    driver.snapshot(epoch),
                    ReceiveSnapshot::RecoveryRequired,
                    "error remains visible even after cleanup SDK request"
                );
            }
        }
        #[test]
        fn failed_normal_up_retains_owned_debt_for_cleanup_but_successful_up_never_repeats() {
            for button in [false, true] {
                let (mut driver, mut engine, mut backend, _) = setup(8);
                let epoch = begin(&mut driver, &mut engine, &mut backend, 0);
                input(
                    &mut driver,
                    &mut engine,
                    &mut backend,
                    epoch,
                    pair(true, button),
                    1,
                )
                .unwrap();
                backend.up_failures_remaining = 1;
                assert_eq!(
                    input(
                        &mut driver,
                        &mut engine,
                        &mut backend,
                        epoch,
                        pair(false, button),
                        2
                    ),
                    Err(ReceiveError::Native(NativeFailure::PermissionLost))
                );
                assert_eq!(
                    up_count(&backend, button),
                    2,
                    "one failed normal submission and one owned cleanup request"
                );
                assert!(!engine
                    .active
                    .as_ref()
                    .unwrap()
                    .ledger
                    .held(pair(false, button)));
                pump(&mut engine, &mut backend, 3);
                assert_eq!(up_count(&backend, button), 2);

                let (mut driver, mut engine, mut backend, _) = setup(8);
                let epoch = begin(&mut driver, &mut engine, &mut backend, 0);
                input(
                    &mut driver,
                    &mut engine,
                    &mut backend,
                    epoch,
                    pair(true, button),
                    1,
                )
                .unwrap();
                input(
                    &mut driver,
                    &mut engine,
                    &mut backend,
                    epoch,
                    pair(false, button),
                    2,
                )
                .unwrap();
                driver.control().request_recovery(epoch);
                pump(&mut engine, &mut backend, 3);
                pump(&mut engine, &mut backend, 4);
                assert_eq!(
                    up_count(&backend, button),
                    1,
                    "successfully submitted normal Up retired the owned bit"
                );
                assert_eq!(driver.snapshot(epoch), ReceiveSnapshot::Retired);
            }
        }
        #[test]
        fn definite_before_submission_rolls_back_only_new_down_and_preserves_repeat_owned_debt() {
            for button in [false, true] {
                for already_owned in [false, true] {
                    let (mut driver, mut engine, mut backend, _) = setup(8);
                    let epoch = begin(&mut driver, &mut engine, &mut backend, 0);
                    if already_owned {
                        input(
                            &mut driver,
                            &mut engine,
                            &mut backend,
                            epoch,
                            pair(true, button),
                            1,
                        )
                        .unwrap();
                    }
                    backend.fail_down = Some(NativeFailure::BeforeSubmission);
                    assert_eq!(
                        input(
                            &mut driver,
                            &mut engine,
                            &mut backend,
                            epoch,
                            pair(true, button),
                            2
                        ),
                        Err(ReceiveError::Native(NativeFailure::BeforeSubmission))
                    );
                    assert_eq!(up_count(&backend,button),usize::from(already_owned),
                    "fresh rejected Down invents no Up; rejected repeat retains the original Down");
                    assert_eq!(driver.snapshot(epoch), ReceiveSnapshot::RecoveryRequired);
                }
            }
        }
        #[tokio::test]
        async fn actual_async_driver_gets_a_reply_without_fixed_per_event_sleep() {
            let (mut driver, mut engine, mut backend, wakes) = setup(8);
            let service = async {
                tokio::task::yield_now().await;
                let clock = now_ms();
                pump(&mut engine, &mut backend, clock);
            };
            let (epoch, ()) = tokio::join!(driver.begin(), service);
            let epoch = epoch.unwrap();
            let service = async {
                tokio::task::yield_now().await;
                let clock = now_ms();
                pump(&mut engine, &mut backend, clock);
            };
            let (ack, ()) = tokio::join!(
                driver.input(
                    epoch,
                    Input::Key {
                        usage: 4,
                        down: true
                    },
                    point()
                ),
                service
            );
            assert_eq!(ack, Ok(ReceiveAck::Submitted));
            assert!(wakes.load(Ordering::Relaxed) >= 2);
            let service = async {
                tokio::task::yield_now().await;
                let clock = now_ms();
                pump(&mut engine, &mut backend, clock);
            };
            let (ack, ()) = tokio::join!(driver.release(epoch, None), service);
            assert!(ack.is_ok());
            assert_eq!(driver.snapshot(epoch), ReceiveSnapshot::Retired);
        }
    }
}

#[cfg(any(windows, target_os = "macos"))]
pub(crate) use implementation::{channel, ReceiveBackend, ReceiveEngine};
#[cfg(any(windows, target_os = "macos", test))]
pub use implementation::{ReceiveControl, ReceiveDriver};

// Unsupported hosts still compile the public session API. They never construct
// a native service, report ready, allocate an input queue or submit an OS event.
#[cfg(not(any(windows, target_os = "macos", test)))]
mod unsupported {
    use super::*;
    use std::{
        cell::Cell,
        marker::PhantomData,
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        },
    };
    #[derive(Clone)]
    pub struct ReceiveControl {
        stop: Arc<AtomicBool>,
    }
    impl ReceiveControl {
        pub fn snapshot(&self, _epoch: CaptureEpoch) -> ReceiveSnapshot {
            ReceiveSnapshot::RecoveryRequired
        }
        pub fn outcome(&self) -> ReceiveAck {
            ReceiveAck::RecoveryRequired
        }
        pub fn request_recovery(&self, _epoch: CaptureEpoch) {}
        pub fn request_stop(&self) {
            self.stop.store(true, Ordering::Release);
        }
        pub fn stop_requested(&self) -> bool {
            self.stop.load(Ordering::Acquire)
        }
        pub fn can_force_return(&self, _epoch: CaptureEpoch) -> bool {
            false
        }
        pub fn request_force_return(&self, _epoch: CaptureEpoch) -> bool {
            false
        }
        pub fn wake_loop(&self) {}
    }
    pub struct ReceiveDriver {
        control: ReceiveControl,
        _single: PhantomData<Cell<()>>,
    }
    impl ReceiveDriver {
        pub fn control(&self) -> ReceiveControl {
            self.control.clone()
        }
        pub fn snapshot(&self, epoch: CaptureEpoch) -> ReceiveSnapshot {
            self.control.snapshot(epoch)
        }
        pub fn is_current(&self, _epoch: CaptureEpoch) -> bool {
            false
        }
        pub async fn begin(&mut self) -> Result<CaptureEpoch, ReceiveError> {
            Err(ReceiveError::Closed)
        }
        pub async fn pointer(
            &mut self,
            _epoch: CaptureEpoch,
            point: ReceivePoint,
        ) -> Result<ReceiveAck, ReceiveError> {
            if !point.valid() {
                return Err(ReceiveError::Invalid);
            }
            Err(ReceiveError::Closed)
        }
        pub async fn input(
            &mut self,
            _epoch: CaptureEpoch,
            event: Input,
            point: ReceivePoint,
        ) -> Result<ReceiveAck, ReceiveError> {
            if !point.valid() || !event.valid() {
                return Err(ReceiveError::Invalid);
            }
            Err(ReceiveError::Closed)
        }
        pub async fn release(
            &mut self,
            _epoch: CaptureEpoch,
            point: Option<ReceivePoint>,
        ) -> Result<ReceiveAck, ReceiveError> {
            if point.is_some_and(|point| !point.valid()) {
                return Err(ReceiveError::Invalid);
            }
            Err(ReceiveError::Closed)
        }
        pub fn heartbeat(&mut self, _epoch: CaptureEpoch) -> bool {
            false
        }
    }
}
#[cfg(not(any(windows, target_os = "macos", test)))]
pub use unsupported::{ReceiveControl, ReceiveDriver};
