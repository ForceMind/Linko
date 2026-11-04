//! Local native window actions, independently consented and target-bound.
//! No binary enables these actions yet. No keyboard injection or remote RPC.
use super::{
    native,
    watch::{FrozenContext, LocalContexts, TargetLease, TriggerMark, TriggerSource, WatchError},
    ContextError,
};
use peerglide_actions::{
    execution::{Backend, Coordinator, NativeFailure, Outcome, Request, RequestId},
    Action, ActionSpec, Capabilities, Context, LocalPolicy, Target, ValidatedAction,
};
use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

const ENABLED: u64 = 1 << 63;
const DESTRUCTIVE: u64 = 1 << 62;
const REVOKED: u64 = 1 << 61;
const REVISION: u64 = REVOKED - 1;
struct ConsentState {
    identity: [u8; 32],
    state: AtomicU64,
}
/// Independent, process-local action consent. Never constructed from KVM input,
/// a rule-file enabled flag, native capabilities or an OS permission result.
#[derive(Clone)]
pub struct LocalActionConsent(Arc<ConsentState>);
#[derive(Clone, Copy)]
struct ConsentStamp {
    identity: [u8; 32],
    state: u64,
}
impl LocalActionConsent {
    pub fn new() -> Result<Self, WatchError> {
        let mut identity = [0; 32];
        getrandom::fill(&mut identity).map_err(|_| WatchError::Entropy)?;
        Ok(Self(Arc::new(ConsentState {
            identity,
            state: AtomicU64::new(1),
        })))
    }
    /// Trusted local UI/worker consent only. Replacing consent invalidates all
    /// older stamps, including an identical repeated selection. Never in callbacks.
    pub fn explicitly_allow(&self, destructive_confirmed: bool) -> Result<(), WatchError> {
        for _ in 0..16 {
            let old = self.0.state.load(Ordering::Acquire);
            if old & REVOKED != 0 {
                return Err(WatchError::Closed);
            }
            let revision = old & REVISION;
            if revision == REVISION {
                self.revoke();
                return Err(WatchError::CounterExhausted);
            }
            let new = (revision + 1)
                | ENABLED
                | if destructive_confirmed {
                    DESTRUCTIVE
                } else {
                    0
                };
            if self
                .0
                .state
                .compare_exchange(old, new, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return Ok(());
            }
        }
        Err(WatchError::Backlog)
    }
    /// Terminal for this consent instance. Restart needs a new explicitly
    /// consented instance; no old trigger can revive after revoke/re-enable.
    pub fn revoke(&self) {
        self.0.state.fetch_or(REVOKED, Ordering::Release);
    }
    fn stamp(&self) -> Result<ConsentStamp, NativeFailure> {
        let state = self.0.state.load(Ordering::Acquire);
        if state & ENABLED == 0 || state & REVOKED != 0 {
            return Err(NativeFailure::PermissionLost);
        }
        Ok(ConsentStamp {
            identity: self.0.identity,
            state,
        })
    }
    fn policy_for(&self, stamp: ConsentStamp) -> Result<LocalPolicy, NativeFailure> {
        if stamp.identity != self.0.identity
            || stamp.state != self.0.state.load(Ordering::Acquire)
            || stamp.state & ENABLED == 0
            || stamp.state & REVOKED != 0
        {
            return Err(NativeFailure::PermissionLost);
        }
        Ok(LocalPolicy {
            enabled: true,
            destructive_confirmed: stamp.state & DESTRUCTIVE != 0,
        })
    }
    fn with_trigger<T>(
        &self,
        capture: impl FnOnce() -> Result<T, NativeFailure>,
    ) -> Result<(T, ConsentStamp), NativeFailure> {
        // Consent is fixed BEFORE the context callback mark, not attached later
        // when a delayed gesture has reached a new consent generation.
        let stamp = self.stamp()?;
        let trigger = capture()?;
        self.policy_for(stamp)?;
        Ok((trigger, stamp))
    }
}

