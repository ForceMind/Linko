//! Reference control lease for one source shared by multiple viewers.
use std::sync::Arc;

use crate::session::{ControlScope, MAX_GRANT_MS};
use crate::{advance, Binding, Error, SurfaceKey};

/// Local to the source owner. The runtime authenticates the connection before
/// looking up this lease. Never use the lease as a network credential.
#[derive(Clone)]
pub struct SourceLease {
    target: Arc<SurfaceKey>,
}

/// Completion must come from the existing local input owner after cleanup.
#[derive(Clone)]
pub struct SourceRelease {
    lease: SourceLease,
}

impl SourceRelease {
    pub fn target(&self) -> SurfaceKey {
        *self.lease.target
    }
}

struct Held {
    lease: SourceLease,
    deadline: u64,
    sequence: u64,
}

enum State {
    Idle,
    Held(Held),
    Releasing(SourceRelease),
}

/// One instance per source, retained by the serial owner across connections.
/// Policy is checked immediately before delivery. The native input owner keeps
/// the release ledger and performs actual injection and recovery.
pub struct SourceControl {
    source: [u8; 32],
    state: State,
    last_now: u64,
}

impl SourceControl {
    pub fn new(source: [u8; 32]) -> Result<Self, Error> {
        if source == [0; 32] {
            return Err(Error::InvalidIdentity);
        }
        Ok(Self {
            source,
            state: State::Idle,
            last_now: 0,
        })
    }

    /// This entry point represents an explicit local decision. The caller
    /// supplies the authenticated target and the current native permission.
    pub fn grant_local(
        &mut self,
        target: SurfaceKey,
        scope: ControlScope,
        native_permission: bool,
        now: u64,
        duration: u64,
    ) -> Result<SourceLease, Error> {
        target.validate()?;
        self.check_source(target.binding)?;
        self.poll(native_permission, now)?;
        if !native_permission || scope != ControlScope::WholeDesktop {
            return Err(Error::PermissionDenied);
        }
        match self.state {
            State::Idle => {}
            State::Held(_) => return Err(Error::ControlBusy),
            State::Releasing(_) => return Err(Error::ReleasePending),
        }
        if duration == 0 || duration > MAX_GRANT_MS {
            return Err(Error::InvalidTime);
        }
        let deadline = now.checked_add(duration).ok_or(Error::InvalidTime)?;
        let lease = SourceLease {
            target: Arc::new(target),
        };
        self.state = State::Held(Held {
            lease: lease.clone(),
            deadline,
            sequence: 0,
        });
        Ok(lease)
    }

    /// Check the target supplied by the authenticated adapter, the lease
    /// instance and the replay watermark. Use the source's monotonic clock.
    pub fn authorize(
        &mut self,
        target: SurfaceKey,
        lease: &SourceLease,
        sequence: u64,
        native_permission: bool,
        now: u64,
    ) -> Result<(), Error> {
        self.poll(native_permission, now)?;
        let held = match &mut self.state {
            State::Held(held) => held,
            State::Idle => return Err(Error::PermissionDenied),
            State::Releasing(_) => return Err(Error::ReleasePending),
        };
        if *held.lease.target != target || !Arc::ptr_eq(&held.lease.target, &lease.target) {
            return Err(Error::StaleTarget);
        }
        if sequence == 0 || sequence <= held.sequence {
            return Err(Error::InvalidSequence);
        }
        held.sequence = sequence;
        Ok(())
    }

    pub fn revoke_local(&mut self, now: u64) -> Result<(), Error> {
        self.clock(now)?;
        self.begin_release();
        Ok(())
    }

    pub fn disconnect(&mut self, binding: Binding, now: u64) -> Result<(), Error> {
        binding.validate()?;
        self.check_source(binding)?;
        self.clock(now)?;
        if matches!(&self.state, State::Held(held) if held.lease.target.binding == binding) {
            self.begin_release();
        }
        Ok(())
    }

    /// Run from the owner loop even when the connection has no input events.
    pub fn poll(&mut self, native_permission: bool, now: u64) -> Result<(), Error> {
        self.clock(now)?;
        if !native_permission {
            self.begin_release();
        }
        Ok(())
    }

    pub fn pending_release(&self) -> Option<&SourceRelease> {
        match &self.state {
            State::Releasing(release) => Some(release),
            _ => None,
        }
    }

    pub fn acknowledge_release(&mut self, release: &SourceRelease) -> Result<(), Error> {
        if !self
            .pending_release()
            .is_some_and(|current| Arc::ptr_eq(&current.lease.target, &release.lease.target))
        {
            return Err(Error::WrongAcknowledgement);
        }
        self.state = State::Idle;
        Ok(())
    }

    fn check_source(&self, binding: Binding) -> Result<(), Error> {
        if binding.source != self.source {
            return Err(Error::InvalidIdentity);
        }
        Ok(())
    }

    fn clock(&mut self, now: u64) -> Result<(), Error> {
        if let Err(error) = advance(&mut self.last_now, now) {
            self.begin_release();
            return Err(error);
        }
        if matches!(&self.state, State::Held(held) if now >= held.deadline) {
            self.begin_release();
        }
        Ok(())
    }

    fn begin_release(&mut self) {
        if let State::Held(held) = &self.state {
            self.state = State::Releasing(SourceRelease {
                lease: held.lease.clone(),
            });
        }
    }
}
