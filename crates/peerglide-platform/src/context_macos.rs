//! NSWorkspace/AX reads only. Never asks for trust or creates an event tap.
#[path = "context_events_macos.rs"]
mod events;
use super::{
    identifier, ContextError, FocusStatus, PermissionReport, PermissionStatus, SecureInput,
};
pub(super) use events::Watch;
use objc2::rc::{autoreleasepool, Retained};
use objc2_app_kit::{NSRunningApplication, NSWorkspace};
use objc2_foundation::NSString;
use std::{
    ffi::{c_char, c_void},
    ptr::{null, NonNull},
};

type Ref = *const c_void;
#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn CGPreflightListenEventAccess() -> bool;
    fn AXUIElementCreateApplication(pid: i32) -> Ref;
    fn AXUIElementCreateSystemWide() -> Ref;
    fn AXUIElementCopyAttributeValue(element: Ref, attribute: Ref, value: *mut Ref) -> i32;
    fn AXUIElementSetMessagingTimeout(element: Ref, seconds: f32) -> i32;
    fn AXUIElementGetTypeID() -> usize;
    fn AXUIElementGetPid(element: Ref, pid: *mut i32) -> i32;
}
#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFStringCreateWithCString(allocator: Ref, bytes: *const c_char, encoding: u32) -> Ref;
    fn CFGetTypeID(value: Ref) -> usize;
    fn CFEqual(first: Ref, second: Ref) -> bool;
    fn CFRelease(value: Ref);
}
#[link(name = "Carbon", kind = "framework")]
extern "C" {
    fn IsSecureEventInputEnabled() -> bool;
}

struct Owned(NonNull<c_void>);
impl Owned {
    fn new(value: Ref) -> Option<Self> {
        NonNull::new(value.cast_mut()).map(Self)
    }
    fn ptr(&self) -> Ref {
        self.0.as_ptr().cast_const()
    }
}
impl Drop for Owned {
    fn drop(&mut self) {
        unsafe { CFRelease(self.ptr()) };
    }
}
struct Focus {
    window: Owned,
    element: Owned,
}
pub(super) struct Target {
    application: Retained<NSRunningApplication>,
    process_id: i32,
    application_id: String,
    focus: Option<Focus>,
}

pub(super) fn permissions() -> PermissionReport {
    PermissionReport {
        accessibility: if unsafe { AXIsProcessTrusted() } {
            PermissionStatus::Granted
        } else {
            PermissionStatus::Denied
        },
        input_monitoring: if unsafe { CGPreflightListenEventAccess() } {
            PermissionStatus::Granted
        } else {
            PermissionStatus::Denied
        },
        input_desktop: PermissionStatus::NotRequired,
        secure_input: if unsafe { IsSecureEventInputEnabled() } {
            SecureInput::Active
        } else {
            SecureInput::Inactive
        },
    }
}

fn bundle_id(application: &NSRunningApplication) -> Result<String, ContextError> {
    let value = application
        .bundleIdentifier()
        .ok_or(ContextError::IdentifierUnavailable)?;
    bounded_bundle_id(&value)
}
fn bounded_bundle_id(value: &NSString) -> Result<String, ContextError> {
    // Only ASCII IDs are accepted, so at most 245 UTF-16 units can fit after
    // "mac-bundle:". Bound conversion before allocating a Rust string.
    if value.length() > 245 {
        return Err(ContextError::IdentifierTooLong);
    }
    let value = value.to_string();
    // Apple bundle IDs are ASCII and case-sensitive. Never invent an ID from a
    // title/display name or accept a lossy NSString replacement character.
    if !valid_bundle_id(&value) {
        return Err(ContextError::IdentifierUnavailable);
    }
    identifier("mac-bundle", &value)
}
fn valid_bundle_id(value: &str) -> bool {
    !value.is_empty()
        && value.split('.').all(|part| !part.is_empty())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.'))
}