/// Clone this source outside callbacks. mark performs fixed-size atomic/clock
/// reads only; it neither grants consent nor creates randomness/OS effects.
#[derive(Clone)]
pub struct ActionTriggerSource {
    context: TriggerSource,
    consent: LocalActionConsent,
}
pub struct ActionTrigger {
    context: TriggerMark,
    consent: ConsentStamp,
}
pub(crate) const ADMISSION_WORDS: usize = 10;
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct AdmissionKey(pub(crate) [u64; ADMISSION_WORDS]);
fn key(device: [u8; 32], epoch: u64, consent: ConsentStamp) -> AdmissionKey {
    let mut words = [0; ADMISSION_WORDS];
    for index in 0..4 {
        words[index] = u64::from_le_bytes(device[index * 8..index * 8 + 8].try_into().expect("fixed nonce chunk"));
        words[5 + index] = u64::from_le_bytes(consent.identity[index * 8..index * 8 + 8].try_into().expect("fixed consent chunk"));
    }
    words[4] = epoch;
    words[9] = consent.state;
    AdmissionKey(words)
}
/// A worker-verified native frame; caller evaluates application rules/exclusions
/// before passing it to the capture admission controller. Not an action grant.
pub struct CaptureAdmission { key: AdmissionKey, application_id: String, expires: Instant }
impl CaptureAdmission {
    pub fn application_id(&self) -> &str { &self.application_id }
    #[cfg(any(windows, test))]
    pub(crate) fn key(&self) -> AdmissionKey { self.key }
    pub fn expires(&self) -> Instant { self.expires }
}
impl ActionTrigger {
    pub(crate) fn frame_key(&self) -> AdmissionKey {
        let (device, epoch) = self.context.frame();
        key(device, epoch, self.consent)
    }
    #[cfg(any(windows, test))]
    pub(crate) fn started(&self) -> Instant { self.context.started() }
}
impl ActionTriggerSource {
    #[cfg(any(windows, test))]
    pub(crate) fn matches_admission(&self, admission: AdmissionKey) -> bool {
        let stamp = match self.consent.stamp() { Ok(stamp) => stamp, Err(_) => return false };
        let mut device = [0; 32];
        for index in 0..4 { device[index * 8..index * 8 + 8].copy_from_slice(&admission.0[index].to_le_bytes()); }
        self.context.matches_frame(device, admission.0[4]) && key(device, admission.0[4], stamp) == admission
    }
    #[cfg(any(windows, test))]
    pub(crate) fn consent_revoked(&self) -> bool { self.consent.0.state.load(Ordering::Acquire) & REVOKED != 0 }
    pub fn mark(&self) -> Result<ActionTrigger, NativeFailure> {
        let (context, consent) = self
            .consent
            .with_trigger(|| self.context.mark().map_err(failure))?;
        Ok(ActionTrigger { context, consent })
    }
    pub fn invalidate(&self) {
        self.context.invalidate();
    }
    pub fn emergency_revoke(&self) {
        self.consent.revoke();
        self.context.invalidate();
    }
}
pub struct PreparedLocalAction {
    frozen: FrozenContext,
    consent: ConsentStamp,
}

pub fn implemented_actions() -> Capabilities {
    native::action_capabilities()
}
fn failure(error: WatchError) -> NativeFailure {
    match error {
        WatchError::Context(ContextError::PermissionDenied) => NativeFailure::PermissionLost,
        WatchError::Expired
        | WatchError::Lifetime
        | WatchError::Capacity
        | WatchError::Backlog
        | WatchError::Entropy
        | WatchError::Clock
        | WatchError::CounterExhausted => NativeFailure::BeforeSubmission,
        _ => NativeFailure::TargetLost,
    }
}
fn rejected(error: peerglide_actions::Rejection) -> NativeFailure {
    use peerglide_actions::Rejection;
    match error {
        Rejection::Disabled | Rejection::ConfirmationRequired => NativeFailure::PermissionLost,
        Rejection::ContextChanged => NativeFailure::TargetLost,
        _ => NativeFailure::BeforeSubmission,
    }
}

