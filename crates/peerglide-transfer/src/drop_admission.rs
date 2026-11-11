//! A local, copy-only native drop ticket. Never serialized or used as identity.
const MAX_LIFETIME_MS: u64 = 60_000;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Content {
    Text,
    Image,
    File,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Context {
    /// Locally assigned after authenticating the connection.
    pub connection: u64,
    pub layout_generation: u64,
    pub gesture_generation: u64,
    pub focused: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Ticket {
    generation: u64,
    transfer: [u8; 16],
    content: Content,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    Copy,
    Move,
    Link,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeOutcome {
    Copied,
    Cancelled,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    WaitingForVerifiedContent,
    Ready,
    Submitted,
    Copied,
    Cancelled,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Unauthorized,
    InvalidContext,
    Busy,
    StaleTicket,
    WrongState,
    UnsupportedEffect,
    Exhausted,
}

struct Pending {
    ticket: Ticket,
    context: Context,
    expires: u64,
    state: State,
}

#[derive(Default)]
pub struct Admission {
    generation: u64,
    pending: Option<Pending>,
}

impl Admission {
    /// Called by a trusted local controller after obtaining per-content and
    /// per-drop approval. Permission is rechecked at the actual submission.
    pub fn begin(
        &mut self,
        context: Context,
        transfer: [u8; 16],
        content: Content,
        now: u64,
        authorized: bool,
    ) -> Result<Ticket, Error> {
        if !authorized {
            return Err(Error::Unauthorized);
        }
        if !context.focused
            || context.connection == 0
            || context.layout_generation == 0
            || context.gesture_generation == 0
            || transfer == [0; 16]
        {
            return Err(Error::InvalidContext);
        }
        if self.pending.as_ref().is_some_and(|pending| {
            matches!(
                pending.state,
                State::WaitingForVerifiedContent | State::Ready | State::Submitted | State::Unknown
            )
        }) {
            return Err(Error::Busy);
        }
        let generation = self.generation.checked_add(1).ok_or(Error::Exhausted)?;
        let expires = now.checked_add(MAX_LIFETIME_MS).ok_or(Error::Exhausted)?;
        self.generation = generation;
        let ticket = Ticket {
            generation,
            transfer,
            content,
        };
        self.pending = Some(Pending {
            ticket,
            context,
            expires,
            state: State::WaitingForVerifiedContent,
        });
        Ok(ticket)
    }

    /// Only the typed data owner calls this after verification is complete.
    pub fn verified(
        &mut self,
        ticket: Ticket,
        context: Context,
        now: u64,
        authorized: bool,
    ) -> Result<(), Error> {
        let pending = self.live(ticket, context, now, authorized)?;
        if pending.state != State::WaitingForVerifiedContent {
            return Err(Error::WrongState);
        }
        pending.state = State::Ready;
        Ok(())
    }

    pub fn submit(
        &mut self,
        ticket: Ticket,
        context: Context,
        now: u64,
        authorized: bool,
        effect: Effect,
    ) -> Result<(), Error> {
        let pending = self.live(ticket, context, now, authorized)?;
        if effect != Effect::Copy {
            return Err(Error::UnsupportedEffect);
        }
        if pending.state != State::Ready {
            return Err(Error::WrongState);
        }
        pending.state = State::Submitted;
        Ok(())
    }

    /// An OS callback may report success only for the current submitted copy.
    pub fn complete(
        &mut self,
        ticket: Ticket,
        context: Context,
        now: u64,
        authorized: bool,
        outcome: NativeOutcome,
    ) -> Result<(), Error> {
        let pending = self.live(ticket, context, now, authorized)?;
        if pending.state != State::Submitted {
            return Err(Error::WrongState);
        }
        pending.state = match outcome {
            NativeOutcome::Copied => State::Copied,
            NativeOutcome::Cancelled => State::Cancelled,
            NativeOutcome::Unknown => State::Unknown,
        };
        Ok(())
    }

    pub fn cancel(&mut self, ticket: Ticket) -> Result<State, Error> {
        let pending = self.lookup(ticket)?;
        pending.cancel();
        Ok(pending.state)
    }

    /// After the local controller has reconciled the destination and input owner.
    pub fn acknowledge_unknown(&mut self, ticket: Ticket) -> Result<(), Error> {
        let pending = self.lookup(ticket)?;
        if pending.state != State::Unknown {
            return Err(Error::WrongState);
        }
        pending.state = State::Cancelled;
        Ok(())
    }

    pub fn state(&self) -> Option<State> {
        self.pending.as_ref().map(|pending| pending.state)
    }

    fn lookup(&mut self, ticket: Ticket) -> Result<&mut Pending, Error> {
        self.pending
            .as_mut()
            .filter(|pending| pending.ticket == ticket)
            .ok_or(Error::StaleTicket)
    }

    fn live(
        &mut self,
        ticket: Ticket,
        context: Context,
        now: u64,
        authorized: bool,
    ) -> Result<&mut Pending, Error> {
        let pending = self.lookup(ticket)?;
        if !authorized {
            pending.cancel();
            return Err(Error::Unauthorized);
        }
        if context != pending.context || now >= pending.expires {
            pending.cancel();
            return Err(Error::InvalidContext);
        }
        Ok(pending)
    }
}

impl Pending {
    fn cancel(&mut self) {
        self.state = match self.state {
            State::WaitingForVerifiedContent | State::Ready => State::Cancelled,
            State::Submitted => State::Unknown,
            other => other,
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> Context {
        Context {
            connection: 1,
            layout_generation: 2,
            gesture_generation: 3,
            focused: true,
        }
    }

    #[test]
    fn copy_requires_verified_content_and_local_permission() {
        let mut owner = Admission::default();
        let ctx = context();
        let ticket = owner.begin(ctx, [1; 16], Content::File, 0, true).unwrap();
        assert_eq!(owner.submit(ticket, ctx, 1, true, Effect::Copy), Err(Error::WrongState));
        owner.verified(ticket, ctx, 2, true).unwrap();
        assert_eq!(owner.submit(ticket, ctx, 3, true, Effect::Move), Err(Error::UnsupportedEffect));
        assert_eq!(owner.submit(ticket, ctx, 4, true, Effect::Link), Err(Error::UnsupportedEffect));
        owner.submit(ticket, ctx, 5, true, Effect::Copy).unwrap();
        owner.complete(ticket, ctx, 6, true, NativeOutcome::Copied).unwrap();
        assert_eq!(owner.state(), Some(State::Copied));
    }

    #[test]
    fn focus_change_and_expiry_cancel_without_native_submission() {
        let mut owner = Admission::default();
        let ctx = context();
        let ticket = owner.begin(ctx, [1; 16], Content::Text, 0, true).unwrap();
        owner.verified(ticket, ctx, 1, true).unwrap();
        let blurred = Context { focused: false, ..ctx };
        assert_eq!(owner.submit(ticket, blurred, 2, true, Effect::Copy), Err(Error::InvalidContext));
        assert_eq!(owner.state(), Some(State::Cancelled));

        let next = owner.begin(ctx, [2; 16], Content::Image, 3, true).unwrap();
        assert_eq!(owner.verified(next, ctx, 3 + MAX_LIFETIME_MS, true), Err(Error::InvalidContext));
        assert_eq!(owner.state(), Some(State::Cancelled));
    }

    #[test]
    fn interrupted_submission_remains_uncertain_until_local_acknowledgment() {
        let mut owner = Admission::default();
        let ctx = context();
        let ticket = owner.begin(ctx, [1; 16], Content::File, 0, true).unwrap();
        owner.verified(ticket, ctx, 1, true).unwrap();
        owner.submit(ticket, ctx, 2, true, Effect::Copy).unwrap();
        let other = Context { connection: 5, ..ctx };
        assert_eq!(owner.complete(ticket, other, 3, true, NativeOutcome::Copied), Err(Error::InvalidContext));
        assert_eq!(owner.state(), Some(State::Unknown));
        assert_eq!(owner.complete(ticket, ctx, 4, true, NativeOutcome::Copied), Err(Error::WrongState));
        assert!(owner.begin(ctx, [2; 16], Content::File, 4, true).is_err());
        owner.acknowledge_unknown(ticket).unwrap();
        let next = owner.begin(ctx, [2; 16], Content::File, 5, true).unwrap();
        assert_eq!(owner.cancel(ticket), Err(Error::StaleTicket));
        assert_eq!(owner.verified(next, ctx, 6, false), Err(Error::Unauthorized));
        assert_eq!(owner.state(), Some(State::Cancelled));
    }
}