fn copy_element(
    application: &Owned,
    name: &'static std::ffi::CStr,
    pid: i32,
) -> Result<Owned, ContextError> {
    let attribute =
        Owned::new(unsafe { CFStringCreateWithCString(null(), name.as_ptr(), 0x0800_0100) })
            .ok_or(ContextError::QueryFailed)?;
    let mut result = null();
    let error =
        unsafe { AXUIElementCopyAttributeValue(application.ptr(), attribute.ptr(), &mut result) };
    // Copy ownership transfers only on success; an error's output is undefined.
    if error != 0 {
        return Err(if error == -25211 {
            ContextError::PermissionDenied
        } else {
            ContextError::NoFocus
        });
    }
    let result = Owned::new(result).ok_or(ContextError::NoFocus)?;
    if unsafe { CFGetTypeID(result.ptr()) } != unsafe { AXUIElementGetTypeID() } {
        return Err(ContextError::NoFocus);
    }
    let mut owner = 0;
    if unsafe { AXUIElementGetPid(result.ptr(), &mut owner) } != 0 || owner != pid {
        return Err(ContextError::ChangedDuringRead);
    }
    Ok(result)
}
fn read_focus(pid: i32) -> Result<Focus, ContextError> {
    let system =
        Owned::new(unsafe { AXUIElementCreateSystemWide() }).ok_or(ContextError::QueryFailed)?;
    // A timeout on the system-wide AX reference applies only to this process's
    // AX messaging, not OS permissions or the other application's settings.
    if unsafe { AXUIElementSetMessagingTimeout(system.ptr(), 0.1) } != 0 {
        return Err(ContextError::QueryFailed);
    }
    let _foreground = copy_element(&system, c"AXFocusedApplication", pid)?;
    let application = Owned::new(unsafe { AXUIElementCreateApplication(pid) })
        .ok_or(ContextError::QueryFailed)?;
    // AX IPC stays outside callbacks and gets an explicit per-object timeout.
    if unsafe { AXUIElementSetMessagingTimeout(application.ptr(), 0.1) } != 0 {
        return Err(ContextError::QueryFailed);
    }
    let window = copy_element(&application, c"AXFocusedWindow", pid)?;
    let element = copy_element(&application, c"AXFocusedUIElement", pid)?;
    let window_after = copy_element(&application, c"AXFocusedWindow", pid)?;
    let element_after = copy_element(&application, c"AXFocusedUIElement", pid)?;
    let _foreground_after = copy_element(&system, c"AXFocusedApplication", pid)?;
    if !unsafe { CFEqual(window.ptr(), window_after.ptr()) }
        || !unsafe { CFEqual(element.ptr(), element_after.ptr()) }
    {
        return Err(ContextError::ChangedDuringRead);
    }
    Ok(Focus { window, element })
}

pub(super) fn observe() -> Result<(String, Target, FocusStatus), ContextError> {
    autoreleasepool(|_| {
        let workspace = NSWorkspace::sharedWorkspace();
        let application = workspace
            .frontmostApplication()
            .ok_or(ContextError::NoForeground)?;
        if application.isTerminated() {
            return Err(ContextError::ChangedDuringRead);
        }
        let application_id = bundle_id(&application)?;
        let pid = application.processIdentifier();
        if pid <= 0 {
            return Err(ContextError::IdentifierUnavailable);
        }
        let permission = permissions();
        let (focus, status) = if permission.accessibility != PermissionStatus::Granted
            || permission.secure_input == SecureInput::Active
        {
            (None, FocusStatus::PermissionDenied)
        } else {
            match read_focus(pid) {
                Ok(focus) => (Some(focus), FocusStatus::Available),
                Err(ContextError::ChangedDuringRead) => {
                    return Err(ContextError::ChangedDuringRead)
                }
                Err(ContextError::PermissionDenied) => (None, FocusStatus::PermissionDenied),
                Err(_) => (None, FocusStatus::Unavailable),
            }
        };
        let after = workspace
            .frontmostApplication()
            .ok_or(ContextError::ChangedDuringRead)?;
        // NSRunningApplication equality compares process instances; PID alone
        // is explicitly insufficient, including after automatic termination.
        if application.isTerminated()
            || application != after
            || after.processIdentifier() != pid
            || bundle_id(&after)? != application_id
        {
            return Err(ContextError::ChangedDuringRead);
        }
        Ok((
            application_id.clone(),
            Target {
                application,
                process_id: pid,
                application_id,
                focus,
            },
            status,
        ))
    })
}
impl Target {
    pub(super) fn matches_current(&self) -> Result<bool, ContextError> {
        if self.application.isTerminated()
            || self.application.processIdentifier() != self.process_id
        {
            return Ok(false);
        }
        let (application_id, current, status) = observe()?;
        if status == FocusStatus::PermissionDenied {
            return Err(ContextError::PermissionDenied);
        }
        let (Some(old), Some(new)) = (&self.focus, &current.focus) else {
            return Err(ContextError::NoFocus);
        };
        Ok(!self.application.isTerminated()
            && self.application == current.application
            && self.process_id == current.process_id
            && self.application_id == application_id
            && unsafe { CFEqual(old.window.ptr(), new.window.ptr()) }
            && unsafe { CFEqual(old.element.ptr(), new.element.ptr()) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bundle_identity_uses_case_preserving_ascii_not_titles_or_paths() {
        assert!(valid_bundle_id("com.Example.Editor"));
        for invalid in [
            "",
            "com..Editor",
            "window title",
            "/Applications/Editor.app",
            "应用",
            "com.\u{fffd}",
        ] {
            assert!(!valid_bundle_id(invalid));
        }
        assert_eq!(
            bounded_bundle_id(&NSString::from_str(&"a".repeat(245)))
                .unwrap()
                .len(),
            256
        );
        assert_eq!(
            bounded_bundle_id(&NSString::from_str(&"a".repeat(246))),
            Err(ContextError::IdentifierTooLong)
        );
    }
}