fn permit_at(
    consent: &LocalActionConsent,
    stamp: ConsentStamp,
    action: Action,
    deadline: Instant,
    now: Instant,
) -> Result<(), NativeFailure> {
    let policy = consent.policy_for(stamp)?;
    if !policy.enabled
        || (action.risk() == peerglide_actions::Risk::Destructive && !policy.destructive_confirmed)
    {
        return Err(NativeFailure::PermissionLost);
    }
    if now >= deadline {
        return Err(NativeFailure::BeforeSubmission);
    }
    Ok(())
}

trait Driver {
    fn context(&self) -> Context;
    fn capabilities(&self) -> Capabilities;
    fn begin(&mut self) -> Result<(), NativeFailure>;
    fn submit(
        &mut self,
        action: Action,
        deadline: Instant,
        permit: &mut dyn FnMut() -> Result<(), NativeFailure>,
    ) -> Result<(), NativeFailure>;
}
struct Guarded<D> {
    driver: D,
    consent: LocalActionConsent,
    stamp: ConsentStamp,
    used: bool,
}
impl<D: Driver> Backend for Guarded<D> {
    fn context(&self) -> Context {
        self.driver.context()
    }
    fn policy(&self) -> LocalPolicy {
        self.consent.policy_for(self.stamp).unwrap_or_default()
    }
    fn capabilities(&self) -> Capabilities {
        self.driver.capabilities()
    }
    fn submit(&mut self, action: ValidatedAction) -> Result<(), NativeFailure> {
        if self.used {
            return Err(NativeFailure::BeforeSubmission);
        }
        self.used = true; // An attempted native submission is never retried.
        let action = action
            .revalidate(
                self.context(),
                Instant::now(),
                self.policy(),
                &self.capabilities(),
            )
            .map_err(rejected)?;
        let deadline = action.expires();
        let spec = action.spec();
        self.driver.begin()?;
        let consent = &self.consent;
        let stamp = self.stamp;
        let mut permit = || permit_at(consent, stamp, spec.action, deadline, Instant::now());
        permit()?;
        self.driver.submit(spec.action, deadline, &mut permit)
    }
}

struct NativeDriver<'a> {
    owner: &'a mut LocalContexts,
    pending: Option<FrozenContext>,
    lease: Option<TargetLease>,
    context: Context,
}
impl Driver for NativeDriver<'_> {
    fn context(&self) -> Context {
        self.context
    }
    fn capabilities(&self) -> Capabilities {
        implemented_actions()
    }
    fn begin(&mut self) -> Result<(), NativeFailure> {
        let frozen = self.pending.take().ok_or(NativeFailure::BeforeSubmission)?;
        self.lease = Some(self.owner.consume(frozen).map_err(failure)?);
        Ok(())
    }
    fn submit(
        &mut self,
        action: Action,
        deadline: Instant,
        permit: &mut dyn FnMut() -> Result<(), NativeFailure>,
    ) -> Result<(), NativeFailure> {
        let lease = self.lease.as_ref().ok_or(NativeFailure::TargetLost)?;
        let owner = &mut *self.owner;
        let mut guard = || {
            permit()?;
            lease.recheck(owner).map_err(failure)?;
            if Instant::now() >= deadline {
                return Err(NativeFailure::BeforeSubmission);
            }
            permit()
        };
        native::submit_window_action(lease.native_target(), action, &mut guard)
    }
}
impl Drop for NativeDriver<'_> {
    fn drop(&mut self) {
        if let Some(frozen) = self.pending.take() {
            let _ = self.owner.cancel(frozen);
        }
    }
}

/// The concrete peerglide-actions::execution::Backend adapter. Only the session
/// constructs it from a real consuming native binding and trigger-time consent.
/// No public constructor accepts test Contexts, HWNDs, PIDs or input grants.
struct NativeBackend<'a>(Guarded<NativeDriver<'a>>);
impl Backend for NativeBackend<'_> {
    fn context(&self) -> Context {
        self.0.context()
    }
    fn policy(&self) -> LocalPolicy {
        self.0.policy()
    }
    fn capabilities(&self) -> Capabilities {
        self.0.capabilities()
    }
    fn submit(&mut self, action: ValidatedAction) -> Result<(), NativeFailure> {
        self.0.submit(action)
    }
}

