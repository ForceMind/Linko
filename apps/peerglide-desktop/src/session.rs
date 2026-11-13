use anyhow::{bail, Result};
use peerglide_core::{EdgeGate, PendingReleases, Receiver};
use peerglide_platform::display_layout::Layout;
use peerglide_platform::input_diagnostics::{Count, InputDiagnostics};
use peerglide_platform::{
    self as platform, now_ms, CaptureCommit, CaptureCommitToken, CaptureEpoch, CaptureRoute,
    Captured,
};
use peerglide_protocol::file_drop_ipc::{Event as FileDropEvent, State as FileDropState};
use peerglide_protocol::{
    decode, encode, layout_ipc::LayoutHint, worker::WorkerInputState, Edge, Input,
    InputCapabilities, Message, Motion,
};
use peerglide_transport as transport;
use platform::{ReceiveAck, ReceiveSnapshot};
use std::{future::Future, pin::Pin, time::Duration};
use tokio::sync::mpsc;

mod recovery;
pub(super) mod routed_source;
#[cfg(target_os = "linux")]
pub(super) mod wayland_source;
#[cfg(target_os = "linux")]
pub(super) mod x11_source;
use super::file_drop_control::TargetAction;
use recovery::Recovery;

pub(super) struct Abort(pub(super) tokio::task::JoinHandle<()>);
impl Drop for Abort {
    fn drop(&mut self) {
        self.0.abort();
    }
}

pub(super) async fn send(tx: &mut quinn::SendStream, m: &Message) -> Result<()> {
    tokio::time::timeout(Duration::from_millis(250), transport::send(tx, m)).await??;
    Ok(())
}

pub(super) const NATIVE_INPUT_RECOVERED: &[u8] = b"linko native input recovered/1";

pub(super) fn close_reason_after_recovery(
    session: &Result<()>,
    state: Option<WorkerInputState>,
    fallback: &'static [u8],
) -> &'static [u8] {
    if session.is_ok() && state == Some(WorkerInputState::Local) {
        NATIVE_INPUT_RECOVERED
    } else {
        fallback
    }
}

fn normal_close(c: &quinn::Connection) -> bool {
    matches!(c.close_reason(),Some(quinn::ConnectionError::ApplicationClosed(closed)) if closed.error_code==0u8.into())
}
fn connection_end(reason: quinn::ConnectionError) -> Result<()> {
    match reason {
        quinn::ConnectionError::ApplicationClosed(closed) if closed.error_code == 0u8.into() => {
            Ok(())
        }
        quinn::ConnectionError::LocallyClosed => Ok(()),
        error => Err(error.into()),
    }
}
pub struct Policy {
    pub allow: bool,
    pub native: bool,
    pub edge: Edge,
    pub local: InputCapabilities,
    pub peer: InputCapabilities,
}
#[derive(Clone, Copy)]
struct Outgoing {
    lease: u64,
    epoch: CaptureEpoch,
}
const MOTION_SEND_INTERVAL: Duration = Duration::from_millis(4);

struct MotionOutbox {
    latest: Option<Motion>,
    next_send: tokio::time::Instant,
}

impl MotionOutbox {
    fn new(now: tokio::time::Instant) -> Self {
        Self {
            latest: None,
            next_send: now,
        }
    }

    fn take_due(&mut self, now: tokio::time::Instant) -> Option<Motion> {
        if now < self.next_send {
            return None;
        }
        let motion = self.latest.take()?;
        self.next_send = now + MOTION_SEND_INTERVAL;
        Some(motion)
    }
}

const ACQUIRE_TIMEOUT_MS: u64 = 1000;
const COMMIT_RETRY_MS: u64 = 10;
const MAX_COMMIT_ATTEMPTS: u16 = 100;
#[derive(Clone, Copy)]
struct CommitRetry {
    next: u64,
    attempts: u16,
}
#[derive(Clone, Copy)]
struct PendingAcquire {
    lease: u64,
    token: CaptureCommitToken,
    deadline: u64,
    commit: Option<CommitRetry>,
}
impl PendingAcquire {
    fn new(lease: u64, token: CaptureCommitToken, now: u64) -> Self {
        Self {
            lease,
            token,
            deadline: now.saturating_add(ACQUIRE_TIMEOUT_MS),
            commit: None,
        }
    }
    fn grant(&mut self, lease: u64, now: u64) -> bool {
        if self.lease != lease || now >= self.deadline {
            return false;
        }
        // Replayed Grants cannot extend either the budget or the next attempt.
        self.commit.get_or_insert(CommitRetry {
            next: now,
            attempts: 0,
        });
        true
    }
    fn wake_at(self) -> u64 {
        self.commit
            .map_or(self.deadline, |retry| retry.next.min(self.deadline))
    }
}
#[derive(Debug, PartialEq, Eq)]
enum PendingStep {
    Waiting,
    Committed { lease: u64, epoch: CaptureEpoch },
    Cancelled { lease: u64, timed_out: bool },
}
/// Pure clock/context transitions; the only effect is the injected atomic commit.
fn advance_pending(
    pending: &mut Option<PendingAcquire>,
    now: u64,
    context_current: bool,
    mut commit: impl FnMut(CaptureCommitToken, u64) -> Result<CaptureCommit>,
) -> Result<PendingStep> {
    let Some(request) = pending.as_mut() else {
        return Ok(PendingStep::Waiting);
    };
    if !context_current || now >= request.deadline {
        let request = pending.take().unwrap();
        return Ok(PendingStep::Cancelled {
            lease: request.lease,
            timed_out: context_current && now >= request.deadline,
        });
    }
    let Some(retry) = request.commit.as_mut() else {
        return Ok(PendingStep::Waiting);
    };
    if now < retry.next {
        return Ok(PendingStep::Waiting);
    }
    retry.attempts += 1;
    match commit(request.token, request.deadline)? {
        CaptureCommit::Committed(epoch) => {
            let lease = request.lease;
            *pending = None;
            Ok(PendingStep::Committed { lease, epoch })
        }
        CaptureCommit::Retry => {
            retry.next = if retry.attempts < MAX_COMMIT_ATTEMPTS {
                now.saturating_add(COMMIT_RETRY_MS)
            } else {
                request.deadline
            };
            Ok(PendingStep::Waiting)
        }
        CaptureCommit::Cancelled => {
            let lease = request.lease;
            *pending = None;
            Ok(PendingStep::Cancelled {
                lease,
                timed_out: false,
            })
        }
    }
}
async fn pending_wake(at: Option<u64>) {
    match at {
        Some(at) => tokio::time::sleep(Duration::from_millis(at.saturating_sub(now_ms()))).await,
        None => std::future::pending::<()>().await,
    }
}
/// State is emitted by this async session only. Routing is not OS/application
/// completion, and a failed cleanup remains unconfirmed even if Drop retries it.
struct InputStatus<'a> {
    native: bool,
    reporter: &'a mut super::InputReporter,
    unconfirmed: bool,
}
impl<'a> InputStatus<'a> {
    fn new(native: bool, reporter: &'a mut super::InputReporter) -> Self {
        Self {
            native,
            reporter,
            unconfirmed: false,
        }
    }
    fn routing(&mut self, outgoing: bool, incoming: bool) {
        let state = if !self.native {
            WorkerInputState::Disabled
        } else if self.unconfirmed {
            WorkerInputState::RecoveryUnconfirmed
        } else if outgoing {
            WorkerInputState::ControllingRemote
        } else if incoming {
            WorkerInputState::ReceivingRemote
        } else {
            WorkerInputState::Local
        };
        self.reporter.set(state);
    }
    fn recovering(&mut self) {
        self.reporter.set(if !self.native {
            WorkerInputState::Disabled
        } else if self.unconfirmed {
            WorkerInputState::RecoveryUnconfirmed
        } else {
            WorkerInputState::Recovering
        });
    }
    fn checked<T>(&mut self, result: Result<T>) -> Result<T> {
        if result.is_err() && self.native {
            self.unconfirmed = true;
            self.reporter.set(WorkerInputState::RecoveryUnconfirmed);
        }
        result
    }
    fn restore(&mut self, capture: Option<&platform::CaptureSession>) -> Result<()> {
        self.recovering();
        if let Some(capture) = capture {
            self.checked(capture.restore_checked())?;
        }
        Ok(())
    }
    fn restore_at_boundary(
        &mut self,
        capture: Option<&platform::CaptureSession>,
        route: CaptureRoute,
    ) -> Result<bool> {
        let (Some(capture), CaptureRoute::Remote(epoch)) = (capture, route) else {
            return Ok(false);
        };
        let restored = self.checked(capture.restore_at_boundary(epoch))?;
        if restored {
            self.recovering();
        }
        Ok(restored)
    }
    async fn release(&mut self, recovery: &mut Recovery) -> Result<()> {
        self.recovering();
        self.checked(recovery.release().await)
    }
    fn finish(&mut self, clean: bool) {
        self.reporter.set(if !self.native {
            WorkerInputState::Disabled
        } else if self.unconfirmed {
            WorkerInputState::RecoveryUnconfirmed
        } else if clean {
            WorkerInputState::Local
        } else {
            WorkerInputState::Unavailable
        });
    }
}

async fn drain_capture(
    initial: (Result<bool>, bool),
    mut poll: impl FnMut() -> (Result<bool>, bool),
    status: &mut InputStatus<'_>,
    started: std::time::Instant,
    budget: Duration,
) -> Result<()> {
    let mut state = initial;
    let mut failure = None;
    loop {
        let (result, pending) = state;
        match result {
            Ok(true) if !pending => return failure.map_or(Ok(()), Err),
            Err(error) if failure.is_none() => {
                failure = status.checked::<()>(Err(error)).err();
            }
            _ => (),
        }
        if !pending {
            return Err(failure
                .unwrap_or_else(|| anyhow::anyhow!("capture ended without confirmed cleanup")));
        }
        if started.elapsed() >= budget && failure.is_none() {
            failure = status.checked::<()>(Err(anyhow::anyhow!(
                "native capture cleanup is still pending; waiting for the original thread and reaper"
            ))).err();
        }
        // The UI can still send stop or explicitly force the process. Ordinary
        // stop must not kill a hook that still owes a suppressed physical Up.
        tokio::time::sleep(Duration::from_millis(20)).await;
        state = poll();
    }
}

#[cfg(test)]
pub async fn run(
    c: quinn::Connection,
    tx: quinn::SendStream,
    rx: quinn::RecvStream,
    policy: Policy,
) -> Result<()> {
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let mut reporter = super::InputReporter::new(false);
    run_with_status(c, tx, rx, policy, stopped, &mut reporter).await
}

#[cfg(test)]
async fn run_with_status(
    c: quinn::Connection,
    tx: quinn::SendStream,
    rx: quinn::RecvStream,
    policy: Policy,
    stop: tokio::sync::watch::Receiver<bool>,
    reporter: &mut super::InputReporter,
) -> Result<()> {
    run_with_diagnostics(
        c,
        tx,
        rx,
        policy,
        stop,
        reporter,
        SessionControls::default(),
    )
    .await
}

#[derive(Default)]
pub(super) struct SessionControls<'a> {
    pub diagnostics: Option<&'a std::sync::Arc<InputDiagnostics>>,
    pub layout: Option<&'a super::layout_control::Controller>,
    pub file_drop: Option<super::file_drop_control::Config<'a>>,
    pub receive_display: Option<std::sync::Arc<platform::ReceiveDisplay>>,
    pub secondary: Option<&'a super::secondary_layout::Routes>,
    pub rebuilding_display_layout: bool,
    pub remote_entry_edge: Option<Edge>,
}

// Store the optional network negotiation outside the inline session future.
// Unused CLI branches still contribute to the Windows debug startup stack.
fn secondary_confirmation<'a>(
    routes: Option<&'a super::secondary_layout::Routes>,
    connection: &'a quinn::Connection,
    stop: &'a mut tokio::sync::watch::Receiver<bool>,
    gate: &'a platform::CaptureGate,
    request: PendingAcquire,
    edge: Edge,
    fraction: f64,
) -> Option<Pin<Box<dyn Future<Output = bool> + 'a>>> {
    let routes = routes.filter(|routes| routes.has_outbound())?;
    Some(Box::pin(async move {
        let confirmed = routes
            .confirm_outbound(connection, stop, request.lease, edge, fraction)
            .await;
        if !confirmed {
            eprintln!("Secondary layout reservation was not confirmed.");
        }
        confirmed
            && now_ms() < request.deadline
            && !*stop.borrow()
            && gate.commit_pending(request.token)
    }))
}

