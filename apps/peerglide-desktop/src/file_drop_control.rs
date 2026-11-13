//! Local supervisor approval for a lease-bound file reservation.
use anyhow::{ensure, Context, Result};
use peerglide_platform::{now_ms, CaptureGate, HeldPrimary};
use peerglide_protocol::{
    file_drop_ipc::{Command, EntryPoint, Event, State},
    Input, Motion,
};
use peerglide_transport::{file_drop_link, layout_reservation, Identity};
use quinn::{Connection, Endpoint};
use std::{
    net::{Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::{sync::watch, task::JoinHandle};

pub(super) type Mailbox = watch::Receiver<Option<Arc<Arm>>>;

#[derive(Default)]
pub(super) struct Controller(Mutex<SessionState>);

#[derive(Default)]
struct SessionState {
    sequence: u32,
    active: Option<Active>,
}

struct Active {
    session: u32,
    watermark: u32,
    sender: watch::Sender<Option<Arc<Arm>>>,
    claim: Option<LocalClaim>,
    target: Option<TargetInput>,
    retired_target: Option<u64>,
}

struct LocalClaim {
    request: u32,
    lease: u64,
    proof: file_drop_link::PendingWorkerClaim,
    readiness: Option<Readiness>,
    deadline: u64,
}

enum Readiness {
    Legacy,
    Native(EntryPoint),
}

#[derive(Clone, Copy)]
enum TargetPhase {
    WaitingForDown(EntryPoint),
    WaitingForStart(u64),
    Dragging,
    WaitingForDrop {
        cursor: Motion,
        deadline: u64,
        ready: bool,
    },
}

struct TargetInput {
    request: u32,
    lease: u64,
    deadline: u64,
    phase: TargetPhase,
    placement: Option<Placement>,
}

#[derive(Clone, Copy)]
struct Placement {
    original: Motion,
    entry: Motion,
}

impl Placement {
    fn map(self, point: Motion) -> Motion {
        // Both ends of each axis stay reachable from the local start point.
        fn axis(value: f64, original: f64, entry: f64) -> f64 {
            if value < original {
                entry * (value / original)
            } else if value > original {
                entry + (1.0 - entry) * ((value - original) / (1.0 - original))
            } else {
                entry
            }
        }
        Motion {
            x: axis(point.x, self.original.x, self.entry.x),
            y: axis(point.y, self.original.y, self.entry.y),
            ..point
        }
    }
}

const TARGET_START_MS: u64 = 750;
const TARGET_DROP_MS: u64 = 500;

pub(super) enum TargetAction {
    Ordinary,
    AtEntry(Motion),
    Drop(Event),
    Abort,
}

pub(super) struct Arm {
    request: u32,
    direction: Direction,
    proof: Mutex<Option<String>>,
    deadline: u64,
}

#[derive(Clone, Copy)]
enum Direction {
    Receive(SocketAddr),
    Send(SocketAddr),
}

#[derive(Clone, Copy)]
pub(super) struct Config<'a> {
    pub controller: &'a Controller,
    pub identity: &'a Identity,
    pub pin: &'a [u8],
}

pub(super) struct Scope<'a> {
    controller: &'a Controller,
    pub session: u32,
}

