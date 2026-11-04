//! Main-thread workspace/AX notifications. No event tap or permission request.
use super::*;
use crate::context::watch::{Signal, WatchError};
use block2::RcBlock;
use objc2::{
    runtime::{AnyObject, ProtocolObject},
    MainThreadMarker,
};
use objc2_app_kit::{
    NSWorkspaceDidActivateApplicationNotification, NSWorkspaceDidDeactivateApplicationNotification,
    NSWorkspaceDidTerminateApplicationNotification, NSWorkspaceDidWakeNotification,
    NSWorkspaceSessionDidBecomeActiveNotification, NSWorkspaceSessionDidResignActiveNotification,
    NSWorkspaceWillSleepNotification,
};
use objc2_foundation::{NSNotification, NSNotificationCenter, NSObjectProtocol};
use std::{cell::Cell, sync::Arc};

type RunRef = *mut c_void;
type Callback = unsafe extern "C" fn(Ref, Ref, Ref, *mut c_void);
#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXObserverCreate(pid: i32, callback: Callback, observer: *mut Ref) -> i32;
    fn AXObserverAddNotification(
        observer: Ref,
        element: Ref,
        name: Ref,
        context: *mut c_void,
    ) -> i32;
    fn AXObserverRemoveNotification(observer: Ref, element: Ref, name: Ref) -> i32;
    fn AXObserverGetRunLoopSource(observer: Ref) -> RunRef;
}
#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFRunLoopDefaultMode: RunRef;
    static kCFRunLoopCommonModes: RunRef;
    fn CFRunLoopGetCurrent() -> RunRef;
    fn CFRunLoopAddSource(loop_: RunRef, source: RunRef, mode: RunRef);
    fn CFRunLoopRemoveSource(loop_: RunRef, source: RunRef, mode: RunRef);
    fn CFRunLoopRunInMode(mode: RunRef, seconds: f64, return_after_source: bool) -> i32;
}
thread_local! { static BUSY: Cell<bool> = const { Cell::new(false) }; }
const MAX_PUMP: usize = 128;

unsafe extern "C" fn invalidated(_: Ref, _: Ref, _: Ref, context: *mut c_void) {
    if !context.is_null() {
        // AX callbacks run only on the owning run loop. Binding teardown removes
        // its source/releases observer before Watch releases the Arc backing it.
        unsafe { (&*context.cast::<Signal>()).notify() };
    }
}
fn cf_name(name: &'static std::ffi::CStr) -> Result<Owned, WatchError> {
    Owned::new(unsafe { CFStringCreateWithCString(null(), name.as_ptr(), 0x0800_0100) })
        .ok_or(WatchError::NativeUnavailable)
}
fn same_target(first: &Target, second: &Target) -> bool {
    let (Some(a), Some(b)) = (&first.focus, &second.focus) else {
        return false;
    };
    first.application == second.application
        && first.process_id == second.process_id
        && first.application_id == second.application_id
        && unsafe { CFEqual(a.window.ptr(), b.window.ptr()) }
        && unsafe { CFEqual(a.element.ptr(), b.element.ptr()) }
}

struct Binding {
    target: Target,
    observer: Option<Owned>,
    application: Owned,
    registrations: Vec<(Ref, Owned)>,
    loop_: RunRef,
    source: RunRef,
    attached: bool,
}
impl Binding {
    fn new(target: Target, signal: &Arc<Signal>) -> Result<Self, WatchError> {
        let mut pointer = null();
        let error = unsafe { AXObserverCreate(target.process_id, invalidated, &mut pointer) };
        if error != 0 {
            return Err(WatchError::NotReady);
        }
        let observer = Owned::new(pointer).ok_or(WatchError::NativeUnavailable)?;
        let application = Owned::new(unsafe { AXUIElementCreateApplication(target.process_id) })
            .ok_or(WatchError::NativeUnavailable)?;
        let source = unsafe { AXObserverGetRunLoopSource(observer.ptr()) };
        if source.is_null() {
            return Err(WatchError::NativeUnavailable);
        }
        let mut binding = Self {
            target,
            observer: Some(observer),
            application,
            registrations: Vec::with_capacity(4),
            loop_: unsafe { CFRunLoopGetCurrent() },
            source,
            attached: false,
        };
        binding.add(binding.application.ptr(), c"AXFocusedWindowChanged", signal)?;
        binding.add(
            binding.application.ptr(),
            c"AXFocusedUIElementChanged",
            signal,
        )?;
        let focus = binding.target.focus.as_ref().ok_or(WatchError::NotReady)?;
        let window = focus.window.ptr();
        let element = focus.element.ptr();
        binding.add(window, c"AXUIElementDestroyed", signal)?;
        if !unsafe { CFEqual(window, element) } {
            binding.add(element, c"AXUIElementDestroyed", signal)?;
        }
        unsafe { CFRunLoopAddSource(binding.loop_, source, kCFRunLoopCommonModes) };
        binding.attached = true;
        Ok(binding)
    }
    fn add(
        &mut self,
        element: Ref,
        name: &'static std::ffi::CStr,
        signal: &Arc<Signal>,
    ) -> Result<(), WatchError> {
        let name = cf_name(name)?;
        let observer = self.observer.as_ref().ok_or(WatchError::Closed)?;
        let error = unsafe {
            AXObserverAddNotification(
                observer.ptr(),
                element,
                name.ptr(),
                Arc::as_ptr(signal).cast_mut().cast(),
            )
        };
        // Every required notification must be supported. No polling fallback.
        if error != 0 {
            return Err(WatchError::NotReady);
        }
        self.registrations.push((element, name));
        Ok(())
    }
    fn stop(&mut self) {
        let Some(observer) = self.observer.take() else {
            return;
        };
        if self.attached {
            unsafe { CFRunLoopRemoveSource(self.loop_, self.source, kCFRunLoopCommonModes) };
            self.attached = false;
        }
        for (element, name) in self.registrations.drain(..) {
            // A destroyed remote element can already have lost its subscription.
            // Releasing this owned observer below definitively removes our local
            // source even when remote explicit removal cannot complete.
            let _ = unsafe { AXObserverRemoveNotification(observer.ptr(), element, name.ptr()) };
        }
        // The run-loop source is borrowed, never separately CFRelease it.
        drop(observer);
    }
}
impl Drop for Binding {
    fn drop(&mut self) {
        self.stop();
    }
}