pub(super) async fn run_with_diagnostics(
    c: quinn::Connection,
    mut tx: quinn::SendStream,
    mut rx: quinn::RecvStream,
    policy: Policy,
    mut stop: tokio::sync::watch::Receiver<bool>,
    reporter: &mut super::InputReporter,
    controls: SessionControls<'_>,
) -> Result<()> {
    let SessionControls {
        diagnostics,
        layout,
        file_drop,
        receive_display,
        secondary,
        rebuilding_display_layout,
        remote_entry_edge,
    } = controls;
    let Policy {
        allow,
        native,
        mut edge,
        local,
        peer,
    } = policy;
    anyhow::ensure!(
        local.valid() && peer.valid(),
        "invalid session capabilities"
    );
    let mut input_status = InputStatus::new(native, reporter);
    if *stop.borrow() {
        input_status.reporter.set(WorkerInputState::Disabled);
        return Ok(());
    }
    let (layout_scope, mut layout_requests) = if let Some(layout) = layout {
        let (scope, receiver, current_edge) = layout.open(edge)?;
        edge = current_edge;
        (Some(scope), receiver)
    } else {
        let (_sender, receiver) = mpsc::channel(1);
        (None, receiver)
    };
    let (drop_scope, mut drop_requests) = if let Some(config) = file_drop {
        let (scope, mailbox) = config.controller.open()?;
        (Some(scope), mailbox)
    } else {
        let (_sender, mailbox) = tokio::sync::watch::channel(None);
        (None, mailbox)
    };
    let mut drop_route = None;
    let mut file_lease: Option<(u64, u32)> = None;
    let mut file_confirmation: Option<super::file_drop_control::SourceReservation> = None;
    // Publish connection readiness only after its private configuration queue
    // exists, so an immediate GUI edit cannot disappear during session startup.
    super::worker_event(
        input_status.reporter.managed,
        super::WorkerState::Authenticated,
    );
    input_status.reporter.peer_motion(peer.motion());
    let (itx, mut input) = platform::capture_event_channel();
    let count = |kind| {
        if let Some(diagnostics) = diagnostics {
            diagnostics.increment(kind);
        }
    };
    // Native display enumeration runs before installing capture, on the
    // worker. A failed OS query keeps the compatible primary-screen behavior.
    let topology = monitor_layout(native);
    if rebuilding_display_layout && topology.is_none() {
        input_status.reporter.set(WorkerInputState::Disabled);
        return Err(DisplayLayoutEnd::Unavailable.into());
    }
    if receive_display.as_ref().is_some_and(|selection| {
        topology
            .as_ref()
            .is_none_or(|current| !selection.matches_snapshot(current))
    }) {
        input_status.reporter.set(WorkerInputState::Disabled);
        bail!(
            "The selected receive display is no longer current. Select it again before restarting."
        );
    }
    let mut display_change = Box::pin(watch_display_layout(topology.as_ref()));
    let mut capture = if native {
        match platform::capture_with_receive_display(
            itx,
            peer.clone(),
            diagnostics.cloned(),
            receive_display,
        ) {
            Ok(capture) => Some(capture),
            Err(error) => {
                input_status.reporter.set(WorkerInputState::Disabled);
                return Err(error);
            }
        }
    } else {
        None
    };
    let (driver, driver_error) = match capture.as_mut() {
        Some(capture) => match capture.take_receive_driver() {
            Ok(driver) => (driver, None),
            Err(error) => (None, Some(error)),
        },
        None => (None, None),
    };
    let _native_stop = driver
        .as_ref()
        .map(|driver| super::NativeRecoveryRegistration::new(driver.control(), *stop.borrow()));
    let (mtx, mut messages) = mpsc::channel(64);
    let invalid_frame = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let reader_invalid = invalid_frame.clone();
    let reader = tokio::spawn(async move {
        loop {
            let m = transport::receive(&mut rx).await;
            let failed = m.is_err();
            if m.as_ref()
                .err()
                .is_some_and(|error| error.is::<transport::InvalidMessageFrame>())
            {
                reader_invalid.store(true, std::sync::atomic::Ordering::Release);
            }
            if mtx.send(m).await.is_err() || failed {
                break;
            }
        }
    });
    let _reader = Abort(reader);
    let mut recovery = Recovery {
        receiver: Receiver::default(),
        native,
        pending_releases: PendingReleases::default(),
        driver,
        receive_epoch: None,
    };
    let mut pending: Option<PendingAcquire> = None;
    let mut outgoing: Option<Outgoing> = None;
    let mut next_lease = 0u64;
    let mut sequence = 0u64;
    let mut motion_outbox = MotionOutbox::new(tokio::time::Instant::now());
    let mut position = (0.5, 0.5);
    let mut local_return = (0.5, 0.5);
    let mut size = (1920.0, 1080.0);
    let mut last_pong = 0u64;
    let mut edge_gate = session_edge_gate(rebuilding_display_layout);
    let mut hint_reported: Option<(Edge, Edge)> = None;
    let mut tick = tokio::time::interval(Duration::from_millis(200));
    let result: Result<()> = async {
    if let Some(error)=driver_error{return Err(error);}
    anyhow::ensure!(!native||recovery.driver.is_some(),"native receive service unavailable");
    loop {
        // A timer only wakes this loop. Stop/closed ownership is checked before
        // every attempt, so an old Granted cannot activate after cancellation.
        if *stop.borrow() || stop.has_changed().is_err() {return Ok(());}
        if let Some(closed)=c.close_reason() {
            return if invalid_frame.load(std::sync::atomic::Ordering::Acquire) {Err(transport::InvalidMessageFrame.into())} else {connection_end(closed)};
        }
        if messages.is_closed() && messages.is_empty(){return Ok(());}
        // Queue pressure or an emergency can retire a capture fence without a
        // queued takeover record. Do not keep renewing its old receive lease.
        if native && recovery.outcome()==ReceiveAck::RecoveryRequired {
            input_status.checked::<()>(Err(anyhow::anyhow!("native receive recovery remains unconfirmed")))?;
        }
        if let Some(lease) = recovery.receiver.active() {
            let file_expired = drop_scope.as_ref().is_some_and(|scope| scope.target_expired(lease));
            if file_expired || (native && recovery.snapshot() != ReceiveSnapshot::Active) {
                edge_gate.require_interior();
                if let Some(event) = drop_scope.as_ref().and_then(|scope| scope.end_target(lease, false)) {
                    input_status.reporter.file_drop(event);
                }
                input_status.release(&mut recovery).await?;
                send(&mut tx, &Message::Release { lease, cursor: None }).await?;
            }
        }
        if recovery.snapshot()==ReceiveSnapshot::Draining || (native && recovery.receiver.active().is_none() && recovery.outcome()==ReceiveAck::Submitted) {input_status.recovering();}
        else{input_status.routing(outgoing.is_some(), recovery.receiver.active().is_some());}
        if let (Some((lease, request)), Some(scope)) = (file_lease, &drop_scope) {
            let acquiring = pending.is_some_and(|pending| pending.lease == lease);
            let sending = outgoing.is_some_and(|outgoing| outgoing.lease == lease);
            if sending && !scope.current(request) {
                input_status.restore(capture.as_ref())?;
                outgoing = None;
                edge_gate.require_interior();
                motion_outbox.latest = None;
                send(&mut tx, &Message::Release { lease, cursor: None }).await?;
            }
            if !acquiring && outgoing.is_none_or(|outgoing| outgoing.lease != lease) {
                if scope.current(request) {
                    input_status.reporter.file_drop(scope.event(request, FileDropState::Rejected));
                }
                scope.retire(request);
                file_lease = None;
                file_confirmation = None;
            }
        }
        let file_current = file_lease.is_none_or(|(_, request)| drop_scope.as_ref().is_some_and(|scope| scope.current(request)));
        let current=file_current && pending.is_none_or(|request|capture.as_ref().is_some_and(|cap|cap.gate.commit_pending(request.token)) && !recovery.protection_pending());
        let now=now_ms();
        // Process already-received reliable Release/Denied/stream errors before
        // retrying. Keep the other select branches fair; only retries yield.
        let network_ready=!messages.is_empty();
        if !network_ready {
            if let Some(scope) = &drop_scope {
                if let Some(event) = scope.advance_claim(|lease| {
                    !*stop.borrow()
                        && recovery.receiver.accepts(lease, now_ms())
                        && (!native || recovery.snapshot() == ReceiveSnapshot::Active)
                }) {
                    input_status.reporter.file_drop(event);
                }
            }
        }
        if !network_ready {
            if let (Some(scope), Some(lease)) = (&drop_scope, recovery.receiver.active()) {
                if let Some(cursor) = scope.confirmed_drop(lease).filter(|_| {
                    !*stop.borrow() && recovery.receiver.accepts(lease, now_ms())
                }) {
                    let applied = recovery.input(
                        lease, Input::Button { button: 1, down: false }, cursor, now_ms(),
                    ).await;
                    let dropped = input_status.checked(applied)?;
                    if let Some(event) = scope.end_target(lease, dropped) {
                        input_status.reporter.file_drop(event);
                    }
                    if !dropped {
                        input_status.release(&mut recovery).await?;
                        send(&mut tx, &Message::Release { lease, cursor: None }).await?;
                    }
                }
            }
        }
        let defer_commit=network_ready && current && pending.is_some_and(|request|now<request.deadline);
        // Stage-only diagnostics: never key/button contents, positions or lease ids.
        let peer_granted=pending.is_some_and(|request|request.commit.is_some());
        let step=if defer_commit {PendingStep::Waiting}else{advance_pending(&mut pending,now,current,|token,deadline| {
            let cap=capture.as_ref().ok_or_else(||anyhow::anyhow!("native capture unavailable during acquire"))?;
            input_status.checked(cap.begin_routed_from(token,deadline,local_return.0,local_return.1))
        })?};
        match step {
            PendingStep::Waiting=>{},
            PendingStep::Committed{lease,epoch}=>{
                count(Count::Committed);
                motion_outbox.latest=None;
                input_status.reporter.switch(super::WorkerSwitchState::Idle);edge_gate.require_interior();outgoing=Some(Outgoing{lease,epoch});sequence=0;last_pong=now_ms();
                if let Some(cap)=&capture{cap.gate.refresh();}
                if let (Some((file, request)), Some(scope)) = (file_lease, &drop_scope) {
                    if file == lease {
                        if *stop.borrow() || !scope.current(request)
                            || !capture.as_ref().is_some_and(|cap| cap.gate.is_current(CaptureRoute::Remote(epoch)))
                        {
                            input_status.restore(capture.as_ref())?;
                            outgoing = None;
                            send(&mut tx, &Message::Release { lease, cursor: None }).await?;
                            continue;
                        }
                        sequence = 1;
                        let cursor = Motion { lease, sequence, x: position.0, y: position.1 };
                        let event = Input::Button { button: 1, down: true };
                        send(&mut tx, &Message::Input { lease, event, cursor }).await?;
                        input_status.reporter.file_drop(FileDropEvent::new(
                            scope.session, request, FileDropState::SourceCommitted, Some(lease),
                        ));
                    }
                }
                continue;
            },
            PendingStep::Cancelled{lease,timed_out}=>{
                count(if timed_out {Count::AcquireTimeout} else {Count::AcquireCancelled});
                eprintln!("{}", match (peer_granted,timed_out) {
                    (false,true)=>"Switch expired before peer grant.",
                    (true,true)=>"Switch expired after peer grant while committing locally.",
                    (_,false)=>"Switch cancelled by local admission protection.",
                });
                edge_gate.require_interior();input_status.reporter.switch(if timed_out {super::WorkerSwitchState::TimedOut}else{super::WorkerSwitchState::Idle});
                send(&mut tx,&Message::Release{lease,cursor:None}).await?;
                continue;
            },
        }
        let acquire_wake=pending.map(PendingAcquire::wake_at);
        let departure=OutgoingEdges::new(edge,remote_entry_edge);
        tokio::select! {
            _=super::stop_requested(&mut stop)=>return Ok(()),
            changed=&mut display_change,if native && cfg!(any(windows,target_os="macos"))=>return changed,
            _=tokio::time::sleep_until(motion_outbox.next_send),if motion_outbox.latest.is_some()=>{
                if *stop.borrow() || stop.has_changed().is_err(){return Ok(());}
                if let Some(m)=motion_outbox.take_due(tokio::time::Instant::now()){
                    // Recheck the lease and capture epoch after waiting. A queued
                    // position cannot outlive its capture ownership.
                    let live=outgoing.filter(|current|current.lease==m.lease).is_some_and(|current| {
                        capture.as_ref().is_some_and(|cap|cap.gate.is_current(CaptureRoute::Remote(current.epoch)))
                    });
                    if live {
                        use quinn::SendDatagramError::*;
                        count(match c.send_datagram(encode(&m)?.into()) {
                            Ok(())=>Count::SendOk,
                            Err(UnsupportedByPeer)=>Count::SendUnsupported,
                            Err(Disabled)=>Count::SendDisabled,
                            Err(TooLarge)=>Count::SendOversize,
                            Err(ConnectionLost(_))=>Count::SendClosed,
                        });
                    }
                }
            },
            request=layout_requests.recv(),if layout_scope.is_some()=>{
                if let Some(request)=request {
                    let applied=now_ms()<request.deadline
                        && !*stop.borrow()
                        && pending.is_none() && outgoing.is_none()
                        && recovery.receiver.active().is_none()
                        && !recovery.protection_pending()
                        && capture.as_ref().is_none_or(|cap|cap.gate.reconfigure_local());
                    if applied {
                        edge=request.update.edge;
                        edge_gate.require_interior();
                        if let Some(layout)=layout {layout.applied(edge);}
                    }
                    println!("{}",request.update.reply(applied).line());
                }
            },
            changed=drop_requests.changed(),if drop_scope.is_some()=>{
                drop(drop_route.take());
                if changed.is_ok() && !*stop.borrow() {
                    if let (Some(scope), Some(config)) = (&drop_scope, file_drop) {
                        let (route, event) = super::file_drop_control::Route::update(
                            scope, &mut drop_requests, config, &c,
                            capture.as_ref().map(|cap| &cap.gate), allow,
                        );
                        drop_route = route;
                        if let Some(event) = event { input_status.reporter.file_drop(event); }
                    }
                }
            },
            closed=c.closed()=>return if invalid_frame.load(std::sync::atomic::Ordering::Acquire) {
                Err(transport::InvalidMessageFrame.into())
            } else { connection_end(closed) },
            _=pending_wake(acquire_wake),if !network_ready=>{},
            _=tick.tick()=>{
                let now=now_ms();
                if let (Some(super::file_drop_control::Route::Source(source)), Some(scope)) =
                    (&drop_route, &drop_scope)
                {
                    let current = scope.current(source.request);
                    let held = capture.as_ref().and_then(|cap| cap.gate.held_primary());
                    if !current || held != Some(source.hold) {
                        if current {
                            input_status.reporter.file_drop(scope.event(source.request, FileDropState::Rejected));
                        }
                        scope.retire(source.request);
                        drop_route = None;
                    }
                }
                if recovery.receiver.expired(now) {
                    if let Some(event) = recovery.receiver.active().and_then(|lease|
                        drop_scope.as_ref().and_then(|scope| scope.end_target(lease, false))
                    ) {
                        input_status.reporter.file_drop(event);
                    }
                    input_status.release(&mut recovery).await?;
                }
                if let Some(Outgoing {lease,epoch})=outgoing {
                    if now.saturating_sub(last_pong)>=1000 {
                        count(Count::PongTimeout);
                        input_status.restore(capture.as_ref())?;
                        outgoing=None; send(&mut tx,&Message::Release { lease, cursor: None }).await?;
                    }else if file_lease.is_none() && input.is_empty() && departure.crossed_back(position) && input_status.restore_at_boundary(capture.as_ref(),CaptureRoute::Remote(epoch))? {
                        count(Count::EdgeReturn);
                        // A motion can wake this task before its callback drops.
                        // Retry on the existing tick after older captured input
                        // drained, even if the user made no further movement.
                        outgoing=None;
                        sequence=sequence.checked_add(1).ok_or_else(||anyhow::anyhow!("input sequence exhausted"))?;
                        let cursor=Motion{lease,sequence,x:position.0,y:position.1};
                        send(&mut tx,&Message::Release{lease,cursor:Some(cursor)}).await?;
                    }else{send(&mut tx,&Message::Ping{lease}).await?;}
                }
                // A full native event queue disables capture in the callback. Reconcile protocol immediately.
                if let (Some(cap),Some(current))=(&capture,outgoing){if !cap.gate.is_current(CaptureRoute::Remote(current.epoch)){input_status.restore(Some(cap))?;outgoing=None;send(&mut tx,&Message::Release { lease: current.lease, cursor: None }).await?;}}
            },
            m=messages.recv()=>{
                let Some(m)=m else{return Ok(());};
                let m=match m {
                    Ok(m)=>m,
                    Err(e) if e.is::<transport::MessageStreamEnded>()=>return c.close_reason().map(connection_end).unwrap_or(Ok(())),
                    Err(e) if !e.is::<transport::InvalidMessageFrame>() && normal_close(&c)=>return Ok(()),
                    Err(e)=>return Err(e),
                };
                let now=now_ms();
                match m {
                    Message::Acquire{lease,edge:remote_edge,along}=>{
                        count(Count::AcquireReceived);
                        if let Some(hint)=layout_hint_once(edge,remote_edge,&mut hint_reported){if input_status.reporter.managed{println!("{}",hint.line());}}
                        // Same short-circuit order as before; the pure checks are split out only to tally why a request is refused.
                        let argument_ok=along.is_finite() && (0.0..=1.0).contains(&along);
                        let idle=pending.is_none() && outgoing.is_none() && capture.as_ref().is_none_or(|cap|cap.gate.idle());
                        let authorized=allow && local.motion();
                        let route_allowed = !argument_ok || !idle || !authorized
                            || secondary.is_none_or(|routes| routes.accepts_incoming(&c, lease, edge, remote_edge, along));
                        let authorized = authorized && route_allowed;
                        let admitted_at = if secondary.is_some() { now_ms() } else { now };
                        let mut granted=argument_ok && idle && recovery.receiver.acquire(lease,admitted_at,authorized);
                        if !granted {
                            count(if !argument_ok {Count::DenyArgument} else if !idle {Count::DenyBusy} else if !authorized {Count::DenyAuthorization} else {Count::DenyLease});
                        }
                        if granted {
                            let begin=recovery.begin_receive().await;
                            if *stop.borrow() && recovery.outcome()!=ReceiveAck::RecoveryRequired{return Ok(());}
                            granted=input_status.checked(begin)?;
                            if !granted{count(Count::DenyBegin);input_status.release(&mut recovery).await?;}
                        }
                        if granted {
                            edge_gate.require_interior();
                            position=entry_position(remote_edge.opposite(),along);
                            let placed=recovery.pointer(position.0,position.1).await;
                            if *stop.borrow() && recovery.outcome()!=ReceiveAck::RecoveryRequired{return Ok(());}
                            granted=input_status.checked(placed)?;
                            if !granted {count(Count::DenyPlacement);input_status.release(&mut recovery).await?;}
                        }
                        if granted {
                            input_status.reporter.switch(super::WorkerSwitchState::Idle);
                            send(&mut tx,&Message::Granted{lease}).await?;
                        }else{send(&mut tx,&Message::Denied{lease}).await?;}
                        if !*stop.borrow() {
                            if let (Some(scope), Some(route), Some(config)) = (&drop_scope, &drop_route, file_drop) {
                                if let Some(event) = scope.resolve(route, &c, config.pin, lease, granted) {
                                    input_status.reporter.file_drop(event);
                                }
                            }
                        }
                    },
                    Message::Granted{lease}=>{
                        let first_grant=pending.is_some_and(|request|request.lease==lease && request.commit.is_none() && now<request.deadline);
                        if let (Some(request), Some((file, number)), Some(scope)) =
                            (pending, file_lease, &drop_scope)
                        {
                            if first_grant && file == lease {
                                let budget = Duration::from_millis(request.deadline.saturating_sub(now));
                                let confirmed = if let Some(proof) = file_confirmation.take() {
                                    let mut updates = drop_requests.clone();
                                    tokio::select! {
                                        biased;
                                        _ = super::stop_requested(&mut stop) => false,
                                        _ = updates.changed() => false,
                                        result = tokio::time::timeout(budget, Box::pin(proof.confirm())) =>
                                            matches!(result, Ok(Ok(()))),
                                    }
                                } else {
                                    false
                                };
                                if !confirmed || !scope.current(number) {
                                    pending = None;
                                    edge_gate.require_interior();
                                    input_status.reporter.switch(super::WorkerSwitchState::Denied);
                                    send(&mut tx, &Message::Release { lease, cursor: None }).await?;
                                    continue;
                                }
                            }
                        }
                        if !pending.as_mut().is_some_and(|request|request.grant(lease,now_ms())) {
                            if pending.is_some_and(|request|request.lease==lease){pending=None;edge_gate.require_interior();input_status.reporter.switch(super::WorkerSwitchState::TimedOut);}
                            // A duplicate Grant for the already active lease is
                            // inert; a late/foreign Grant is explicitly released.
                            if !outgoing.is_some_and(|current|current.lease==lease){send(&mut tx,&Message::Release { lease, cursor: None }).await?;}
                        }else if first_grant{
                            count(Count::Granted);
                            eprintln!("Peer grant received; checking local capture admission.");
                        }
                    },
                    Message::Denied{lease}=>{if pending.is_some_and(|request|request.lease==lease){count(Count::DeniedReceived);pending=None;edge_gate.require_interior();input_status.reporter.switch(super::WorkerSwitchState::Denied);}},
                    Message::Input{lease,event,cursor}=>{
                        count(Count::ReliableReceived);
                        if recovery.receiver.accepts(lease,now) && !local.supports(event) { bail!("peer sent input outside advertised capabilities"); }
                        let mut target_down = false;
                        let cursor = if recovery.receiver.accepts(lease, now) {
                            match drop_scope.as_ref().map_or(TargetAction::Ordinary, |scope| scope.target_input(lease, event, cursor)) {
                                TargetAction::Ordinary => cursor,
                                TargetAction::AtEntry(entry) => {
                                    target_down = true;
                                    entry
                                }
                                TargetAction::Drop(event) => {
                                    input_status.reporter.file_drop(event);
                                    continue;
                                }
                                TargetAction::Abort => {
                                    edge_gate.require_interior();
                                    if let Some(event) = drop_scope.as_ref().and_then(|scope| scope.end_target(lease, false)) {
                                        input_status.reporter.file_drop(event);
                                    }
                                    input_status.release(&mut recovery).await?;
                                    send(&mut tx, &Message::Release { lease, cursor: None }).await?;
                                    continue;
                                }
                            }
                        } else {
                            cursor
                        };
                        let result=recovery.input(lease,event,cursor,now).await;
                        if target_down {
                            if let Some(event) = drop_scope.as_ref().and_then(|scope| {
                                if matches!(&result, Ok(true)) && !*stop.borrow() {
                                    scope.target_down(lease)
                                } else {
                                    scope.end_target(lease, false)
                                }
                            }) {
                                input_status.reporter.file_drop(event);
                            }
                        }
                        match &result {
                            Ok(true)=>{
                                count(Count::ReliableSubmitted);
                                if matches!(event,Input::Scroll{..}){count(Count::ScrollApplied);}
                            },
                            Err(_)=>count(Count::ReliableFailed),
                            Ok(false)=>{},
                        }
                        if *stop.borrow() && recovery.outcome()!=ReceiveAck::RecoveryRequired{return Ok(());}
                        input_status.checked(result)?;
                    },
                    Message::Release { lease, cursor }=>{
                        if pending.is_some_and(|request|request.lease==lease){pending=None;edge_gate.require_interior();input_status.reporter.switch(super::WorkerSwitchState::Idle);}
                        if recovery.receiver.active()==Some(lease){
                            if let Some(event) = drop_scope.as_ref().and_then(|scope| scope.end_target(lease, false)) {
                                input_status.reporter.file_drop(event);
                            }
                            input_status.recovering();
                        }
                        input_status.checked(recovery.finish(lease,cursor,now).await)?;
                        if outgoing.is_some_and(|current|current.lease==lease){count(Count::PeerRelease);outgoing=None;input_status.restore(capture.as_ref())?;}
                    },
                    Message::Ping{lease}=>{if lease==0 || recovery.heartbeat(lease,now){send(&mut tx,&Message::Pong{lease}).await?;}},
                    Message::Pong{lease}=>{if outgoing.is_some_and(|current|current.lease==lease){count(Count::PongReceived);last_pong=now;if let Some(cap)=&capture{cap.gate.refresh();}}},
                    Message::Hello{..}=>bail!("unexpected repeated handshake"),
                }
            },
            d=c.read_datagram()=>{
                let d=match d {Ok(d)=>d,Err(_) if normal_close(&c)=>return Ok(()),Err(e)=>return Err(e.into())};
                if let Some(m)=read_motion_batch(&c,&mut recovery.receiver,&d,&count).await {
                    let point = match &drop_scope {
                        Some(scope) => scope.target_motion(m),
                        None => Some(m),
                    };
                    let result=if let Some(point) = point.filter(|point| recovery.receiver.accepts(point.lease,now_ms())) {
                        recovery.pointer(point.x,point.y).await
                    } else {
                        Ok(false)
                    };
                    count(match &result {
                        Ok(true)=>Count::PointerAccepted,
                        Ok(false)=>Count::PointerRetired,
                        Err(_)=>Count::PointerFailed,
                    });
                    if *stop.borrow() && recovery.outcome()!=ReceiveAck::RecoveryRequired{return Ok(());}
                    input_status.checked(result)?;
                }
            },
            e=input.recv(),if native=>{
                let Some(e)=e else{
                    if *stop.borrow() && recovery.outcome()!=ReceiveAck::RecoveryRequired{return Ok(());}
                    bail!("native capture channel closed");
                };
                let motion=matches!(e,Captured::Motion{..});
                if motion {count(Count::Dequeued);}
                if let Some(route)=captured_route(e){if !capture.as_ref().is_some_and(|cap|cap.gate.is_current(route)){
                    if motion {count(Count::StaleRoute);}
                    continue;
                }}
                if let Some(lease)=local_takeover(e,recovery.receiver.active().zip(recovery.receive_epoch),&mut edge_gate) {
                    if let Some(event) = drop_scope.as_ref().and_then(|scope| scope.end_target(lease, false)) {
                        input_status.reporter.file_drop(event);
                    }
                    input_status.release(&mut recovery).await?;
                    input_status.reporter.switch(super::WorkerSwitchState::Idle);

                    send(&mut tx,&Message::Release { lease, cursor: None }).await?;
                    // This event only reclaims this computer. It cannot also
                    // request control of the peer in the same select iteration.
                    continue;
                }
                match e {
                    Captured::Emergency|Captured::UnsupportedInput=>{
                        edge_gate.require_interior();
                        if matches!(e,Captured::UnsupportedInput) {eprintln!("Unsupported input capability: stopping control; input was not forwarded.");}
                        input_status.restore(capture.as_ref())?;
                        if let Some(request)=pending.take(){send(&mut tx,&Message::Release { lease:request.lease, cursor: None }).await?;}
                        input_status.reporter.switch(super::WorkerSwitchState::Idle); if let Some(Outgoing{lease,..})=outgoing.take(){send(&mut tx,&Message::Release { lease, cursor: None }).await?;}
                        if let Some(lease)=recovery.receiver.active(){
                            if let Some(event) = drop_scope.as_ref().and_then(|scope| scope.end_target(lease, false)) {
                                input_status.reporter.file_drop(event);
                            }
                            input_status.release(&mut recovery).await?;
                            send(&mut tx,&Message::Release { lease, cursor: None }).await?;
                        }
                    },
                    Captured::Position{x,y,width,height,route,primary}=>{
                        if !matches!(route,CaptureRoute::Local(_)){continue;}
                        if !width.is_finite() || !height.is_finite() || width<1.0 || height<1.0 {continue;}
                        size=(width,height);
                        let entry=primary_edge_entry(&mut edge_gate,edge,(x,y),size,now_ms(),topology.as_ref());
                        let cross=entry.is_some();
                        if cross {count(Count::EdgeHit);}
                        // Same order as the original condition chain; only the first blocker is tallied.
                        let gate_idle=capture.as_ref().is_some_and(|cap|cap.gate.idle());
                        let source_hold = match &drop_route {
                            Some(super::file_drop_control::Route::Source(source))
                                if primary == Some(source.hold) && peer.supports(Input::Button { button: 1, down: true }) =>
                            {
                                Some(source.hold)
                            }
                            _ => None,
                        };
                        let blocker=if !cross {None}
                            else if !peer.motion() {Some(Count::BlockPeer)}
                            else if !gate_idle && source_hold.is_none() {Some(Count::BlockGate)}
                            else if outgoing.is_some() || pending.is_some() {Some(Count::BlockBusy)}
                            else if recovery.receiver.active().is_some() {Some(Count::BlockReceiving)}
                            else if recovery.protection_pending() {Some(Count::BlockProtection)}
                            else {None};
                        if let Some(kind)=blocker {count(kind);}
                        if let Some(along)=entry.filter(|_|blocker.is_none()){
                            let CaptureRoute::Local(epoch)=route else{continue;};
                            let Some(cap)=&capture else{continue;};
                            let token = match source_hold {
                                Some(hold) => cap.gate.prepare_file_drop(epoch, hold),
                                None => cap.gate.prepare_commit(epoch),
                            };
                            let Some(token) = token else { continue; };
                            next_lease=next_lease.checked_add(1).ok_or_else(||anyhow::anyhow!("lease counter exhausted"))?;
                            let acquire=PendingAcquire::new(next_lease,token,now_ms());
                            let wire_edge=departure.wire_edge();
                            if let Some(confirmation) = secondary_confirmation(secondary, &c, &mut stop, &cap.gate, acquire, wire_edge, along) {
                                if !confirmation.await {
                                    edge_gate.require_interior();
                                    input_status.reporter.switch(super::WorkerSwitchState::Denied);
                                    continue;
                                }
                            }
                            if source_hold.is_some() {
                                let (Some(scope), Some(config), Some(super::file_drop_control::Route::Source(source))) =
                                    (&drop_scope, file_drop, drop_route.take()) else { continue; };
                                let request = source.request;
                                let budget = Duration::from_millis(acquire.deadline.saturating_sub(now_ms()).min(450));
                                let mut updates = drop_requests.clone();
                                if scope.current(request) && cap.gate.commit_pending(token) {
                                    file_confirmation = tokio::select! {
                                        biased;
                                        _ = super::stop_requested(&mut stop) => None,
                                        _ = updates.changed() => None,
                                        result = tokio::time::timeout(budget, Box::pin(source.reserve(config, &c, next_lease))) =>
                                            result.ok().and_then(Result::ok).flatten(),
                                    };
                                }
                                if file_confirmation.is_none() || !scope.current(request) || !cap.gate.commit_pending(token) || now_ms() >= acquire.deadline {
                                    file_confirmation = None;
                                    if scope.current(request) {
                                        input_status.reporter.file_drop(scope.event(request, FileDropState::Rejected));
                                    }
                                    scope.retire(request);
                                    edge_gate.require_interior();
                                    continue;
                                }
                                file_lease = Some((next_lease, request));
                            }
                            (position,local_return)=departure.positions(along);
                            pending=Some(acquire);input_status.reporter.switch(super::WorkerSwitchState::Pending);count(Count::AcquireSent);send(&mut tx,&Message::Acquire{lease:next_lease,edge:wire_edge,along}).await?;
                        }
                    },
                    Captured::Toggle{route}=>{
                        if let Some(Outgoing{lease,..})=outgoing.take(){input_status.restore(capture.as_ref())?;send(&mut tx,&Message::Release { lease, cursor: Some(Motion {lease,sequence,x:position.0,y:position.1}) }).await?;}
                        else if let Some(request)=pending.take(){edge_gate.require_interior();input_status.reporter.switch(super::WorkerSwitchState::Idle);send(&mut tx,&Message::Release{lease:request.lease,cursor:None}).await?;}
                        else if let CaptureRoute::Local(epoch)=route {if peer.motion() && recovery.receiver.active().is_none() && !recovery.protection_pending(){
                            let Some(cap)=&capture else{continue;};
                            let Some(token)=cap.gate.prepare_commit(epoch) else{continue;};
                            next_lease=next_lease.checked_add(1).ok_or_else(||anyhow::anyhow!("lease counter exhausted"))?;
                            let acquire=PendingAcquire::new(next_lease,token,now_ms());
                            let wire_edge=departure.wire_edge();
                            if let Some(confirmation) = secondary_confirmation(secondary, &c, &mut stop, &cap.gate, acquire, wire_edge, 0.5) {
                                if !confirmation.await {
                                    edge_gate.require_interior();
                                    input_status.reporter.switch(super::WorkerSwitchState::Denied);
                                    continue;
                                }
                            }
                            (position,local_return)=departure.positions(0.5);pending=Some(acquire);input_status.reporter.switch(super::WorkerSwitchState::Pending);count(Count::AcquireSent);send(&mut tx,&Message::Acquire{lease:next_lease,edge:wire_edge,along:0.5}).await?;
                        }}
                    },
                    Captured::Motion{dx,dy,route}=>{if let Some(lease)=capture.as_ref().and_then(|cap|forwarding_lease(route,outgoing,&cap.gate)) {
                        // Sign tallies only; displacement values are never recorded.
                        if dx>0.0 {count(Count::MovePositive);} else if dx<0.0 {count(Count::MoveNegative);}
                        let (dx,dy)=departure.motion(dx/size.0,dy/size.1);
                        position.0=(position.0+dx).clamp(0.0,1.0);position.1=(position.1+dy).clamp(0.0,1.0);
                        sequence=sequence.checked_add(1).ok_or_else(||anyhow::anyhow!("input sequence exhausted"))?;let m=Motion{lease,sequence,x:position.0,y:position.1};
                        if file_lease.is_none() && departure.crossed_back(position) && input_status.restore_at_boundary(capture.as_ref(),route)? {
                            count(Count::EdgeReturn);
                            // Same reliable stream as earlier discrete input: motion cannot
                            // overtake those events and revoke their lease at the receiver.
                            outgoing=None;
                            motion_outbox.latest=None;
                            send(&mut tx,&Message::Release { lease, cursor: Some(m) }).await?;
                        } else if motion_outbox.latest.replace(m).is_some() {
                            count(Count::MotionCoalesced);
                        }
                    }else{count(Count::NoLease);}},
                    Captured::Input{event,route}=>{if let Some(lease)=capture.as_ref().and_then(|cap|forwarding_lease(route,outgoing,&cap.gate)){
                        if is_zero_scroll(event){count(Count::ScrollZero);continue;}
                        if !peer.supports(event) { count(Count::CapabilityStop);input_status.restore(capture.as_ref())?; outgoing=None; send(&mut tx,&Message::Release{lease,cursor:None}).await?; continue; }
                        sequence=sequence.checked_add(1).ok_or_else(||anyhow::anyhow!("input sequence exhausted"))?;let cursor=Motion{lease,sequence,x:position.0,y:position.1};
                        // The reliable cursor supersedes the pending datagram.
                        motion_outbox.latest=None;
                        let result=send(&mut tx,&Message::Input{lease,event,cursor}).await;
                        count(if result.is_ok(){Count::ReliableSent}else{Count::ReliableFailed});
                        if let (Ok(()),Input::Scroll{..})=(&result,event){count(Count::ScrollSent);}
                        result?;
                        if matches!(event, Input::Button { button: 1, down: false }) {
                            if let (Some((file, request)), Some(scope)) = (file_lease, &drop_scope) {
                                if file == lease {
                                    if scope.current(request) {
                                        input_status.reporter.file_drop(FileDropEvent::new(
                                            scope.session, request, FileDropState::SourceReleased, Some(lease),
                                        ));
                                    }
                                    scope.retire(request);
                                    file_lease = None;
                                }
                            }
                        }
                    }},
                }
            }
        }
    }    }.await;
    if let (Some(scope), Some(lease)) = (&drop_scope, recovery.receiver.active()) {
        if let Some(event) = scope.end_target(lease, false) {
            input_status.reporter.file_drop(event);
        }
    }
    drop(drop_route);
    drop(drop_scope);
    // Always finish local release and capture shutdown before returning to the
    // supervisor, including managed stop and network errors. Both are attempted
    // even when one fails; no canceled future bypasses this tail.
    input_status.reporter.clear_switch();
    input_status.recovering();
    let receiving = recovery.receiver.active();
    let sending = outgoing.take().map(|current| current.lease);
    let acquiring = pending.take().map(|request| request.lease);
    let cleanup = input_status.checked(recovery.release().await);
    let stop_started = std::time::Instant::now();
    let first_shutdown = capture.as_mut().map(|capture| {
        let result = capture.shutdown_poll();
        (result, capture.shutdown_pending())
    });
    // Notify a still-connected source/receiver after stopping local routing. A
    // queued Release is not an acknowledgement of the peer's native effects.
    let mut notification = Ok(());
    for lease in [receiving, sending, acquiring].into_iter().flatten() {
        if let Err(error) = send(
            &mut tx,
            &Message::Release {
                lease,
                cursor: None,
            },
        )
        .await
        {
            if native && notification.is_ok() {
                notification = Err(error);
            }
        }
    }
    let shutdown = if let (Some(capture), Some(first)) = (capture.as_mut(), first_shutdown) {
        drain_capture(
            first,
            || {
                let result = capture.shutdown_poll();
                (result, capture.shutdown_pending())
            },
            &mut input_status,
            stop_started,
            Duration::from_millis(500),
        )
        .await
    } else {
        Ok(())
    };
    let shutdown = input_status.checked(shutdown);
    let clean = cleanup.is_ok() && shutdown.is_ok();
    input_status.finish(clean);
    cleanup?;
    shutdown?;
    // A failed remote notification must not turn an unavailable local
    // snapshot into a generic network retry or bypass a fresh-layout check.
    if result
        .as_ref()
        .is_err_and(|error| error.is::<DisplayLayoutEnd>())
    {
        return result;
    }
    notification?;
    result
}

