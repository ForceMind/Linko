//! Read-only native foreground/focus observations, never execution authority.
//! No input hook, permission prompt, window title, injected event or network I/O.
//! A point-in-time match cannot detect focus leaving and returning between reads;
//! G1 still needs native change notifications, token invalidation and consent.

#[path = "context_watch.rs"]
pub mod watch;

#[cfg(target_os = "macos")]
#[path = "context_macos.rs"]
mod native;
#[cfg(windows)]
#[path = "context_windows.rs"]
mod native;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PermissionStatus {
    Granted,
    Denied,
    NotRequired,
    Unknown,
    Unsupported,
}
impl PermissionStatus {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Granted => "granted",
            Self::Denied => "denied",
            Self::NotRequired => "not_required",
            Self::Unknown => "unknown",
            Self::Unsupported => "unsupported",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SecureInput {
    Active,
    Inactive,
    Unknown,
    Unsupported,
}
impl SecureInput {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Inactive => "inactive",
            Self::Unknown => "unknown",
            Self::Unsupported => "unsupported",
        }
    }
}

/// OS observations only. Even Granted/NotRequired never creates a user grant,
/// proves injection can succeed, or enables an action Backend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PermissionReport {
    pub accessibility: PermissionStatus,
    pub input_monitoring: PermissionStatus,
    pub input_desktop: PermissionStatus,
    pub secure_input: SecureInput,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FocusStatus {
    Available,
    Unavailable,
    PermissionDenied,
    Unsupported,
}
impl FocusStatus {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Unavailable => "unavailable",
            Self::PermissionDenied => "permission_denied",
            Self::Unsupported => "unsupported",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContextError {
    Unsupported,
    NoForeground,
    NoFocus,
    ChangedDuringRead,
    PermissionDenied,
    IdentifierUnavailable,
    IdentifierTooLong,
    QueryFailed,
}
impl ContextError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Unsupported => "unsupported",
            Self::NoForeground => "no_foreground",
            Self::NoFocus => "no_focus",
            Self::ChangedDuringRead => "changed_during_read",
            Self::PermissionDenied => "permission_denied",
            Self::IdentifierUnavailable => "identifier_unavailable",
            Self::IdentifierTooLong => "identifier_too_long",
            Self::QueryFailed => "query_failed",
        }
    }
}

/// Private native references live only in this process/thread. This type has no
/// Debug/Serialize implementation and is NOT an actions::Context or target ticket.
pub struct Foreground {
    application_id: String,
    focus: FocusStatus,
    target: native::Target,
}
impl Foreground {
    pub fn application_id(&self) -> &str {
        &self.application_id
    }
    pub fn focus(&self) -> FocusStatus {
        self.focus
    }
    /// Requery the native application, window and focused element. This is only
    /// a new observation, not an atomic check-and-dispatch or focus generation.
    pub fn matches_current(&self) -> Result<bool, ContextError> {
        match self.focus {
            FocusStatus::Available => self.target.matches_current(),
            FocusStatus::PermissionDenied => Err(ContextError::PermissionDenied),
            _ => Err(ContextError::NoFocus),
        }
    }
}

pub fn permissions() -> PermissionReport {
    native::permissions()
}
pub fn observe_foreground() -> Result<Foreground, ContextError> {
    let (application_id, target, focus) = native::observe()?;
    // Reuse the exact rule-store bound, never truncate or invent a basename ID.
    if application_id.len() > 256 {
        return Err(ContextError::IdentifierTooLong);
    }
    if !peerglide_actions::rules::valid_application_id(&application_id) {
        return Err(ContextError::IdentifierUnavailable);
    }
    Ok(Foreground {
        application_id,
        focus,
        target,
    })
}

#[cfg(any(windows, target_os = "macos", test))]
fn identifier(prefix: &str, value: &str) -> Result<String, ContextError> {
    if value.is_empty() || value.trim() != value || value.chars().any(char::is_control) {
        return Err(ContextError::IdentifierUnavailable);
    }
    let id = format!("{prefix}:{value}");
    if id.len() > 256 {
        return Err(ContextError::IdentifierTooLong);
    }
    Ok(id)
}

#[cfg(not(any(windows, target_os = "macos")))]
mod native {
    use super::watch::{Signal, WatchError};
    use super::*;
    use std::sync::Arc;
    pub(super) struct Watch;
    impl Watch {
        pub(super) fn new(_: Arc<Signal>) -> Result<Self, WatchError> {
            Err(WatchError::Unsupported)
        }
        pub(super) fn pump(&mut self) -> Result<(), WatchError> {
            Err(WatchError::Unsupported)
        }
        pub(super) fn refresh(&mut self) -> Result<(), WatchError> {
            Err(WatchError::Unsupported)
        }
        pub(super) fn accepts(&self, _: &Target) -> Result<bool, WatchError> {
            Err(WatchError::Unsupported)
        }
        pub(super) fn shutdown(&mut self) -> Result<(), WatchError> {
            Ok(())
        }
    }
    pub(super) struct Target;
    pub(super) fn permissions() -> PermissionReport {
        PermissionReport {
            accessibility: PermissionStatus::Unsupported,
            input_monitoring: PermissionStatus::Unsupported,
            input_desktop: PermissionStatus::Unsupported,
            secure_input: SecureInput::Unsupported,
        }
    }
    pub(super) fn observe() -> Result<(String, Target, FocusStatus), ContextError> {
        Err(ContextError::Unsupported)
    }
    impl Target {
        pub(super) fn matches_current(&self) -> Result<bool, ContextError> {
            Err(ContextError::Unsupported)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_ids_preserve_case_unicode_and_exact_rule_byte_bound() {
        assert_eq!(
            identifier("mac-bundle", "com.Example.Editor").unwrap(),
            "mac-bundle:com.Example.Editor"
        );
        assert_eq!(
            identifier("win-exe", r"C:\应用\Editor.exe").unwrap(),
            r"win-exe:C:\应用\Editor.exe"
        );
        assert_eq!(identifier("x", &"a".repeat(254)).unwrap().len(), 256);
        assert_eq!(
            identifier("x", &"a".repeat(255)),
            Err(ContextError::IdentifierTooLong)
        );
        assert_eq!(
            identifier("x", &"中".repeat(85)),
            Err(ContextError::IdentifierTooLong)
        );
        for invalid in ["", " leading", "trailing ", "bad\nvalue", "bad\0value"] {
            assert_eq!(
                identifier("x", invalid),
                Err(ContextError::IdentifierUnavailable)
            );
        }
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    #[test]
    fn unsupported_platform_does_not_fabricate_context_or_permission() {
        assert!(matches!(
            observe_foreground(),
            Err(ContextError::Unsupported)
        ));
        assert_eq!(permissions().accessibility, PermissionStatus::Unsupported);
    }
}
