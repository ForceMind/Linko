//! Receiver injection worker owned by the Windows Native loop. It installs no
//! hook, reads no physical input and never blocks its producer: the Native loop
//! only takes a short queue lock, and fences are polled, never waited on.
use crate::{receive_driver::ReceivePoint, CaptureEpoch};
use peerglide_actions::execution::NativeFailure;
use peerglide_protocol::Input;
use std::{
    collections::VecDeque,
    sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError},
    thread::JoinHandle,
    time::Duration,
};

/// One fixed effect. Pointer positions are normalized receiver coordinates.
#[derive(Clone, Copy)]
pub(crate) enum Item {
    Pointer(ReceivePoint),
    Input(Input),
}

/// Runs only on the injection thread.
pub(crate) trait Sink: Send + 'static {
    fn start(&mut self) {}
    /// Rechecked immediately before each SDK call; false never submits.
    fn admit(&mut self, epoch: CaptureEpoch) -> bool;
    fn submit(&mut self, item: Item) -> Result<(), NativeFailure>;
}

pub(crate) type Wake = Arc<dyn Fn() + Send + Sync>;

struct State {
    items: VecDeque<(CaptureEpoch, Item)>,
    busy: bool,
    unreported: bool,
    poisoned: bool,
    waiting: bool,
    failed_keys: [u64; 4],
    failed_buttons: u8,
    stop: bool,
    exited: bool,
}
struct Shared {
    state: Mutex<State>,
    work: Condvar,
    idle: Condvar,
}
impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
    fn wait<'a>(&self, state: MutexGuard<'a, State>) -> MutexGuard<'a, State> {
        let result = self.work.wait(state);
        result.unwrap_or_else(PoisonError::into_inner)
    }
}

fn release(item: Item) -> Option<Input> {
    match item {
        Item::Input(event @ Input::Key { down: false, .. })
        | Item::Input(event @ Input::Button { down: false, .. }) => Some(event),
        _ => None,
    }
}

struct Exit(Arc<Shared>);
impl Drop for Exit {
    fn drop(&mut self) {
        let mut state = self.0.lock();
        state.exited = true;
        state.busy = false;
        drop(state);
        self.0.idle.notify_all();
    }
}

fn run<S: Sink>(mut sink: S, shared: Arc<Shared>, wake: Wake) {
    let _exit = Exit(shared.clone());
    sink.start();
    loop {
        let (epoch, item, skip) = {
            let mut state = shared.lock();
            loop {
                if state.stop {
                    return;
                }
                if let Some((epoch, item)) = state.items.pop_front() {
                    state.busy = true;
                    break (epoch, item, state.poisoned && release(item).is_none());
                }
                state = shared.wait(state);
            }
        };
        // No lock is held across the SDK call. After a failure the synchronous
        // owner would not have issued later presses/moves; releases still run.
        let failed = !skip && (!sink.admit(epoch) || sink.submit(item).is_err());
        let notify = {
            let mut state = shared.lock();
            state.busy = false;
            let mut notify = false;
            if failed {
                state.unreported = true;
                state.poisoned = true;
                match release(item) {
                    Some(Input::Key { usage, .. }) => {
                        state.failed_keys[usize::from(usage / 64)] |= 1 << (usage % 64);
                    }
                    Some(Input::Button { button, .. }) => {
                        state.failed_buttons |= 1 << (button & 7);
                    }
                    _ => (),
                }
                notify = true;
            }
            if state.items.is_empty() {
                notify |= std::mem::take(&mut state.waiting);
            }
            notify
        };
        shared.idle.notify_all();
        if notify {
            wake();
        }
    }
}

/// Created and dropped by the Native loop outside callbacks.
pub(crate) struct InjectThread {
    shared: Arc<Shared>,
    join: Option<JoinHandle<()>>,
    capacity: usize,
}