impl Controller {
    pub(super) fn open(&self) -> Result<(Scope<'_>, Mailbox)> {
        let mut state = self
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("file control unavailable"))?;
        ensure!(
            state.active.is_none(),
            "file control session already active"
        );
        let session = state
            .sequence
            .checked_add(1)
            .context("file control sequence exhausted")?;
        let (sender, receiver) = watch::channel(None);
        state.sequence = session;
        state.active = Some(Active {
            session,
            watermark: 0,
            sender,
            claim: None,
            target: None,
            retired_target: None,
        });
        Ok((
            Scope {
                controller: self,
                session,
            },
            receiver,
        ))
    }

    pub(super) fn submit(&self, command: Command) -> Option<Event> {
        let mut state = self.0.lock().ok()?;
        let active = state.active.as_mut()?;
        let (session, request, lifetime_ms, direction, proof) = match command {
            Command::Inspect => {
                return Some(Event::new(active.session, 0, State::Session, None));
            }
            Command::Arm {
                session,
                request,
                bind,
                lifetime_ms,
                proof,
            } => (
                session,
                request,
                lifetime_ms,
                Direction::Receive(bind),
                proof,
            ),
            Command::Send {
                session,
                request,
                target,
                lifetime_ms,
                proof,
            } => (
                session,
                request,
                lifetime_ms,
                Direction::Send(target),
                proof,
            ),
            Command::Ready {
                session,
                request,
                lease,
            } => {
                active.ready(session, request, lease, Readiness::Legacy);
                return None;
            }
            Command::PrepareTarget {
                session,
                request,
                lease,
                entry,
            } => {
                active.ready(session, request, lease, Readiness::Native(entry));
                return None;
            }
            Command::TargetStarted {
                session,
                request,
                lease,
            } => {
                if let Some(target) = active.target.as_mut().filter(|target| {
                    active.session == session && target.request == request && target.lease == lease
                }) {
                    if matches!(target.phase, TargetPhase::WaitingForStart(deadline) if now_ms() < deadline)
                    {
                        target.phase = TargetPhase::Dragging;
                        active.sender.send_modify(|_| {});
                    }
                }
                return None;
            }
            Command::TargetDropReady {
                session,
                request,
                lease,
            } => {
                if let Some(target) = active.target.as_mut().filter(|target| {
                    active.session == session && target.request == request && target.lease == lease
                }) {
                    if let TargetPhase::WaitingForDrop {
                        deadline, ready, ..
                    } = &mut target.phase
                    {
                        if !*ready && now_ms() < *deadline {
                            *ready = true;
                            active.sender.send_modify(|_| {});
                        }
                    }
                }
                return None;
            }
            Command::Cancel { session, request } => {
                let pending = active
                    .claim
                    .as_ref()
                    .is_some_and(|claim| claim.request == request);
                let armed = active
                    .sender
                    .borrow()
                    .as_ref()
                    .is_some_and(|arm| arm.request == request);
                let targeting = active
                    .target
                    .as_ref()
                    .is_some_and(|target| target.request == request);
                if session != active.session || !(armed || pending || targeting) {
                    return None;
                }
                active.claim = None;
                if targeting {
                    active.retire_target();
                }
                active.sender.send_replace(None);
                return Some(Event::new(session, request, State::Cancelled, None));
            }
        };
        if session == active.session && request > active.watermark {
            active.watermark = request;
            active.claim = None;
            active.retire_target();
            let deadline = now_ms().saturating_add(u64::from(lifetime_ms));
            active.sender.send_replace(Some(Arc::new(Arm {
                request,
                direction,
                proof: Mutex::new(Some(proof)),
                deadline,
            })));
        }
        None
    }
}

impl Active {
    fn ready(&mut self, session: u32, request: u32, lease: u64, readiness: Readiness) {
        let Some(claim) = self.claim.as_mut().filter(|claim| {
            self.session == session
                && claim.request == request
                && claim.lease == lease
                && claim.readiness.is_none()
                && claim.proof.is_live()
        }) else {
            return;
        };
        claim.readiness = Some(readiness);
        self.sender.send_modify(|_| {});
    }

    fn retire_target(&mut self) {
        if let Some(target) = self.target.take() {
            self.retired_target = Some(target.lease);
        }
    }
}