type ObserverToken = Retained<ProtocolObject<dyn NSObjectProtocol>>;
pub(in crate::context) struct Watch {
    center: Retained<NSNotificationCenter>,
    tokens: Vec<ObserverToken>,
    binding: Option<Binding>,
    signal: Arc<Signal>,
    permission: Option<PermissionReport>,
    stopped: bool,
    _main: MainThreadMarker,
}
impl Watch {
    pub(in crate::context) fn new(signal: Arc<Signal>) -> Result<Self, WatchError> {
        let main = MainThreadMarker::new().ok_or(WatchError::WrongThread)?;
        let permission = permissions();
        if permission.accessibility != PermissionStatus::Granted
            || permission.secure_input == SecureInput::Active
        {
            return Err(WatchError::Context(ContextError::PermissionDenied));
        }
        if BUSY.with(|busy| busy.replace(true)) {
            return Err(WatchError::Busy);
        }
        let center = NSWorkspace::sharedWorkspace().notificationCenter();
        let mut watch = Self {
            center,
            tokens: Vec::with_capacity(7),
            binding: None,
            signal,
            permission: None,
            stopped: false,
            _main: main,
        };
        let names = unsafe {
            [
                NSWorkspaceDidActivateApplicationNotification,
                NSWorkspaceDidDeactivateApplicationNotification,
                NSWorkspaceDidTerminateApplicationNotification,
                NSWorkspaceWillSleepNotification,
                NSWorkspaceDidWakeNotification,
                NSWorkspaceSessionDidBecomeActiveNotification,
                NSWorkspaceSessionDidResignActiveNotification,
            ]
        };
        for name in names {
            let signal = watch.signal.clone();
            let block: RcBlock<dyn Fn(NonNull<NSNotification>)> =
                RcBlock::new(move |_: NonNull<NSNotification>| signal.notify());
            // The block captures only thread-safe atomic state. It never touches
            // main-thread objects, allocates or queries AX inside the callback.
            let token = unsafe {
                watch.center.addObserverForName_object_queue_usingBlock(
                    Some(name),
                    None,
                    None,
                    &block,
                )
            };
            watch.tokens.push(token);
        }
        watch.refresh()?;
        Ok(watch)
    }
    pub(in crate::context) fn pump(&mut self) -> Result<(), WatchError> {
        if self.stopped {
            return Err(WatchError::Closed);
        }
        for _ in 0..MAX_PUMP {
            // Default is a concrete mode; common modes are only for registration.
            let result = unsafe { CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.0, true) };
            match result {
                4 => (),
                3 => return Ok(()),
                _ => {
                    self.signal.notify();
                    return Err(WatchError::NotReady);
                }
            }
        }
        self.signal.notify();
        Err(WatchError::Backlog)
    }
    pub(in crate::context) fn refresh(&mut self) -> Result<(), WatchError> {
        if self.stopped {
            return Err(WatchError::Closed);
        }
        let permission = permissions();
        if self.permission != Some(permission) {
            self.signal.notify();
            self.permission = Some(permission);
        }
        if permission.accessibility != PermissionStatus::Granted
            || permission.secure_input == SecureInput::Active
        {
            self.signal.notify();
            self.binding.take();
            return Err(WatchError::Context(ContextError::PermissionDenied));
        }
        let target = match observe() {
            Ok((_, target, FocusStatus::Available)) => target,
            Ok(_) => {
                self.signal.notify();
                self.binding.take();
                return Err(WatchError::NotReady);
            }
            Err(error) => {
                self.signal.notify();
                self.binding.take();
                return Err(error.into());
            }
        };
        if self
            .binding
            .as_ref()
            .is_some_and(|binding| same_target(&binding.target, &target))
        {
            return Ok(());
        }
        self.signal.notify();
        // Drop old native subscription before replacing it. A failed new bind
        // leaves no usable cached target and all existing contexts invalid.
        self.binding.take();
        self.binding = Some(Binding::new(target, &self.signal)?);
        Ok(())
    }
    pub(in crate::context) fn accepts(&self, target: &Target) -> Result<bool, WatchError> {
        if self.stopped {
            return Err(WatchError::Closed);
        }
        if !self
            .binding
            .as_ref()
            .is_some_and(|binding| same_target(&binding.target, target))
        {
            self.signal.notify();
            return Ok(false);
        }
        let current = target.matches_current()?;
        if !current {
            self.signal.notify();
        }
        Ok(current)
    }
    pub(in crate::context) fn shutdown(&mut self) -> Result<(), WatchError> {
        if self.stopped {
            return Ok(());
        }
        self.signal.close();
        self.stopped = true;
        self.binding.take();
        for token in self.tokens.drain(..) {
            let object: &AnyObject =
                <ProtocolObject<dyn NSObjectProtocol> as AsRef<AnyObject>>::as_ref(&*token);
            unsafe { self.center.removeObserver(object) };
        }
        BUSY.with(|busy| busy.set(false));
        Ok(())
    }
}
impl Drop for Watch {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}