pub(crate) fn spawn<S: Sink>(
    sink: S,
    capacity: usize,
    wake: Wake,
) -> std::io::Result<InjectThread> {
    let shared = Arc::new(Shared {
        state: Mutex::new(State {
            items: VecDeque::with_capacity(capacity),
            busy: false,
            unreported: false,
            poisoned: false,
            waiting: false,
            failed_keys: [0; 4],
            failed_buttons: 0,
            stop: false,
            exited: false,
        }),
        work: Condvar::new(),
        idle: Condvar::new(),
    });
    let worker = shared.clone();
    let join = std::thread::Builder::new()
        .name("peerglide-inject".into())
        .spawn(move || run(sink, worker, wake))?;
    Ok(InjectThread {
        shared,
        join: Some(join),
        capacity,
    })
}

impl InjectThread {
    /// Ok means queued (an SDK request will be attempted), never delivery.
    /// An earlier unreported failure is returned here once as UnknownOutcome.
    pub(crate) fn push(&self, epoch: CaptureEpoch, item: Item) -> Result<(), NativeFailure> {
        let mut state = self.shared.lock();
        if std::mem::take(&mut state.unreported) {
            return Err(NativeFailure::UnknownOutcome);
        }
        if state.stop || state.exited || (state.poisoned && release(item).is_none()) {
            return Err(NativeFailure::BeforeSubmission);
        }
        if let Item::Pointer(point) = item {
            // Only the newest position matters; a later key keeps its own
            // preceding snapshot because only the unstarted tail is replaced.
            if let Some((tail_epoch, Item::Pointer(tail))) = state.items.back_mut() {
                if *tail_epoch == epoch {
                    *tail = point;
                    return Ok(());
                }
            }
        }
        if state.items.len() >= self.capacity {
            return Err(NativeFailure::BeforeSubmission);
        }
        state.items.push_back((epoch, item));
        drop(state);
        self.shared.work.notify_one();
        Ok(())
    }
    /// True while an accepted effect has not returned; requests a completion wake.
    pub(crate) fn pending(&self) -> bool {
        let mut state = self.shared.lock();
        let pending = state.busy || !state.items.is_empty();
        state.waiting |= pending;
        pending
    }
    /// Consumes one failure not yet reported through push.
    pub(crate) fn take_failure(&self) -> bool {
        std::mem::take(&mut self.shared.lock().unreported)
    }
    pub(crate) fn take_failed_release(&self) -> Option<Input> {
        let mut state = self.shared.lock();
        if let Some(word) = state.failed_keys.iter().position(|word| *word != 0) {
            let bit = state.failed_keys[word].trailing_zeros();
            state.failed_keys[word] &= !(1 << bit);
            return Some(Input::Key {
                usage: (word * 64) as u16 + bit as u16,
                down: false,
            });
        }
        if state.failed_buttons != 0 {
            let bit = state.failed_buttons.trailing_zeros();
            state.failed_buttons &= !(1 << bit);
            return Some(Input::Button {
                button: bit as u8,
                down: false,
            });
        }
        None
    }
    /// Drops every unstarted item. True when no SDK call is still in flight.
    pub(crate) fn discard(&self) -> bool {
        let mut state = self.shared.lock();
        state.items.clear();
        let busy = state.busy;
        state.waiting |= busy;
        !busy
    }
    /// Non-blocking: no new SDK call starts after this returns.
    pub(crate) fn request_stop(&self) {
        let mut state = self.shared.lock();
        state.stop = true;
        state.items.clear();
        drop(state);
        self.shared.work.notify_all();
    }
    /// Bounded wait for the in-flight call to return, then join.
    pub(crate) fn join(&mut self, timeout: Duration) -> bool {
        let state = self.shared.lock();
        let (state, _) = self
            .shared
            .idle
            .wait_timeout_while(state, timeout, |state| !state.exited)
            .unwrap_or_else(PoisonError::into_inner);
        let exited = state.exited;
        drop(state);
        exited && self.join.take().is_some_and(|join| join.join().is_ok())
    }
}
impl Drop for InjectThread {
    fn drop(&mut self) {
        // Never outlive the capture thread: the queue is already discarded,
        // so only an in-flight SDK call can delay this join.
        self.request_stop();
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    };
    use std::time::Instant;