impl Scope<'_> {
    pub(super) fn event(&self, request: u32, state: State) -> Event {
        Event::new(self.session, request, state, None)
    }

    pub(super) fn current(&self, request: u32) -> bool {
        self.controller.0.lock().ok().is_some_and(|state| {
            state.active.as_ref().is_some_and(|active| {
                active.session == self.session && matches_arm(&active.sender.borrow(), request)
            })
        })
    }

    pub(super) fn retire(&self, request: u32) {
        if let Ok(state) = self.controller.0.lock() {
            if let Some(active) = state.active.as_ref().filter(|a| a.session == self.session) {
                active.sender.send_if_modified(|arm| {
                    if arm.as_ref().is_some_and(|arm| arm.request == request) {
                        *arm = None;
                        true
                    } else {
                        false
                    }
                });
            }
        }
    }

    /// Called after the ordinary v5 admission decision. A match is sent to
    /// the GUI so it can check local readiness before the proof goes on wire.
    pub(super) fn resolve(
        &self,
        route: &Route,
        input: &Connection,
        pin: &[u8],
        lease: u64,
        granted: bool,
    ) -> Option<Event> {
        let Route::Target(route) = route else {
            return None;
        };
        let mut state = self.controller.0.lock().ok()?;
        let active = state.active.as_mut()?;
        let current =
            active.session == self.session && matches_arm(&active.sender.borrow(), route.request);
        let event = if granted {
            let proof = route.slot.begin_claim(input, pin, lease, current).ok()??;
            active.claim = Some(LocalClaim {
                request: route.request,
                lease,
                proof,
                readiness: None,
                deadline: route.deadline,
            });
            route.claimed.store(true, Ordering::Release);
            Event::new(self.session, route.request, State::Matched, Some(lease))
        } else {
            if !route.slot.retire_denied(input, pin, lease, current).ok()? {
                return None;
            }
            self.event(route.request, State::Rejected)
        };
        active.sender.send_replace(None);
        Some(event)
    }

    pub(super) fn advance_claim(&self, accepts: impl FnOnce(u64) -> bool) -> Option<Event> {
        let mut state = self.controller.0.lock().ok()?;
        let active = state
            .active
            .as_mut()
            .filter(|active| active.session == self.session)?;
        let claim = active.claim.as_ref()?;
        let live = claim.proof.is_live() && accepts(claim.lease);
        if live && claim.readiness.is_none() {
            return None;
        }
        let claim = active.claim.take()?;
        Some(if live && claim.proof.confirm() {
            if let Some(Readiness::Native(entry)) = claim.readiness {
                active.target = Some(TargetInput {
                    request: claim.request,
                    lease: claim.lease,
                    deadline: claim.deadline,
                    phase: TargetPhase::WaitingForDown(entry),
                    placement: None,
                });
            }
            Event::new(
                self.session,
                claim.request,
                State::Claimed,
                Some(claim.lease),
            )
        } else {
            self.event(claim.request, State::Rejected)
        })
    }

    fn with_active<T>(&self, action: impl FnOnce(&mut Active) -> T) -> Option<T> {
        let mut state = self.controller.0.lock().ok()?;
        state
            .active
            .as_mut()
            .filter(|active| active.session == self.session)
            .map(action)
    }

    pub(super) fn target_input(&self, lease: u64, event: Input, cursor: Motion) -> TargetAction {
        self.with_active(|active| {
            if active.retired_target == Some(lease) {
                return TargetAction::Abort;
            }
            let Some(target) = active
                .target
                .as_mut()
                .filter(|target| target.lease == lease)
            else {
                return TargetAction::Ordinary;
            };
            if now_ms() >= target.deadline {
                return TargetAction::Abort;
            }
            match (target.phase, event) {
                (
                    TargetPhase::WaitingForDown(entry),
                    Input::Button {
                        button: 1,
                        down: true,
                    },
                ) if cursor.lease == lease && cursor.sequence == 1 && cursor.valid() => {
                    let (x, y) = entry.coordinates();
                    let entry = Motion { x, y, ..cursor };
                    target.placement = Some(Placement {
                        original: cursor,
                        entry,
                    });
                    TargetAction::AtEntry(entry)
                }
                (
                    TargetPhase::Dragging,
                    Input::Button {
                        button: 1,
                        down: false,
                    },
                ) if cursor.lease == lease && cursor.sequence > 1 && cursor.valid() => {
                    let Some(placement) = target.placement else {
                        return TargetAction::Abort;
                    };
                    target.phase = TargetPhase::WaitingForDrop {
                        cursor: placement.map(cursor),
                        deadline: now_ms().saturating_add(TARGET_DROP_MS),
                        ready: false,
                    };
                    TargetAction::Drop(Event::new(
                        self.session,
                        target.request,
                        State::TargetDrop,
                        Some(lease),
                    ))
                }
                _ => TargetAction::Abort,
            }
        })
        .unwrap_or(TargetAction::Abort)
    }

    pub(super) fn target_down(&self, lease: u64) -> Option<Event> {
        self.with_active(|active| {
            let target = active
                .target
                .as_mut()
                .filter(|target| target.lease == lease)?;
            if !matches!(target.phase, TargetPhase::WaitingForDown(_)) {
                return None;
            }
            target.phase = TargetPhase::WaitingForStart(now_ms().saturating_add(TARGET_START_MS));
            Some(Event::new(
                self.session,
                target.request,
                State::TargetDown,
                Some(lease),
            ))
        })
        .flatten()
    }

    pub(super) fn target_motion(&self, motion: Motion) -> Option<Motion> {
        self.with_active(|active| {
            if active.retired_target == Some(motion.lease) {
                return None;
            }
            let Some(target) = active
                .target
                .as_ref()
                .filter(|target| target.lease == motion.lease)
            else {
                return Some(motion);
            };
            if !matches!(target.phase, TargetPhase::Dragging) || now_ms() >= target.deadline {
                return None;
            }
            target.placement.map(|placement| placement.map(motion))
        })
        .flatten()
    }

    pub(super) fn target_expired(&self, lease: u64) -> bool {
        self.with_active(|active| {
            let now = now_ms();
            active.retired_target == Some(lease)
                || active
                    .target
                    .as_ref()
                    .filter(|target| target.lease == lease)
                    .is_some_and(|target| {
                        now >= target.deadline
                            || matches!(target.phase,
                            TargetPhase::WaitingForStart(deadline)
                            | TargetPhase::WaitingForDrop { deadline, .. } if now >= deadline)
                    })
        })
        .unwrap_or(true)
    }

    pub(super) fn confirmed_drop(&self, lease: u64) -> Option<Motion> {
        self.with_active(|active| {
            let target = active
                .target
                .as_ref()
                .filter(|target| target.lease == lease)?;
            match target.phase {
                TargetPhase::WaitingForDrop {
                    cursor,
                    deadline,
                    ready: true,
                } if now_ms() < deadline.min(target.deadline) => Some(cursor),
                _ => None,
            }
        })
        .flatten()
    }

    pub(super) fn end_target(&self, lease: u64, dropped: bool) -> Option<Event> {
        self.with_active(|active| {
            if active.retired_target == Some(lease) {
                active.retired_target = None;
            }
            if !active
                .target
                .as_ref()
                .is_some_and(|target| target.lease == lease)
            {
                return None;
            }
            let target = active.target.take()?;
            (!dropped).then(|| self.event(target.request, State::Rejected))
        })
        .flatten()
    }
}

