//! Worker-side arbitration across authenticated v5 sessions.
//! Per-connection receiver logic still owns replay, injection and held inputs.
use peerglide_protocol::LEASE_MS;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Sending,
    Receiving,
}

/// The connection number is assigned by the trusted local worker after it
/// authenticates the peer. Only the lease number comes from the v5 exchange.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Route {
    pub connection: u64,
    pub lease: u64,
    pub direction: Direction,
}

/// A local commit and cleanup token. Never serialize or send it to a peer.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Ticket {
    route: Route,
    generation: u64,
}

impl Ticket {
    pub fn route(self) -> Route {
        self.route
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Denial {
    Unauthorized,
    InvalidRoute,
    Busy,
    GenerationExhausted,
}

#[derive(Default)]
enum State {
    #[default]
    Local,
    Pending {
        ticket: Ticket,
        deadline: u64,
    },
    Active {
        ticket: Ticket,
        deadline: u64,
    },
    Recovering(Ticket),
}

/// One arbiter per local input owner. It never installs capture or injects input.
/// Times are read from the same local monotonic clock by the worker.
#[derive(Default)]
pub struct Owner {
    generation: u64,
    state: State,
}

impl Owner {
    pub fn reserve(&mut self, route: Route, now: u64, authorized: bool) -> Result<Ticket, Denial> {
        if !authorized {
            return Err(Denial::Unauthorized);
        }
        if route.connection == 0 || route.lease == 0 {
            return Err(Denial::InvalidRoute);
        }
        if !matches!(self.state, State::Local) {
            return Err(Denial::Busy);
        }
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or(Denial::GenerationExhausted)?;
        let ticket = Ticket {
            route,
            generation: self.generation,
        };
        self.state = State::Pending {
            ticket,
            deadline: now.saturating_add(LEASE_MS),
        };
        Ok(ticket)
    }

    /// Commit after the existing capture or receive owner confirms admission.
    /// Failure for the current ticket requires reconciliation with that owner.
    pub fn commit(&mut self, ticket: Ticket, now: u64, authorized: bool) -> bool {
        let State::Pending {
            ticket: pending,
            deadline,
        } = self.state
        else {
            return false;
        };
        if pending != ticket {
            return false;
        }
        if !authorized || now >= deadline {
            self.state = State::Recovering(ticket);
            return false;
        }
        self.state = State::Active {
            ticket,
            deadline: now.saturating_add(LEASE_MS),
        };
        true
    }

    pub fn allows(&self, ticket: Ticket, now: u64, authorized: bool) -> bool {
        authorized
            && matches!(self.state, State::Active { ticket: active, deadline }
                if active == ticket && now < deadline)
    }

    pub fn renew(&mut self, ticket: Ticket, now: u64, authorized: bool) -> bool {
        if !self.allows(ticket, now, authorized) {
            self.release(ticket);
            return false;
        }
        self.state = State::Active {
            ticket,
            deadline: now.saturating_add(LEASE_MS),
        };
        true
    }

    /// Expiry closes admission immediately. It does not certify cleanup.
    pub fn expire(&mut self, now: u64) -> Option<Ticket> {
        let ticket = match self.state {
            State::Active { ticket, deadline } | State::Pending { ticket, deadline }
                if now >= deadline =>
            {
                ticket
            }
            _ => return None,
        };
        self.release(ticket);
        Some(ticket)
    }

    /// A disconnect from another authenticated session cannot stop the owner.
    pub fn disconnect(&mut self, connection: u64) -> Option<Ticket> {
        let ticket = match self.state {
            State::Active { ticket, .. } | State::Pending { ticket, .. }
                if ticket.route.connection == connection =>
            {
                ticket
            }
            _ => return None,
        };
        self.release(ticket);
        Some(ticket)
    }

    pub fn release(&mut self, ticket: Ticket) -> bool {
        match self.state {
            State::Active {
                ticket: current, ..
            }
            | State::Pending {
                ticket: current, ..
            } if current == ticket => {
                self.state = State::Recovering(ticket);
                true
            }
            _ => false,
        }
    }

    /// Only the existing native owner may acknowledge successful recovery.
    pub fn recovered(&mut self, ticket: Ticket) -> bool {
        if !matches!(self.state, State::Recovering(current) if current == ticket) {
            return false;
        }
        self.state = State::Local;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(connection: u64, direction: Direction) -> Route {
        Route {
            connection,
            lease: 10,
            direction,
        }
    }

    #[test]
    fn input_owner_is_exclusive_across_directions_and_equal_wire_leases() {
        let mut owner = Owner::default();
        let inbound = route(1, Direction::Receiving);
        let outbound = route(2, Direction::Sending);
        assert_eq!(
            owner.reserve(inbound, 0, false).err(),
            Some(Denial::Unauthorized)
        );
        let first = owner.reserve(inbound, 0, true).unwrap();
        assert_eq!(owner.reserve(outbound, 1, true).err(), Some(Denial::Busy));
        assert!(!owner.allows(first, 1, true));
        assert!(owner.commit(first, 2, true));
        assert!(owner.disconnect(outbound.connection).is_none());
        assert!(owner.allows(first, 3, true));
        assert!(owner.disconnect(inbound.connection) == Some(first));
        assert!(!owner.allows(first, 3, true));
        assert_eq!(owner.reserve(outbound, 3, true).err(), Some(Denial::Busy));
        assert!(owner.recovered(first));
        let next = owner.reserve(outbound, 4, true).unwrap();
        assert!(!owner.commit(first, 4, true));
        assert!(owner.commit(next, 4, true));
        assert!(!owner.recovered(first));
        assert!(owner.allows(next, 4, true));
    }

    #[test]
    fn revoked_or_expired_tickets_require_the_exact_recovery_ack() {
        let mut owner = Owner::default();
        let request = route(8, Direction::Receiving);
        let pending = owner.reserve(request, 0, true).unwrap();
        assert!(!owner.commit(pending, LEASE_MS, true));
        assert_eq!(
            owner.reserve(request, LEASE_MS, true).err(),
            Some(Denial::Busy)
        );
        assert!(owner.recovered(pending));

        let current = owner.reserve(request, LEASE_MS, true).unwrap();
        assert!(owner.commit(current, LEASE_MS, true));
        assert!(!owner.renew(current, LEASE_MS + 1, false));
        assert!(!owner.recovered(pending));
        assert!(owner.recovered(current));
        let last = owner.reserve(request, 2 * LEASE_MS, true).unwrap();
        assert!(owner.commit(last, 2 * LEASE_MS, true));
        assert!(owner.expire(3 * LEASE_MS) == Some(last));
        assert!(!owner.allows(last, 3 * LEASE_MS, true));
        assert!(owner.recovered(last));
    }
}
