//! Failure-aware source-window visibility transaction. Commands are intentions;
//! native effects and restoration require explicit acknowledgements from adapters.
use crate::geometry::Rect;
use crate::session::SessionGate;
use crate::{advance, Error, SurfaceKey};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum State {
    Preparing,
    HideRequested,
    Remote,
    RestoreRequested,
    Local,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Command {
    HideSource,
    RestoreSource(Rect),
}

pub struct Handoff {
    key: SurfaceKey,
    attempt: u64,
    original_bounds: Rect,
    state: State,
    deadline: u64,
    last_now: u64,
    accepted_context: Option<(u64, u64)>,
}

impl Handoff {
    pub fn begin(
        key: SurfaceKey,
        attempt: u64,
        original_bounds: Rect,
        now: u64,
        timeout: u64,
    ) -> Result<Self, Error> {
        key.validate()?;
        original_bounds.validate()?;
        if attempt == 0 || timeout == 0 || timeout > 30_000 {
            return Err(Error::InvalidState);
        }
        let deadline = now.checked_add(timeout).ok_or(Error::InvalidTime)?;
        Ok(Self {
            key,
            attempt,
            original_bounds,
            state: State::Preparing,
            deadline,
            last_now: now,
            accepted_context: None,
        })
    }

    pub fn state(&self) -> State {
        self.state
    }

    /// Pending intent, not authorization to execute an OS action. The adapter
    /// MUST call revalidate immediately before hiding and from its watchdog.
    /// Restore remains available even when all remote authorization is revoked.
    pub fn command(&self) -> Option<Command> {
        match self.state {
            State::HideRequested => Some(Command::HideSource),
            State::RestoreRequested => Some(Command::RestoreSource(self.original_bounds)),
            _ => None,
        }
    }

    /// Only after real destination presentation and independent control consent.
    /// Authenticity of the destination ACK belongs to the transport adapter.
    pub fn destination_ready(
        &mut self,
        gate: &SessionGate,
        attempt: u64,
        now: u64,
    ) -> Result<(), Error> {
        self.tick(now)?;
        if self.state != State::Preparing || gate.key() != self.key || attempt != self.attempt {
            return Err(Error::StaleTarget);
        }
        gate.can_control(now)?;
        self.accepted_context = Some((gate.policy_epoch(), gate.revision()));
        self.state = State::HideRequested;
        Ok(())
    }

    pub fn acknowledge_hidden(
        &mut self,
        gate: &SessionGate,
        attempt: u64,
        now: u64,
    ) -> Result<(), Error> {
        self.revalidate(gate, now)?;
        if attempt != self.attempt || self.state != State::HideRequested {
            return Err(Error::WrongAcknowledgement);
        }
        self.state = State::Remote;
        Ok(())
    }

    /// If hiding may have happened, do not claim local recovery without an OS ACK.
    pub fn cancel(&mut self) {
        self.state = match self.state {
            State::Preparing | State::Local => State::Local,
            _ => State::RestoreRequested,
        };
    }

    pub fn acknowledge_restored(&mut self, attempt: u64) -> Result<(), Error> {
        if attempt != self.attempt || self.state != State::RestoreRequested {
            return Err(Error::WrongAcknowledgement);
        }
        self.state = State::Local;
        Ok(())
    }

    pub fn tick(&mut self, now: u64) -> Result<(), Error> {
        advance(&mut self.last_now, now)?;
        if now >= self.deadline && matches!(self.state, State::Preparing | State::HideRequested) {
            self.cancel();
        }
        Ok(())
    }

    /// Poll while idle, and immediately before native effects. Permission or
    /// geometry replacement cannot re-authorize an already pending transaction.
    pub fn revalidate(&mut self, gate: &SessionGate, now: u64) -> Result<(), Error> {
        self.tick(now)?;
        if gate.key() != self.key {
            return Err(Error::StaleTarget);
        }
        if self
            .accepted_context
            .is_some_and(|context| context != (gate.policy_epoch(), gate.revision()))
        {
            self.cancel();
            return Err(Error::StaleTarget);
        }
        if let Err(error) = gate.can_control(now) {
            self.cancel();
            return Err(error);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture_key;
    use crate::session::{ControlScope, SurfaceKind};

    fn gate() -> SessionGate {
        let mut g = SessionGate::new(fixture_key(1), SurfaceKind::Window, 1, false).unwrap();
        g.grant_view(0, 10_000).unwrap();
        g.grant_control(ControlScope::WholeDesktop, 0, 10_000)
            .unwrap();
        g.presented(g.frame_stamp(1), 0).unwrap();
        g
    }

    fn handoff() -> Handoff {
        Handoff::begin(
            fixture_key(1),
            7,
            Rect {
                x: -100.0,
                y: 10.0,
                width: 800.0,
                height: 600.0,
            },
            0,
            1000,
        )
        .unwrap()
    }

    #[test]
    fn never_hide_before_destination_is_ready() {
        let g = gate();
        let mut h = handoff();
        assert_eq!(h.command(), None);
        h.destination_ready(&g, 7, 1).unwrap();
        assert_eq!(h.command(), Some(Command::HideSource));
        h.acknowledge_hidden(&g, 7, 2).unwrap();
        assert_eq!(h.state(), State::Remote);
    }

    #[test]
    fn failure_after_hide_request_retains_restore_until_ack() {
        let mut h = handoff();
        h.destination_ready(&gate(), 7, 1).unwrap();
        h.cancel();
        let command = h.command();
        assert!(matches!(command, Some(Command::RestoreSource(_))));
        h.cancel();
        assert_eq!(h.command(), command);
        assert_eq!(h.acknowledge_restored(6), Err(Error::WrongAcknowledgement));
        assert_eq!(h.command(), command);
        h.acknowledge_restored(7).unwrap();
        assert_eq!(h.state(), State::Local);
    }

    #[test]
    fn timeout_before_ready_keeps_source_local() {
        let mut h = handoff();
        h.tick(1000).unwrap();
        assert_eq!(h.state(), State::Local);
        assert_eq!(h.command(), None);
        assert_eq!(
            h.destination_ready(&gate(), 7, 1000),
            Err(Error::StaleTarget)
        );
    }

    #[test]
    fn timeout_after_request_cannot_accept_late_hide_ack() {
        let g = gate();
        let mut h = handoff();
        h.destination_ready(&g, 7, 1).unwrap();
        assert_eq!(
            h.acknowledge_hidden(&g, 7, 1000),
            Err(Error::WrongAcknowledgement)
        );
        assert_eq!(h.state(), State::RestoreRequested);
    }

    #[test]
    fn disconnect_requires_native_restore_not_just_success_message() {
        let mut g = gate();
        let mut h = handoff();
        h.destination_ready(&g, 7, 1).unwrap();
        h.acknowledge_hidden(&g, 7, 2).unwrap();
        g.disconnect(3).unwrap();
        assert_eq!(h.revalidate(&g, 3), Err(Error::StaleTarget));
        assert_eq!(h.state(), State::RestoreRequested);
    }

    #[test]
    fn wrong_attempt_does_not_hide_source() {
        let mut h = handoff();
        assert_eq!(h.destination_ready(&gate(), 8, 1), Err(Error::StaleTarget));
        assert_eq!(h.state(), State::Preparing);
    }

    #[test]
    fn revoked_then_regranted_control_cannot_resurrect_handoff() {
        let mut g = gate();
        let mut h = handoff();
        h.destination_ready(&g, 7, 1).unwrap();
        g.revoke_control(2).unwrap();
        g.grant_control(ControlScope::WholeDesktop, 3, 100).unwrap();
        assert_eq!(h.acknowledge_hidden(&g, 7, 4), Err(Error::StaleTarget));
        assert_eq!(h.state(), State::RestoreRequested);
    }

    #[test]
    fn geometry_change_before_hide_ack_requires_restoration() {
        let mut g = gate();
        let mut h = handoff();
        h.destination_ready(&g, 7, 1).unwrap();
        g.revise_geometry(2, 2).unwrap();
        assert_eq!(h.acknowledge_hidden(&g, 7, 3), Err(Error::StaleTarget));
        assert!(matches!(h.command(), Some(Command::RestoreSource(_))));
    }
}
