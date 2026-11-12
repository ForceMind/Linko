//! Local supervisor approval and one lease-bound target reservation.
use anyhow::{ensure, Context, Result};
use peerglide_platform::now_ms;
use peerglide_protocol::file_drop_ipc::{Command, Event, State};
use peerglide_transport::{file_drop_link, layout_reservation, Identity};
use quinn::{Connection, Endpoint};
use std::{net::SocketAddr, sync::{Arc, Mutex}, time::Duration};
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
}

pub(super) struct Arm {
    request: u32,
    bind: SocketAddr,
    proof: Mutex<Option<String>>,
    deadline: u64,
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
        let mut state = self.0.lock().map_err(|_| anyhow::anyhow!("file control unavailable"))?;
        ensure!(state.active.is_none(), "file control session already active");
        let session = state.sequence.checked_add(1).context("file control sequence exhausted")?;
        let (sender, receiver) = watch::channel(None);
        state.sequence = session;
        state.active = Some(Active { session, watermark: 0, sender });
        Ok((Scope { controller: self, session }, receiver))
    }

    pub(super) fn submit(&self, command: Command) -> Option<Event> {
        let mut state = self.0.lock().ok()?;
        let active = state.active.as_mut()?;
        match command {
            Command::Inspect => Some(Event::new(active.session, 0, State::Session, None)),
            Command::Arm { session, request, bind, lifetime_ms, proof }
                if session == active.session && request > active.watermark =>
            {
                active.watermark = request;
                let deadline = now_ms().saturating_add(u64::from(lifetime_ms));
                active.sender.send_replace(Some(Arc::new(Arm {
                    request, bind, proof: Mutex::new(Some(proof)), deadline,
                })));
                None
            }
            Command::Cancel { session, request } if session == active.session => {
                let matches = active.sender.borrow().as_ref().is_some_and(|arm| arm.request == request);
                if !matches {
                    return None;
                }
                active.sender.send_replace(None);
                Some(Event::new(session, request, State::Cancelled, None))
            }
            _ => None,
        }
    }
}

impl Scope<'_> {
    pub(super) fn event(&self, request: u32, state: State) -> Event {
        Event::new(self.session, request, state, None)
    }

    /// Called only after the normal v5 receive grant. The supervisor can
    /// revoke the arm even while the peer's sidecar reply is in flight.
    pub(super) fn resolve(
        &self,
        route: &Route,
        input: &Connection,
        pin: &[u8],
        lease: u64,
        granted: bool,
    ) -> Option<Event> {
        let mut state = self.controller.0.lock().ok()?;
        let active = state.active.as_mut()?;
        let current = active.session == self.session
            && matches_arm(&active.sender.borrow(), route.request);
        let matched = if granted {
            route.slot.claim(input, pin, lease, current)
        } else {
            route.slot.retire_denied(input, pin, lease, current)
        };
        if !matched.ok()? {
            return None;
        }
        active.sender.send_replace(None);
        Some(if granted {
            Event::new(self.session, route.request, State::Claimed, Some(lease))
        } else {
            self.event(route.request, State::Rejected)
        })
    }
}

impl Drop for Scope<'_> {
    fn drop(&mut self) {
        if let Ok(mut state) = self.controller.0.lock() {
            if state.active.as_ref().is_some_and(|active| active.session == self.session) {
                let Some(active) = state.active.take() else { return; };
                active.sender.send_replace(None);
            }
        }
    }
}

fn matches_arm(arm: &Option<Arc<Arm>>, request: u32) -> bool {
    arm.as_ref().is_some_and(|arm| arm.request == request && now_ms() < arm.deadline)
}

pub(super) struct Route {
    request: u32,
    slot: Arc<file_drop_link::WorkerSlot>,
    task: JoinHandle<()>,
}

impl Route {
    pub(super) fn update(
        scope: &Scope<'_>,
        mailbox: &mut Mailbox,
        config: Config<'_>,
        input: &Connection,
    ) -> (Option<Self>, Option<Event>) {
        let Some(arm) = mailbox.borrow_and_update().clone() else { return (None, None); };
        let result = Self::bind(&arm, mailbox.clone(), config, input);
        if !matches_arm(&mailbox.borrow(), arm.request) {
            return (None, None);
        }
        match result {
            Ok((route, port)) => (Some(route), Some(Event::armed(scope.session, arm.request, port))),
            Err(_) => (None, Some(scope.event(arm.request, State::Rejected))),
        }
    }

    fn bind(arm: &Arm, mailbox: Mailbox, config: Config<'_>, input: &Connection) -> Result<(Self, u16)> {
        let proof = arm.proof.lock().ok().and_then(|mut proof| proof.take()).context("file reservation proof unavailable")?;
        ensure!(!arm.bind.ip().is_multicast(), "invalid file reservation bind address");
        let remaining = u32::try_from(arm.deadline.saturating_sub(now_ms()))?;
        let slot = Arc::new(file_drop_link::WorkerSlot::from_local_arm(&proof, remaining, config.pin)?);
        let listener = layout_reservation::endpoint(arm.bind, config.identity, config.pin)?;
        let port = listener.local_addr()?.port();
        let task = tokio::spawn(serve(listener, input.clone(), Arc::from(config.pin), slot.clone(), mailbox, arm.request, arm.deadline));
        Ok((Self { request: arm.request, slot, task }, port))
    }
}

impl Drop for Route {
    fn drop(&mut self) {
        self.task.abort();
        let _ = self.slot.clear();
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
        let Some(incoming) = incoming else { break; };
        if let Ok(Ok(sidecar)) = tokio::time::timeout(Duration::from_secs(1), incoming).await {
            let _ = file_drop_link::accept_worker(&sidecar, &input, &pin, &slot, || {
                matches_arm(&mailbox.borrow(), request)
            }).await;
            let _ = tokio::time::timeout(Duration::from_secs(1), sidecar.closed()).await;
            sidecar.close(0u8.into(), b"file reservation finished");
        }
      }
    }).await;
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
        assert_eq!(controller.submit(Command::Cancel { session: old, request: 5 }).unwrap().state, State::Cancelled);
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