impl Drop for Scope<'_> {
    fn drop(&mut self) {
        if let Ok(mut state) = self.controller.0.lock() {
            if state
                .active
                .as_ref()
                .is_some_and(|active| active.session == self.session)
            {
                let Some(active) = state.active.take() else {
                    return;
                };
                active.sender.send_replace(None);
            }
        }
    }
}

fn matches_arm(arm: &Option<Arc<Arm>>, request: u32) -> bool {
    arm.as_ref()
        .is_some_and(|arm| arm.request == request && now_ms() < arm.deadline)
}

pub(super) enum Route {
    Target(Target),
    Source(Source),
}

pub(super) struct Target {
    request: u32,
    deadline: u64,
    slot: Arc<file_drop_link::WorkerSlot>,
    task: JoinHandle<()>,
    claimed: AtomicBool,
}

pub(super) struct Source {
    pub request: u32,
    pub hold: HeldPrimary,
    target: SocketAddr,
    receipt: file_drop_link::SentReceipt,
}

pub(super) struct SourceReservation {
    _endpoint: Endpoint,
    confirmation: file_drop_link::SourceReservation,
}

impl SourceReservation {
    pub(super) async fn confirm(self) -> Result<()> {
        self.confirmation.confirm().await
    }
}

impl Source {
    pub(super) async fn reserve(
        self,
        config: Config<'_>,
        input: &Connection,
        lease: u64,
    ) -> Result<Option<SourceReservation>> {
        let bind = if self.target.is_ipv4() {
            SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0))
        } else {
            SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0))
        };
        let endpoint = layout_reservation::endpoint(bind, config.identity, config.pin)?;
        let sidecar = endpoint.connect(self.target, "peerglide.local")?.await?;
        let confirmation =
            file_drop_link::reserve_for_worker(&sidecar, input, config.pin, self.receipt, lease)
                .await?;
        Ok(confirmation.map(|confirmation| SourceReservation {
            _endpoint: endpoint,
            confirmation,
        }))
    }
}

