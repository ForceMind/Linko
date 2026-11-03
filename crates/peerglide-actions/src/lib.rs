//! G01 semantic action model. No OS executor, input hook, RPC or implicit grant.
//!
//! Contexts are caller-supplied snapshots, not authenticated OS capabilities.
//! A future adapter must mint tokens, revalidate focus immediately before dispatch,
//! and report OS submission separately from application completion. This crate's
//! successful validation means only that the pure model's conditions held.

use serde::{Deserialize, Serialize};
use std::time::Instant;
pub const MAX_ACTION_LIFETIME: std::time::Duration = std::time::Duration::from_secs(5);
pub mod execution;
pub mod rules;

pub const SCHEMA_VERSION: u16 = 1;

/// Stable, parameter-free IDs. Raw keys, paths and commands are deliberately absent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Copy,
    Paste,
    Back,
    Forward,
    CloseTab,
    MinimizeWindow,
    MaximizeWindow,
    ToggleFullscreen,
    CloseWindow,
    QuitApplication,
    ShowPeerGlide,
}

pub const ACTIONS: [Action; 11] = [
    Action::Copy,
    Action::Paste,
    Action::Back,
    Action::Forward,
    Action::CloseTab,
    Action::MinimizeWindow,
    Action::MaximizeWindow,
    Action::ToggleFullscreen,
    Action::CloseWindow,
    Action::QuitApplication,
    Action::ShowPeerGlide,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Risk {
    Standard,
    Destructive,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Replay {
    /// Repeating has the same intended effect only in an unchanged context.
    IdempotentInContext,
    /// Never automatically repeat after an unknown outcome.
    NeverAutomatic,
}

impl Action {
    pub const fn risk(self) -> Risk {
        match self {
            Self::CloseTab | Self::CloseWindow | Self::QuitApplication => Risk::Destructive,
            _ => Risk::Standard,
        }
    }

    pub const fn replay(self) -> Replay {
        match self {
            Self::MinimizeWindow | Self::MaximizeWindow | Self::ShowPeerGlide => {
                Replay::IdempotentInContext
            }
            _ => Replay::NeverAutomatic,
        }
    }
}

/// Configuration shape only; this is not a network request or execution permission.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionSpec {
    pub schema_version: u16,
    pub action: Action,
}

impl ActionSpec {
    pub const fn new(action: Action) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            action,
        }
    }
}

/// Native adapter support, independent of authorization and keyboard capabilities.
/// Empty by default: no native action backend has been implemented yet.
#[derive(Clone, Debug, Default)]
pub struct Capabilities(Vec<Action>);

impl Capabilities {
    pub fn new(actions: impl IntoIterator<Item = Action>) -> Self {
        let mut supported = Vec::new();
        for action in actions {
            if !supported.contains(&action) {
                supported.push(action);
            }
        }
        Self(supported)
    }

    pub fn supports(&self, action: Action) -> bool {
        self.0.contains(&action)
    }
}

/// Local-only binding. Opaque tokens remain in the process and are never serialized.
/// The adapter must change the token whenever its window/focus target changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Context {
    pub device: [u8; 32],
    pub target: [u8; 32],
    pub focus_generation: u64,
    pub configuration_generation: u64,
    pub trigger_sequence: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Local,
    Remote,
}

/// Explicit local consent, not derivable from KVM trust or InputCapabilities.
#[derive(Clone, Copy, Debug, Default)]
pub struct LocalPolicy {
    pub enabled: bool,
    pub destructive_confirmed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rejection {
    UnsupportedSchema,
    RemoteUnavailable,
    Disabled,
    Unsupported,
    ContextChanged,
    Expired,
    ConfirmationRequired,
    InvalidLifetime,
}

/// Non-cloneable validation record; it does not assert an action was executed.
#[derive(Debug)]
pub struct ValidatedAction {
    spec: ActionSpec,
    context: Context,
    expires: Instant,
}

impl ValidatedAction {
    pub fn spec(&self) -> ActionSpec {
        self.spec
    }

