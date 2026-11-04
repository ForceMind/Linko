//! Narrow window-action submission to a frozen local foreground HWND.
//! Successful async submission never proves the application completed the action.
use crate::context::ContextError;
use peerglide_actions::{execution::NativeFailure, Action, Capabilities};
use windows_sys::Win32::{
    Foundation::{
        GetLastError, SetLastError, ERROR_ACCESS_DENIED, ERROR_INVALID_WINDOW_HANDLE, HWND,
    },
    UI::WindowsAndMessaging::{
        GetClassLongW, GetMenuState, GetSystemMenu, GetWindowLongW, IsWindow, PostMessageW,
        ShowWindowAsync, CS_NOCLOSE, GCL_STYLE, GWL_STYLE, MF_BYCOMMAND, MF_DISABLED, MF_GRAYED,
        MF_POPUP, MF_SEPARATOR, SC_CLOSE, SW_MAXIMIZE, SW_MINIMIZE, WM_CLOSE, WS_CHILD,
        WS_DISABLED, WS_MAXIMIZEBOX, WS_MINIMIZEBOX, WS_SYSMENU, WS_VISIBLE,
    },
};

pub(in crate::context) fn action_capabilities() -> Capabilities {
    Capabilities::new([
        Action::MinimizeWindow,
        Action::MaximizeWindow,
        Action::CloseWindow,
    ])
}

fn native_error(error: u32) -> NativeFailure {
    match error {
        ERROR_ACCESS_DENIED => NativeFailure::PermissionLost,
        ERROR_INVALID_WINDOW_HANDLE => NativeFailure::TargetLost,
        _ => NativeFailure::BeforeSubmission,
    }
}

fn context_error(error: ContextError) -> NativeFailure {
    match error {
        ContextError::PermissionDenied => NativeFailure::PermissionLost,
        ContextError::NoForeground | ContextError::NoFocus | ContextError::ChangedDuringRead => {
            NativeFailure::TargetLost
        }
        _ => NativeFailure::BeforeSubmission,
    }
}

/// This seam injects queries and submissions only in tests. It is not an action
/// grant, and opaque test HWNDs must never be used by the native implementation.
trait WindowOps {
    fn validate_target(&mut self) -> Result<HWND, NativeFailure>;
    fn window_style(&mut self, window: HWND) -> Result<u32, NativeFailure>;
    fn class_style(&mut self, window: HWND) -> Result<u32, NativeFailure>;
    fn close_item_state(&mut self, window: HWND) -> Result<u32, NativeFailure>;
    fn show_async(&mut self, window: HWND, command: i32) -> Result<(), NativeFailure>;
    fn post_close(&mut self, window: HWND) -> Result<(), NativeFailure>;
}

struct NativeWindow<'a> {
    target: &'a super::Target,
}
impl WindowOps for NativeWindow<'_> {
    fn validate_target(&mut self) -> Result<HWND, NativeFailure> {
        let window = self.target.window.foreground;
        if window.is_null() || !self.target.matches_current().map_err(context_error)? {
            return Err(NativeFailure::TargetLost);
        }
        if unsafe { IsWindow(window) } == 0 {
            return Err(NativeFailure::TargetLost);
        }
        let owner = super::owner(window).map_err(context_error)?;
        if owner != (self.target.window.process_id, self.target.window.thread_id) {
            return Err(NativeFailure::TargetLost);
        }
        Ok(window)
    }

    fn window_style(&mut self, window: HWND) -> Result<u32, NativeFailure> {
        // Only 32-bit style bits are requested, never a pointer/handle value.
        // Zero can be a valid result, so clear and immediately save last-error.
        unsafe { SetLastError(0) };
        let style = unsafe { GetWindowLongW(window, GWL_STYLE) };
        let error = unsafe { GetLastError() };
        if style == 0 && error != 0 {
            return Err(native_error(error));
        }
        Ok(style as u32)
    }

    fn class_style(&mut self, window: HWND) -> Result<u32, NativeFailure> {
        unsafe { SetLastError(0) };
        let style = unsafe { GetClassLongW(window, GCL_STYLE) };
        let error = unsafe { GetLastError() };
        if style == 0 && error != 0 {
            return Err(native_error(error));
        }
        Ok(style)
    }

    fn close_item_state(&mut self, window: HWND) -> Result<u32, NativeFailure> {
        // FALSE accesses the current window menu. This handle is borrowed: do
        // not reset it, modify an item, destroy the menu or display it.
        let menu = unsafe { GetSystemMenu(window, 0) };
        let _error = unsafe { GetLastError() };
        if menu.is_null() {
            return Err(NativeFailure::BeforeSubmission);
        }
        let state = unsafe { GetMenuState(menu, SC_CLOSE, MF_BYCOMMAND) };
        let _error = unsafe { GetLastError() };
        if state == u32::MAX {
            // Missing/unknown items are never inferred to be enabled. Neither
            // query grants a permission, regardless of an optional last-error.
            return Err(NativeFailure::BeforeSubmission);
        }
        Ok(state)
    }

    fn show_async(&mut self, window: HWND, command: i32) -> Result<(), NativeFailure> {
        // ShowWindowAsync reports whether the operation was started; ShowWindow
        // reports previous visibility and is intentionally not used here.
        unsafe { SetLastError(0) };
        let submitted = unsafe { ShowWindowAsync(window, command) };
        let error = unsafe { GetLastError() };
        if submitted == 0 {
            // ShowWindowAsync does not promise a detailed error on failure. An
            // unset/unrecognized code means before-submission failure only.
            return Err(native_error(error));
        }
        Ok(())
    }

    fn post_close(&mut self, window: HWND) -> Result<(), NativeFailure> {
        unsafe { SetLastError(0) };
        let submitted = unsafe { PostMessageW(window, WM_CLOSE, 0, 0) };
        let error = unsafe { GetLastError() };
        if submitted == 0 {
            return Err(native_error(error));
        }
        Ok(())
    }
}

