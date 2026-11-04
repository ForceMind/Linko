//! Session-local data permissions. This is not TLS authentication or a wire protocol.
//! The adapter must supply verified identities and a fresh random session nonce.
use crate::Digest;

pub const MAX_GRANT_MS: u64 = 60 * 60 * 1000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Capability {
    TextSend,
    TextReceive,
    FileSend,
    FileReceive,
}

#[derive(Clone, Copy, Default)]
pub struct Permissions {
    pub text_send: bool,
    pub text_receive: bool,
    pub file_send: bool,
    pub file_receive: bool,
}
impl Permissions {
    fn allows(self, capability: Capability) -> bool {
        match capability {
            Capability::TextSend => self.text_send,
            Capability::TextReceive => self.text_receive,
            Capability::FileSend => self.file_send,
            Capability::FileReceive => self.file_receive,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Denied {
    NoConsent,
    Expired,
    StalePermit,
    WrongSession,
    ClockRegression,
    InvalidLifetime,
    Exhausted,
}

/// Contains no public constructor: obtain after the local consent check.
/// Do not serialize, persist, or send this local ticket to a peer.
pub struct Permit {
    peer: Digest,
    nonce: Digest,
    epoch: u64,
    capability: Capability,
}

pub struct DataSession {
    peer: Digest,
    nonce: Digest,
    permissions: Permissions,
    epoch: u64,
    deadline: u64,
    last_now: u64,
    closed: bool,
}
impl DataSession {
    /// Call only after TLS pin verification. Values are bindings, not credentials.
    /// Reconnecting must construct a fresh object with a fresh random nonce.
    pub fn new(peer: Digest, nonce: Digest, now: u64) -> Self {
        Self {
            peer,
            nonce,
            permissions: Permissions::default(),
            epoch: 0,
            deadline: now,
            last_now: now,
            closed: false,
        }
    }

    fn tick(&mut self, now: u64) -> Result<(), Denied> {
        if now < self.last_now {
            self.close();
            return Err(Denied::ClockRegression);
        }
        self.last_now = now;
        if self.closed {
            return Err(Denied::NoConsent);
        }
        Ok(())
    }

    /// Explicit local consent replacement invalidates every queued old ticket.
    /// Input permission and remote requests must never call this as consent.
    pub fn consent(
        &mut self,
        permissions: Permissions,
        lifetime_ms: u64,
        now: u64,
    ) -> Result<(), Denied> {
        self.tick(now)?;
        // Invalid replacement revokes the old permission too.
        self.permissions = Permissions::default();
        self.deadline = now;
        self.epoch = self.epoch.checked_add(1).ok_or_else(|| {
            self.closed = true;
            Denied::Exhausted
        })?;
        if lifetime_ms == 0 || lifetime_ms > MAX_GRANT_MS {
            return Err(Denied::InvalidLifetime);
        }
        let deadline = now.checked_add(lifetime_ms).ok_or(Denied::InvalidLifetime)?;
        self.permissions = permissions;
        self.deadline = deadline;
        Ok(())
    }

    pub fn revoke(&mut self) {
        self.permissions = Permissions::default();
        self.deadline = 0;
        if let Some(next) = self.epoch.checked_add(1) {
            self.epoch = next;
        } else {
            self.closed = true;
        }
    }

    /// Disconnect is terminal; a caller cannot re-consent the same session.
    pub fn close(&mut self) {
        self.revoke();
        self.closed = true;
    }

    pub fn authorize(&mut self, capability: Capability, now: u64) -> Result<Permit, Denied> {
        self.tick(now)?;
        if now >= self.deadline {
            return Err(Denied::Expired);
        }
        if !self.permissions.allows(capability) {
            return Err(Denied::NoConsent);
        }
        Ok(Permit {
            peer: self.peer,
            nonce: self.nonce,
            epoch: self.epoch,
            capability,
        })
    }

    /// Recheck immediately before every disk/OS/network side effect.
    pub fn validate(
        &mut self,
        permit: &Permit,
        capability: Capability,
        now: u64,
    ) -> Result<(), Denied> {
        self.tick(now)?;
        if permit.peer != self.peer || permit.nonce != self.nonce {
            return Err(Denied::WrongSession);
        }
        if permit.epoch != self.epoch || permit.capability != capability {
            return Err(Denied::StalePermit);
        }
        if now >= self.deadline {
            return Err(Denied::Expired);
        }
        if !self.permissions.allows(capability) {
            return Err(Denied::NoConsent);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn receive() -> Permissions {
        Permissions {
            text_receive: true,
            ..Permissions::default()
        }
    }

    #[test]
    fn independent_direction_expiry_and_replacement() {
        let mut session = DataSession::new([1; 32], [2; 32], 0);
        assert!(session.authorize(Capability::TextReceive, 0).is_err());
        session.consent(receive(), 10, 1).unwrap();
        assert!(session.authorize(Capability::TextSend, 2).is_err());
        assert!(session.authorize(Capability::FileReceive, 2).is_err());
        let permit = session.authorize(Capability::TextReceive, 2).unwrap();
        assert_eq!(session.validate(&permit, Capability::TextReceive, 10), Ok(()));
        assert_eq!(
            session.validate(&permit, Capability::TextReceive, 11),
            Err(Denied::Expired)
        );
        session.consent(receive(), 10, 11).unwrap();
        assert_eq!(
            session.validate(&permit, Capability::TextReceive, 11),
            Err(Denied::StalePermit)
        );
    }

    #[test]
    fn foreign_session_revoke_close_and_clock_regression() {
        let mut first = DataSession::new([1; 32], [2; 32], 0);
        let mut second = DataSession::new([1; 32], [3; 32], 0);
        first.consent(receive(), 100, 0).unwrap();
        second.consent(receive(), 100, 0).unwrap();
        let permit = first.authorize(Capability::TextReceive, 1).unwrap();
        assert_eq!(
            second.validate(&permit, Capability::TextReceive, 1),
            Err(Denied::WrongSession)
        );
        first.revoke();
        assert!(first.validate(&permit, Capability::TextReceive, 1).is_err());
        first.consent(receive(), 100, 2).unwrap();
        assert!(matches!(
            first.authorize(Capability::TextReceive, 1),
            Err(Denied::ClockRegression)
        ));
        assert_eq!(first.consent(receive(), 100, 2), Err(Denied::NoConsent));
        second.close();
        assert_eq!(second.consent(receive(), 100, 2), Err(Denied::NoConsent));
    }

    #[test]
    fn invalid_replacement_fails_closed_and_overflow_is_rejected() {
        let mut session = DataSession::new([1; 32], [2; 32], 0);
        session.consent(receive(), 10, 0).unwrap();
        let permit = session.authorize(Capability::TextReceive, 0).unwrap();
        assert_eq!(session.consent(receive(), 0, 1), Err(Denied::InvalidLifetime));
        assert!(session.validate(&permit, Capability::TextReceive, 1).is_err());
        let mut end = DataSession::new([1; 32], [2; 32], u64::MAX - 1);
        assert_eq!(
            end.consent(receive(), 10, u64::MAX - 1),
            Err(Denied::InvalidLifetime)
        );
    }
}