    /// Consume and recheck immediately before a future adapter's OS submission.
    /// The adapter must also recheck current permissions/capabilities via validate.
    pub fn revalidate(
        self,
        current: Context,
        now: Instant,
        policy: LocalPolicy,
        capabilities: &Capabilities,
    ) -> Result<Self, Rejection> {
        validate(
            Candidate {
                spec: self.spec,
                target: Target::Local,
                captured: self.context,
                expires: self.expires,
            },
            current,
            now,
            policy,
            capabilities,
        )
    }
}

/// Immutable candidate captured at trigger time, not an execution permission.
#[derive(Debug)]
pub struct Candidate {
    pub spec: ActionSpec,
    pub target: Target,
    pub captured: Context,
    pub expires: Instant,
}

/// Pure preflight only. Deadlines use this process's monotonic clock. No fallback
/// from remote to local; all remote targets are rejected until a separate G2 design.
pub fn validate(
    candidate: Candidate,
    current: Context,
    now: Instant,
    policy: LocalPolicy,
    capabilities: &Capabilities,
) -> Result<ValidatedAction, Rejection> {
    let Candidate {
        spec,
        target,
        captured,
        expires,
    } = candidate;
    if spec.schema_version != SCHEMA_VERSION {
        return Err(Rejection::UnsupportedSchema);
    }
    if target != Target::Local {
        return Err(Rejection::RemoteUnavailable);
    }
    if !policy.enabled {
        return Err(Rejection::Disabled);
    }
    if !capabilities.supports(spec.action) {
        return Err(Rejection::Unsupported);
    }
    if captured != current {
        return Err(Rejection::ContextChanged);
    }
    if now >= expires {
        return Err(Rejection::Expired);
    }
    if expires.duration_since(now) > MAX_ACTION_LIFETIME {
        return Err(Rejection::InvalidLifetime);
    }
    if spec.action.risk() == Risk::Destructive && !policy.destructive_confirmed {
        return Err(Rejection::ConfirmationRequired);
    }
    Ok(ValidatedAction {
        spec,
        context: captured,
        expires,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const CONTEXT: Context = Context {
        device: [1; 32],
        target: [2; 32],
        focus_generation: 3,
        configuration_generation: 4,
        trigger_sequence: 5,
    };
    const ENABLED: LocalPolicy = LocalPolicy {
        enabled: true,
        destructive_confirmed: false,
    };

    #[test]
    fn stable_ids_and_strict_schema() {
        let ids = [
            "copy",
            "paste",
            "back",
            "forward",
            "close_tab",
            "minimize_window",
            "maximize_window",
            "toggle_fullscreen",
            "close_window",
            "quit_application",
            "show_peer_glide",
        ];
        for (action, id) in ACTIONS.into_iter().zip(ids) {
            let spec = ActionSpec::new(action);
            let encoded = serde_json::to_value(spec).unwrap();
            assert_eq!(encoded["action"], id);
            assert_eq!(serde_json::from_value::<ActionSpec>(encoded).unwrap(), spec);
        }
        for invalid in [
            r#"{"schema_version":1,"action":"run_command"}"#,
            r#"{"schema_version":1,"action":"copy","command":"x"}"#,
            r#"{"action":"copy"}"#,
        ] {
            assert!(serde_json::from_str::<ActionSpec>(invalid).is_err());
        }
    }

    #[test]
    fn capability_never_implies_local_or_remote_consent() {
        let now = Instant::now();
        let expires = now + Duration::from_secs(1);
        let all = Capabilities::new(ACTIONS);
        let check = |target, policy, caps: &Capabilities| {
            validate(
                Candidate {
                    spec: ActionSpec::new(Action::Copy),
                    target,
                    captured: CONTEXT,
                    expires,
                },
                CONTEXT,
                now,
                policy,
                caps,
            )
            .unwrap_err()
        };
        assert_eq!(
            check(Target::Local, LocalPolicy::default(), &all),
            Rejection::Disabled
        );
        assert_eq!(
            check(Target::Local, ENABLED, &Capabilities::default()),
            Rejection::Unsupported
        );
        assert_eq!(
            check(Target::Remote, ENABLED, &all),
            Rejection::RemoteUnavailable
        );
        let invalid = ActionSpec {
            schema_version: 2,
            action: Action::Copy,
        };
        assert_eq!(
            validate(
                Candidate {
                    spec: invalid,
                    target: Target::Local,
                    captured: CONTEXT,
                    expires
                },
                CONTEXT,
                now,
                ENABLED,
                &all
            )
            .unwrap_err(),
            Rejection::UnsupportedSchema
        );
    }

    #[test]
    fn every_binding_and_exact_deadline_are_rechecked() {
        let now = Instant::now();
        let expires = now + Duration::from_secs(1);
        let all = Capabilities::new(ACTIONS);
        let prepare = || {
            validate(
                Candidate {
                    spec: ActionSpec::new(Action::Copy),
                    target: Target::Local,
                    captured: CONTEXT,
                    expires,
                },
                CONTEXT,
                now,
                ENABLED,
                &all,
            )
            .unwrap()
        };
        let mut variants = [CONTEXT; 5];
        variants[0].device[0] ^= 1;
        variants[1].target[0] ^= 1;
        variants[2].focus_generation += 1;
        variants[3].configuration_generation += 1;
        variants[4].trigger_sequence += 1;
        for changed in variants {
            assert_eq!(
                prepare()
                    .revalidate(changed, now, ENABLED, &all)
                    .unwrap_err(),
                Rejection::ContextChanged
            );
        }
        assert_eq!(
            prepare()
                .revalidate(CONTEXT, expires, ENABLED, &all)
                .unwrap_err(),
            Rejection::Expired
        );
        assert_eq!(
            prepare()
                .revalidate(CONTEXT, now, LocalPolicy::default(), &all)
                .unwrap_err(),
            Rejection::Disabled
        );
        assert_eq!(
            prepare()
                .revalidate(CONTEXT, now, ENABLED, &Capabilities::default())
                .unwrap_err(),
            Rejection::Unsupported
        );
        assert_eq!(
            prepare()
                .revalidate(CONTEXT, now, ENABLED, &all)
                .unwrap()
                .spec()
                .action,
            Action::Copy
        );
    }

    #[test]
    fn destructive_and_non_idempotent_actions_are_distinct() {
        let now = Instant::now();
        let caps = Capabilities::new(ACTIONS);
        for action in [
            Action::CloseTab,
            Action::CloseWindow,
            Action::QuitApplication,
        ] {
            let prepare = |policy| {
                validate(
                    Candidate {
                        spec: ActionSpec::new(action),
                        target: Target::Local,
                        captured: CONTEXT,
                        expires: now + Duration::from_secs(1),
                    },
                    CONTEXT,
                    now,
                    policy,
                    &caps,
                )
            };
            assert_eq!(
                prepare(ENABLED).unwrap_err(),
                Rejection::ConfirmationRequired
            );
            let confirmed = LocalPolicy {
                destructive_confirmed: true,
                ..ENABLED
            };
            assert!(prepare(confirmed).is_ok());
            assert_eq!(
                prepare(confirmed)
                    .unwrap()
                    .revalidate(CONTEXT, now, ENABLED, &caps)
                    .unwrap_err(),
                Rejection::ConfirmationRequired
            );
        }
        for action in [
            Action::Paste,
            Action::Back,
            Action::Forward,
            Action::ToggleFullscreen,
            Action::Copy,
        ] {
            assert_eq!(action.replay(), Replay::NeverAutomatic);
        }
        assert_eq!(Action::MaximizeWindow.replay(), Replay::IdempotentInContext);
        assert_ne!(Action::MaximizeWindow, Action::ToggleFullscreen);
    }
}