fn submit_with(
    ops: &mut impl WindowOps,
    action: Action,
    guard: &mut dyn FnMut() -> Result<(), NativeFailure>,
) -> Result<(), NativeFailure> {
    // The Coordinator rejects these as Unsupported via capabilities. Reject a
    // direct unsupported invocation before any query, guard or effect as well.
    if !matches!(
        action,
        Action::MinimizeWindow | Action::MaximizeWindow | Action::CloseWindow
    ) {
        return Err(NativeFailure::BeforeSubmission);
    }
    let window = ops.validate_target()?;
    if window.is_null() {
        return Err(NativeFailure::TargetLost);
    }
    let style = ops.window_style(window)?;
    if style & (WS_CHILD | WS_DISABLED) != 0 || style & WS_VISIBLE == 0 || style & WS_SYSMENU == 0 {
        return Err(NativeFailure::BeforeSubmission);
    }
    match action {
        Action::MinimizeWindow => {
            if style & WS_MINIMIZEBOX == 0 {
                return Err(NativeFailure::BeforeSubmission);
            }
            guard()?;
            ops.show_async(window, SW_MINIMIZE)
        }
        Action::MaximizeWindow => {
            if style & WS_MAXIMIZEBOX == 0 {
                return Err(NativeFailure::BeforeSubmission);
            }
            guard()?;
            ops.show_async(window, SW_MAXIMIZE)
        }
        Action::CloseWindow => {
            if ops.class_style(window)? & CS_NOCLOSE != 0 {
                return Err(NativeFailure::BeforeSubmission);
            }
            let state = ops
                .close_item_state(window)
                .map_err(|_| NativeFailure::BeforeSubmission)?;
            if state == u32::MAX || state & (MF_DISABLED | MF_GRAYED | MF_POPUP | MF_SEPARATOR) != 0
            {
                return Err(NativeFailure::BeforeSubmission);
            }
            guard()?;
            // Exactly WM_CLOSE to the frozen top-level window. No tab shortcut,
            // application-quit command, process termination or follow-new-focus.
            ops.post_close(window)
        }
        _ => Err(NativeFailure::BeforeSubmission),
    }
}

