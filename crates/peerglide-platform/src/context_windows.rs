//! Read-only foreground queries. No hooks, injection, focus changes or prompts.
#[path = "context_actions_windows.rs"]
mod actions;
#[path = "context_recovery_windows.rs"]
mod recovery;
pub(super) use actions::{action_capabilities, submit_window_action};
pub(super) use recovery::{recovery_dpi, validate_recovery};
#[path = "context_events_windows.rs"]
mod events;
use super::{
    identifier, ContextError, FocusStatus, PermissionReport, PermissionStatus, SecureInput,
};
pub(super) use events::Watch;
use std::mem::size_of;
use windows_sys::Win32::{
    Foundation::{
        CloseHandle, GetLastError, ERROR_ACCESS_DENIED, ERROR_INSUFFICIENT_BUFFER, FILETIME,
        HANDLE, HWND,
    },
    System::{
        StationsAndDesktops::{
            CloseDesktop, GetThreadDesktop, GetUserObjectInformationW, OpenInputDesktop,
            DESKTOP_READOBJECTS, HDESK, UOI_NAME,
        },
        Threading::{
            GetCurrentThreadId, GetProcessTimes, OpenProcess, QueryFullProcessImageNameW,
            PROCESS_QUERY_LIMITED_INFORMATION,
        },
    },
    UI::WindowsAndMessaging::{
        GetForegroundWindow, GetGUIThreadInfo, GetWindowThreadProcessId, GUITHREADINFO,
    },
};

const PATH_CAPACITY: usize = 32_768;
const DESKTOP_NAME_CAPACITY: usize = 1024;

/// Only OpenInputDesktop handles are owned. GetThreadDesktop returns a borrowed
/// handle which must never be placed in this wrapper or closed by us.
struct InputDesktop(HDESK);
impl Drop for InputDesktop {
    fn drop(&mut self) {
        unsafe { CloseDesktop(self.0) };
    }
}