fn captured_route(event: Captured) -> Option<CaptureRoute> {
    match event {
        Captured::Position { route, .. }
        | Captured::Motion { route, .. }
        | Captured::Input { route, .. }
        | Captured::Toggle { route } => Some(route),
        Captured::Emergency | Captured::UnsupportedInput => None,
    }
}
#[cfg(test)]
fn retired_receiving_lease(
    receiving: Option<(u64, CaptureEpoch)>,
    current: Option<CaptureRoute>,
) -> Option<u64> {
    let (lease, epoch) = receiving?;
    (current != Some(CaptureRoute::Local(epoch))).then_some(lease)
}
fn forwarding_lease(
    route: CaptureRoute,
    outgoing: Option<Outgoing>,
    gate: &platform::CaptureGate,
) -> Option<u64> {
    let current = outgoing?;
    (route == CaptureRoute::Remote(current.epoch) && gate.is_current(route))
        .then_some(current.lease)
}
fn local_takeover(
    event: Captured,
    receiving: Option<(u64, CaptureEpoch)>,
    edge_gate: &mut EdgeGate,
) -> Option<u64> {
    if !matches!(
        event,
        Captured::Position { .. } | Captured::Input { .. } | Captured::Toggle { .. }
    ) {
        return None;
    }
    let (lease, epoch) = receiving?;
    if captured_route(event) != Some(CaptureRoute::Local(epoch)) {
        return None;
    }
    edge_gate.require_interior();
    Some(lease)
}
/// A scroll that rounded to nothing was already swallowed locally; sending it is waste.
fn is_zero_scroll(event: Input) -> bool {
    matches!(event, Input::Scroll { x: 0, y: 0 })
}
/// Reports each new (local, peer) observation once: every mismatch, and a consistent
/// pair only when it follows a reported mismatch, so the GUI can withdraw a stale offer.
fn layout_hint_once(
    local: Edge,
    peer: Edge,
    reported: &mut Option<(Edge, Edge)>,
) -> Option<LayoutHint> {
    let previous = reported.replace((local, peer));
    if previous == Some((local, peer)) {
        return None;
    }
    let hint = LayoutHint::observe(local, peer);
    let withdraws = previous.is_some_and(|(l, p)| p != l.opposite());
    (hint.mismatched() || withdraws).then_some(hint)
}
// Bound the work before returning to the select loop for reliable input and stop.
const MOTION_RECEIVE_BATCH: usize = 32;