    // Opaque test epochs and effects exercise software ordering only. They are
    // not native input, delivery receipts, permissions or authentication.
    const EPOCH: CaptureEpoch = CaptureEpoch {
        instance: 7,
        generation: 1,
    };
    #[derive(Debug, PartialEq)]
    enum Seen {
        Pointer(u32),
        Key(u16, bool),
        Button(u8, bool),
        Scroll,
    }
    fn seen(item: Item) -> Seen {
        match item {
            Item::Pointer(point) => Seen::Pointer((point.x * 100.0).round() as u32),
            Item::Input(Input::Key { usage, down }) => Seen::Key(usage, down),
            Item::Input(Input::Button { button, down }) => Seen::Button(button, down),
            Item::Input(Input::Scroll { .. }) => Seen::Scroll,
        }
    }
    fn pointer(x: u32) -> Item {
        Item::Pointer(ReceivePoint {
            x: f64::from(x) / 100.0,
            y: 0.5,
        })
    }
    fn key(usage: u16, down: bool) -> Item {
        Item::Input(Input::Key { usage, down })
    }
    #[derive(Default)]
    struct Controls {
        log: Mutex<Vec<Seen>>,
        held: Mutex<bool>,
        open: Condvar,
        refuse: AtomicBool,
        fail_key: Mutex<Option<u16>>,
        started: AtomicBool,
    }
    struct FakeSink {
        controls: Arc<Controls>,
        entered: mpsc::Sender<()>,
    }
    impl Sink for FakeSink {
        fn start(&mut self) {
            self.controls.started.store(true, Ordering::Release);
        }
        fn admit(&mut self, epoch: CaptureEpoch) -> bool {
            epoch == EPOCH && !self.controls.refuse.load(Ordering::Acquire)
        }
        fn submit(&mut self, item: Item) -> Result<(), NativeFailure> {
            let _ = self.entered.send(());
            let mut held = self.controls.held.lock().unwrap();
            while *held {
                held = self.controls.open.wait(held).unwrap();
            }
            drop(held);
            self.controls.log.lock().unwrap().push(seen(item));
            let fail = *self.controls.fail_key.lock().unwrap();
            match item {
                Item::Input(Input::Key { usage, .. }) if fail == Some(usage) => {
                    Err(NativeFailure::PermissionLost)
                }
                _ => Ok(()),
            }
        }
    }
    struct Rig {
        thread: InjectThread,
        controls: Arc<Controls>,
        entered: mpsc::Receiver<()>,
        wakes: Arc<AtomicUsize>,
    }
    fn rig(capacity: usize) -> Rig {
        let controls = Arc::new(Controls::default());
        let (entered_tx, entered) = mpsc::channel();
        let wakes = Arc::new(AtomicUsize::new(0));
        let count = wakes.clone();
        let thread = spawn(
            FakeSink {
                controls: controls.clone(),
                entered: entered_tx,
            },
            capacity,
            Arc::new(move || {
                count.fetch_add(1, Ordering::AcqRel);
            }),
        )
        .unwrap();
        Rig {
            thread,
            controls,
            entered,
            wakes,
        }
    }
    impl Rig {
        fn hold(&self, held: bool) {
            *self.controls.held.lock().unwrap() = held;
            self.controls.open.notify_all();
        }
        fn in_flight(&self) {
            self.entered
                .recv_timeout(Duration::from_secs(5))
                .expect("item reached the sink");
        }
        fn settle(&self) {
            let deadline = Instant::now() + Duration::from_secs(5);
            while self.thread.pending() {
                assert!(Instant::now() < deadline, "injector failed to drain");
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        fn log(&self) -> Vec<Seen> {
            std::mem::take(&mut *self.controls.log.lock().unwrap())
        }
    }

    #[test]
    fn keys_buttons_and_scroll_keep_their_queue_order() {
        let rig = rig(16);
        for item in [
            key(4, true),
            Item::Input(Input::Button {
                button: 1,
                down: true,
            }),
            Item::Input(Input::Scroll { x: 0, y: 1 }),
            Item::Input(Input::Button {
                button: 1,
                down: false,
            }),
            key(4, false),
        ] {
            assert!(rig.thread.push(EPOCH, item).is_ok());
        }
        rig.settle();
        assert_eq!(
            rig.log(),
            [
                Seen::Key(4, true),
                Seen::Button(1, true),
                Seen::Scroll,
                Seen::Button(1, false),
                Seen::Key(4, false),
            ]
        );
        assert!(rig.controls.started.load(Ordering::Acquire));
        assert!(!rig.thread.take_failure());
    }

    #[test]
    fn only_adjacent_unstarted_pointer_moves_coalesce_to_the_newest_position() {
        let rig = rig(16);
        rig.hold(true);
        assert!(rig.thread.push(EPOCH, pointer(1)).is_ok());
        rig.in_flight();
        for item in [pointer(2), pointer(3), pointer(4), key(4, true)] {
            assert!(rig.thread.push(EPOCH, item).is_ok());
        }
        for item in [pointer(5), pointer(6), key(4, false)] {
            assert!(rig.thread.push(EPOCH, item).is_ok());
        }
        // A different epoch never rewrites the previous owner's tail.
        let other = CaptureEpoch {
            instance: 7,
            generation: 2,
        };
        assert!(rig.thread.push(EPOCH, pointer(7)).is_ok());
        assert!(rig.thread.push(other, pointer(8)).is_ok());
        rig.hold(false);
        rig.settle();
        assert_eq!(
            rig.log(),
            [
                Seen::Pointer(1),
                Seen::Pointer(4),
                Seen::Key(4, true),
                Seen::Pointer(6),
                Seen::Key(4, false),
                Seen::Pointer(7),
            ]
        );
        // The refused other-epoch move is a failure, never an SDK call.
        assert!(rig.thread.take_failure());
    }

    #[test]
    fn fence_stays_pending_until_the_in_flight_call_returns_and_then_wakes_the_loop() {
        let rig = rig(16);
        rig.hold(true);
        assert!(rig.thread.push(EPOCH, key(4, false)).is_ok());
        rig.in_flight();
        assert!(rig.thread.pending(), "an in-flight Up is not yet settled");
        assert!(
            !rig.thread.discard(),
            "discard cannot cancel an in-flight call"
        );
        let before = rig.wakes.load(Ordering::Acquire);
        rig.hold(false);
        rig.settle();
        assert!(rig.wakes.load(Ordering::Acquire) > before);
        assert!(rig.thread.discard());
        assert_eq!(rig.log(), [Seen::Key(4, false)]);
    }

    #[test]
    fn async_failure_is_reported_once_and_a_failed_release_is_returned_for_retry() {
        let rig = rig(16);
        *rig.controls.fail_key.lock().unwrap() = Some(5);
        rig.hold(true);
        for item in [key(5, true), key(5, false)] {
            assert!(rig.thread.push(EPOCH, item).is_ok());
        }
        rig.in_flight();
        rig.hold(false);
        rig.settle();
        assert!(rig.thread.take_failure());
        assert!(!rig.thread.take_failure());
        assert!(matches!(
            rig.thread.take_failed_release(),
            Some(Input::Key {
                usage: 5,
                down: false
            })
        ));
        assert!(rig.thread.take_failed_release().is_none());
        // After a failure new presses are refused; releases are still queued.
        assert!(matches!(
            rig.thread.push(EPOCH, key(6, true)),
            Err(NativeFailure::BeforeSubmission)
        ));
        assert_eq!(rig.log(), [Seen::Key(5, true), Seen::Key(5, false)]);
        *rig.controls.fail_key.lock().unwrap() = None;
        assert!(rig.thread.push(EPOCH, key(5, false)).is_ok());
        rig.settle();
        assert_eq!(rig.log(), [Seen::Key(5, false)]);
    }

    #[test]
    fn an_unreported_failure_surfaces_on_the_next_push_and_later_presses_are_skipped() {
        let rig = rig(16);
        *rig.controls.fail_key.lock().unwrap() = Some(4);
        rig.hold(true);
        assert!(rig.thread.push(EPOCH, key(4, true)).is_ok());
        rig.in_flight();
        for item in [pointer(2), key(7, true), key(7, false)] {
            assert!(rig.thread.push(EPOCH, item).is_ok());
        }
        rig.hold(false);
        rig.settle();
        assert!(matches!(
            rig.thread.push(EPOCH, key(8, false)),
            Err(NativeFailure::UnknownOutcome)
        ));
        assert!(!rig.thread.take_failure());
        assert_eq!(rig.log(), [Seen::Key(4, true), Seen::Key(7, false)]);
    }

    #[test]
    fn a_refused_recheck_never_submits_and_keeps_the_release_owed() {
        let rig = rig(16);
        rig.controls.refuse.store(true, Ordering::Release);
        let up = Item::Input(Input::Button {
            button: 2,
            down: false,
        });
        assert!(rig.thread.push(EPOCH, up).is_ok());
        rig.settle();
        assert!(rig.log().is_empty(), "source loss never blindly sends Up");
        assert!(rig.thread.take_failure());
        assert!(matches!(
            rig.thread.take_failed_release(),
            Some(Input::Button {
                button: 2,
                down: false
            })
        ));
    }

    #[test]
    fn a_full_queue_refuses_discrete_input_but_still_merges_the_newest_move() {
        let rig = rig(2);
        rig.hold(true);
        assert!(rig.thread.push(EPOCH, key(4, true)).is_ok());
        rig.in_flight();
        assert!(rig.thread.push(EPOCH, key(5, true)).is_ok());
        assert!(rig.thread.push(EPOCH, pointer(3)).is_ok());
        // The newest move still merges into the unstarted tail when full.
        assert!(rig.thread.push(EPOCH, pointer(9)).is_ok());
        assert!(matches!(
            rig.thread.push(EPOCH, key(6, true)),
            Err(NativeFailure::BeforeSubmission)
        ));
        assert!(matches!(
            rig.thread.push(EPOCH, Item::Input(Input::Scroll { x: 1, y: 0 })),
            Err(NativeFailure::BeforeSubmission)
        ));
        rig.hold(false);
        rig.settle();
        assert_eq!(
            rig.log(),
            [Seen::Key(4, true), Seen::Key(5, true), Seen::Pointer(9)]
        );
        assert!(!rig.thread.take_failure());
    }

    #[test]
    fn stop_discards_queued_work_and_bounds_the_wait_for_an_in_flight_call() {
        let mut rig = rig(16);
        rig.hold(true);
        assert!(rig.thread.push(EPOCH, key(4, true)).is_ok());
        rig.in_flight();
        assert!(rig.thread.push(EPOCH, key(4, false)).is_ok());
        rig.thread.request_stop();
        assert!(!rig.thread.join(Duration::from_millis(20)));
        assert!(matches!(
            rig.thread.push(EPOCH, key(5, false)),
            Err(NativeFailure::BeforeSubmission)
        ));
        rig.hold(false);
        assert!(rig.thread.join(Duration::from_secs(5)));
        assert_eq!(rig.log(), [Seen::Key(4, true)]);
        assert!(!rig.thread.pending());
    }

    #[test]
    fn dropping_an_idle_injector_joins_its_thread() {
        let rig = rig(4);
        let shared = rig.thread.shared.clone();
        drop(rig);
        assert!(shared.lock().exited);
    }
}