impl Route {
    pub(super) fn update(
        scope: &Scope<'_>,
        mailbox: &mut Mailbox,
        config: Config<'_>,
        input: &Connection,
        gate: Option<&CaptureGate>,
        allow_receive: bool,
    ) -> (Option<Self>, Option<Event>) {
        let Some(arm) = mailbox.borrow_and_update().clone() else {
            return (None, None);
        };
        let result = Self::arm(
            scope,
            &arm,
            mailbox.clone(),
            config,
            input,
            gate,
            allow_receive,
        );
        if !matches_arm(&mailbox.borrow(), arm.request) {
            return (None, None);
        }
        match result {
            Ok((route, event)) => (Some(route), Some(event)),
            Err(_) => {
                scope.retire(arm.request);
                (None, Some(scope.event(arm.request, State::Rejected)))
            }
        }
    }

    fn arm(
        scope: &Scope<'_>,
        arm: &Arm,
        mailbox: Mailbox,
        config: Config<'_>,
        input: &Connection,
        gate: Option<&CaptureGate>,
        allow_receive: bool,
    ) -> Result<(Self, Event)> {
        let proof = arm
            .proof
            .lock()
            .ok()
            .and_then(|mut proof| proof.take())
            .context("file reservation proof unavailable")?;
        match arm.direction {
            Direction::Receive(bind) => {
                ensure!(allow_receive, "file receive control is disabled");
                let (route, port) = Target::bind(arm, bind, &proof, mailbox, config, input)?;
                Ok((
                    Self::Target(route),
                    Event::armed(scope.session, arm.request, port),
                ))
            }
            Direction::Send(target) => {
                ensure!(
                    target.port() != 0
                        && target.ip().to_canonical() == input.remote_address().ip().to_canonical(),
                    "file reservation target is outside the current peer"
                );
                let hold = gate
                    .and_then(CaptureGate::held_primary)
                    .context("no eligible local primary hold")?;
                let receipt = file_drop_link::SentReceipt::from_local_worker(&proof, config.pin)?;
                Ok((
                    Self::Source(Source {
                        request: arm.request,
                        hold,
                        target,
                        receipt,
                    }),
                    scope.event(arm.request, State::SourceArmed),
                ))
            }
        }
    }
}

