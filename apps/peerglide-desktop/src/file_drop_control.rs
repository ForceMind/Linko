//! Local supervisor approval for a lease-bound file reservation.
use anyhow::{ensure, Context, Result};
use peerglide_platform::{now_ms, CaptureGate, HeldPrimary};
use peerglide_protocol::file_drop_ipc::{Command, Event, State};
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
}

struct LocalClaim {
    request: u32,
    lease: u64,
    proof: file_drop_link::PendingWorkerClaim,
    ready: bool,
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
                let claim = active.claim.as_mut().filter(|claim| {
                    session == active.session
                        && claim.request == request
                        && claim.lease == lease
                        && claim.proof.is_live()
                })?;
                if !claim.ready {
                    claim.ready = true;
                    active.sender.send_modify(|_| {});
                }
                return None;
            }
            Command::Cancel { session, request } => {
                let pending = active.claim.as_ref().is_some_and(|claim| claim.request == request);
                let armed = active
                    .sender
                    .borrow()
                    .as_ref()
                    .is_some_and(|arm| arm.request == request);
                if session != active.session || !(armed || pending) {
                    return None;
                }
                active.claim = None;
                active.sender.send_replace(None);
                return Some(Event::new(session, request, State::Cancelled, None));
            }
        };
        if session == active.session && request > active.watermark {
            active.watermark = request;
            active.claim = None;
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
                ready: false,
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

    pub(super) fn advance_claim(&self, live_lease: Option<u64>) -> Option<Event> {
        let mut state = self.controller.0.lock().ok()?;
        let active = state.active.as_mut().filter(|active| active.session == self.session)?;
        let claim = active.claim.as_ref()?;
        let live = live_lease == Some(claim.lease) && claim.proof.is_live();
        if live && !claim.ready {
            return None;
        }
        let claim = active.claim.take()?;
        Some(if live && claim.proof.confirm() {
            Event::new(self.session, claim.request, State::Claimed, Some(claim.lease))
        } else {
            self.event(claim.request, State::Rejected)
        })
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
