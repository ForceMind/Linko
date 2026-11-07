//! A local policy gate, not a network authentication or permission protocol.
use crate::{advance, Error, SurfaceKey};

pub const FRAME_FRESHNESS_MS: u64 = 2_000;
const MAX_GRANT_MS: u64 = 86_400_000;

/// Explicit scope of control. Window imagery does not imply window-only input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlScope {
    WholeDesktop,
    WindowOnly,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SurfaceKind {
    Display,
    VirtualDisplay,
    Window,
}

/// Process-local policy instance for ONE source and ONE authenticated viewer.
/// Deliberately not Clone/Serialize: snapshots must not preserve authorization.
/// The production adapter must check OS permission and current user consent.
pub struct SessionGate {
    key: SurfaceKey,
    kind: SurfaceKind,
    revision: u64,
    policy_epoch: u64,
    connected: bool,
    view_until: Option<u64>,
    control_until: Option<u64>,
    scope: Option<ControlScope>,
    window_input_supported: bool,
    last_frame: Option<(u64, u64)>,
    last_now: u64,
}

impl SessionGate {
    pub fn new(
        key: SurfaceKey,
        kind: SurfaceKind,
        revision: u64,
        window_input_supported: bool,
    ) -> Result<Self, Error> {
        key.validate()?;
        if revision == 0 || (window_input_supported && kind != SurfaceKind::Window) {
            return Err(Error::InvalidState);
        }
        Ok(Self {
            key,
            kind,
            revision,
            policy_epoch: 1,
            connected: true,
            view_until: None,
            control_until: None,
            scope: None,
            window_input_supported,
            last_frame: None,
            last_now: 0,
        })
    }

