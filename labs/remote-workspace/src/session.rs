//! Local policy gates, not network authentication or an OS permission backend.
use crate::{advance, Error, SurfaceKey};

pub const FRAME_FRESHNESS_MS: u64 = 2_000;
pub const CONTENT_CHECK_MS: u64 = 1_000;
const MAX_GRANT_MS: u64 = 86_400_000;

/// Window imagery alone does not imply window-only input isolation.
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

/// Metadata bound to one viewing consent. This is NOT an authentication token.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrameStamp {
    pub key: SurfaceKey,
    pub view_epoch: u64,
    pub revision: u64,
    pub sequence: u64,
}

/// Process-local challenge. The transport sends only request_id + stamp and
/// verifies the authenticated SOURCE response. Deadline is viewer-local, not a
/// serialized timestamp or a clock shared between devices.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContentCheck {
    request_id: u64,
    stamp: FrameStamp,
    deadline: u64,
}

impl ContentCheck {
    pub fn request_id(self) -> u64 {
        self.request_id
    }

    pub fn stamp(self) -> FrameStamp {
        self.stamp
    }
}

/// One source and one authenticated viewer, with independent view/input consent.
/// Deliberately not Clone/Serialize: saving a workspace must not save authority.
/// The caller must supply verified OS permission and explicit user decisions.
pub struct SessionGate {
    key: SurfaceKey,
    kind: SurfaceKind,
    revision: u64,
    policy_epoch: u64,
    view_epoch: u64,
    connected: bool,
    view_until: Option<u64>,
    control_until: Option<u64>,
    scope: Option<ControlScope>,
    window_input_supported: bool,
    last_frame: Option<(u64, u64)>,
    pending_check: Option<ContentCheck>,
    check_sequence: u64,
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
            view_epoch: 1,
            connected: true,
            view_until: None,
            control_until: None,
            scope: None,
            window_input_supported,
            last_frame: None,
            pending_check: None,
            check_sequence: 0,
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

    pub fn view_epoch(&self) -> u64 {
        self.view_epoch
    }

    pub fn control_scope(&self) -> Option<ControlScope> {
        self.scope
    }

    /// Label a frame; this does not authorize capture, transmission, or viewing.
    pub fn frame_stamp(&self, sequence: u64) -> FrameStamp {
        FrameStamp {
            key: self.key,
            view_epoch: self.view_epoch,
            revision: self.revision,
            sequence,
        }
    }

    fn matches_stamp(&self, stamp: FrameStamp) -> bool {
        stamp.key == self.key
            && stamp.view_epoch == self.view_epoch
            && stamp.revision == self.revision
            && stamp.sequence != 0
    }

    fn check_time(&mut self, now: u64) -> Result<(), Error> {
        advance(&mut self.last_now, now)
    }

    fn next_epoch(&mut self) -> Result<(), Error> {
        self.policy_epoch = self.policy_epoch.checked_add(1).ok_or(Error::Capacity)?;
        Ok(())
    }

    fn next_view_epoch(&mut self) -> Result<(), Error> {
        let view = self.view_epoch.checked_add(1).ok_or(Error::Capacity)?;
        self.next_epoch()?;
        self.view_epoch = view;
        Ok(())
    }

    fn deadline(now: u64, duration: u64) -> Result<u64, Error> {
        if duration == 0 || duration > MAX_GRANT_MS {
            return Err(Error::InvalidTime);
        }
        now.checked_add(duration).ok_or(Error::InvalidTime)
    }

    /// Explicit local consent only. Re-consenting invalidates buffered old video
    /// even when the device, connection, object, and dimensions did not change.
    pub fn grant_view(&mut self, now: u64, duration: u64) -> Result<(), Error> {
        self.check_time(now)?;
        if !self.connected {
            return Err(Error::Disconnected);
        }
        let until = Self::deadline(now, duration)?;
        self.next_view_epoch()?;
        self.view_until = Some(until);
        self.control_until = None;
        self.scope = None;
        self.last_frame = None;
        self.pending_check = None;
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
        if self.view_until.is_none_or(|until| now >= until) {
            return Err(Error::PermissionDenied);
        }
        Ok(())
    }