impl Target {
    fn bind(
        arm: &Arm,
        bind: SocketAddr,
        proof: &str,
        mailbox: Mailbox,
        config: Config<'_>,
        input: &Connection,
    ) -> Result<(Self, u16)> {
        ensure!(
            !bind.ip().is_multicast(),
            "invalid file reservation bind address"
        );
        let remaining = u32::try_from(arm.deadline.saturating_sub(now_ms()))?;
        let slot = Arc::new(file_drop_link::WorkerSlot::from_local_arm(
            proof, remaining, config.pin,
        )?);
        let listener = layout_reservation::endpoint(bind, config.identity, config.pin)?;
        let port = listener.local_addr()?.port();
        let task = tokio::spawn(serve(
            listener,
            input.clone(),
            Arc::from(config.pin),
            slot.clone(),
            mailbox,
            arm.request,
            arm.deadline,
        ));
        Ok((
            Self {
                request: arm.request,
                deadline: arm.deadline,
                slot,
                task,
                claimed: AtomicBool::new(false),
            },
            port,
        ))
    }
}

impl Drop for Target {
    fn drop(&mut self) {
        // A matched v5 grant moves the bounded reply into the local claim.
        // The route no longer owns whether that reply is sent.
        if !self.claimed.load(Ordering::Acquire) {
            self.task.abort();
            let _ = self.slot.clear();
        }
    }
}

async fn serve(
    listener: Endpoint,
    input: Connection,
    pin: Arc<[u8]>,
    slot: Arc<file_drop_link::WorkerSlot>,
    mailbox: Mailbox,
    request: u32,
    deadline: u64,
) {
    let remaining = Duration::from_millis(deadline.saturating_sub(now_ms()));
    let _ = tokio::time::timeout(remaining, async {
        while matches_arm(&mailbox.borrow(), request) {
            let incoming = tokio::select! {
                biased;
                _ = input.closed() => break,
                incoming = listener.accept() => incoming,
            };
            let Some(incoming) = incoming else {
                break;
            };
            if let Ok(Ok(sidecar)) = tokio::time::timeout(Duration::from_secs(1), incoming).await {
                let _ = file_drop_link::accept_worker(&sidecar, &input, &pin, &slot, || {
                    matches_arm(&mailbox.borrow(), request)
                })
                .await;
                let _ = tokio::time::timeout(Duration::from_secs(1), sidecar.closed()).await;
                sidecar.close(0u8.into(), b"file reservation finished");
            }
        }
    })
    .await;
    let _ = slot.clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_coordinates_keep_the_first_press_at_the_local_entry() {
        let point = |x, y| Motion {
            lease: 1,
            sequence: 1,
            x,
            y,
        };
        let placement = Placement {
            original: point(0.002, 0.8),
            entry: point(0.25, 0.5),
        };
        let initial = placement.map(placement.original);
        assert!((initial.x - 0.25).abs() < f64::EPSILON);
        assert!((initial.y - 0.5).abs() < f64::EPSILON);
        let low = placement.map(point(0.0, 0.0));
        let high = placement.map(point(1.0, 1.0));
        assert!(low.x.abs() < f64::EPSILON && low.y.abs() < f64::EPSILON);
        assert!((high.x - 1.0).abs() < f64::EPSILON && (high.y - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn local_commands_cannot_rearm_an_old_session_or_replay_a_request() -> Result<()> {
        let controller = Controller::default();
        assert!(controller.submit(Command::Inspect).is_none());
        let (first, mut old_mailbox) = controller.open()?;
        let old = first.session;
        let arm = |session| Command::Arm {
            session,
            request: 5,
            bind: "127.0.0.1:0".parse().unwrap(),
            lifetime_ms: 1000,
            proof: "ab".repeat(80),
        };
        controller.submit(arm(old));
        assert!(matches_arm(&old_mailbox.borrow_and_update(), 5));
        assert_eq!(
            controller
                .submit(Command::Cancel {
                    session: old,
                    request: 5
                })
                .unwrap()
                .state,
            State::Cancelled
        );
        controller.submit(arm(old));
        assert!(old_mailbox.borrow().is_none());
        drop(first);
        let (next, mailbox) = controller.open()?;
        assert_ne!(next.session, old);
        controller.submit(arm(old));
        assert!(mailbox.borrow().is_none());
        Ok(())
    }
}
