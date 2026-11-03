//! Local request/result coordinator. No native backend or remote RPC is provided.
use crate::{
    validate, ActionSpec, Candidate, Capabilities, Context, LocalPolicy, Rejection, Target,
    ValidatedAction,
};
use std::time::Instant;
pub const MAX_PENDING_RESULTS: usize = 128;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RequestId(pub [u8; 16]);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Request {
    pub id: RequestId,
    pub action: ActionSpec,
    pub target: Target,
    pub context: Context,
    pub expires: Instant,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeFailure {
    PermissionLost,
    TargetLost,
    BeforeSubmission,
    UnknownOutcome,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Rejected(Rejection),
    Submitted,
    Failed(NativeFailure),
    DuplicateConflict,
    Capacity,
}
/// Snapshot and submission happen in the same worker. Implementations must still
/// recheck the real OS target/permissions at dispatch and tag owned injections.
pub trait Backend {
    fn context(&self) -> Context;
    fn policy(&self) -> LocalPolicy;
    fn capabilities(&self) -> Capabilities;
    /// Ok means submitted to OS, never proof the application performed the action.
    fn submit(&mut self, action: ValidatedAction) -> Result<(), NativeFailure>;
}
struct Entry {
    request: Request,
    outcome: Outcome,
    retain_until: Instant,
}
#[derive(Default)]
pub struct Coordinator {
    entries: Vec<Entry>,
}
impl Coordinator {
    /// Same ID/body returns historical status, including unknown outcome, without
    /// retry. Conflicting bodies reject. Live entries are never evicted to execute
    /// another request; expired retention can be pruned. This is not G2 dedup.
    pub fn run(&mut self, request: Request, now: Instant, backend: &mut impl Backend) -> Outcome {
        if let Some(entry) = self.entries.iter().find(|e| e.request.id == request.id) {
            return if entry.request == request {
                entry.outcome
            } else {
                Outcome::DuplicateConflict
            };
        }
        self.entries.retain(|e| e.retain_until > now);
        if self.entries.len() >= MAX_PENDING_RESULTS {
            return Outcome::Capacity;
        }
        let candidate = Candidate {
            spec: request.action,
            target: request.target,
            captured: request.context,
            expires: request.expires,
        };
        let outcome = match validate(
            candidate,
            backend.context(),
            now,
            backend.policy(),
            &backend.capabilities(),
        ) {
            Err(e) => Outcome::Rejected(e),
            Ok(action) => match backend.submit(action) {
                Ok(()) => Outcome::Submitted,
                Err(e) => Outcome::Failed(e),
            },
        };
        let retain_until = now
            .checked_add(crate::MAX_ACTION_LIFETIME)
            .unwrap_or(now)
            .min(request.expires);
        self.entries.push(Entry {
            request,
            outcome,
            retain_until,
        });
        outcome
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::Action;
    use std::time::Duration;
    struct Fake {
        context: Context,
        enabled: bool,
        confirmed: bool,
        failure: Option<NativeFailure>,
        calls: usize,
    }
    impl Backend for Fake {
        fn context(&self) -> Context {
            self.context
        }
        fn policy(&self) -> LocalPolicy {
            LocalPolicy {
                enabled: self.enabled,
                destructive_confirmed: self.confirmed,
            }
        }
        fn capabilities(&self) -> Capabilities {
            Capabilities::new(crate::ACTIONS)
        }
        fn submit(&mut self, _: ValidatedAction) -> Result<(), NativeFailure> {
            self.calls += 1;
            self.failure.map_or(Ok(()), Err)
        }
    }
    fn backend() -> Fake {
        Fake {
            context: Context {
                device: [1; 32],
                target: [2; 32],
                focus_generation: 1,
                configuration_generation: 1,
                trigger_sequence: 1,
            },
            enabled: true,
            confirmed: false,
            failure: None,
            calls: 0,
        }
    }
    fn request(b: &Fake, now: Instant) -> Request {
        Request {
            id: RequestId([1; 16]),
            action: ActionSpec::new(Action::Paste),
            target: Target::Local,
            context: b.context,
            expires: now + Duration::from_secs(1),
        }
    }
    #[test]
    fn unknown_or_submitted_effect_is_never_automatically_replayed() {
        for failure in [
            None,
            Some(NativeFailure::UnknownOutcome),
            Some(NativeFailure::BeforeSubmission),
        ] {
            let now = Instant::now();
            let mut b = backend();
            b.failure = failure;
            let r = request(&b, now);
            let mut c = Coordinator::default();
            let first = c.run(r, now, &mut b);
            assert_eq!(c.run(r, now, &mut b), first);
            assert_eq!(b.calls, 1);
            let mut conflicting = r;
            conflicting.action = ActionSpec::new(Action::Copy);
            assert_eq!(c.run(conflicting, now, &mut b), Outcome::DuplicateConflict);
            assert_eq!(b.calls, 1);
        }
    }
    #[test]
    fn stale_remote_expired_or_unconfirmed_request_never_reaches_backend() {
        let now = Instant::now();
        let mut b = backend();
        let r = request(&b, now);
        let mut cases = [r; 4];
        cases[0].context.focus_generation += 1;
        cases[1].target = Target::Remote;
        cases[2].expires = now;
        cases[3].action = ActionSpec::new(Action::CloseWindow);
        for candidate in cases {
            let mut c = Coordinator::default();
            assert!(matches!(
                c.run(candidate, now, &mut b),
                Outcome::Rejected(_)
            ));
        }
        assert_eq!(b.calls, 0);
    }
    #[test]
    fn bounded_results_do_not_evict_live_non_idempotent_history() {
        let now = Instant::now();
        let mut b = backend();
        let mut c = Coordinator::default();
        for id in 0..MAX_PENDING_RESULTS {
            let mut r = request(&b, now);
            r.id = RequestId([id as u8; 16]);
            assert_eq!(c.run(r, now, &mut b), Outcome::Submitted);
        }
        let mut extra = request(&b, now);
        extra.id = RequestId([255; 16]);
        assert_eq!(c.run(extra, now, &mut b), Outcome::Capacity);
        assert_eq!(b.calls, 128);
        extra.expires = now + Duration::from_secs(3);
        assert_eq!(
            c.run(extra, now + Duration::from_secs(2), &mut b),
            Outcome::Submitted
        );
        assert_eq!(b.calls, 129);
    }
    #[test]
    fn far_future_deadlines_cannot_pin_the_cache_or_execute() {
        let now = Instant::now();
        let mut b = backend();
        let mut c = Coordinator::default();
        let mut r = request(&b, now);
        r.expires = now + Duration::from_secs(60);
        assert_eq!(
            c.run(r, now, &mut b),
            Outcome::Rejected(Rejection::InvalidLifetime)
        );
        assert_eq!(b.calls, 0);
        assert!(c.entries[0].retain_until <= now + crate::MAX_ACTION_LIFETIME);
    }
}