    pub fn can_control(&self, now: u64) -> Result<(), Error> {
        self.can_view(now)?;
        if self.control_until.is_none_or(|until| now >= until) {
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

    /// Actual presentation acknowledgement, not merely receipt of a packet.
    /// Repainting the same old texture cannot refresh source liveness.
    pub fn presented(&mut self, stamp: FrameStamp, now: u64) -> Result<(), Error> {
        self.check_time(now)?;
        self.can_view(now)?;
        if !self.matches_stamp(stamp) {
            return Err(Error::StaleTarget);
        }
        if self
            .last_frame
            .is_some_and(|(last, _)| stamp.sequence <= last)
        {
            return Err(Error::InvalidSequence);
        }
        self.last_frame = Some((stamp.sequence, now));
        self.pending_check = None;
        Ok(())
    }

    /// A static desktop may generate no new frames. Ask its SOURCE to confirm
    /// that capture is healthy and the currently presented content is unchanged.
    /// A generic TCP/QUIC heartbeat or a local redraw is not such confirmation.
    pub fn request_content_check(&mut self, now: u64) -> Result<ContentCheck, Error> {
        self.check_time(now)?;
        self.can_view(now)?;
        if self.pending_check.is_some_and(|check| now < check.deadline) {
            return Err(Error::InvalidState);
        }
        let (sequence, _) = self.last_frame.ok_or(Error::StaleFrame)?;
        let request_id = self.check_sequence.checked_add(1).ok_or(Error::Capacity)?;
        let check = ContentCheck {
            request_id,
            stamp: self.frame_stamp(sequence),
            deadline: now
                .checked_add(CONTENT_CHECK_MS)
                .ok_or(Error::InvalidTime)?,
        };
        self.check_sequence = request_id;
        self.pending_check = Some(check);
        Ok(check)
    }

    /// Call only after the trusted source confirmed the exact challenge/content.
    /// The request counter is a correlation ID, not cryptographic authentication.
    pub fn confirm_unchanged(&mut self, check: ContentCheck, now: u64) -> Result<(), Error> {
        self.check_time(now)?;
        self.can_view(now)?;
        if self.pending_check != Some(check)
            || !self.matches_stamp(check.stamp)
            || self.last_frame.map(|(sequence, _)| sequence) != Some(check.stamp.sequence)
        {
            return Err(Error::StaleTarget);
        }
        self.pending_check = None;
        if now >= check.deadline {
            return Err(Error::StaleFrame);
        }
        self.last_frame = Some((check.stamp.sequence, now));
        Ok(())
    }

    /// Resize/rotation invalidates presentation, input tickets and challenges.
    pub fn revise_geometry(&mut self, revision: u64, now: u64) -> Result<(), Error> {
        self.check_time(now)?;
        if !self.connected || revision <= self.revision {
            return Err(Error::StaleTarget);
        }
        self.next_epoch()?;
        self.revision = revision;
        self.last_frame = None;
        self.pending_check = None;
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
        self.pending_check = None;
        self.next_view_epoch()
    }

    /// Terminal instance. Reconnect needs a fresh authenticated binding and gate.
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

    fn gate() -> SessionGate {
        SessionGate::new(fixture_key(1), SurfaceKind::Window, 1, false).unwrap()
    }

    fn live() -> SessionGate {
        let mut g = gate();
        g.grant_view(0, 10_000).unwrap();
        g.grant_control(ControlScope::WholeDesktop, 0, 10_000)
            .unwrap();
        g.presented(g.frame_stamp(1), 0).unwrap();
        g
    }

    #[test]
    fn default_deny_and_view_is_not_control() {
        let mut g = gate();
        assert_eq!(g.can_view(0), Err(Error::PermissionDenied));
        g.grant_view(0, 10_000).unwrap();
        g.presented(g.frame_stamp(1), 1).unwrap();
        assert_eq!(g.can_control(1), Err(Error::PermissionDenied));
        g.grant_control(ControlScope::WholeDesktop, 1, 9_000)
            .unwrap();
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
        g.grant_control(ControlScope::WholeDesktop, 0, 10_000)
            .unwrap();
        assert_eq!(g.can_control(0), Err(Error::StaleFrame));
        g.presented(g.frame_stamp(1), 1).unwrap();
        assert_eq!(g.can_control(2000), Ok(()));
        assert_eq!(g.can_control(2001), Err(Error::StaleFrame));
    }

    #[test]
    fn geometry_and_revocation_invalidate_prior_control() {
        let mut g = live();
        let stamp = g.frame_stamp(2);
        let epoch = g.policy_epoch();
        g.revise_geometry(2, 2).unwrap();
        assert!(g.policy_epoch() > epoch);
        assert_eq!(g.presented(stamp, 3), Err(Error::StaleTarget));
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

    #[test]
    fn reconsent_cannot_present_buffered_old_frame() {
        let mut g = live();
        let old = g.frame_stamp(2);
        g.revoke_all(1).unwrap();
        g.grant_view(2, 100).unwrap();
        assert_eq!(g.presented(old, 3), Err(Error::StaleTarget));
        g.presented(g.frame_stamp(1), 4).unwrap();
    }

    #[test]
    fn input_consent_does_not_invalidate_video_consent() {
        let mut g = gate();
        g.grant_view(0, 100).unwrap();
        let stamp = g.frame_stamp(1);
        g.grant_control(ControlScope::WholeDesktop, 0, 100).unwrap();
        assert_eq!(stamp.view_epoch, g.view_epoch());
        g.presented(stamp, 1).unwrap();
    }

    #[test]
    fn healthy_static_screen_can_confirm_without_replaying_a_frame() {
        let mut g = live();
        let check = g.request_content_check(1500).unwrap();
        g.confirm_unchanged(check, 1600).unwrap();
        assert_eq!(g.can_control(3500), Ok(()));
        assert_eq!(g.can_control(3600), Err(Error::StaleFrame));
        assert_eq!(g.confirm_unchanged(check, 3601), Err(Error::StaleTarget));
    }

    #[test]
    fn stale_challenge_and_local_repaint_cannot_refresh_capture_liveness() {
        let mut g = live();
        let check = g.request_content_check(10).unwrap();
        assert_eq!(g.request_content_check(11), Err(Error::InvalidState));
        assert_eq!(g.confirm_unchanged(check, 1010), Err(Error::StaleFrame));
        assert_eq!(
            g.presented(g.frame_stamp(1), 1011),
            Err(Error::InvalidSequence)
        );
        assert_eq!(g.can_control(2000), Err(Error::StaleFrame));
    }

    #[test]
    fn new_content_or_consent_invalidates_outstanding_confirmation() {
        let mut g = live();
        let check = g.request_content_check(10).unwrap();
        g.presented(g.frame_stamp(2), 11).unwrap();
        assert_eq!(g.confirm_unchanged(check, 12), Err(Error::StaleTarget));
        let check = g.request_content_check(13).unwrap();
        g.grant_view(14, 100).unwrap();
        assert_eq!(g.confirm_unchanged(check, 15), Err(Error::StaleTarget));
    }
}