async fn read_motion_batch(
    connection: &quinn::Connection,
    receiver: &mut Receiver,
    first: &[u8],
    count: &impl Fn(Count),
) -> Option<Motion> {
    let mut latest = None;
    observe_motion_datagram(receiver, &mut latest, first, now_ms(), count);
    for _ in 1..MOTION_RECEIVE_BATCH {
        // Poll once and take immediately ready packets without awaiting new motion.
        let next = tokio::select! {
            biased;
            result = connection.read_datagram() => result.ok(),
            _ = std::future::ready(()) => None,
        };
        let Some(datagram) = next else { break };
        observe_motion_datagram(receiver, &mut latest, &datagram, now_ms(), count);
    }
    latest
}

fn observe_motion_datagram(
    receiver: &mut Receiver,
    latest: &mut Option<Motion>,
    datagram: &[u8],
    now: u64,
    count: &impl Fn(Count),
) {
    count(Count::Received);
    let motion = match decode::<Motion>(datagram) {
        Ok(motion) if motion.valid() => motion,
        _ => {
            count(Count::DecodeRejected);
            return;
        }
    };
    if !receiver.accepts(motion.lease, now) {
        count(Count::LeaseRejected);
    } else if !receiver.motion(motion, now) {
        count(Count::ReplayRejected);
    } else if latest.replace(motion).is_some() {
        count(Count::PointerCoalesced);
    }
}

#[cfg(any(windows, target_os = "macos"))]
fn monitor_layout(native: bool) -> Option<Layout> {
    native
        .then(platform::read_display_layout)
        .and_then(Result::ok)
}

#[cfg(not(any(windows, target_os = "macos")))]
fn monitor_layout(_: bool) -> Option<Layout> {
    None
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DisplayLayoutEnd {
    Changed,
    Unavailable,
}

impl DisplayLayoutEnd {
    pub fn may_rebuild(self, reconnect: bool, selected_display: bool) -> bool {
        self == Self::Changed && reconnect && !selected_display
    }
}

impl std::fmt::Display for DisplayLayoutEnd {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Changed => {
                "Display layout changed. Check the display selection before restarting."
            }
            Self::Unavailable => "Display layout could not be checked. Restart the session.",
        })
    }
}

impl std::error::Error for DisplayLayoutEnd {}

fn layout_change(original: Option<&Layout>, current: Option<&Layout>) -> Option<DisplayLayoutEnd> {
    match (original, current) {
        (Some(original), Some(current)) if original.same_configuration(current) => None,
        (None, None) => None,
        (Some(_), None) => Some(DisplayLayoutEnd::Unavailable),
        (_, Some(_)) => Some(DisplayLayoutEnd::Changed),
    }
}

fn session_edge_gate(rebuilding: bool) -> EdgeGate {
    let mut gate = EdgeGate::default();
    if rebuilding {
        gate.require_interior();
    }
    gate
}

async fn watch_display_layout(original: Option<&Layout>) -> Result<()> {
    let period = Duration::from_secs(1);
    loop {
        tokio::time::sleep(period).await;
        // Display enumeration can block in the OS. Keep it off the async
        // session and the native input owner, with at most one query in flight.
        let current =
            tokio::time::timeout(period, tokio::task::spawn_blocking(|| monitor_layout(true)))
                .await
                .ok()
                .and_then(Result::ok)
                .ok_or(DisplayLayoutEnd::Unavailable)?;
        if let Some(change) = layout_change(original, current.as_ref()) {
            return Err(change.into());
        }
    }
}

fn exposed_primary_edge(
    layout: Option<&Layout>,
    edge: Edge,
    (x, y): (f64, f64),
    (width, height): (f64, f64),
) -> bool {
    let Some(layout) = layout else {
        return true;
    };
    let primary = layout.primary();
    let Some(display) = layout
        .displays()
        .iter()
        .find(|display| display.id == primary)
    else {
        return false;
    };
    let bounds = display.bounds;
    if (bounds.right - bounds.left - width).abs() > f64::EPSILON
        || (bounds.bottom - bounds.top - height).abs() > f64::EPSILON
    {
        return false;
    }
    let axis = match edge {
        Edge::Left | Edge::Right => width,
        Edge::Top | Edge::Bottom => height,
    };
    layout
        .edge_at(edge, bounds.left + x, bounds.top + y, 1.0 + axis * 0.001)
        .is_some_and(|anchor| anchor.display == primary)
}

fn primary_edge_entry(
    gate: &mut EdgeGate,
    edge: Edge,
    (x, y): (f64, f64),
    (width, height): (f64, f64),
    now: u64,
    layout: Option<&Layout>,
) -> Option<f64> {
    // A secondary display can produce coordinates beyond the primary rectangle.
    // Only an observed position inside that rectangle can rearm the entry gate.
    if !(0.0..width).contains(&x) || !(0.0..height).contains(&y) {
        return None;
    }
    let nx = (x / (width - 1.0).max(1.0)).min(1.0);
    let ny = (y / (height - 1.0).max(1.0)).min(1.0);
    let along = match edge {
        Edge::Left | Edge::Right => ny,
        Edge::Top | Edge::Bottom => nx,
    };
    let at_edge = distance(edge, nx, ny) <= 0.001;
    if at_edge && !exposed_primary_edge(layout, edge, (x, y), (width, height)) {
        return None;
    }
    gate.update(at_edge, now).then_some(along)
}

#[derive(Clone, Copy)]
struct OutgoingEdges {
    local: Edge,
    remote: Edge,
}

impl OutgoingEdges {
    fn new(local: Edge, remote: Option<Edge>) -> Self {
        Self {
            local,
            remote: remote.unwrap_or_else(|| local.opposite()),
        }
    }

    /// v5 receivers place the cursor on the opposite edge of the Acquire.
    fn wire_edge(&self) -> Edge {
        self.remote.opposite()
    }

    fn positions(&self, along: f64) -> ((f64, f64), (f64, f64)) {
        (
            entry_position(self.remote, along),
            entry_position(self.local, along),
        )
    }

    fn crossed_back(&self, (x, y): (f64, f64)) -> bool {
        distance(self.remote, x, y) <= 0.001
    }