    pub fn key(&self) -> SurfaceKey {
        self.key
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn policy_epoch(&self) -> u64 {
        self.policy_epoch
    }

    pub fn control_scope(&self) -> Option<ControlScope> {
        self.scope
    }

    fn check_time(&mut self, now: u64) -> Result<(), Error> {
        advance(&mut self.last_now, now)
    }

    fn next_epoch(&mut self) -> Result<(), Error> {
        self.policy_epoch = self.policy_epoch.checked_add(1).ok_or(Error::Capacity)?;
        Ok(())
    }

    fn deadline(now: u64, duration: u64) -> Result<u64, Error> {
        if duration == 0 || duration > MAX_GRANT_MS {
            return Err(Error::InvalidTime);
        }
        now.checked_add(duration).ok_or(Error::InvalidTime)
    }

    /// Called only for explicit local consent. It does NOT grant input.
    /// Replacing consent invalidates existing control and the last rendered frame.
    pub fn grant_view(&mut self, now: u64, duration: u64) -> Result<(), Error> {
        self.check_time(now)?;
        if !self.connected {
            return Err(Error::Disconnected);
        }
        let until = Self::deadline(now, duration)?;
        self.next_epoch()?;
        self.view_until = Some(until);
        self.control_until = None;
        self.scope = None;
        self.last_frame = None;
        Ok(())
    }

    pub fn grant_control(
        &mut self,
        scope: ControlScope,
        now: u64,
        duration: u64,
    ) -> Result<(), Error> {
        self.check_time(now)?;
        self.can_view(now)?;
        let until = Self::deadline(now, duration)?;
        if until > self.view_until.unwrap_or(0) {
            return Err(Error::PermissionDenied);
        }
        if scope == ControlScope::WindowOnly
            && (self.kind != SurfaceKind::Window || !self.window_input_supported)
        {
            return Err(Error::PermissionDenied);
        }
        self.next_epoch()?;
        self.control_until = Some(until);
        self.scope = Some(scope);
        Ok(())
    }

    pub fn can_view(&self, now: u64) -> Result<(), Error> {
        if now < self.last_now {
            return Err(Error::InvalidTime);
        }
        if !self.connected {
            return Err(Error::Disconnected);
        }
        if !self.view_until.is_some_and(|until| now < until) {
            return Err(Error::PermissionDenied);
        }
        Ok(())
    }

    pub fn can_control(&self, now: u64) -> Result<(), Error> {
        self.can_view(now)?;
        if !self.control_until.is_some_and(|until| now < until) {
            return Err(Error::PermissionDenied);
        }
        if !self
            .last_frame
            .is_some_and(|(_, time)| now >= time && now - time < FRAME_FRESHNESS_MS)
        {
            return Err(Error::StaleFrame);
        }
        Ok(())
    }

    /// Record actual presentation acknowledgement, NOT just receipt of a packet.
    pub fn presented(&mut self, revision: u64, sequence: u64, now: u64) -> Result<(), Error> {
        self.check_time(now)?;
        self.can_view(now)?;
        if revision != self.revision {
            return Err(Error::StaleTarget);
        }
        if sequence == 0 || self.last_frame.is_some_and(|(last, _)| sequence <= last) {
            return Err(Error::InvalidSequence);
        }
        self.last_frame = Some((sequence, now));
        Ok(())
    }

    /// Resize/rotation invalidates old presentation and input tickets.
    pub fn revise_geometry(&mut self, revision: u64, now: u64) -> Result<(), Error> {
        self.check_time(now)?;
        if !self.connected || revision <= self.revision {
            return Err(Error::StaleTarget);
        }
        self.next_epoch()?;
        self.revision = revision;
        self.last_frame = None;
        self.control_until = None;
        self.scope = None;
        Ok(())
    }

    pub fn revoke_control(&mut self, now: u64) -> Result<(), Error> {
        self.check_time(now)?;
        self.control_until = None;
        self.scope = None;
        self.next_epoch()
    }

    pub fn revoke_all(&mut self, now: u64) -> Result<(), Error> {
        self.check_time(now)?;
        self.view_until = None;
        self.control_until = None;
        self.scope = None;
        self.last_frame = None;
        self.next_epoch()
    }

    /// Terminal for this session generation. Reconnect constructs a NEW gate
    /// with a fresh authenticated binding, not a reset of this object.
    pub fn disconnect(&mut self, now: u64) -> Result<(), Error> {
        self.revoke_all(now)?;
        self.connected = false;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture_key;

    pub(crate) fn gate() -> SessionGate {
        SessionGate::new(fixture_key(1), SurfaceKind::Window, 1, false).unwrap()
    }

    #[test]
    fn default_deny_and_view_is_not_control() {
        let mut g = gate();
        assert_eq!(g.can_view(0), Err(Error::PermissionDenied));
        g.grant_view(0, 10_000).unwrap();
        g.presented(1, 1, 1).unwrap();
        assert_eq!(g.can_control(1), Err(Error::PermissionDenied));
        g.grant_control(ControlScope::WholeDesktop, 1, 9_000).unwrap();
        assert_eq!(g.can_control(1), Ok(()));
    }

    #[test]
    fn window_capture_cannot_claim_window_input_isolation() {
        let mut g = gate();
        g.grant_view(0, 100).unwrap();
        assert_eq!(
            g.grant_control(ControlScope::WindowOnly, 0, 99),
            Err(Error::PermissionDenied)
        );
        assert_eq!(g.control_scope(), None);
    }

    #[test]
    fn exact_expiry_and_backwards_clock_fail_closed() {
        let mut g = gate();
        g.grant_view(10, 100).unwrap();
        assert_eq!(g.can_view(9), Err(Error::InvalidTime));
        assert_eq!(g.can_view(109), Ok(()));
        assert_eq!(g.can_view(110), Err(Error::PermissionDenied));
        assert_eq!(g.grant_view(9, 10), Err(Error::InvalidTime));
    }

    #[test]
    fn stale_or_unpresented_video_blocks_input() {
        let mut g = gate();
        g.grant_view(0, 10_000).unwrap();
        g.grant_control(ControlScope::WholeDesktop, 0, 10_000).unwrap();
        assert_eq!(g.can_control(0), Err(Error::StaleFrame));
        g.presented(1, 1, 1).unwrap();
        assert_eq!(g.can_control(2000), Ok(()));
        assert_eq!(g.can_control(2001), Err(Error::StaleFrame));
    }

    #[test]
    fn geometry_and_revocation_invalidate_prior_control() {
        let mut g = gate();
        g.grant_view(0, 100).unwrap();
        g.grant_control(ControlScope::WholeDesktop, 0, 99).unwrap();
        g.presented(1, 1, 1).unwrap();
        let epoch = g.policy_epoch();
        g.revise_geometry(2, 2).unwrap();
        assert!(g.policy_epoch() > epoch);
        assert_eq!(g.presented(1, 2, 3), Err(Error::StaleTarget));
        assert_eq!(g.can_control(3), Err(Error::PermissionDenied));
        g.revoke_all(4).unwrap();
        assert_eq!(g.can_view(4), Err(Error::PermissionDenied));
    }

    #[test]
    fn reconnect_cannot_reanimate_a_disconnected_gate() {
        let mut g = gate();
        g.grant_view(0, 100).unwrap();
        g.disconnect(1).unwrap();
        assert_eq!(g.grant_view(2, 100), Err(Error::Disconnected));
        assert_eq!(g.can_view(2), Err(Error::Disconnected));
    }

    #[test]
    fn invalid_identity_and_time_overflow_reject() {
        let mut key = fixture_key(1);
        key.binding.nonce = [0; 16];
        assert!(SessionGate::new(key, SurfaceKind::Display, 1, false).is_err());
        let mut g = gate();
        assert_eq!(g.grant_view(u64::MAX, 1), Err(Error::InvalidTime));
    }
}
