//! Executable, offline remote-workspace foundations, not a remote desktop server.
//!
//! No screen capture, native injection, sockets, global hooks, secrets, or disk
//! persistence. Identifiers are bindings, NOT authentication credentials. Native
//! adapters must obtain them from an authenticated session and independent user
//! consent. No existing PeerGlide v5 protocol or permission is extended here.
#![forbid(unsafe_code)]

pub mod geometry;
pub mod handoff;
pub mod media;
pub mod routing;
pub mod scheduling;
pub mod session;

/// Identity and connection generation supplied by the authenticated runtime.
/// Never construct production identities from fixture IDs, PIDs, or timestamps.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Binding {
    pub source: [u8; 32],
    pub viewer: [u8; 32],
    pub nonce: [u8; 16],
}

impl Binding {
    pub fn validate(self) -> Result<(), Error> {
        if self.source == [0; 32]
            || self.viewer == [0; 32]
            || self.source == self.viewer
            || self.nonce == [0; 16]
        {
            return Err(Error::InvalidIdentity);
        }
        Ok(())
    }
}

/// Ephemeral surface identity, invalidated on source recreation/reconnection.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct SurfaceKey {
    pub binding: Binding,
    pub object: u64,
    pub generation: u64,
}

impl SurfaceKey {
    pub fn validate(self) -> Result<(), Error> {
        self.binding.validate()?;
        if self.object == 0 || self.generation == 0 {
            return Err(Error::InvalidIdentity);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    InvalidIdentity,
    InvalidGeometry,
    InvalidTime,
    InvalidState,
    InvalidSequence,
    PermissionDenied,
    Disconnected,
    StaleFrame,
    StaleTarget,
    OutsideContent,
    ReleasePending,
    WrongAcknowledgement,
    Capacity,
    InvalidFrame,
    TruncatedFrame,
    UnsupportedVersion,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for Error {}

/// Advance a process-local monotonic clock watermark. Wall-clock time is invalid.
pub(crate) fn advance(last: &mut u64, now: u64) -> Result<(), Error> {
    if now < *last {
        return Err(Error::InvalidTime);
    }
    *last = now;
    Ok(())
}

#[cfg(test)]
pub(crate) fn fixture_key(seed: u8) -> SurfaceKey {
    SurfaceKey {
        binding: Binding {
            source: [seed; 32],
            viewer: [255; 32],
            nonce: [seed; 16],
        },
        object: 1,
        generation: 1,
    }
}
