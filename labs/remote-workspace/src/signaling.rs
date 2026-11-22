//! Admit descriptions only for the current viewing grant and media attempt.
//! The control adapter must supply a verified TLS peer and typed control fields.
//! The media adapter must extract actual DTLS and ICE parameters from the same
//! description that it will apply. This model does not parse SDP or authenticate.

use crate::session::SessionGate;
use crate::{advance, Error, SurfaceKey};

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum Role {
    Offer,
    Answer,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum Direction {
    SendOnly,
    ReceiveOnly,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub struct MediaParameters {
    pub direction: Direction,
    pub dtls_sha256: [u8; 32],
    /// Digest of both ICE username and password using the adapter's canonical encoding.
    pub ice_credentials_digest: [u8; 32],
}

pub struct ControlDescription {
    pub surface: SurfaceKey,
    pub view_epoch: u64,
    pub generation: u64,
    pub role: Role,
    pub parameters: MediaParameters,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum State {
    Idle,
    AwaitingOffer,
    AwaitingAnswer,
    Ready,
}

/// This guard binds to one viewing grant. The grant owns the attempt counter
/// so recreating a guard after a media worker failure cannot reuse a generation.
pub struct Negotiation {
    surface: SurfaceKey,
    view_epoch: u64,
    generation: u64,
    state: State,
    last_now: u64,
}

impl Negotiation {
    pub fn new(gate: &SessionGate, now: u64) -> Result<Self, Error> {
        gate.can_view(now)?;
        Ok(Self {
            surface: gate.key(),
            view_epoch: gate.view_epoch(),
            generation: 0,
            state: State::Idle,
            last_now: now,
        })
    }

    /// Start or replace one media attempt without renewing viewing consent.
    pub fn begin(&mut self, gate: &mut SessionGate, now: u64) -> Result<u64, Error> {
        self.check_grant(gate, now)?;
        self.generation = gate.begin_media(now)?;
        self.state = State::AwaitingOffer;
        Ok(self.generation)
    }

    pub fn admit(
        &mut self,
        gate: &SessionGate,
        verified_peer: [u8; 32],
        control: &ControlDescription,
        actual: MediaParameters,
        now: u64,
    ) -> Result<(), Error> {
        self.check_grant(gate, now)?;
        if control.surface != self.surface || control.view_epoch != self.view_epoch {
            return Err(Error::StaleTarget);
        }
        if control.generation != self.generation || self.generation != gate.media_generation() {
            return Err(Error::InvalidSequence);
        }
        let (peer, direction, current, next) = match control.role {
            Role::Offer => (
                self.surface.binding.source,
                Direction::SendOnly,
                State::AwaitingOffer,
                State::AwaitingAnswer,
            ),
            Role::Answer => (
                self.surface.binding.viewer,
                Direction::ReceiveOnly,
                State::AwaitingAnswer,
                State::Ready,
            ),
        };
        if verified_peer != peer {
            return Err(Error::InvalidIdentity);
        }
        if actual != control.parameters
            || actual.direction != direction
            || actual.dtls_sha256 == [0; 32]
            || actual.ice_credentials_digest == [0; 32]
        {
            return Err(Error::PermissionDenied);
        }
        if self.state != current {
            return Err(Error::InvalidSequence);
        }
        self.state = next;
        Ok(())
    }

    /// Check again before admitting media. An accepted description is insufficient
    /// once the viewing grant has ended or the adapter has replaced the attempt.
    pub fn ready(&mut self, gate: &SessionGate, generation: u64, now: u64) -> Result<(), Error> {
        self.check_grant(gate, now)?;
        if self.generation != generation
            || generation != gate.media_generation()
            || self.state != State::Ready
        {
            return Err(Error::InvalidSequence);
        }
        Ok(())
    }

    fn check_grant(&mut self, gate: &SessionGate, now: u64) -> Result<(), Error> {
        gate.can_view(now)?;
        if gate.key() != self.surface || gate.view_epoch() != self.view_epoch {
            return Err(Error::StaleTarget);
        }
        advance(&mut self.last_now, now)
    }
}