struct ProcessHandle(HANDLE);
impl Drop for ProcessHandle {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct WindowSnapshot {
    foreground: HWND,
    focused: HWND,
    process_id: u32,
    thread_id: u32,
}

/// These private native references are observations, never authentication or an
/// execution ticket. Keeping the process handle binds its creation-time query
/// to that process instance, even if the numeric PID is subsequently reused.
pub(super) struct Target {
    desktop: String,
    window: WindowSnapshot,
    process: ProcessHandle,
    created: u64,
}
impl Target {
    pub(super) fn matches_current(&self) -> Result<bool, ContextError> {
        let (_, current, focus) = observe()?;
        Ok(focus == FocusStatus::Available
            && self.desktop == current.desktop
            && same_target(self.window, self.created, current.window, current.created)
            && creation_time(&self.process)? == self.created)
    }
}

fn same_target(
    previous: WindowSnapshot,
    previous_created: u64,
    current: WindowSnapshot,
    current_created: u64,
) -> bool {
    previous == current && previous_created == current_created
}

pub(super) fn permissions() -> PermissionReport {
    // A successful read only proves this desktop handle was obtainable. It does
    // not prove UIPI/integrity compatibility or authorize capture/injection.
    let desktop = unsafe { OpenInputDesktop(0, 0, DESKTOP_READOBJECTS) };
    let input_desktop = if desktop.is_null() {
        desktop_failure(unsafe { GetLastError() })
    } else {
        let _desktop = InputDesktop(desktop);
        PermissionStatus::Granted
    };
    PermissionReport {
        // Windows has no identically named macOS consent gate; these statuses
        // do not mean all Windows security boundaries have been satisfied.
        accessibility: PermissionStatus::NotRequired,
        input_monitoring: PermissionStatus::NotRequired,
        input_desktop,
        secure_input: SecureInput::Unsupported,
    }
}

fn desktop_failure(error: u32) -> PermissionStatus {
    if error == ERROR_ACCESS_DENIED {
        PermissionStatus::Denied
    } else {
        PermissionStatus::Unknown
    }
}

fn query_error(error: u32) -> ContextError {
    if error == ERROR_ACCESS_DENIED {
        ContextError::PermissionDenied
    } else {
        ContextError::QueryFailed
    }
}

fn desktop_name_from_buffer(buffer: &[u16], needed_bytes: u32) -> Result<String, ContextError> {
    let needed_bytes = needed_bytes as usize;
    if needed_bytes == 0 || !needed_bytes.is_multiple_of(size_of::<u16>()) {
        return Err(ContextError::QueryFailed);
    }
    let units = needed_bytes / size_of::<u16>();
    let contents = buffer.get(..units).ok_or(ContextError::QueryFailed)?;
    let Some((&0, name)) = contents.split_last() else {
        return Err(ContextError::QueryFailed);
    };
    if name.is_empty() || name.contains(&0) {
        return Err(ContextError::QueryFailed);
    }
    String::from_utf16(name).map_err(|_| ContextError::QueryFailed)
}

fn desktop_name(desktop: HDESK) -> Result<String, ContextError> {
    let mut buffer = [0u16; DESKTOP_NAME_CAPACITY];
    let mut needed_bytes = 0;
    if unsafe {
        GetUserObjectInformationW(
            desktop,
            UOI_NAME,
            buffer.as_mut_ptr().cast(),
            size_of::<[u16; DESKTOP_NAME_CAPACITY]>() as u32,
            &mut needed_bytes,
        )
    } == 0
    {
        return Err(query_error(unsafe { GetLastError() }));
    }
    desktop_name_from_buffer(&buffer, needed_bytes)
}

fn require_same_desktop(thread: &str, input: &str) -> Result<(), ContextError> {
    if thread.is_empty() || input.is_empty() {
        Err(ContextError::QueryFailed)
    } else if thread != input {
        // A mismatch can have many causes. Never label it UAC/secure desktop,
        // switch desktops or return a stale foreground from the old desktop.
        Err(ContextError::PermissionDenied)
    } else {
        Ok(())
    }
}

fn confirmed_input_desktop() -> Result<String, ContextError> {
    let desktop = unsafe { OpenInputDesktop(0, 0, DESKTOP_READOBJECTS) };
    if desktop.is_null() {
        return Err(query_error(unsafe { GetLastError() }));
    }
    let input = InputDesktop(desktop);
    let thread = unsafe { GetThreadDesktop(GetCurrentThreadId()) };
    if thread.is_null() {
        return Err(query_error(unsafe { GetLastError() }));
    }
    // Both objects belong to the calling process's window station. Compare
    // bounded, losslessly decoded names; never compare unequal handle values.
    let thread_name = desktop_name(thread)?;
    let input_name = desktop_name(input.0)?;
    require_same_desktop(&thread_name, &input_name)?;
    Ok(input_name)
}

fn owner(window: HWND) -> Result<(u32, u32), ContextError> {
    let mut process_id = 0;
    let thread_id = unsafe { GetWindowThreadProcessId(window, &mut process_id) };
    if thread_id == 0 {
        return Err(query_error(unsafe { GetLastError() }));
    }
    if process_id == 0 {
        return Err(ContextError::ChangedDuringRead);
    }
    Ok((process_id, thread_id))
}

fn focus_status(
    focused: HWND,
    owner_pid: Option<u32>,
    foreground_pid: u32,
) -> Result<FocusStatus, ContextError> {
    if focused.is_null() {
        return Ok(FocusStatus::Unavailable);
    }
    if owner_pid != Some(foreground_pid) {
        return Err(ContextError::ChangedDuringRead);
    }
    Ok(FocusStatus::Available)
}

fn window_snapshot() -> Result<WindowSnapshot, ContextError> {
    let foreground = unsafe { GetForegroundWindow() };
    if foreground.is_null() {
        return Err(ContextError::NoForeground);
    }
    let (process_id, thread_id) = owner(foreground)?;
    let mut info: GUITHREADINFO = unsafe { std::mem::zeroed() };
    info.cbSize = size_of::<GUITHREADINFO>() as u32;
    if unsafe { GetGUIThreadInfo(thread_id, &mut info) } == 0 {
        return Err(query_error(unsafe { GetLastError() }));
    }
    let focused = info.hwndFocus;
    let focused_pid = if focused.is_null() {
        None
    } else {
        Some(owner(focused)?.0)
    };
    focus_status(focused, focused_pid, process_id)?;
    if unsafe { GetForegroundWindow() } != foreground
        || owner(foreground)? != (process_id, thread_id)
    {
        return Err(ContextError::ChangedDuringRead);
    }
    Ok(WindowSnapshot {
        foreground,
        focused,
        process_id,
        thread_id,
    })
}

fn creation_time(process: &ProcessHandle) -> Result<u64, ContextError> {
    let mut created: FILETIME = unsafe { std::mem::zeroed() };
    let mut exited = created;
    let mut kernel = created;
    let mut user = created;
    if unsafe { GetProcessTimes(process.0, &mut created, &mut exited, &mut kernel, &mut user) } == 0
    {
        return Err(query_error(unsafe { GetLastError() }));
    }
    Ok((u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
}

fn path_identifier(buffer: &[u16]) -> Result<String, ContextError> {
    let path = String::from_utf16(buffer).map_err(|_| ContextError::IdentifierUnavailable)?;
    identifier("win-exe", &path)
}

fn application_id(process: &ProcessHandle) -> Result<String, ContextError> {
    // One bounded allocation. An insufficient buffer is an explicit rejection,
    // never a truncated path, basename fallback or lossy UTF-16 conversion.
    let mut buffer = vec![0u16; PATH_CAPACITY];
    let mut length = buffer.len() as u32;
    if unsafe { QueryFullProcessImageNameW(process.0, 0, buffer.as_mut_ptr(), &mut length) } == 0 {
        let error = unsafe { GetLastError() };
        return Err(if error == ERROR_INSUFFICIENT_BUFFER {
            ContextError::IdentifierTooLong
        } else {
            query_error(error)
        });
    }
    let length = length as usize;
    if length == 0 || length > buffer.len() {
        return Err(ContextError::IdentifierUnavailable);
    }
    path_identifier(&buffer[..length])
}

pub(super) fn observe() -> Result<(String, Target, FocusStatus), ContextError> {
    let desktop = confirmed_input_desktop()?;
    let window = window_snapshot()?;
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, window.process_id) };
    if handle.is_null() {
        return Err(query_error(unsafe { GetLastError() }));
    }
    let process = ProcessHandle(handle);
    let created = creation_time(&process)?;
    let application_id = application_id(&process)?;
    if window_snapshot()? != window {
        return Err(ContextError::ChangedDuringRead);
    }
    let after_desktop = confirmed_input_desktop()?;
    require_same_desktop(&desktop, &after_desktop)?;
    let focus = if window.focused.is_null() {
        FocusStatus::Unavailable
    } else {
        FocusStatus::Available
    };
    Ok((
        application_id,
        Target {
            desktop,
            window,
            process,
            created,
        },
        focus,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ptr::null_mut;

    #[test]
    fn desktop_confirmation_requires_matching_nonempty_names() {
        assert_eq!(require_same_desktop("Default", "Default"), Ok(()));
        assert_eq!(
            require_same_desktop("Default", "AnotherDesktop"),
            Err(ContextError::PermissionDenied)
        );
        assert_eq!(
            require_same_desktop("", "Default"),
            Err(ContextError::QueryFailed)
        );
        assert_eq!(
            require_same_desktop("Default", ""),
            Err(ContextError::QueryFailed)
        );
    }

    #[test]
    fn desktop_names_require_complete_bounded_lossless_utf16() {
        let name = "桌面";
        let mut encoded: Vec<_> = name.encode_utf16().collect();
        encoded.push(0);
        let bytes = (encoded.len() * size_of::<u16>()) as u32;
        assert_eq!(desktop_name_from_buffer(&encoded, bytes).unwrap(), name);
        for (buffer, bytes) in [
            (&[0u16][..], 2),
            (&[0xd800, 0][..], 4),
            (&[65u16][..], 2),
            (&[65u16, 0][..], 6),
            (&[65u16, 0][..], 3),
            (&[65u16, 0][..], 0),
            (&[65u16, 0, 66, 0][..], 8),
        ] {
            assert_eq!(
                desktop_name_from_buffer(buffer, bytes),
                Err(ContextError::QueryFailed)
            );
        }
    }

    #[test]
    fn full_unicode_path_is_preserved_and_invalid_utf16_is_rejected() {
        let path = r"C:\应用\Editor.EXE";
        let encoded: Vec<_> = path.encode_utf16().collect();
        assert_eq!(
            path_identifier(&encoded).unwrap(),
            format!("win-exe:{path}")
        );
        assert_eq!(
            path_identifier(&[0xd800]),
            Err(ContextError::IdentifierUnavailable)
        );
        assert_eq!(
            path_identifier(&[]),
            Err(ContextError::IdentifierUnavailable)
        );
        assert_eq!(
            path_identifier(&[u16::from(b'a'); 249]),
            Err(ContextError::IdentifierTooLong)
        );
        assert_eq!(path_identifier(&[u16::from(b'a'); 248]).unwrap().len(), 256);
    }

    #[test]
    fn absent_focus_is_unavailable_and_foreign_process_focus_is_rejected() {
        let focused = std::ptr::dangling_mut::<std::ffi::c_void>();
        assert_eq!(
            focus_status(null_mut(), None, 7),
            Ok(FocusStatus::Unavailable)
        );
        assert_eq!(
            focus_status(focused, Some(7), 7),
            Ok(FocusStatus::Available)
        );
        assert_eq!(
            focus_status(focused, Some(8), 7),
            Err(ContextError::ChangedDuringRead)
        );
    }

    #[test]
    fn target_equality_checks_focused_child_and_process_instance() {
        let window = WindowSnapshot {
            foreground: null_mut(),
            focused: null_mut(),
            process_id: 7,
            thread_id: 9,
        };
        assert!(same_target(window, 11, window, 11));
        let mut changed = window;
        changed.foreground = std::ptr::dangling_mut::<std::ffi::c_void>();
        assert!(!same_target(window, 11, changed, 11));
        changed = window;
        changed.focused = std::ptr::dangling_mut::<std::ffi::c_void>();
        assert!(!same_target(window, 11, changed, 11));
        changed = window;
        changed.process_id += 1;
        assert!(!same_target(window, 11, changed, 11));
        changed = window;
        changed.thread_id += 1;
        assert!(!same_target(window, 11, changed, 11));
        assert!(!same_target(window, 11, window, 12));
    }

    #[test]
    fn access_denial_is_not_confused_with_unknown_query_failures() {
        assert_eq!(
            desktop_failure(ERROR_ACCESS_DENIED),
            PermissionStatus::Denied
        );
        assert_eq!(desktop_failure(0), PermissionStatus::Unknown);
        assert_eq!(
            query_error(ERROR_ACCESS_DENIED),
            ContextError::PermissionDenied
        );
        assert_eq!(query_error(0), ContextError::QueryFailed);
    }
}