pub(in crate::context) fn submit_window_action(
    target: &super::Target,
    action: Action,
    guard: &mut dyn FnMut() -> Result<(), NativeFailure>,
) -> Result<(), NativeFailure> {
    submit_with(&mut NativeWindow { target }, action, guard)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, rc::Rc};

    const FULL_STYLE: u32 = WS_VISIBLE | WS_SYSMENU | WS_MINIMIZEBOX | WS_MAXIMIZEBOX;
    type Trace = Rc<RefCell<Vec<&'static str>>>;

    // Test-only values never leave FakeWindow and are never native/auth handles.
    fn fake_window() -> HWND {
        std::ptr::dangling_mut::<std::ffi::c_void>()
    }
    struct FakeWindow {
        trace: Trace,
        target: Result<HWND, NativeFailure>,
        style: Result<u32, NativeFailure>,
        class: Result<u32, NativeFailure>,
        close_state: Result<u32, NativeFailure>,
        submission: Result<(), NativeFailure>,
        effects: Vec<i32>,
    }
    impl FakeWindow {
        fn new() -> Self {
            Self {
                trace: Rc::new(RefCell::new(Vec::new())),
                target: Ok(fake_window()),
                style: Ok(FULL_STYLE),
                class: Ok(0),
                close_state: Ok(0),
                submission: Ok(()),
                effects: Vec::new(),
            }
        }
        fn effect(
            &mut self,
            window: HWND,
            command: i32,
            label: &'static str,
        ) -> Result<(), NativeFailure> {
            assert_eq!(window, fake_window());
            assert_eq!(self.trace.borrow().last(), Some(&"guard"));
            self.trace.borrow_mut().push(label);
            self.effects.push(command);
            self.submission
        }
    }
    impl WindowOps for FakeWindow {
        fn validate_target(&mut self) -> Result<HWND, NativeFailure> {
            self.trace.borrow_mut().push("target");
            self.target
        }
        fn window_style(&mut self, window: HWND) -> Result<u32, NativeFailure> {
            assert_eq!(window, fake_window());
            self.trace.borrow_mut().push("style");
            self.style
        }
        fn class_style(&mut self, window: HWND) -> Result<u32, NativeFailure> {
            assert_eq!(window, fake_window());
            self.trace.borrow_mut().push("class");
            self.class
        }
        fn close_item_state(&mut self, window: HWND) -> Result<u32, NativeFailure> {
            assert_eq!(window, fake_window());
            self.trace.borrow_mut().push("menu");
            self.close_state
        }
        fn show_async(&mut self, window: HWND, command: i32) -> Result<(), NativeFailure> {
            assert!(matches!(command, SW_MINIMIZE | SW_MAXIMIZE));
            self.effect(window, command, "show")
        }
        fn post_close(&mut self, window: HWND) -> Result<(), NativeFailure> {
            self.effect(window, WM_CLOSE as i32, "close")
        }
    }
    fn guard(
        trace: Trace,
        result: Result<(), NativeFailure>,
    ) -> impl FnMut() -> Result<(), NativeFailure> {
        move || {
            trace.borrow_mut().push("guard");
            result
        }
    }

    #[test]
    fn unsupported_actions_have_no_queries_guards_or_effects_and_are_not_aliases() {
        let capabilities = action_capabilities();
        for action in peerglide_actions::ACTIONS {
            let supported = matches!(
                action,
                Action::MinimizeWindow | Action::MaximizeWindow | Action::CloseWindow
            );
            assert_eq!(capabilities.supports(action), supported);
            if supported {
                continue;
            }
            let mut ops = FakeWindow::new();
            let mut guard = guard(ops.trace.clone(), Ok(()));
            assert_eq!(
                submit_with(&mut ops, action, &mut guard),
                Err(NativeFailure::BeforeSubmission)
            );
            assert!(ops.trace.borrow().is_empty() && ops.effects.is_empty());
        }
    }

    #[test]
    fn each_action_guards_after_queries_and_submits_exactly_one_distinct_operation() {
        for (action, command, expected) in [
            (
                Action::MinimizeWindow,
                SW_MINIMIZE,
                vec!["target", "style", "guard", "show"],
            ),
            (
                Action::MaximizeWindow,
                SW_MAXIMIZE,
                vec!["target", "style", "guard", "show"],
            ),
            (
                Action::CloseWindow,
                WM_CLOSE as i32,
                vec!["target", "style", "class", "menu", "guard", "close"],
            ),
        ] {
            let mut ops = FakeWindow::new();
            let mut guard = guard(ops.trace.clone(), Ok(()));
            assert_eq!(submit_with(&mut ops, action, &mut guard), Ok(()));
            assert_eq!(ops.effects, [command]);
            assert_eq!(*ops.trace.borrow(), expected);
        }
    }

    #[test]
    fn late_guard_failure_never_submits_even_after_successful_native_queries() {
        for action in [
            Action::MinimizeWindow,
            Action::MaximizeWindow,
            Action::CloseWindow,
        ] {
            for failure in [
                NativeFailure::PermissionLost,
                NativeFailure::TargetLost,
                NativeFailure::BeforeSubmission,
            ] {
                let mut ops = FakeWindow::new();
                let mut guard = guard(ops.trace.clone(), Err(failure));
                assert_eq!(submit_with(&mut ops, action, &mut guard), Err(failure));
                assert_eq!(ops.trace.borrow().last(), Some(&"guard"));
                assert!(ops.effects.is_empty());
            }
        }
    }

    #[test]
    fn unavailable_window_capabilities_and_disabled_close_never_submit() {
        for (action, style, class) in [
            (Action::MinimizeWindow, FULL_STYLE & !WS_MINIMIZEBOX, 0),
            (Action::MaximizeWindow, FULL_STYLE & !WS_MAXIMIZEBOX, 0),
            (Action::CloseWindow, FULL_STYLE, CS_NOCLOSE),
            (Action::CloseWindow, FULL_STYLE | WS_CHILD, 0),
            (Action::CloseWindow, FULL_STYLE | WS_DISABLED, 0),
            (Action::CloseWindow, FULL_STYLE & !WS_VISIBLE, 0),
            (Action::CloseWindow, FULL_STYLE & !WS_SYSMENU, 0),
        ] {
            let mut ops = FakeWindow::new();
            ops.style = Ok(style);
            ops.class = Ok(class);
            let mut guard = guard(ops.trace.clone(), Ok(()));
            assert_eq!(
                submit_with(&mut ops, action, &mut guard),
                Err(NativeFailure::BeforeSubmission)
            );
            assert!(!ops.trace.borrow().contains(&"guard") && ops.effects.is_empty());
        }
    }

    #[test]
    fn disabled_missing_or_unknown_close_menu_item_never_reaches_guard_or_submission() {
        for state in [
            Ok(MF_DISABLED),
            Ok(MF_GRAYED),
            Ok(MF_DISABLED | MF_GRAYED),
            Ok(MF_POPUP),
            Ok(MF_SEPARATOR),
            Ok(u32::MAX),
            Err(NativeFailure::BeforeSubmission),
            Err(NativeFailure::PermissionLost),
            Err(NativeFailure::TargetLost),
            Err(NativeFailure::UnknownOutcome),
        ] {
            let mut ops = FakeWindow::new();
            ops.close_state = state;
            let mut guard = guard(ops.trace.clone(), Ok(()));
            assert_eq!(
                submit_with(&mut ops, Action::CloseWindow, &mut guard),
                Err(NativeFailure::BeforeSubmission)
            );
            assert_eq!(*ops.trace.borrow(), ["target", "style", "class", "menu"]);
            assert!(ops.effects.is_empty());
        }
    }

    #[test]
    fn native_query_failures_precede_guard_and_failed_submission_is_never_retried() {
        let mut missing = FakeWindow::new();
        missing.target = Ok(std::ptr::null_mut());
        let mut missing_guard = guard(missing.trace.clone(), Ok(()));
        assert_eq!(
            submit_with(&mut missing, Action::CloseWindow, &mut missing_guard),
            Err(NativeFailure::TargetLost)
        );
        assert!(!missing.trace.borrow().contains(&"guard") && missing.effects.is_empty());
        for failure in [
            NativeFailure::PermissionLost,
            NativeFailure::TargetLost,
            NativeFailure::BeforeSubmission,
        ] {
            for phase in 0..4 {
                let mut ops = FakeWindow::new();
                match phase {
                    0 => ops.target = Err(failure),
                    1 => ops.style = Err(failure),
                    2 => ops.class = Err(failure),
                    _ => ops.close_state = Err(failure),
                }
                let mut guard = guard(ops.trace.clone(), Ok(()));
                let expected = if phase == 3 {
                    NativeFailure::BeforeSubmission
                } else {
                    failure
                };
                assert_eq!(
                    submit_with(&mut ops, Action::CloseWindow, &mut guard),
                    Err(expected)
                );
                assert!(!ops.trace.borrow().contains(&"guard") && ops.effects.is_empty());
            }
            for action in [
                Action::MinimizeWindow,
                Action::MaximizeWindow,
                Action::CloseWindow,
            ] {
                let mut ops = FakeWindow::new();
                ops.submission = Err(failure);
                let mut guard = guard(ops.trace.clone(), Ok(()));
                assert_eq!(submit_with(&mut ops, action, &mut guard), Err(failure));
                assert_eq!(ops.effects.len(), 1);
            }
        }
    }

    #[test]
    fn native_error_classification_preserves_known_permission_and_target_failures() {
        assert_eq!(
            native_error(ERROR_ACCESS_DENIED),
            NativeFailure::PermissionLost
        );
        assert_eq!(
            native_error(ERROR_INVALID_WINDOW_HANDLE),
            NativeFailure::TargetLost
        );
        assert_eq!(native_error(0), NativeFailure::BeforeSubmission);
        assert_eq!(native_error(1816), NativeFailure::BeforeSubmission);
        assert_eq!(
            context_error(ContextError::PermissionDenied),
            NativeFailure::PermissionLost
        );
        assert_eq!(
            context_error(ContextError::ChangedDuringRead),
            NativeFailure::TargetLost
        );
        assert_eq!(
            context_error(ContextError::QueryFailed),
            NativeFailure::BeforeSubmission
        );
    }
}