    fn motion(&self, dx: f64, dy: f64) -> (f64, f64) {
        // Progress out of the source edge becomes progress into the receiver.
        // Keep the along-edge direction consistent with both endpoint fractions.
        let (forward, along) = match self.local {
            Edge::Left => (-dx, dy),
            Edge::Right => (dx, dy),
            Edge::Top => (-dy, dx),
            Edge::Bottom => (dy, dx),
        };
        match self.remote {
            Edge::Left => (forward, along),
            Edge::Right => (-forward, along),
            Edge::Top => (along, forward),
            Edge::Bottom => (along, -forward),
        }
    }
}
fn distance(edge: Edge, x: f64, y: f64) -> f64 {
    match edge {
        Edge::Left => x,
        Edge::Right => 1.0 - x,
        Edge::Top => y,
        Edge::Bottom => 1.0 - y,
    }
}
pub(super) fn entry_position(edge: Edge, along: f64) -> (f64, f64) {
    match edge {
        Edge::Left => (0.002, along),
        Edge::Right => (0.998, along),
        Edge::Top => (along, 0.002),
        Edge::Bottom => (along, 0.998),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Context;
    use transport::Identity;

    #[test]
    fn native_display_watch_rejects_an_obsolete_snapshot() {
        use platform::display_layout::{Bounds, Display, DisplayId, Rotation};

        let primary = Display {
            id: DisplayId(1),
            bounds: Bounds {
                left: 0.0,
                top: 0.0,
                right: 100.0,
                bottom: 100.0,
            },
            scale: 1.0,
            rotation: Rotation::Upright,
        };
        let secondary = Display {
            id: DisplayId(2),
            bounds: Bounds {
                left: -100.0,
                right: 0.0,
                ..primary.bounds
            },
            ..primary
        };
        let original = Layout::new(primary.id, vec![primary, secondary]).unwrap();
        let same = Layout::new(primary.id, vec![secondary, primary]).unwrap();
        let removed = Layout::new(primary.id, vec![primary]).unwrap();
        let other_primary = Layout::new(secondary.id, vec![primary, secondary]).unwrap();
        let scaled = Display {
            scale: 2.0,
            ..secondary
        };
        let rotated = Display {
            rotation: Rotation::Clockwise90,
            ..secondary
        };
        let moved = Display {
            bounds: Bounds {
                top: 50.0,
                bottom: 150.0,
                ..secondary.bounds
            },
            ..secondary
        };
        let changed_layout = Some(DisplayLayoutEnd::Changed);
        assert_eq!(layout_change(Some(&original), Some(&same)), None);
        assert_eq!(layout_change(None, None), None);
        assert_eq!(layout_change(None, Some(&same)), changed_layout);
        assert_eq!(
            layout_change(Some(&original), None),
            Some(DisplayLayoutEnd::Unavailable)
        );
        for changed in [removed, other_primary] {
            assert_eq!(
                layout_change(Some(&original), Some(&changed)),
                changed_layout
            );
        }
        for display in [scaled, rotated, moved] {
            let changed = Layout::new(primary.id, vec![primary, display]).unwrap();
            assert_eq!(
                layout_change(Some(&original), Some(&changed)),
                changed_layout
            );
        }
    }

    #[test]
    fn display_rebuild_requires_opt_in_current_geometry_and_a_fresh_edge() {
        let changed = DisplayLayoutEnd::Changed;
        assert!(changed.may_rebuild(true, false));
        assert!(!changed.may_rebuild(false, false));
        assert!(!changed.may_rebuild(true, true));
        assert!(!DisplayLayoutEnd::Unavailable.may_rebuild(true, false));
        let mut rebuilt = session_edge_gate(true);
        assert!(!rebuilt.update(true, 1000));
        assert!(!rebuilt.update(false, 1001));
        assert!(rebuilt.update(true, 1002));
        assert!(session_edge_gate(false).update(true, 1000));
    }

    fn pending_request(lease: u64, started: u64) -> (PendingAcquire, CaptureEpoch) {
        let gate = platform::CaptureGate::default();
        let Some(CaptureRoute::Local(epoch)) = gate.route() else {
            panic!("expected local capture");
        };
        let token = gate.prepare_commit(epoch).unwrap();
        (PendingAcquire::new(lease, token, started), epoch)
    }
    #[test]
    fn only_zero_by_zero_scroll_is_skipped_before_sending() {
        assert!(is_zero_scroll(Input::Scroll { x: 0, y: 0 }));
        assert!(!is_zero_scroll(Input::Scroll { x: 0, y: 1 }));
        assert!(!is_zero_scroll(Input::Scroll { x: -1, y: 0 }));
        let key = Input::Key {
            usage: 4,
            down: true,
        };
        let button = Input::Button {
            button: 1,
            down: true,
        };
        assert!(!is_zero_scroll(key));
        assert!(!is_zero_scroll(button));
    }
    #[test]
    fn layout_hint_is_reported_once_per_mismatch_and_withdrawn_when_consistent() {
        let mut reported = None;
        let mirrored = layout_hint_once(Edge::Left, Edge::Right, &mut reported);
        assert!(mirrored.is_none());
        let first = layout_hint_once(Edge::Right, Edge::Right, &mut reported);
        assert_eq!(first.map(LayoutHint::suggested), Some(Edge::Left));
        let repeated = layout_hint_once(Edge::Right, Edge::Right, &mut reported);
        assert!(repeated.is_none());
        let local_changed = layout_hint_once(Edge::Top, Edge::Right, &mut reported);
        assert_eq!(local_changed.map(|hint| hint.local), Some(Edge::Top));
        let peer_changed = layout_hint_once(Edge::Top, Edge::Top, &mut reported);
        assert_eq!(peer_changed.map(|hint| hint.peer), Some(Edge::Top));
        // The peer fixed its side: one consistent record withdraws the GUI offer.
        let fixed = layout_hint_once(Edge::Top, Edge::Bottom, &mut reported);
        assert!(fixed.is_some_and(|hint| !hint.mismatched()));
        let still_fixed = layout_hint_once(Edge::Top, Edge::Bottom, &mut reported);
        assert!(still_fixed.is_none());
        let again = layout_hint_once(Edge::Top, Edge::Top, &mut reported);
        assert!(again.is_some_and(LayoutHint::mismatched));
    }
    #[test]
    fn granted_callback_retry_commits_once_with_the_original_request_and_budget() -> Result<()> {
        let (mut request, epoch) = pending_request(7, 100);
        let token = request.token;
        assert!(request.grant(7, 200));
        let mut pending = Some(request);
        let mut calls = 0;
        assert_eq!(
            advance_pending(&mut pending, 200, true, |observed, deadline| {
                calls += 1;
                assert_eq!(observed, token);
                assert_eq!(deadline, 1100);
                Ok(CaptureCommit::Retry)
            })?,
            PendingStep::Waiting
        );
        assert_eq!(calls, 1);
        assert_eq!(pending.unwrap().wake_at(), 210);
        assert_eq!(
            advance_pending(&mut pending, 209, true, |_, _| panic!("retry cannot spin"))?,
            PendingStep::Waiting
        );
        assert!(pending.as_mut().unwrap().grant(7, 209));
        assert_eq!(
            pending.unwrap().wake_at(),
            210,
            "duplicate Grant cannot accelerate attempts"
        );
        assert_eq!(
            advance_pending(&mut pending, 210, true, |observed, deadline| {
                calls += 1;
                assert_eq!(observed, token);
                assert_eq!(deadline, 1100);
                Ok(CaptureCommit::Committed(epoch))
            })?,
            PendingStep::Committed { lease: 7, epoch }
        );
        assert_eq!(calls, 2);
        assert!(pending.is_none());
        assert_eq!(
            advance_pending(&mut pending, 220, true, |_, _| {
                panic!("committed request was removed")
            })?,
            PendingStep::Waiting
        );
        Ok(())
    }
    #[test]
    fn cancelled_retry_is_removed_and_cannot_wait_for_a_later_release_of_held_input() -> Result<()>
    {
        let (mut request, _) = pending_request(9, 100);
        assert!(request.grant(9, 200));
        let mut pending = Some(request);
        assert_eq!(
            advance_pending(&mut pending, 200, true, |_, _| Ok(CaptureCommit::Retry))?,
            PendingStep::Waiting
        );
        assert_eq!(
            advance_pending(&mut pending, 210, true, |_, _| Ok(CaptureCommit::Cancelled))?,
            PendingStep::Cancelled {
                lease: 9,
                timed_out: false
            }
        );
        assert!(pending.is_none());
        assert_eq!(
            advance_pending(&mut pending, 400, true, |_, _| {
                panic!("released old grant cannot revive")
            })?,
            PendingStep::Waiting
        );
        Ok(())
    }
    #[test]
    fn late_grant_and_retry_at_the_original_deadline_never_activate() -> Result<()> {
        let (mut request, _) = pending_request(11, 100);
        assert!(!request.grant(11, 1100));
        assert!(request.commit.is_none());
        assert!(request.grant(11, 1095));
        let mut pending = Some(request);
        assert_eq!(
            advance_pending(&mut pending, 1095, true, |_, deadline| {
                assert_eq!(deadline, 1100);
                Ok(CaptureCommit::Retry)
            })?,
            PendingStep::Waiting
        );
        assert_eq!(pending.unwrap().wake_at(), 1100);
        assert_eq!(
            advance_pending(&mut pending, 1100, true, |_, _| panic!("late commit"))?,
            PendingStep::Cancelled {
                lease: 11,
                timed_out: true
            }
        );
        assert!(pending.is_none());
        Ok(())
    }
    #[test]
    fn lost_context_or_foreign_grant_cannot_commit_or_rebind_a_pending_request() -> Result<()> {
        let (mut request, _) = pending_request(13, 100);
        assert!(!request.grant(12, 200));
        assert!(request.commit.is_none());
        assert!(request.grant(13, 200));
        let mut pending = Some(request);
        // The production caller checks Stop/closed before this transition, then
        // capture epoch/request admission and receiver protection for `current`.
        assert_eq!(
            advance_pending(&mut pending, 200, false, |_, _| panic!("invalid context"))?,
            PendingStep::Cancelled {
                lease: 13,
                timed_out: false
            }
        );
        let (replacement, _) = pending_request(14, 300);
        pending = Some(replacement);
        assert!(!pending.as_mut().unwrap().grant(13, 301));
        assert!(pending.unwrap().commit.is_none());
        assert_eq!(pending.unwrap().deadline, 1300);
        Ok(())
    }
    #[test]
    fn commit_retry_attempts_are_bounded_without_sleeping_or_refreshing_deadline() -> Result<()> {
        let (mut request, _) = pending_request(15, 0);
        assert!(request.grant(15, 0));
        let mut pending = Some(request);
        let mut calls = 0;
        for attempt in 0..MAX_COMMIT_ATTEMPTS {
            let step = advance_pending(
                &mut pending,
                u64::from(attempt) * COMMIT_RETRY_MS,
                true,
                |_, deadline| {
                    calls += 1;
                    assert_eq!(deadline, 1000);
                    Ok(CaptureCommit::Retry)
                },
            )?;
            assert_eq!(step, PendingStep::Waiting);
        }
        assert_eq!(calls, MAX_COMMIT_ATTEMPTS);
        assert_eq!(pending.unwrap().wake_at(), 1000);
        assert_eq!(
            advance_pending(&mut pending, 1000, true, |_, _| {
                panic!("attempt budget is exhausted")
            })?,
            PendingStep::Cancelled {
                lease: 15,
                timed_out: true
            }
        );
        assert!(pending.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn clean_stream_fin_and_truncated_frame_take_distinct_session_cleanup_paths() -> Result<()>
    {
        for truncated in [false, true] {
            let a = Identity::generate()?;
            let b = Identity::generate()?;
            let ea = transport::endpoint("127.0.0.1:0".parse()?, &a, &b.certificate)?;
            let eb = transport::endpoint("127.0.0.1:0".parse()?, &b, &a.certificate)?;
            let (_stop, stopped) = tokio::sync::watch::channel(false);
            let server = async {
                let c = eb.accept().await.unwrap().await?;
                let (tx, rx) = c.accept_bi().await?;
                let mut reporter = super::super::InputReporter::new(false);
                let result = run_with_status(
                    c.clone(),
                    tx,
                    rx,
                    Policy {
                        allow: false,
                        native: false,
                        edge: Edge::Right,
                        local: InputCapabilities::default(),
                        peer: InputCapabilities::default(),
                    },
                    stopped,
                    &mut reporter,
                )
                .await;
                if truncated {
                    assert!(result.unwrap_err().is::<transport::InvalidMessageFrame>());
                } else {
                    result?;
                }
                assert_eq!(reporter.last, Some(WorkerInputState::Disabled));
                c.close(0u8.into(), b"session framing checked");
                Ok::<_, anyhow::Error>(())
            };
            let client = async {
                let c = ea.connect(eb.local_addr()?, "peerglide.local")?.await?;
                let (mut tx, mut rx) = c.open_bi().await?;
                send(&mut tx, &Message::Ping { lease: 0 }).await?;
                assert!(matches!(
                    transport::receive(&mut rx).await?,
                    Message::Pong { lease: 0 }
                ));
                if truncated {
                    tx.write_all(&[0, 0]).await?;
                }
                tx.finish()?;
                let _ = c.closed().await;
                Ok::<_, anyhow::Error>(())
            };
            tokio::time::timeout(Duration::from_secs(5), async {
                tokio::try_join!(server, client)
            })
            .await??;
        }
        assert!(connection_end(quinn::ConnectionError::TimedOut).is_err());
        assert!(connection_end(quinn::ConnectionError::Reset).is_err());
        Ok(())
    }

    #[tokio::test]
    async fn recovery_receipt_is_a_normal_close_on_the_authenticated_connection() -> Result<()> {
        let sender = Identity::generate()?;
        let receiver = Identity::generate()?;
        let bind = "127.0.0.1:0".parse()?;
        let source = transport::endpoint(bind, &sender, &receiver.certificate)?;
        let target = transport::endpoint(bind, &receiver, &sender.certificate)?;
        let address = target.local_addr()?;
        let generic = b"session ended";
        for (state, clean) in [
            (WorkerInputState::Local, true),
            (WorkerInputState::Local, false),
            (WorkerInputState::RecoveryUnconfirmed, true),
            (WorkerInputState::Disabled, true),
        ] {
            let receive = async {
                let c = target.accept().await.unwrap().await?;
                transport::verify_peer(&c, &sender.certificate)?;
                let (_, mut rx) = c.accept_bi().await?;
                assert!(matches!(
                    transport::receive(&mut rx).await?,
                    Message::Ping { lease: 0 }
                ));
                let result = if clean {
                    Ok(())
                } else {
                    Err(anyhow::anyhow!("session failed"))
                };
                let reason = close_reason_after_recovery(&result, Some(state), generic);
                c.close(0u8.into(), reason);
                Ok::<_, anyhow::Error>(())
            };
            let send_probe = async {
                let c = source.connect(address, "peerglide.local")?.await?;
                transport::verify_peer(&c, &receiver.certificate)?;
                let (mut tx, _rx) = c.open_bi().await?;
                send(&mut tx, &Message::Ping { lease: 0 }).await?;
                let quinn::ConnectionError::ApplicationClosed(closed) = c.closed().await else {
                    anyhow::bail!("expected authenticated application close");
                };
                assert_eq!(closed.error_code, 0u8.into());
                let expected = if clean && state == WorkerInputState::Local {
                    NATIVE_INPUT_RECOVERED
                } else {
                    generic
                };
                assert_eq!(closed.reason.as_ref(), expected);
                Ok::<_, anyhow::Error>(())
            };
            tokio::time::timeout(Duration::from_secs(5), async {
                tokio::try_join!(receive, send_probe)
            })
            .await??;
        }
        Ok(())
    }

    #[tokio::test]
    async fn shutdown_timeout_keeps_the_worker_future_alive_until_the_original_capture_drains() {
        use std::sync::atomic::{AtomicBool, Ordering};
        for initial_failure in [false, true] {
            let drained = AtomicBool::new(false);
            let mut reporter = super::super::InputReporter::new(false);
            {
                let mut status = InputStatus::new(true, &mut reporter);
                let first = if initial_failure {
                    Err(anyhow::anyhow!("fake pending restore failure"))
                } else {
                    Ok(false)
                };
                let future = drain_capture(
                    (first, true),
                    || {
                        let done = drained.load(Ordering::Acquire);
                        (Ok(done), !done)
                    },
                    &mut status,
                    std::time::Instant::now(),
                    Duration::ZERO,
                );
                tokio::pin!(future);
                tokio::select! {
                    _ = &mut future => panic!("pending capture must not let the worker exit"),
                    _ = tokio::time::sleep(Duration::from_millis(10)) => (),
                }
                drained.store(true, Ordering::Release);
                assert!(future.await.is_err());
            }
            assert_eq!(reporter.last, Some(WorkerInputState::RecoveryUnconfirmed));
        }
    }
    #[tokio::test]
    async fn already_reaped_source_failure_does_not_wait_forever() {
        let mut reporter = super::super::InputReporter::new(false);
        let mut status = InputStatus::new(true, &mut reporter);
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            drain_capture(
                (Err(anyhow::anyhow!("fake source lost after reap")), false),
                || panic!("already reaped capture must not be polled again"),
                &mut status,
                std::time::Instant::now(),
                Duration::ZERO,
            ),
        )
        .await
        .unwrap();
        assert!(result.is_err());
        assert_eq!(reporter.last, Some(WorkerInputState::RecoveryUnconfirmed));
    }

    #[test]
    fn diagnostic_mode_never_claims_native_ownership_even_for_simulated_leases() {
        let mut reporter = super::super::InputReporter::new(false);
        {
            let mut status = InputStatus::new(false, &mut reporter);
            status.routing(false, false);
            status.routing(true, false);
            status.routing(false, true);
            status.recovering();
            assert!(status
                .checked::<()>(Err(anyhow::anyhow!("simulated failure")))
                .is_err());
            status.finish(false);
        }
        assert_eq!(reporter.observed, [WorkerInputState::Disabled]);
    }

    #[tokio::test]
    async fn real_receiver_ledger_transitions_are_separate_from_connection_and_cleanup_failure_is_sticky(
    ) {
        // Reporting is exercised against a real Receiver ledger and fake native
        // effects. No hook, OS injection, permissions or current input are used.
        let mut recovery = held_recovery();
        let mut reporter = super::super::InputReporter::new(false);
        {
            let mut status = InputStatus::new(true, &mut reporter);
            status.routing(false, recovery.receiver.active().is_some());
            status.release(&mut recovery).await.unwrap();
            status.routing(false, recovery.receiver.active().is_some());
            status.routing(true, false);
            status.recovering();
            assert!(status
                .checked::<()>(Err(anyhow::anyhow!("restore rejected")))
                .is_err());
            status.routing(false, false);
            status.finish(true);
        }
        assert_eq!(
            reporter.observed,
            [
                WorkerInputState::ReceivingRemote,
                WorkerInputState::Recovering,
                WorkerInputState::Local,
                WorkerInputState::ControllingRemote,
                WorkerInputState::Recovering,
                WorkerInputState::RecoveryUnconfirmed,
            ]
        );
    }

    #[test]
    fn paced_motion_keeps_the_latest_pending_position_until_due() {
        let start = tokio::time::Instant::now();
        let mut outbox = MotionOutbox::new(start);
        let first = Motion {
            lease: 1,
            sequence: 1,
            x: 0.25,
            y: 0.5,
        };
        outbox.latest = Some(first);
        assert_eq!(
            outbox.take_due(start).map(|motion| motion.sequence),
            Some(1)
        );
        outbox.latest = Some(Motion {
            sequence: 2,
            x: 0.5,
            ..first
        });
        assert!(outbox.take_due(start + Duration::from_millis(1)).is_none());
        outbox.latest = Some(Motion {
            sequence: 3,
            x: 0.75,
            ..first
        });
        let due = start + MOTION_SEND_INTERVAL;
        let latest = outbox.take_due(due).unwrap();
        assert_eq!((latest.sequence, latest.x), (3, 0.75));
        assert!(outbox.take_due(due + MOTION_SEND_INTERVAL).is_none());
    }

    #[test]
    fn receive_batch_keeps_the_highest_valid_sequence_for_the_active_lease() -> Result<()> {
        let mut receiver = Receiver::default();
        let mut latest = None;
        let diagnostics = InputDiagnostics::default();
        let count = |kind| diagnostics.increment(kind);
        assert!(receiver.acquire(7, 100, true));
        for (lease, sequence) in [(7, 1), (7, 3), (7, 2), (8, 4)] {
            let payload = encode(&Motion {
                lease,
                sequence,
                x: 0.5,
                y: 0.5,
            })?;
            observe_motion_datagram(&mut receiver, &mut latest, &payload, 101, &count);
        }
        observe_motion_datagram(&mut receiver, &mut latest, b"invalid", 101, &count);
        assert_eq!(latest.map(|motion| motion.sequence), Some(3));
        let summary = diagnostics
            .snapshot(peerglide_platform::input_diagnostics::Phase::Sample)
            .to_string();
        for field in ["rx=5", "decode=1", "lease=1", "replay=1", "rx_coalesced=1"] {
            assert!(summary.split_ascii_whitespace().any(|value| value == field));
        }
        Ok(())
    }

    #[test]
    fn primary_edge_entry_ignores_secondary_displays_without_rearming() {
        let size = (1920.0, 1080.0);
        let interior = (959.5, 539.5);
        for (edge, boundary, outside) in [
            (Edge::Left, (0.0, interior.1), (-1.0, interior.1)),
            (Edge::Right, (1919.5, interior.1), (1920.0, interior.1)),
            (Edge::Top, (interior.0, 0.0), (interior.0, -1.0)),
            (Edge::Bottom, (interior.0, 1079.5), (interior.0, 1080.0)),
        ] {
            let mut gate = EdgeGate::default();
            assert_eq!(
                primary_edge_entry(&mut gate, edge, outside, size, 500, None),
                None
            );
            assert_eq!(
                primary_edge_entry(&mut gate, edge, boundary, size, 501, None),
                Some(0.5)
            );
            assert_eq!(
                primary_edge_entry(&mut gate, edge, outside, size, 900, None),
                None
            );
            assert_eq!(
                primary_edge_entry(&mut gate, edge, boundary, size, 901, None),
                None
            );
            assert_eq!(
                primary_edge_entry(&mut gate, edge, interior, size, 902, None),
                None
            );
            assert_eq!(
                primary_edge_entry(&mut gate, edge, boundary, size, 903, None),
                Some(0.5)
            );
        }
    }

    #[test]
    fn primary_edge_entry_requires_both_coordinates_inside_the_primary_display() {
        let mut gate = EdgeGate::default();
        let size = (1920.0, 1080.0);
        for y in [-1.0, 1080.0, f64::NAN, f64::INFINITY] {
            assert_eq!(
                primary_edge_entry(&mut gate, Edge::Right, (1919.0, y), size, 500, None),
                None
            );
        }
        assert_eq!(
            primary_edge_entry(&mut gate, Edge::Right, (1919.0, 539.5), size, 501, None),
            Some(0.5)
        );
    }

    #[test]
    fn attached_screen_blocks_only_its_shared_primary_edge() {
        use platform::display_layout::{Bounds, Display, DisplayId, Rotation};
        let primary = Display {
            id: DisplayId(1),
            bounds: Bounds {
                left: 0.0,
                top: 0.0,
                right: 1920.0,
                bottom: 1080.0,
            },
            scale: 1.0,
            rotation: Rotation::Upright,
        };
        let right = Display {
            id: DisplayId(2),
            bounds: Bounds {
                left: 1920.0,
                top: 0.0,
                right: 2920.0,
                bottom: 600.0,
            },
            ..primary
        };
        let layout = Layout::new(primary.id, vec![primary, right]).unwrap();
        let mut gate = EdgeGate::default();
        let size = (1920.0, 1080.0);
        assert!(primary_edge_entry(
            &mut gate,
            Edge::Right,
            (1919.0, 200.0),
            size,
            500,
            Some(&layout)
        )
        .is_none());
        assert!(primary_edge_entry(
            &mut gate,
            Edge::Right,
            (1919.0, 700.0),
            size,
            501,
            Some(&layout)
        )
        .is_some());
        let mut rebuilding = session_edge_gate(true);
        for (at, now, crossed) in [
            ((1919.0, 200.0), 600, false),
            ((1919.0, 700.0), 601, false),
            ((1800.0, 700.0), 602, false),
            ((1919.0, 700.0), 603, true),
        ] {
            assert_eq!(
                primary_edge_entry(&mut rebuilding, Edge::Right, at, size, now, Some(&layout))
                    .is_some(),
                crossed
            );
        }
        let mut gate = EdgeGate::default();
        assert!(primary_edge_entry(
            &mut gate,
            Edge::Right,
            (1918.0, 700.0),
            (1919.0, 1080.0),
            600,
            Some(&layout)
        )
        .is_none());
    }

    #[test]
    fn source_return_boundary_handles_all_edges_without_minimum_travel() {
        for edge in [Edge::Left, Edge::Right, Edge::Top, Edge::Bottom] {
            let departure = OutgoingEdges::new(edge, None);
            let (entry, local_return) = departure.positions(0.5);
            assert_eq!(departure.wire_edge(), edge);
            assert_eq!(entry, entry_position(edge.opposite(), 0.5));
            assert_eq!(local_return, entry_position(edge, 0.5));
            assert_eq!(departure.motion(0.1, -0.25), (0.1, -0.25));
            assert!(!departure.crossed_back(entry));
            assert!(!departure.crossed_back((0.5, 0.5)));
            let boundary = match edge {
                Edge::Left => (1.0, 0.5),
                Edge::Right => (0.0, 0.5),
                Edge::Top => (0.5, 1.0),
                Edge::Bottom => (0.5, 0.0),
            };
            assert!(departure.crossed_back(boundary));
        }
    }

    #[test]
    fn outgoing_entry_and_return_follow_the_selected_remote_edge() {
        let departure = OutgoingEdges::new(Edge::Right, Some(Edge::Top));
        let (entry, local_return) = departure.positions(0.25);
        assert_eq!(departure.wire_edge(), Edge::Bottom);
        assert_eq!(entry, (0.25, 0.002));
        assert_eq!(local_return, (0.998, 0.25));
        assert_eq!(departure.motion(0.1, 0.25), (0.25, 0.1));
        assert_eq!(departure.motion(-0.1, 0.25), (0.25, -0.1));
        assert!(departure.crossed_back((0.25, 0.0)));
        assert!(!departure.crossed_back((0.0, 0.25)));
    }
    #[test]
    fn physical_takeover_at_each_edge_cannot_immediately_reverse_control() {
        for edge in [Edge::Left, Edge::Right, Edge::Top, Edge::Bottom] {
            let mut gate = EdgeGate::default();
            let capture = platform::CaptureGate::default();
            let epoch = capture.try_local_epoch().unwrap();
            let (nx, ny) = match edge {
                Edge::Left => (0.0, 0.5),
                Edge::Right => (1.0, 0.5),
                Edge::Top => (0.5, 0.0),
                Edge::Bottom => (0.5, 1.0),
            };
            let event = Captured::Position {
                x: nx * 1919.0,
                y: ny * 1079.0,
                width: 1920.0,
                height: 1080.0,
                route: CaptureRoute::Local(epoch),
                primary: None,
            };
            assert_eq!(local_takeover(event, Some((7, epoch)), &mut gate), Some(7));
            assert!(!gate.update(distance(edge, nx, ny) <= 0.001, 1000));
            assert!(!gate.update(false, 1001));
            assert!(gate.update(true, 1002));
            assert_eq!(local_takeover(event, None, &mut gate), None);
        }
    }
    #[test]
    fn queued_local_and_old_remote_routes_cannot_enter_a_new_outgoing_lease() {
        let gate = platform::CaptureGate::default();
        let local = gate.route().unwrap();
        assert!(gate.try_activate());
        let first = gate.route().unwrap();
        let CaptureRoute::Remote(epoch) = first else {
            panic!("expected remote route")
        };
        let outgoing = Some(Outgoing { lease: 1, epoch });
        assert_eq!(forwarding_lease(local, outgoing, &gate), None);
        assert_eq!(forwarding_lease(first, outgoing, &gate), Some(1));
        gate.disable();
        assert_eq!(forwarding_lease(first, outgoing, &gate), None);
        assert!(gate.try_activate());
        let second = gate.route().unwrap();
        let CaptureRoute::Remote(epoch) = second else {
            panic!("expected remote route")
        };
        let outgoing = Some(Outgoing { lease: 2, epoch });
        assert_eq!(forwarding_lease(first, outgoing, &gate), None);
        assert_eq!(forwarding_lease(local, outgoing, &gate), None);
        assert_eq!(forwarding_lease(second, outgoing, &gate), Some(2));
    }
    #[test]
    fn local_events_queued_before_an_incoming_fence_do_not_reclaim_it() {
        let capture = platform::CaptureGate::default();
        let old_route = capture.route().unwrap();
        let epoch = capture.try_local_epoch().unwrap();
        let mut gate = EdgeGate::default();
        let event = Captured::Input {
            event: Input::Key {
                usage: 4,
                down: false,
            },
            route: old_route,
        };
        assert_eq!(local_takeover(event, Some((7, epoch)), &mut gate), None);
        let event = Captured::Input {
            event: Input::Key {
                usage: 4,
                down: true,
            },
            route: CaptureRoute::Local(epoch),
        };
        assert_eq!(local_takeover(event, Some((7, epoch)), &mut gate), Some(7));
    }
    #[tokio::test]
    async fn retired_or_lost_capture_fence_releases_the_receiver_and_ping_cannot_revive_it(
    ) -> Result<()> {
        for lost in [false, true] {
            let mut recovery = held_recovery();
            let capture = platform::CaptureGate::default();
            let epoch = capture.try_local_epoch().unwrap();
            assert_eq!(
                retired_receiving_lease(Some((1, epoch)), capture.route()),
                None
            );
            capture.disable();
            let route = if lost { None } else { capture.route() };
            let lease = retired_receiving_lease(recovery.receiver.active().zip(Some(epoch)), route);
            assert_eq!(lease, Some(1));
            let mut reporter = super::super::InputReporter::new(false);
            let mut status = InputStatus::new(false, &mut reporter);
            status.release(&mut recovery).await?;
            assert!(recovery.receiver.active().is_none());
            assert!(!recovery.receiver.heartbeat(1, 4));
            assert!(recovery.receiver.release().is_empty());
        }
        assert_eq!(retired_receiving_lease(None, None), None);
        Ok(())
    }
    #[test]
    fn retired_fence_release_failure_stays_unconfirmed_and_retains_the_owned_up() -> Result<()> {
        let mut recovery = held_recovery();
        let capture = platform::CaptureGate::default();
        let epoch = capture.try_local_epoch().unwrap();
        capture.disable();
        assert_eq!(
            retired_receiving_lease(Some((1, epoch)), capture.route()),
            Some(1)
        );
        let mut reporter = super::super::InputReporter::new(false);
        {
            let mut status = InputStatus::new(true, &mut reporter);
            status.recovering();
            assert!(status
                .checked(
                    recovery.release_with(|_| anyhow::bail!("fake retired-fence release rejected"))
                )
                .is_err());
            assert!(!recovery.receiver.heartbeat(1, 3));
            status.finish(true);
        }
        assert_eq!(reporter.last, Some(WorkerInputState::RecoveryUnconfirmed));
        let mut released = Vec::new();
        recovery
            .pending_releases
            .flush(|event| {
                released.push(event);
                Ok::<_, ()>(())
            })
            .unwrap();
        assert!(matches!(
            released.as_slice(),
            [Input::Key {
                usage: 0xe0,
                down: false
            }]
        ));
        recovery
            .pending_releases
            .flush(|_| -> Result<(), ()> { panic!("successful cleanup cannot replay") })
            .unwrap();
        Ok(())
    }
    fn held_recovery() -> Recovery {
        let mut recovery = Recovery {
            receiver: Receiver::default(),
            native: false,
            pending_releases: PendingReleases::default(),
            driver: None,
            receive_epoch: None,
        };
        assert!(recovery.receiver.acquire(1, 0, true));
        assert!(recovery.receiver.input(
            1,
            Input::Key {
                usage: 0xe0,
                down: true
            },
            1
        ));
        recovery
    }
    #[test]
    fn normal_up_failures_keep_key_and_button_ownership_until_cleanup() -> Result<()> {
        for (down, up) in [
            (
                Input::Key {
                    usage: 0xe0,
                    down: true,
                },
                Input::Key {
                    usage: 0xe0,
                    down: false,
                },
            ),
            (
                Input::Button {
                    button: 1,
                    down: true,
                },
                Input::Button {
                    button: 1,
                    down: false,
                },
            ),
        ] {
            for fail_pointer in [true, false] {
                let mut recovery = Recovery {
                    receiver: Receiver::default(),
                    native: false,
                    pending_releases: PendingReleases::default(),
                    driver: None,
                    receive_epoch: None,
                };
                assert!(recovery.receiver.acquire(1, 0, true));
                let cursor = Motion {
                    lease: 1,
                    sequence: 1,
                    x: 0.5,
                    y: 0.5,
                };
                assert!(recovery.input_with(1, down, cursor, 1, |_| Ok(()), |_| Ok(()))?);
                let mut up_attempts = 0;
                assert!(recovery
                    .input_with(
                        1,
                        up,
                        Motion {
                            sequence: 2,
                            ..cursor
                        },
                        2,
                        |_| {
                            if fail_pointer {
                                anyhow::bail!("fake pointer failure before Up");
                            }
                            Ok(())
                        },
                        |_| {
                            up_attempts += 1;
                            anyhow::bail!("fake Up injection failure")
                        },
                    )
                    .is_err());
                assert_eq!(up_attempts, usize::from(!fail_pointer));
                let mut released = Vec::new();
                recovery.finish_with(
                    1,
                    None,
                    3,
                    |_| Ok(()),
                    |event| {
                        released.push(event);
                        Ok(())
                    },
                )?;
                assert!(released == [up]);
                recovery.finish_with(
                    1,
                    None,
                    4,
                    |_| Ok(()),
                    |_| panic!("acknowledged cleanup must not replay an Up"),
                )?;
            }
        }
        Ok(())
    }
    #[test]
    fn successful_up_and_unowned_up_never_move_or_release_again() -> Result<()> {
        let mut recovery = held_recovery();
        let up = Input::Key {
            usage: 0xe0,
            down: false,
        };
        let cursor = Motion {
            lease: 1,
            sequence: 1,
            x: 0.5,
            y: 0.5,
        };
        assert!(recovery.input_with(1, up, cursor, 2, |_| Ok(()), |_| Ok(()))?);
        assert!(!recovery.input_with(
            1,
            up,
            Motion {
                sequence: 2,
                ..cursor
            },
            3,
            |_| panic!("unowned Up must not position the pointer"),
            |_| panic!("unowned Up must not reach the native injector"),
        )?);
        recovery.finish_with(
            1,
            None,
            4,
            |_| Ok(()),
            |_| panic!("successful Up was already acknowledged"),
        )?;
        Ok(())
    }
    #[test]
    fn terminal_pointer_failure_still_releases_pressed_input() {
        let mut recovery = held_recovery();
        let mut releases = Vec::new();
        let result = recovery.finish_with(
            1,
            Some(Motion {
                lease: 1,
                sequence: 4,
                x: 0.0,
                y: 0.7,
            }),
            2,
            |_| anyhow::bail!("pointer rejected"),
            |event| {
                releases.push(event);
                Ok(())
            },
        );
        assert!(result.is_err());
        assert!(recovery.receiver.active().is_none());
        assert!(releases.contains(&Input::Key {
            usage: 0xe0,
            down: false
        }));
    }
    #[test]
    fn terminal_release_failure_remains_pending_for_cleanup() {
        let mut recovery = held_recovery();
        let mut attempts = 0;
        assert!(recovery
            .finish_with(
                1,
                None,
                2,
                |_| Ok(()),
                |_| {
                    attempts += 1;
                    anyhow::bail!("inject rejected")
                }
            )
            .is_err());
        assert_eq!(attempts, 3);
        let mut remaining = Vec::new();
        assert!(recovery
            .pending_releases
            .flush(|event| {
                remaining.push(event);
                Ok::<_, ()>(())
            })
            .is_ok());
        assert_eq!(remaining.len(), 1);
        assert!(recovery.receiver.active().is_none());
    }

    #[test]
    fn failed_terminal_cleanup_is_not_reported_local_after_a_later_software_retry() {
        let mut recovery = held_recovery();
        let mut reporter = super::super::InputReporter::new(false);
        let mut attempts = 0;
        {
            let mut status = InputStatus::new(true, &mut reporter);
            status.routing(false, recovery.receiver.active().is_some());
            status.recovering();
            assert!(status
                .checked(recovery.finish_with(
                    1,
                    None,
                    2,
                    |_| Ok(()),
                    |_| {
                        attempts += 1;
                        anyhow::bail!("fake native release rejected")
                    },
                ))
                .is_err());
            assert_eq!(attempts, 3);
            let mut remaining = Vec::new();
            recovery
                .pending_releases
                .flush(|event| {
                    remaining.push(event);
                    Ok::<_, ()>(())
                })
                .unwrap();
            assert!(matches!(
                remaining.as_slice(),
                [Input::Key {
                    usage: 0xe0,
                    down: false
                }]
            ));
            status.routing(false, recovery.receiver.active().is_some());
            status.finish(true);
        }
        assert_eq!(
            reporter.observed,
            [
                WorkerInputState::ReceivingRemote,
                WorkerInputState::Recovering,
                WorkerInputState::RecoveryUnconfirmed,
            ]
        );
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum FileDecision {
        Ready,
        CancelBeforeGrant,
        CancelAfterGrant,
        ReleaseBeforeReady,
        Expire,
        TargetDrop,
        TargetEarlyRelease,
        TargetCancel,
    }

    #[tokio::test]
    async fn managed_file_reservation_requires_v5_grant_and_matching_local_readiness() -> Result<()>
    {
        exercise_file_worker(FileDecision::Ready).await
    }

    #[tokio::test]
    async fn local_cancel_before_v5_grant_retires_the_file_reservation() -> Result<()> {
        exercise_file_worker(FileDecision::CancelBeforeGrant).await
    }

    #[tokio::test]
    async fn local_cancel_after_v5_grant_withholds_the_file_proof() -> Result<()> {
        exercise_file_worker(FileDecision::CancelAfterGrant).await
    }

    #[tokio::test]
    async fn v5_release_retires_the_pending_local_file_claim() -> Result<()> {
        exercise_file_worker(FileDecision::ReleaseBeforeReady).await
    }

    #[tokio::test]
    async fn expired_local_file_claim_cannot_be_confirmed() -> Result<()> {
        exercise_file_worker(FileDecision::Expire).await
    }

    #[tokio::test]
    async fn target_drop_waits_for_matching_native_start_and_release_confirmation() -> Result<()> {
        exercise_file_worker(FileDecision::TargetDrop).await
    }

    #[tokio::test]
    async fn target_release_before_native_start_cannot_drop() -> Result<()> {
        exercise_file_worker(FileDecision::TargetEarlyRelease).await
    }

    #[tokio::test]
    async fn target_cancel_after_down_retires_the_live_input_lease() -> Result<()> {
        exercise_file_worker(FileDecision::TargetCancel).await
    }

    async fn exercise_file_worker(decision: FileDecision) -> Result<()> {
        use file_transfer::drop_admission::{Admission, Content, Context as DropContext};
        use peerglide_protocol::file_drop_ipc::{Command, EntryPoint, Event, State};
        use transport::{file_drop_link, file_transfer, layout_reservation};

        let a = Identity::generate()?;
        let b = Identity::generate()?;
        let (receipt, file) = test_file_transfer(&a, &b).await?;
        let prepared = file.received.prepare_drop(|| Ok(()))?;
        let origin = file.received.drop_origin();
        let context = DropContext {
            connection: origin.connection(),
            layout_epoch: (1, 1),
            gesture_generation: 1,
            focused: true,
        };
        let mut admission = Admission::default();
        let now = now_ms();
        let ticket = admission
            .begin(context, origin.transfer(), Content::File, now, true)
            .unwrap();
        admission.verified(ticket, context, now, true).unwrap();
        let slot = file_drop_link::Slot::arm(prepared, &mut admission, ticket, context, now, true)?;
        let (local, proof, lifetime_ms) = slot.into_worker()?;

        let bind = "127.0.0.1:0".parse()?;
        let ea = transport::endpoint(bind, &a, &b.certificate)?;
        let eb = transport::endpoint(bind, &b, &a.certificate)?;
        let controller = super::super::file_drop_control::Controller::default();
        let (stop, stopped) = tokio::sync::watch::channel(false);
        let (events, mut observed) = tokio::sync::mpsc::channel(8);
        let server = async {
            let c = eb.accept().await.context("missing v5 connection")?.await?;
            let (tx, rx) = c.accept_bi().await?;
            let mut reporter = super::super::InputReporter::new(false);
            reporter.file_drop_events = Some(events);
            run_with_diagnostics(
                c.clone(),
                tx,
                rx,
                Policy {
                    allow: true,
                    native: false,
                    edge: Edge::Left,
                    local: InputCapabilities::new(&[4], true, 1, true).unwrap(),
                    peer: InputCapabilities::default(),
                },
                stopped,
                &mut reporter,
                SessionControls {
                    file_drop: Some(super::super::file_drop_control::Config {
                        controller: &controller,
                        identity: &b,
                        pin: &a.certificate,
                    }),
                    ..SessionControls::default()
                },
            )
            .await?;
            assert!(controller.submit(Command::Inspect).is_none());
            c.close(0u8.into(), b"file reservation fixture finished");
            Ok::<_, anyhow::Error>(())
        };
        let client = async {
            let c = ea.connect(eb.local_addr()?, "peerglide.local")?.await?;
            let (mut tx, mut rx) = c.open_bi().await?;
            send(&mut tx, &Message::Ping { lease: 0 }).await?;
            assert!(matches!(
                transport::receive(&mut rx).await?,
                Message::Pong { lease: 0 }
            ));
            let session = controller
                .submit(Command::Inspect)
                .context("missing worker session")?
                .session;
            let request = 4;
            let arm = Command::Arm {
                session,
                request,
                bind,
                lifetime_ms,
                proof,
            };
            controller.submit(Command::parse(&arm.line()).context("invalid arm fixture")?);
            let armed = observed.recv().await.context("missing armed event")?;
            assert_eq!(
                (armed.session, armed.request, armed.state),
                (session, request, State::Armed)
            );
            let address = (
                std::net::Ipv4Addr::LOCALHOST,
                armed.port.context("missing sidecar port")?,
            )
                .into();
            let sidecar = layout_reservation::endpoint(bind, &a, &b.certificate)?;
            let side = sidecar.connect(address, "peerglide.local")?.await?;
            let lease = 7;
            let source_proof = receipt.into_local_worker();
            assert!(
                file_drop_link::SentReceipt::from_local_worker(&source_proof, &a.certificate)
                    .is_err()
            );
            let receipt =
                file_drop_link::SentReceipt::from_local_worker(&source_proof, &b.certificate)?;
            let confirmation =
                file_drop_link::reserve_for_worker(&side, &c, &b.certificate, receipt, lease)
                    .await?
                    .context("receiver declined initial reservation")?;
            if decision == FileDecision::CancelBeforeGrant {
                let event = controller
                    .submit(Command::Cancel { session, request })
                    .context("cancel not accepted")?;
                assert_eq!(event.state, State::Cancelled);
            }
            send(
                &mut tx,
                &Message::Acquire {
                    lease,
                    edge: Edge::Right,
                    along: 0.5,
                },
            )
            .await?;
            assert!(
                matches!(transport::receive(&mut rx).await?, Message::Granted { lease: granted } if granted == lease)
            );
            let target = matches!(
                decision,
                FileDecision::TargetDrop
                    | FileDecision::TargetEarlyRelease
                    | FileDecision::TargetCancel
            );
            let mut confirm = std::pin::pin!(confirmation.confirm());
            if decision != FileDecision::CancelBeforeGrant {
                let matched = observed
                    .recv()
                    .await
                    .context("missing match before claim")?;
                assert_eq!(
                    matched,
                    Event::new(session, request, State::Matched, Some(lease))
                );
                let ready = |session, request, lease| Command::Ready {
                    session,
                    request,
                    lease,
                };
                match decision {
                    FileDecision::Ready
                    | FileDecision::TargetDrop
                    | FileDecision::TargetEarlyRelease
                    | FileDecision::TargetCancel => {
                        controller.submit(ready(session + 1, request, lease));
                        controller.submit(ready(session, request + 1, lease));
                        controller.submit(ready(session, request, lease + 1));
                        assert!(
                            tokio::time::timeout(Duration::from_millis(50), confirm.as_mut())
                                .await
                                .is_err()
                        );
                        let approval = if target {
                            Command::PrepareTarget {
                                session,
                                request,
                                lease,
                                entry: EntryPoint::new(0.25, 0.75).unwrap(),
                            }
                        } else {
                            ready(session, request, lease)
                        };
                        controller.submit(approval);
                        assert_eq!(
                            observed.recv().await,
                            Some(Event::new(session, request, State::Claimed, Some(lease)))
                        );
                        controller.submit(ready(session, request, lease));
                    }
                    FileDecision::CancelAfterGrant => {
                        let cancelled = controller
                            .submit(Command::Cancel { session, request })
                            .context("pending claim cancel not accepted")?;
                        assert_eq!(cancelled.state, State::Cancelled);
                        controller.submit(ready(session, request, lease));
                    }
                    FileDecision::ReleaseBeforeReady | FileDecision::Expire => {
                        if decision == FileDecision::ReleaseBeforeReady {
                            send(
                                &mut tx,
                                &Message::Release {
                                    lease,
                                    cursor: None,
                                },
                            )
                            .await?;
                        }
                        assert_eq!(
                            observed.recv().await,
                            Some(Event::new(session, request, State::Rejected, None))
                        );
                        controller.submit(ready(session, request, lease));
                    }
                    FileDecision::CancelBeforeGrant => unreachable!("already cancelled"),
                }
            }
            let proof = tokio::time::timeout(Duration::from_secs(3), confirm.as_mut()).await?;
            assert_eq!(proof.is_ok(), target || decision == FileDecision::Ready);
            side.close(0u8.into(), b"reservation resolved");
            if target {
                let input = |lease, sequence, down| Message::Input {
                    lease,
                    event: Input::Button { button: 1, down },
                    cursor: Motion {
                        lease,
                        sequence,
                        x: 0.5,
                        y: 0.5,
                    },
                };
                send(&mut tx, &input(lease + 1, 1, true)).await?;
                send(&mut tx, &input(lease, 1, true)).await?;
                assert_eq!(
                    observed.recv().await,
                    Some(Event::new(session, request, State::TargetDown, Some(lease)))
                );
                let start = |lease| Command::TargetStarted {
                    session,
                    request,
                    lease,
                };
                controller.submit(start(lease + 1));
                match decision {
                    FileDecision::TargetDrop => {
                        controller.submit(start(lease));
                        send(&mut tx, &input(lease, 2, false)).await?;
                        assert_eq!(
                            observed.recv().await,
                            Some(Event::new(session, request, State::TargetDrop, Some(lease)))
                        );
                        let confirm = |lease| Command::TargetDropReady {
                            session,
                            request,
                            lease,
                        };
                        controller.submit(confirm(lease + 1));
                        assert!(
                            tokio::time::timeout(Duration::from_millis(30), observed.recv())
                                .await
                                .is_err()
                        );
                        controller.submit(confirm(lease));
                        assert_eq!(
                            observed.recv().await,
                            Some(Event::new(
                                session,
                                request,
                                State::TargetReleased,
                                Some(lease)
                            ))
                        );
                        // Confirmed Up removes the drop state. The later ordinary
                        // v5 Release will not reject or repeat this file drop.
                        send(&mut tx, &Message::Ping { lease }).await?;
                        assert!(
                            matches!(transport::receive(&mut rx).await?, Message::Pong { lease: active } if active == lease)
                        );
                    }
                    FileDecision::TargetEarlyRelease => {
                        send(&mut tx, &input(lease, 2, false)).await?;
                        assert_eq!(
                            observed.recv().await,
                            Some(Event::new(session, request, State::Rejected, None))
                        );
                    }
                    FileDecision::TargetCancel => {
                        assert_eq!(
                            controller.submit(Command::Cancel { session, request }),
                            Some(Event::new(session, request, State::Cancelled, None))
                        );
                    }
                    _ => unreachable!("not a native target fixture"),
                }
                if decision != FileDecision::TargetDrop {
                    assert!(
                        matches!(transport::receive(&mut rx).await?, Message::Release { lease: retired, .. } if retired == lease)
                    );
                }
            }
            if decision == FileDecision::Ready {
                admission
                    .submit(
                        local.ticket,
                        context,
                        now_ms(),
                        true,
                        file_transfer::drop_admission::Effect::Copy,
                    )
                    .unwrap();
            }
            send(
                &mut tx,
                &Message::Release {
                    lease,
                    cursor: None,
                },
            )
            .await?;
            send(&mut tx, &Message::Ping { lease: 0 }).await?;
            assert!(matches!(
                transport::receive(&mut rx).await?,
                Message::Pong { lease: 0 }
            ));
            assert!(observed.try_recv().is_err());
            let _ = stop.send(true);
            Ok::<_, anyhow::Error>(())
        };
        tokio::time::timeout(Duration::from_secs(10), async {
            tokio::try_join!(server, client)
        })
        .await??;
        Ok(())
    }

    struct FileFixture {
        received: transport::file_transfer::ReceivedFile,
        _directory: TestFileDirectory,
    }

    struct TestFileDirectory(std::path::PathBuf);

    impl Drop for TestFileDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    async fn test_file_transfer(
        a: &Identity,
        b: &Identity,
    ) -> Result<(transport::file_drop_link::SentReceipt, FileFixture)> {
        use transport::file_transfer::{self, Cancellation, Decision, ReceiveOptions, Source};
        let directory =
            std::env::temp_dir().join(format!("linko-file-reservation-{}", a.fingerprint()));
        std::fs::create_dir(&directory)?;
        let cleanup = TestFileDirectory(directory.clone());
        let path = directory.join("source.txt");
        let target = directory.join("target");
        std::fs::write(&path, b"one file reservation")?;
        std::fs::create_dir(&target)?;
        let bind = "127.0.0.1:0".parse()?;
        let sender = transport::data::endpoint(bind, a, &b.certificate)?;
        let receiver = transport::data::endpoint(bind, b, &a.certificate)?;
        let (send, receive) = tokio::try_join!(
            async {
                Ok::<_, anyhow::Error>(
                    sender
                        .connect(receiver.local_addr()?, "peerglide.local")?
                        .await?,
                )
            },
            async {
                Ok::<_, anyhow::Error>(
                    receiver
                        .accept()
                        .await
                        .context("missing file connection")?
                        .await?,
                )
            },
        )?;
        let source = Source::open(path, None, Cancellation::default()).await?;
        let ((_, receipt), result) = tokio::try_join!(
            file_transfer::send_for_drop(
                &send,
                &b.certificate,
                source,
                Cancellation::default(),
                |_| {}
            ),
            file_transfer::receive(
                &receive,
                &a.certificate,
                ReceiveOptions {
                    budget: 1024 * 1024,
                    stop: Cancellation::default()
                },
                |_| async {
                    Ok(Decision::Accept {
                        directory: target,
                        resume: false,
                    })
                },
                |_| {},
            ),
        )?;
        let received = result.file.context("missing verified file")?;
        let receipt = receipt.context("missing verified send receipt")?;
        Ok((
            receipt,
            FileFixture {
                received,
                _directory: cleanup,
            },
        ))
    }

    #[tokio::test]
    async fn cooperative_stop_releases_a_real_session_before_return_without_native_input(
    ) -> Result<()> {
        let a = Identity::generate()?;
        let b = Identity::generate()?;
        let ea = transport::endpoint("127.0.0.1:0".parse()?, &a, &b.certificate)?;
        let eb = transport::endpoint("127.0.0.1:0".parse()?, &b, &a.certificate)?;
        let (stop, stopped) = tokio::sync::watch::channel(false);
        let (release_seen, release_confirmed) = tokio::sync::oneshot::channel();
        let server = async {
            let c = eb.accept().await.unwrap().await?;
            let (tx, rx) = c.accept_bi().await?;
            let mut reporter = super::super::InputReporter::new(false);
            run_with_status(
                c.clone(),
                tx,
                rx,
                Policy {
                    allow: true,
                    native: false,
                    edge: Edge::Right,
                    local: InputCapabilities::new(&[0xe0], true, 1, true).unwrap(),
                    peer: InputCapabilities::default(),
                },
                stopped,
                &mut reporter,
            )
            .await?;
            assert_eq!(reporter.observed, [WorkerInputState::Disabled]);
            assert_eq!(
                reporter.observed_switches,
                [
                    super::super::WorkerSwitchState::PeerUnavailable,
                    super::super::WorkerSwitchState::Idle
                ]
            );
            release_confirmed.await?;
            c.close(0u8.into(), b"stopped without native input");
            Ok::<_, anyhow::Error>(())
        };
        let client = async {
            let c = ea.connect(eb.local_addr()?, "peerglide.local")?.await?;
            let (mut tx, mut rx) = c.open_bi().await?;
            // Unsolicited refusals cannot invent a locally pending switch.
            send(&mut tx, &Message::Denied { lease: 42 }).await?;
            send(
                &mut tx,
                &Message::Acquire {
                    lease: 1,
                    edge: Edge::Right,
                    along: 0.5,
                },
            )
            .await?;
            assert!(matches!(
                transport::receive(&mut rx).await?,
                Message::Granted { lease: 1 }
            ));
            send(
                &mut tx,
                &Message::Input {
                    lease: 1,
                    event: Input::Key {
                        usage: 0xe0,
                        down: true,
                    },
                    cursor: Motion {
                        lease: 1,
                        sequence: 1,
                        x: 0.5,
                        y: 0.5,
                    },
                },
            )
            .await?;
            send(&mut tx, &Message::Ping { lease: 1 }).await?;
            assert!(matches!(
                transport::receive(&mut rx).await?,
                Message::Pong { lease: 1 }
            ));
            stop.send(true)?;
            assert!(matches!(
                transport::receive(&mut rx).await?,
                Message::Release {
                    lease: 1,
                    cursor: None
                }
            ));
            let _ = release_seen.send(());
            c.close(0u8.into(), b"release observed");
            Ok::<_, anyhow::Error>(())
        };
        tokio::time::timeout(Duration::from_secs(8), async {
            tokio::try_join!(server, client)
        })
        .await??;
        Ok(())
    }

    async fn exercise(allow: bool) -> Result<()> {
        let a = Identity::generate()?;
        let b = Identity::generate()?;
        let ea = transport::endpoint("127.0.0.1:0".parse()?, &a, &b.certificate)?;
        let eb = transport::endpoint("127.0.0.1:0".parse()?, &b, &a.certificate)?;
        let server = async {
            let c = eb.accept().await.unwrap().await?;
            let (tx, rx) = c.accept_bi().await?;
            run(
                c,
                tx,
                rx,
                Policy {
                    allow,
                    native: false,
                    edge: Edge::Right,
                    local: InputCapabilities::new(&[0xe0], true, 1, true).unwrap(),
                    peer: InputCapabilities::default(),
                },
            )
            .await
        };
        let client = async {
            let c = ea.connect(eb.local_addr()?, "peerglide.local")?.await?;
            let (mut tx, mut rx) = c.open_bi().await?;
            send(&mut tx, &Message::Ping { lease: 0 }).await?;
            assert!(matches!(
                transport::receive(&mut rx).await?,
                Message::Pong { lease: 0 }
            ));
            send(
                &mut tx,
                &Message::Acquire {
                    lease: 1,
                    edge: Edge::Right,
                    along: 0.5,
                },
            )
            .await?;
            let response = transport::receive(&mut rx).await?;
            if allow {
                assert!(matches!(response, Message::Granted { lease: 1 }));
                send(
                    &mut tx,
                    &Message::Input {
                        lease: 1,
                        cursor: Motion {
                            lease: 1,
                            sequence: 1,
                            x: 0.5,
                            y: 0.5,
                        },
                        event: Input::Key {
                            usage: 0xe0,
                            down: true,
                        },
                    },
                )
                .await?;
                send(&mut tx, &Message::Ping { lease: 1 }).await?;
                assert!(matches!(
                    transport::receive(&mut rx).await?,
                    Message::Pong { lease: 1 }
                ));
                send(
                    &mut tx,
                    &Message::Acquire {
                        lease: 2,
                        edge: Edge::Right,
                        along: 0.5,
                    },
                )
                .await?;
                assert!(matches!(
                    transport::receive(&mut rx).await?,
                    Message::Denied { lease: 2 }
                ));
                tokio::time::sleep(Duration::from_millis(1800)).await;
                send(
                    &mut tx,
                    &Message::Acquire {
                        lease: 3,
                        edge: Edge::Right,
                        along: 0.5,
                    },
                )
                .await?;
                assert!(matches!(
                    transport::receive(&mut rx).await?,
                    Message::Granted { lease: 3 }
                ));
                c.send_datagram(
                    encode(&Motion {
                        lease: 3,
                        sequence: 1,
                        x: 0.0,
                        y: 0.5,
                    })?
                    .into(),
                )?;
                // Let the edge datagram overtake a deliberately delayed reliable input.
                // It must not terminate the lease or discard that input.
                assert!(tokio::time::timeout(
                    Duration::from_millis(100),
                    transport::receive(&mut rx)
                )
                .await
                .is_err());
                send(
                    &mut tx,
                    &Message::Input {
                        lease: 3,
                        event: Input::Button {
                            button: 1,
                            down: true,
                        },
                        cursor: Motion {
                            lease: 3,
                            sequence: 2,
                            x: 0.5,
                            y: 0.5,
                        },
                    },
                )
                .await?;
                send(&mut tx, &Message::Ping { lease: 3 }).await?;
                assert!(matches!(
                    transport::receive(&mut rx).await?,
                    Message::Pong { lease: 3 }
                ));
                // Source-originated release shares the discrete input stream.
                send(
                    &mut tx,
                    &Message::Release {
                        lease: 3,
                        cursor: Some(Motion {
                            lease: 3,
                            sequence: 3,
                            x: 0.0,
                            y: 0.7,
                        }),
                    },
                )
                .await?;
                send(
                    &mut tx,
                    &Message::Acquire {
                        lease: 4,
                        edge: Edge::Right,
                        along: 0.5,
                    },
                )
                .await?;
                assert!(matches!(
                    transport::receive(&mut rx).await?,
                    Message::Granted { lease: 4 }
                ));
                send(
                    &mut tx,
                    &Message::Acquire {
                        lease: 1,
                        edge: Edge::Left,
                        along: 0.5,
                    },
                )
                .await?;
                assert!(matches!(
                    transport::receive(&mut rx).await?,
                    Message::Denied { lease: 1 }
                ));
            } else {
                assert!(matches!(response, Message::Denied { lease: 1 }));
            }
            c.close(0u8.into(), b"done");
            tokio::time::sleep(Duration::from_millis(50)).await;
            Ok::<_, anyhow::Error>(())
        };
        tokio::time::timeout(Duration::from_secs(8), async {
            tokio::try_join!(server, client)
        })
        .await??;
        Ok(())
    }
    async fn capability_case(motion: bool) -> Result<()> {
        let a = Identity::generate()?;
        let b = Identity::generate()?;
        let ea = transport::endpoint("127.0.0.1:0".parse()?, &a, &b.certificate)?;
        let eb = transport::endpoint("127.0.0.1:0".parse()?, &b, &a.certificate)?;
        let server = async {
            let c = eb.accept().await.unwrap().await?;
            let (tx, rx) = c.accept_bi().await?;
            let result = run(
                c.clone(),
                tx,
                rx,
                Policy {
                    allow: true,
                    native: false,
                    edge: Edge::Right,
                    local: InputCapabilities::new(&[4], motion, 1, true).unwrap(),
                    peer: InputCapabilities::default(),
                },
            )
            .await;
            if motion {
                assert!(result
                    .unwrap_err()
                    .to_string()
                    .contains("outside advertised"));
            } else {
                result?;
            }
            c.close(0u8.into(), b"test complete");
            Ok::<_, anyhow::Error>(())
        };
        let client = async {
            let c = ea.connect(eb.local_addr()?, "peerglide.local")?.await?;
            let (mut tx, mut rx) = c.open_bi().await?;
            send(
                &mut tx,
                &Message::Acquire {
                    lease: 1,
                    edge: Edge::Right,
                    along: 0.5,
                },
            )
            .await?;
            let response = transport::receive(&mut rx).await?;
            if motion {
                assert!(matches!(response, Message::Granted { lease: 1 }));
                send(
                    &mut tx,
                    &Message::Input {
                        lease: 1,
                        event: Input::Key {
                            usage: 5,
                            down: true,
                        },
                        cursor: Motion {
                            lease: 1,
                            sequence: 1,
                            x: 0.5,
                            y: 0.5,
                        },
                    },
                )
                .await?;
                c.closed().await;
            } else {
                assert!(matches!(response, Message::Denied { lease: 1 }));
                c.close(0u8.into(), b"done");
            }
            Ok::<_, anyhow::Error>(())
        };
        tokio::time::timeout(Duration::from_secs(5), async {
            tokio::try_join!(server, client)
        })
        .await??;
        Ok(())
    }
    #[tokio::test]
    async fn unsupported_input_is_rejected_before_injection() -> Result<()> {
        capability_case(true).await
    }
    #[tokio::test]
    async fn allow_flag_does_not_make_diagnostic_endpoint_controllable() -> Result<()> {
        capability_case(false).await
    }
    #[tokio::test]
    async fn default_denies_input() -> Result<()> {
        exercise(false).await
    }
    #[tokio::test]
    async fn live_session_grants_conflicts_expires_and_rejects_replay() -> Result<()> {
        exercise(true).await
    }
}
