//! Public AX window actions only. No guessed AXFullScreen, zoom alias or keys.
use super::*;
use peerglide_actions::{Action, Capabilities};
use peerglide_actions::execution::NativeFailure;

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXUIElementIsAttributeSettable(element: Ref, attribute: Ref, settable: *mut u8) -> i32;
    fn AXUIElementSetAttributeValue(element: Ref, attribute: Ref, value: Ref) -> i32;
    fn AXUIElementPerformAction(element: Ref, action: Ref) -> i32;
}
#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFBooleanTrue: Ref;
    fn CFBooleanGetTypeID() -> usize;
    fn CFBooleanGetValue(value: Ref) -> u8;
}

pub(in crate::context) fn action_capabilities() -> Capabilities {
    Capabilities::new([Action::MinimizeWindow, Action::CloseWindow])
}
fn query_failure(error: i32) -> NativeFailure {
    match error {
        -25211 => NativeFailure::PermissionLost,
        -25202 => NativeFailure::TargetLost,
        _ => NativeFailure::BeforeSubmission,
    }
}
fn mutation_result(error: i32) -> Result<(), NativeFailure> {
    match error {
        0 => Ok(()),
        -25211 => Err(NativeFailure::PermissionLost),
        -25202 => Err(NativeFailure::TargetLost),
        -25201 | -25205 | -25206 | -25208 => Err(NativeFailure::BeforeSubmission),
        // CannotComplete may mean an application entered a modal operation and
        // did not return promptly. Never retry or call that definite failure.
        _ => Err(NativeFailure::UnknownOutcome),
    }
}
fn name(value: &'static std::ffi::CStr) -> Result<Owned, NativeFailure> {
    Owned::new(unsafe { CFStringCreateWithCString(null(), value.as_ptr(), 0x0800_0100) })
        .ok_or(NativeFailure::BeforeSubmission)
}
fn enabled_value(is_boolean: bool, value: u8) -> Result<bool, NativeFailure> {
    if !is_boolean { return Err(NativeFailure::BeforeSubmission); }
    Ok(value != 0)
}
fn enabled_button(button: &Owned) -> Result<bool, NativeFailure> {
    let attribute = name(c"AXEnabled")?;
    let mut result = null();
    let error = unsafe { AXUIElementCopyAttributeValue(button.ptr(), attribute.ptr(), &mut result) };
    if error != 0 { return Err(query_failure(error)); }
    let value = Owned::new(result).ok_or(NativeFailure::BeforeSubmission)?;
    let is_boolean = unsafe { CFGetTypeID(value.ptr()) == CFBooleanGetTypeID() };
    if !is_boolean { return enabled_value(false, 0); }
    enabled_value(true, unsafe { CFBooleanGetValue(value.ptr()) })
}
trait WindowOps {
    type Button;
    fn current(&mut self) -> Result<bool, NativeFailure>;
    fn can_minimize(&mut self) -> Result<bool, NativeFailure>;
    fn close_button(&mut self) -> Result<Self::Button, NativeFailure>;
    fn minimize(&mut self) -> Result<(), NativeFailure>;
    fn press_close(&mut self, button: Self::Button) -> Result<(), NativeFailure>;
}
fn submit<O: WindowOps>(action: Action, ops: &mut O, guard: &mut dyn FnMut() -> Result<(), NativeFailure>) -> Result<(), NativeFailure> {
    if !matches!(action, Action::MinimizeWindow | Action::CloseWindow) { return Err(NativeFailure::BeforeSubmission); }
    if !ops.current()? { return Err(NativeFailure::TargetLost); }
    match action {
        Action::MinimizeWindow => {
            if !ops.can_minimize()? { return Err(NativeFailure::BeforeSubmission); }
            guard()?;
            ops.minimize()
        }
        Action::CloseWindow => {
            let button = ops.close_button()?;
            guard()?;
            ops.press_close(button)
        }
        _ => Err(NativeFailure::BeforeSubmission),
    }
}
struct NativeOps<'a> { target: &'a Target, minimize_attribute: Option<Owned> }
impl WindowOps for NativeOps<'_> {
    type Button = (Owned, Owned);
    fn current(&mut self) -> Result<bool, NativeFailure> {
        self.target.matches_current().map_err(|error| match error {
            ContextError::PermissionDenied => NativeFailure::PermissionLost,
            _ => NativeFailure::TargetLost,
        })
    }
    fn can_minimize(&mut self) -> Result<bool, NativeFailure> {
        let window = &self.target.focus.as_ref().ok_or(NativeFailure::TargetLost)?.window;
        let attribute = name(c"AXMinimized")?;
        let mut settable = 0u8;
        let result = unsafe { AXUIElementIsAttributeSettable(window.ptr(), attribute.ptr(), &mut settable) };
        if result != 0 { return Err(query_failure(result)); }
        self.minimize_attribute = Some(attribute);
        Ok(settable != 0)
    }
    fn close_button(&mut self) -> Result<Self::Button, NativeFailure> {
        let window = &self.target.focus.as_ref().ok_or(NativeFailure::TargetLost)?.window;
        let button = copy_element(window, c"AXCloseButton", self.target.process_id).map_err(|error| match error {
            ContextError::PermissionDenied => NativeFailure::PermissionLost,
            ContextError::ChangedDuringRead => NativeFailure::TargetLost,
            _ => NativeFailure::BeforeSubmission,
        })?;
        let owner = copy_element(&button, c"AXWindow", self.target.process_id).map_err(|_| NativeFailure::BeforeSubmission)?;
        if !unsafe { CFEqual(owner.ptr(), window.ptr()) } { return Err(NativeFailure::TargetLost); }
        if !enabled_button(&button)? { return Err(NativeFailure::BeforeSubmission); }
        Ok((button, name(c"AXPress")?))
    }
    fn minimize(&mut self) -> Result<(), NativeFailure> {
        let window = &self.target.focus.as_ref().ok_or(NativeFailure::TargetLost)?.window;
        let attribute = self.minimize_attribute.take().ok_or(NativeFailure::BeforeSubmission)?;
        // Borrowed global constant: never wrap it in Owned or CFRelease it.
        mutation_result(unsafe { AXUIElementSetAttributeValue(window.ptr(), attribute.ptr(), kCFBooleanTrue) })
    }
    fn press_close(&mut self, (button, action): Self::Button) -> Result<(), NativeFailure> {
        mutation_result(unsafe { AXUIElementPerformAction(button.ptr(), action.ptr()) })
    }
}
pub(in crate::context) fn submit_window_action(target: &Target, action: Action, guard: &mut dyn FnMut() -> Result<(), NativeFailure>) -> Result<(), NativeFailure> {
    submit(action, &mut NativeOps { target, minimize_attribute: None }, guard)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Default)]
    struct Fake { queries: usize, effects: usize, supported: bool, close_enabled: bool, failure: Option<NativeFailure> }
    impl WindowOps for Fake {
        type Button = ();
        fn current(&mut self) -> Result<bool, NativeFailure> { self.queries += 1; Ok(true) }
        fn can_minimize(&mut self) -> Result<bool, NativeFailure> { self.queries += 1; Ok(self.supported) }
        fn close_button(&mut self) -> Result<(), NativeFailure> { self.queries += 1; if self.close_enabled { Ok(()) } else { Err(NativeFailure::BeforeSubmission) } }
        fn minimize(&mut self) -> Result<(), NativeFailure> { self.effects += 1; self.failure.map_or(Ok(()), Err) }
        fn press_close(&mut self, _: ()) -> Result<(), NativeFailure> { self.effects += 1; self.failure.map_or(Ok(()), Err) }
    }
    #[test]
    fn unsupported_semantics_never_query_or_submit_or_alias() {
        let caps = action_capabilities();
        let mut fake = Fake::default();
        for action in [Action::Copy, Action::Paste, Action::CloseTab, Action::QuitApplication, Action::MaximizeWindow, Action::ToggleFullscreen] {
            assert!(!caps.supports(action));
            assert_eq!(submit(action, &mut fake, &mut || panic!("unsupported guard")), Err(NativeFailure::BeforeSubmission));
        }
        assert_eq!((fake.queries, fake.effects), (0, 0));
    }
    #[test]
    fn late_guard_and_settable_checks_precede_every_effect() {
        let mut fake = Fake::default();
        assert_eq!(submit(Action::MinimizeWindow, &mut fake, &mut || Ok(())), Err(NativeFailure::BeforeSubmission));
        fake.supported = true;
        fake.close_enabled = true;
        for action in [Action::MinimizeWindow, Action::CloseWindow] {
            assert_eq!(submit(action, &mut fake, &mut || Err(NativeFailure::PermissionLost)), Err(NativeFailure::PermissionLost));
        }
        assert_eq!(fake.effects, 0);
        submit(Action::MinimizeWindow, &mut fake, &mut || Ok(())).unwrap();
        submit(Action::CloseWindow, &mut fake, &mut || Ok(())).unwrap();
        assert_eq!(fake.effects, 2);
    }
    #[test]
    fn disabled_or_untyped_close_buttons_never_reach_guard_or_effect() {
        assert_eq!(enabled_value(false, 1), Err(NativeFailure::BeforeSubmission));
        assert_eq!(enabled_value(true, 0), Ok(false));
        assert_eq!(enabled_value(true, 1), Ok(true));
        let mut fake = Fake::default();
        assert_eq!(submit(Action::CloseWindow, &mut fake, &mut || panic!("disabled close guard")), Err(NativeFailure::BeforeSubmission));
        assert_eq!(fake.effects, 0);
    }
    #[test]
    fn submission_uncertainty_is_not_retried_or_reported_executed() {
        let mut fake = Fake { supported: true, close_enabled: true, failure: Some(NativeFailure::UnknownOutcome), ..Fake::default() };
        assert_eq!(submit(Action::CloseWindow, &mut fake, &mut || Ok(())), Err(NativeFailure::UnknownOutcome));
        assert_eq!(fake.effects, 1);
        assert_eq!(mutation_result(-25204), Err(NativeFailure::UnknownOutcome));
        assert_eq!(mutation_result(-25200), Err(NativeFailure::UnknownOutcome));
        assert_eq!(query_failure(-25204), NativeFailure::BeforeSubmission);
        assert_eq!(mutation_result(-25211), Err(NativeFailure::PermissionLost));
        assert_eq!(mutation_result(0), Ok(()));
    }
}