/// No identity/pairing/network prerequisite. Construction subscribes only to
/// context notifications; consent is always OFF and no input capture starts.
/// No shipped binary enables or dispatches these native actions yet.
pub struct LocalActions {
    contexts: LocalContexts,
    consent: LocalActionConsent,
    results: Coordinator,
}
impl LocalActions {
    pub fn new() -> Result<Self, WatchError> {
        let consent = LocalActionConsent::new()?;
        let contexts = LocalContexts::new()?;
        Ok(Self {
            contexts,
            consent,
            results: Coordinator::default(),
        })
    }
    pub fn consent_control(&self) -> LocalActionConsent {
        self.consent.clone()
    }
    pub fn trigger_source(&self) -> ActionTriggerSource {
        ActionTriggerSource {
            context: self.contexts.trigger_source(),
            consent: self.consent.clone(),
        }
    }
    pub fn freeze(
        &mut self,
        trigger: ActionTrigger,
        lifetime: Duration,
    ) -> Result<PreparedLocalAction, NativeFailure> {
        self.consent.policy_for(trigger.consent)?;
        let frozen = self
            .contexts
            .capture(trigger.context, lifetime)
            .map_err(failure)?;
        if let Err(error) = self.consent.policy_for(trigger.consent) {
            let _ = self.contexts.cancel(frozen);
            return Err(error);
        }
        Ok(PreparedLocalAction {
            frozen,
            consent: trigger.consent,
        })
    }
    /// Read the real target and issue a frame-bound admission proof outside the
    /// input callback. This consumes/drops a native lease, never submits an action.
    pub fn capture_admission(&mut self) -> Result<CaptureAdmission, NativeFailure> {
        self.poll().map_err(failure)?;
        let trigger = self.trigger_source().mark()?;
        let key = trigger.frame_key();
        let prepared = self.freeze(trigger, peerglide_actions::MAX_ACTION_LIFETIME)?;
        let expires = prepared.frozen.expires();
        let lease = self.contexts.consume(prepared.frozen).map_err(failure)?;
        lease.recheck(&mut self.contexts).map_err(failure)?;
        self.consent.policy_for(prepared.consent)?;
        Ok(CaptureAdmission { key, application_id: lease.application_id().to_owned(), expires })
    }
    pub fn cancel(&mut self, prepared: PreparedLocalAction) -> Result<(), WatchError> {
        self.contexts.cancel(prepared.frozen)
    }
    /// Request IDs identify result history, not authentication. The production
    /// worker must generate unique IDs outside callbacks; this is never a remote API.
    pub fn run(
        &mut self,
        prepared: PreparedLocalAction,
        id: RequestId,
        spec: ActionSpec,
    ) -> Outcome {
        let context = prepared.frozen.context();
        let request = Request {
            id,
            action: spec,
            target: Target::Local,
            context,
            expires: prepared.frozen.expires(),
        };
        let mut backend = NativeBackend(Guarded {
            driver: NativeDriver {
                owner: &mut self.contexts,
                pending: Some(prepared.frozen),
                lease: None,
                context,
            },
            consent: self.consent.clone(),
            stamp: prepared.consent,
            used: false,
        });
        self.results.run(request, Instant::now(), &mut backend)
    }
    pub fn configuration_changed(&mut self) -> Result<(), WatchError> {
        self.contexts.configuration_changed()
    }
    pub fn poll(&mut self) -> Result<(), WatchError> {
        self.contexts.poll()
    }
    pub fn shutdown(&mut self) -> Result<(), WatchError> {
        self.consent.revoke();
        self.contexts.shutdown()
    }
}
impl Drop for LocalActions {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peerglide_actions::{validate, Candidate, Rejection};
    fn context() -> Context {
        // Pure fake driver only; never a native target or authentication fixture.
        Context {
            device: [1; 32],
            target: [2; 32],
            focus_generation: 1,
            configuration_generation: 1,
            trigger_sequence: 1,
        }
    }
    struct Fake {
        began: usize,
        submitted: usize,
        failure: Option<NativeFailure>,
        revoke: Option<LocalActionConsent>,
    }
    impl Driver for Fake {
        fn context(&self) -> Context {
            context()
        }
        fn capabilities(&self) -> Capabilities {
            Capabilities::new([Action::MinimizeWindow, Action::CloseWindow])
        }
        fn begin(&mut self) -> Result<(), NativeFailure> {
            self.began += 1;
            Ok(())
        }
        fn submit(
            &mut self,
            _: Action,
            _: Instant,
            permit: &mut dyn FnMut() -> Result<(), NativeFailure>,
        ) -> Result<(), NativeFailure> {
            permit()?;
            if let Some(consent) = &self.revoke {
                consent.revoke();
            }
            permit()?;
            self.submitted += 1;
            self.failure.map_or(Ok(()), Err)
        }
    }
    fn backend(consent: LocalActionConsent, stamp: ConsentStamp) -> Guarded<Fake> {
        Guarded {
            driver: Fake {
                began: 0,
                submitted: 0,
                failure: None,
                revoke: None,
            },
            consent,
            stamp,
            used: false,
        }
    }
    fn request(now: Instant, action: Action) -> Request {
        Request {
            id: RequestId([3; 16]),
            action: ActionSpec::new(action),
            target: Target::Local,
            context: context(),
            expires: now + peerglide_actions::MAX_ACTION_LIFETIME,
        }
    }
    #[test]
    fn consent_is_default_off_and_changes_never_revive_old_triggers() {
        let consent = LocalActionConsent::new().unwrap();
        assert!(matches!(
            consent.with_trigger(|| -> Result<(), NativeFailure> {
                panic!("disabled must not read context")
            }),
            Err(NativeFailure::PermissionLost)
        ));
        consent.explicitly_allow(false).unwrap();
        let stamp = consent.stamp().unwrap();
        assert!(consent.policy_for(stamp).unwrap().enabled);
        consent.explicitly_allow(false).unwrap();
        assert!(matches!(
            consent.policy_for(stamp),
            Err(NativeFailure::PermissionLost)
        ));
        let current = consent.stamp().unwrap();
        consent.revoke();
        assert!(matches!(
            consent.policy_for(current),
            Err(NativeFailure::PermissionLost)
        ));
        assert_eq!(consent.explicitly_allow(true), Err(WatchError::Closed));
        let another = LocalActionConsent::new().unwrap();
        another.explicitly_allow(true).unwrap();
        assert!(matches!(
            another.policy_for(current),
            Err(NativeFailure::PermissionLost)
        ));
    }
    #[test]
    fn consent_replacement_during_context_mark_rejects_the_combined_trigger() {
        let consent = LocalActionConsent::new().unwrap();
        consent.explicitly_allow(false).unwrap();
        let result = consent.with_trigger(|| {
            consent.explicitly_allow(false).unwrap();
            Ok(())
        });
        assert!(matches!(result, Err(NativeFailure::PermissionLost)));
    }
    #[test]
    fn disabled_unsupported_remote_stale_and_destructive_requests_have_no_effect() {
        let now = Instant::now();
        let consent = LocalActionConsent::new().unwrap();
        let off = ConsentStamp {
            identity: consent.0.identity,
            state: consent.0.state.load(Ordering::Acquire),
        };
        let mut off_backend = backend(consent.clone(), off);
        let mut coordinator = Coordinator::default();
        assert_eq!(
            coordinator.run(request(now, Action::MinimizeWindow), now, &mut off_backend),
            Outcome::Rejected(Rejection::Disabled)
        );
        assert_eq!(off_backend.driver.began, 0);
        consent.explicitly_allow(false).unwrap();
        let stamp = consent.stamp().unwrap();
        let mut cases = [
            request(now, Action::Copy),
            request(now, Action::CloseWindow),
            request(now, Action::MinimizeWindow),
            request(now, Action::MinimizeWindow),
        ];
        cases[2].target = Target::Remote;
        cases[3].context.focus_generation += 1;
        for case in cases {
            let mut backend = backend(consent.clone(), stamp);
            assert!(matches!(
                Coordinator::default().run(case, now, &mut backend),
                Outcome::Rejected(_)
            ));
            assert_eq!((backend.driver.began, backend.driver.submitted), (0, 0));
        }
    }
    #[test]
    fn revocation_during_native_preparation_prevents_submission() {
        let now = Instant::now();
        let consent = LocalActionConsent::new().unwrap();
        consent.explicitly_allow(false).unwrap();
        let mut backend = backend(consent.clone(), consent.stamp().unwrap());
        backend.driver.revoke = Some(consent);
        assert_eq!(
            Coordinator::default().run(request(now, Action::MinimizeWindow), now, &mut backend),
            Outcome::Failed(NativeFailure::PermissionLost)
        );
        assert_eq!((backend.driver.began, backend.driver.submitted), (1, 0));
    }
    #[test]
    fn unknown_submission_is_cached_and_the_same_lease_never_replays() {
        let now = Instant::now();
        let consent = LocalActionConsent::new().unwrap();
        consent.explicitly_allow(false).unwrap();
        let mut backend = backend(consent.clone(), consent.stamp().unwrap());
        backend.driver.failure = Some(NativeFailure::UnknownOutcome);
        let mut coordinator = Coordinator::default();
        let original = request(now, Action::MinimizeWindow);
        assert_eq!(
            coordinator.run(original, now, &mut backend),
            Outcome::Failed(NativeFailure::UnknownOutcome)
        );
        assert_eq!(
            coordinator.run(original, now, &mut backend),
            Outcome::Failed(NativeFailure::UnknownOutcome)
        );
        let mut conflict = original;
        conflict.action = ActionSpec::new(Action::CloseWindow);
        assert_eq!(
            coordinator.run(conflict, now, &mut backend),
            Outcome::DuplicateConflict
        );
        assert_eq!(backend.driver.submitted, 1);
        let candidate = validate(
            Candidate {
                spec: original.action,
                target: Target::Local,
                captured: context(),
                expires: original.expires,
            },
            context(),
            now,
            backend.policy(),
            &backend.capabilities(),
        )
        .unwrap();
        assert_eq!(
            backend.submit(candidate),
            Err(NativeFailure::BeforeSubmission)
        );
        assert_eq!(backend.driver.submitted, 1);
    }
    #[test]
    fn original_action_deadline_and_confirmation_are_checked_at_submission() {
        let now = Instant::now();
        let deadline = now + Duration::from_secs(1);
        let consent = LocalActionConsent::new().unwrap();
        consent.explicitly_allow(false).unwrap();
        let stamp = consent.stamp().unwrap();
        assert_eq!(
            permit_at(&consent, stamp, Action::MinimizeWindow, deadline, deadline),
            Err(NativeFailure::BeforeSubmission)
        );
        assert_eq!(
            permit_at(&consent, stamp, Action::CloseWindow, deadline, now),
            Err(NativeFailure::PermissionLost)
        );
        consent.explicitly_allow(true).unwrap();
        assert_eq!(
            permit_at(
                &consent,
                consent.stamp().unwrap(),
                Action::CloseWindow,
                deadline,
                now
            ),
            Ok(())
        );
        assert!(matches!(
            permit_at(&consent, stamp, Action::MinimizeWindow, deadline, now),
            Err(NativeFailure::PermissionLost)
        ));
    }
    #[test]
    fn consent_counter_overflow_closes_without_a_reusable_revision() {
        let consent = LocalActionConsent::new().unwrap();
        consent.0.state.store(ENABLED | REVISION, Ordering::Release);
        assert_eq!(
            consent.explicitly_allow(false),
            Err(WatchError::CounterExhausted)
        );
        assert!(matches!(
            consent.stamp(),
            Err(NativeFailure::PermissionLost)
        ));
    }
}
