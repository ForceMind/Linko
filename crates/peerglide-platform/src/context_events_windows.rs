//! Worker-thread WinEvent/session invalidation. Never captures or injects input.
//! OUTOFCONTEXT callbacks run on the installing thread and only set an atomic bit.
use crate::context::watch::{Signal, WatchError};
use crate::context::{ContextError, FocusStatus, PermissionStatus};
use std::{
    cell::Cell,
    ffi::c_void,
    marker::PhantomData,
    mem::size_of,
    ptr::{null, null_mut},
    rc::Rc,
    sync::Arc,
};
use windows_sys::Win32::{
    Foundation::{GetLastError, HINSTANCE, HWND, LPARAM, LRESULT, WPARAM},
    System::{
        LibraryLoader::GetModuleHandleW,
        RemoteDesktop::{
            WTSActive, WTSConnectState, WTSFreeMemory, WTSQuerySessionInformationW,
            WTSRegisterSessionNotification, WTSUnRegisterSessionNotification,
            NOTIFY_FOR_THIS_SESSION, WTS_CURRENT_SERVER_HANDLE, WTS_CURRENT_SESSION,
        },
        Threading::{GetCurrentProcessId, GetCurrentThreadId},
    },
    UI::{
        Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK},
        WindowsAndMessaging::*,
    },
};

const MESSAGE_BUDGET: usize = 128;
const EVENTS: [u32; 4] = [
    EVENT_SYSTEM_FOREGROUND,
    EVENT_OBJECT_FOCUS,
    EVENT_OBJECT_DESTROY,
    EVENT_SYSTEM_DESKTOPSWITCH,
];

#[derive(Clone, Copy)]
struct Callbacks {
    signal: *const Signal,
    hooks: [HWINEVENTHOOK; EVENTS.len()],
    window: HWND,
    occupied: bool,
    poisoned: bool,
}
impl Callbacks {
    const EMPTY: Self = Self {
        signal: null(),
        hooks: [null_mut(); EVENTS.len()],
        window: null_mut(),
        occupied: false,
        poisoned: false,
    };

    fn detached(self) -> Self {
        Self {
            signal: null(),
            hooks: [null_mut(); EVENTS.len()],
            window: null_mut(),
            ..self
        }
    }

    fn released(self, failed: bool) -> Self {
        Self {
            poisoned: self.poisoned || failed,
            ..Self::EMPTY
        }
    }
}
thread_local! {
    // Constant, destructor-free TLS: callbacks perform no lazy allocation/locking.
    static CALLBACKS: Cell<Callbacks> = const { Cell::new(Callbacks::EMPTY) };
}

fn reservation(occupied: bool, poisoned: bool) -> Result<(), WatchError> {
    if poisoned {
        Err(WatchError::NativeUnavailable)
    } else if occupied {
        Err(WatchError::Busy)
    } else {
        Ok(())
    }
}

fn reserve_callbacks(signal: &Arc<Signal>) -> Result<(), WatchError> {
    CALLBACKS.with(|cell| {
        let current = cell.get();
        reservation(current.occupied, current.poisoned)?;
        cell.set(Callbacks {
            signal: Arc::as_ptr(signal),
            occupied: true,
            ..Callbacks::EMPTY
        });
        Ok(())
    })
}

trait CleanupOps {
    fn current_thread(&mut self) -> u32;
    fn unhook(&mut self, hook: HWINEVENTHOOK) -> bool;
    fn unregister_session(&mut self, window: HWND) -> bool;
    fn destroy_window(&mut self, window: HWND) -> bool;
    fn unregister_class(&mut self, class: &[u16], instance: HINSTANCE) -> bool;
}

struct NativeCleanup;
impl CleanupOps for NativeCleanup {
    fn current_thread(&mut self) -> u32 {
        unsafe { GetCurrentThreadId() }
    }
    fn unhook(&mut self, hook: HWINEVENTHOOK) -> bool {
        unsafe { UnhookWinEvent(hook) != 0 }
    }
    fn unregister_session(&mut self, window: HWND) -> bool {
        unsafe { WTSUnRegisterSessionNotification(window) != 0 }
    }
    fn destroy_window(&mut self, window: HWND) -> bool {
        unsafe { DestroyWindow(window) != 0 }
    }
    fn unregister_class(&mut self, class: &[u16], instance: HINSTANCE) -> bool {
        unsafe { UnregisterClassW(class.as_ptr(), instance) != 0 }
    }
}

unsafe extern "system" fn event_callback(
    hook: HWINEVENTHOOK,
    _event: u32,
    _window: HWND,
    _object: i32,
    _child: i32,
    _thread: u32,
    _time: u32,
) {
    let _ = CALLBACKS.try_with(|cell| {
        let state = cell.get();
        if !state.signal.is_null() && !hook.is_null() && state.hooks.contains(&hook) {
            // Arc storage belongs to this same-thread Watch. Shutdown clears
            // this pointer before any native unregister call or Arc release.
            unsafe { &*state.signal }.notify();
        }
    });
}

fn invalidating_message(message: u32) -> bool {
    matches!(
        message,
        WM_WTSSESSION_CHANGE
            | WM_POWERBROADCAST
            | WM_DISPLAYCHANGE
            | WM_SETTINGCHANGE
            | WM_ENDSESSION
            | WM_QUERYENDSESSION
            | WM_DESTROY
    )
}

unsafe extern "system" fn window_callback(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if invalidating_message(message) {
        let _ = CALLBACKS.try_with(|cell| {
            let state = cell.get();
            if !state.signal.is_null() && state.window == window {
                unsafe { &*state.signal }.notify();
            }
        });
        // Accept shutdown/power notification queries; never impede OS shutdown.
        if matches!(message, WM_QUERYENDSESSION | WM_POWERBROADCAST) {
            return 1;
        }
        return 0;
    }
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}

struct WtsMemory(*mut c_void);
impl Drop for WtsMemory {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { WTSFreeMemory(self.0) };
        }
    }
}

fn active_state(bytes: &[u8]) -> Result<(), WatchError> {
    let value: [u8; size_of::<i32>()] = bytes.try_into().map_err(|_| WatchError::NotReady)?;
    if i32::from_ne_bytes(value) == WTSActive {
        Ok(())
    } else {
        Err(WatchError::NotReady)
    }
}

fn active_session() -> Result<(), WatchError> {
    let mut buffer = null_mut();
    let mut bytes = 0;
    let success = unsafe {
        WTSQuerySessionInformationW(
            WTS_CURRENT_SERVER_HANDLE,
            WTS_CURRENT_SESSION,
            WTSConnectState,
            &mut buffer,
            &mut bytes,
        )
    };
    let error = if success == 0 {
        unsafe { GetLastError() }
    } else {
        0
    };
    if success == 0 {
        return Err(match super::query_error(error) {
            ContextError::PermissionDenied => ContextError::PermissionDenied.into(),
            _ => WatchError::NotReady,
        });
    }
    let memory = WtsMemory(buffer.cast());
    if memory.0.is_null() || bytes as usize != size_of::<i32>() {
        return Err(WatchError::NotReady);
    }
    // WTS owns this fixed-size buffer until WTSFreeMemory; do not assume alignment.
    let value = unsafe { memory.0.cast::<[u8; size_of::<i32>()]>().read_unaligned() };
    active_state(&value)
}

#[derive(PartialEq, Eq)]
struct Environment {
    desktop: String,
    permissions: crate::context::PermissionReport,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct BoundIdentity<'a> {
    desktop: &'a str,
    window: super::WindowSnapshot,
    created: u64,
}
fn bound_identity(target: &super::Target) -> BoundIdentity<'_> {
    BoundIdentity {
        desktop: &target.desktop,
        window: target.window,
        created: target.created,
    }
}

fn environment() -> Result<Environment, WatchError> {
    let desktop = super::confirmed_input_desktop()?;
    active_session()?;
    let permissions = super::permissions();
    match permissions.input_desktop {
        PermissionStatus::Granted => (),
        PermissionStatus::Denied => return Err(ContextError::PermissionDenied.into()),
        _ => return Err(WatchError::NotReady),
    }
    let after = super::confirmed_input_desktop()?;
    super::require_same_desktop(&desktop, &after)?;
    // Recheck active state after desktop reads: disconnected sessions may expose
    // the desktop that would become active only upon a future reconnection.
    active_session()?;
    Ok(Environment {
        desktop,
        permissions,
    })
}

/// One dedicated worker queue owner. PhantomData prevents safe cross-thread
/// movement; Windows requires unregistering WinEvent on its installing thread.
pub(in crate::context) struct Watch {
    signal: Arc<Signal>,
    thread: u32,
    hooks: [HWINEVENTHOOK; EVENTS.len()],
    window: HWND,
    instance: HINSTANCE,
    class: Vec<u16>,
    class_registered: bool,
    session_registered: bool,
    reserved: bool,
    closed: bool,
    cleanup_failed: bool,
    ready: bool,
    observed: Option<Environment>,
    cached: Option<super::Target>,
    _thread: PhantomData<Rc<()>>,
}
impl Watch {
    pub(in crate::context) fn new(signal: Arc<Signal>) -> Result<Self, WatchError> {
        reserve_callbacks(&signal)?;
        let mut watch = Self {
            signal,
            thread: unsafe { GetCurrentThreadId() },
            hooks: [null_mut(); EVENTS.len()],
            window: null_mut(),
            instance: null_mut(),
            class: Vec::new(),
            class_registered: false,
            session_registered: false,
            reserved: true,
            closed: false,
            cleanup_failed: false,
            ready: false,
            observed: None,
            cached: None,
            _thread: PhantomData,
        };
        if let Err(error) = watch.install().and_then(|_| watch.refresh()) {
            watch.shutdown()?;
            return Err(error);
        }
        Ok(watch)
    }

    fn install(&mut self) -> Result<(), WatchError> {
        self.instance = unsafe { GetModuleHandleW(null()) };
        if self.instance.is_null() {
            return Err(WatchError::NativeUnavailable);
        }
        self.class = format!(
            "PeerGlide.ContextWatch.{}.{}",
            unsafe { GetCurrentProcessId() },
            self.thread,
        )
        .encode_utf16()
        .chain(Some(0))
        .collect();
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_callback),
            hInstance: self.instance,
            lpszClassName: self.class.as_ptr(),
            ..Default::default()
        };
        if unsafe { RegisterClassW(&class) } == 0 {
            return Err(WatchError::NativeUnavailable);
        }
        self.class_registered = true;
        // Hidden top-level window receives broadcasts. No WS_VISIBLE, ShowWindow,
        // activation, message-only parent, keyboard hook or input registration.
        self.window = unsafe {
            CreateWindowExW(
                WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
                self.class.as_ptr(),
                self.class.as_ptr(),
                WS_POPUP,
                0,
                0,
                0,
                0,
                null_mut(),
                null_mut(),
                self.instance,
                null(),
            )
        };
        if self.window.is_null() {
            return Err(WatchError::NativeUnavailable);
        }
        self.publish_callbacks();
        if unsafe { WTSRegisterSessionNotification(self.window, NOTIFY_FOR_THIS_SESSION) } == 0 {
            return Err(WatchError::NativeUnavailable);
        }
        self.session_registered = true;
        for (index, event) in EVENTS.into_iter().enumerate() {
            let hook = unsafe {
                SetWinEventHook(
                    event,
                    event,
                    null_mut(),
                    Some(event_callback),
                    0,
                    0,
                    WINEVENT_OUTOFCONTEXT,
                )
            };
            if hook.is_null() {
                return Err(WatchError::NativeUnavailable);
            }
            self.hooks[index] = hook;
            self.publish_callbacks();
        }
        self.signal.notify();
        Ok(())
    }

    fn publish_callbacks(&self) {
        CALLBACKS.with(|cell| {
            let mut state = cell.get();
            state.hooks = self.hooks;
            state.window = self.window;
            cell.set(state);
        });
    }

    fn check(&self) -> Result<(), WatchError> {
        if unsafe { GetCurrentThreadId() } != self.thread {
            return Err(WatchError::WrongThread);
        }
        if self.closed {
            return Err(WatchError::Closed);
        }
        let registered = CALLBACKS.with(|cell| {
            let state = cell.get();
            !state.poisoned
                && state.occupied
                && state.signal == Arc::as_ptr(&self.signal)
                && state.hooks == self.hooks
                && state.window == self.window
        });
        if !registered
            || !self.session_registered
            || self.hooks.iter().any(|hook| hook.is_null())
            || self.window.is_null()
            || unsafe { IsWindow(self.window) } == 0
        {
            self.signal.close();
            return Err(WatchError::NativeUnavailable);
        }
        Ok(())
    }

    pub(in crate::context) fn pump(&mut self) -> Result<(), WatchError> {
        self.check()?;
        let mut message: MSG = unsafe { std::mem::zeroed() };
        for _ in 0..MESSAGE_BUDGET {
            if unsafe { PeekMessageW(&mut message, null_mut(), 0, 0, PM_REMOVE) } == 0 {
                return Ok(());
            }
            if message.message == WM_QUIT {
                self.shutdown()?;
                return Err(WatchError::Closed);
            }
            unsafe { DispatchMessageW(&message) };
        }
        if unsafe { PeekMessageW(&mut message, null_mut(), 0, 0, PM_NOREMOVE) } != 0 {
            self.signal.notify();
            self.ready = false;
            return Err(WatchError::Backlog);
        }
        Ok(())
    }

    pub(in crate::context) fn refresh(&mut self) -> Result<(), WatchError> {
        self.check()?;
        let observed = (|| {
            let current = environment()?;
            let (_, target, focus) = super::observe()?;
            if focus != FocusStatus::Available || target.desktop != current.desktop {
                return Err(WatchError::NotReady);
            }
            if environment()? != current {
                return Err(WatchError::NotReady);
            }
            Ok((current, target))
        })();
        let (current, target) = match observed {
            Ok(observed) => observed,
            Err(error) => {
                self.signal.notify();
                self.ready = false;
                self.observed = None;
                self.cached = None;
                return Err(error);
            }
        };
        if self.observed.as_ref() != Some(&current)
            || self.cached.as_ref().map(bound_identity) != Some(bound_identity(&target))
        {
            // Invalidate BEFORE replacing the binding. A delayed gesture mark
            // must never adopt a different application/window/focused child.
            self.signal.notify();
        }
        self.observed = Some(current);
        self.cached = Some(target);
        self.ready = true;
        Ok(())
    }

    pub(in crate::context) fn accepts(&self, target: &super::Target) -> Result<bool, WatchError> {
        self.check()?;
        if !self.ready {
            return Err(WatchError::NotReady);
        }
        if self.cached.as_ref().map(bound_identity) != Some(bound_identity(target)) {
            self.signal.notify();
            return Ok(false);
        }
        let current = environment().inspect_err(|_| self.signal.notify())?;
        if self.observed.as_ref() != Some(&current) || target.desktop != current.desktop {
            self.signal.notify();
            return Err(WatchError::NotReady);
        }
        let matched = target
            .matches_current()
            .inspect_err(|_| self.signal.notify())?;
        let after = environment().inspect_err(|_| self.signal.notify())?;
        if after != current {
            self.signal.notify();
            return Err(WatchError::NotReady);
        }
        if !matched {
            self.signal.notify();
        }
        Ok(matched)
    }

    pub(in crate::context) fn shutdown(&mut self) -> Result<(), WatchError> {
        self.shutdown_with(&mut NativeCleanup)
    }

    fn shutdown_with(&mut self, ops: &mut impl CleanupOps) -> Result<(), WatchError> {
        if self.closed {
            return if self.cleanup_failed {
                Err(WatchError::CleanupFailed)
            } else {
                Ok(())
            };
        }
        self.signal.close();
        self.ready = false;
        self.cached = None;
        self.observed = None;
        if ops.current_thread() != self.thread {
            // !Send forbids this in safe Rust. If violated by unsafe callers,
            // keep the callback allocation alive and its original slot occupied.
            std::mem::forget(self.signal.clone());
            self.closed = true;
            self.cleanup_failed = true;
            return Err(WatchError::WrongThread);
        }
        // Null the callback pointer FIRST, including on partial initialization.
        CALLBACKS.with(|cell| {
            cell.set(cell.get().detached());
        });
        let mut failed = false;
        for hook in &mut self.hooks {
            if !hook.is_null() {
                if !ops.unhook(*hook) {
                    failed = true;
                }
                *hook = null_mut();
            }
        }
        if self.session_registered {
            if !ops.unregister_session(self.window) {
                failed = true;
            }
            self.session_registered = false;
        }
        if !self.window.is_null() {
            if !ops.destroy_window(self.window) {
                failed = true;
            }
            self.window = null_mut();
        }
        if self.class_registered {
            if !ops.unregister_class(&self.class, self.instance) {
                failed = true;
            }
            self.class_registered = false;
        }
        if self.reserved {
            CALLBACKS.with(|cell| {
                cell.set(cell.get().released(failed));
            });
            self.reserved = false;
        }
        self.closed = true;
        self.cleanup_failed = failed;
        if failed {
            Err(WatchError::CleanupFailed)
        } else {
            Ok(())
        }
    }
}
impl Drop for Watch {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::ManuallyDrop;

    const FAKE_THREAD: u32 = 17;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum CleanupStep {
        Hook(usize),
        Session,
        Window,
        Class,
    }

    struct FakeCleanup {
        signal: Arc<Signal>,
        fail: Option<CleanupStep>,
        calls: Vec<CleanupStep>,
        thread_reads: usize,
    }
    impl FakeCleanup {
        fn record(&mut self, step: CleanupStep) -> bool {
            assert!(self.signal.closed_for_test());
            CALLBACKS.with(|cell| {
                let state = cell.get();
                assert!(state.signal.is_null());
                assert!(state.hooks.iter().all(|hook| hook.is_null()));
                assert!(state.window.is_null());
                assert!(state.occupied);
            });
            self.calls.push(step);
            self.fail != Some(step)
        }
    }
    impl CleanupOps for FakeCleanup {
        fn current_thread(&mut self) -> u32 {
            assert!(self.signal.closed_for_test());
            self.thread_reads += 1;
            FAKE_THREAD
        }
        fn unhook(&mut self, hook: HWINEVENTHOOK) -> bool {
            self.record(CleanupStep::Hook(hook.addr()))
        }
        fn unregister_session(&mut self, window: HWND) -> bool {
            assert_eq!(window, fake_handle(42));
            self.record(CleanupStep::Session)
        }
        fn destroy_window(&mut self, window: HWND) -> bool {
            assert_eq!(window, fake_handle(42));
            self.record(CleanupStep::Window)
        }
        fn unregister_class(&mut self, class: &[u16], instance: HINSTANCE) -> bool {
            assert_eq!(class.last(), Some(&0));
            assert_eq!(instance, fake_handle(43));
            self.record(CleanupStep::Class)
        }
    }

    struct RestoreCallbacks(Callbacks);
    impl Drop for RestoreCallbacks {
        fn drop(&mut self) {
            CALLBACKS.with(|cell| cell.set(self.0));
        }
    }

    // Test-only opaque values are never dereferenced, passed to native APIs or
    // used as application identities/authentication. ManuallyDrop also prevents
    // accidental NativeCleanup if an assertion panics before fake shutdown.
    fn fake_handle(value: usize) -> *mut c_void {
        std::ptr::without_provenance_mut(value)
    }

    fn fake_watch(
        hooks: usize,
        window: bool,
        session: bool,
        class: bool,
        fail: Option<CleanupStep>,
    ) -> (ManuallyDrop<Watch>, RestoreCallbacks, FakeCleanup) {
        let restore = RestoreCallbacks(CALLBACKS.with(|cell| cell.replace(Callbacks::EMPTY)));
        let signal = Arc::new(Signal::new_for_test());
        reserve_callbacks(&signal).unwrap();
        let watch = ManuallyDrop::new(Watch {
            signal: signal.clone(),
            thread: FAKE_THREAD,
            hooks: std::array::from_fn(|index| {
                if index < hooks {
                    fake_handle(index + 1)
                } else {
                    null_mut()
                }
            }),
            window: if window { fake_handle(42) } else { null_mut() },
            instance: fake_handle(43),
            class: "test-only".encode_utf16().chain(Some(0)).collect(),
            class_registered: class,
            session_registered: session,
            reserved: true,
            closed: false,
            cleanup_failed: false,
            ready: true,
            observed: None,
            cached: None,
            _thread: PhantomData,
        });
        watch.publish_callbacks();
        let ops = FakeCleanup {
            signal,
            fail,
            calls: Vec::new(),
            thread_reads: 0,
        };
        (watch, restore, ops)
    }

    fn expected_cleanup(
        hooks: usize,
        window: bool,
        session: bool,
        class: bool,
    ) -> Vec<CleanupStep> {
        let mut steps: Vec<_> = (1..=hooks).map(CleanupStep::Hook).collect();
        if session {
            steps.push(CleanupStep::Session);
        }
        if window {
            steps.push(CleanupStep::Window);
        }
        if class {
            steps.push(CleanupStep::Class);
        }
        steps
    }

    #[test]
    fn shutdown_cleans_every_partial_install_once_without_native_apis() {
        for (hooks, window, session, class) in [
            (0, false, false, false),
            (0, false, false, true),
            (0, true, false, true),
            (0, true, true, true),
            (1, true, true, true),
            (2, true, true, true),
            (3, true, true, true),
            (4, true, true, true),
        ] {
            let (mut watch, _restore, mut ops) = fake_watch(hooks, window, session, class, None);
            assert_eq!(
                reserve_callbacks(&Arc::new(Signal::new_for_test())),
                Err(WatchError::Busy)
            );
            assert_eq!(watch.shutdown_with(&mut ops), Ok(()));
            assert_eq!(ops.calls, expected_cleanup(hooks, window, session, class));
            assert!(ops.signal.closed_for_test());
            assert!(watch.closed && !watch.cleanup_failed && !watch.ready);
            assert!(watch.hooks.iter().all(|hook| hook.is_null()));
            assert!(watch.window.is_null());
            assert!(!watch.session_registered && !watch.class_registered && !watch.reserved);
            assert_eq!(watch.shutdown_with(&mut ops), Ok(()));
            assert_eq!(ops.thread_reads, 1);
            assert_eq!(ops.calls, expected_cleanup(hooks, window, session, class));
            let next_signal = Arc::new(Signal::new_for_test());
            assert_eq!(reserve_callbacks(&next_signal), Ok(()));
            CALLBACKS.with(|cell| cell.set(Callbacks::EMPTY));
            // Closed Drop returns before even asking NativeCleanup for a thread.
            drop(ManuallyDrop::into_inner(watch));
        }
    }

    #[test]
    fn shutdown_attempts_all_resources_and_poison_blocks_reopen_on_each_failure() {
        let expected = expected_cleanup(4, true, true, true);
        for failure in expected.iter().copied() {
            let (mut watch, _restore, mut ops) = fake_watch(4, true, true, true, Some(failure));
            assert_eq!(
                watch.shutdown_with(&mut ops),
                Err(WatchError::CleanupFailed)
            );
            assert_eq!(ops.calls, expected);
            assert!(ops.signal.closed_for_test());
            assert!(watch.closed && watch.cleanup_failed && !watch.ready);
            CALLBACKS.with(|cell| {
                let state = cell.get();
                assert!(state.poisoned && !state.occupied && state.signal.is_null());
            });
            assert_eq!(
                reserve_callbacks(&Arc::new(Signal::new_for_test())),
                Err(WatchError::NativeUnavailable)
            );
            assert_eq!(
                watch.shutdown_with(&mut ops),
                Err(WatchError::CleanupFailed)
            );
            assert_eq!(ops.thread_reads, 1);
            assert_eq!(ops.calls, expected);
            drop(ManuallyDrop::into_inner(watch));
        }
    }

    #[test]
    fn cached_binding_detects_window_focus_owner_and_instance_changes() {
        let baseline = BoundIdentity {
            desktop: "Default",
            window: super::super::WindowSnapshot {
                dpi:96,
                foreground: null_mut(),
                focused: null_mut(),
                process_id: 7,
                thread_id: 9,
            },
            created: 11,
        };
        let absent: Option<BoundIdentity<'_>> = None;
        assert!(absent != Some(baseline));
        let unchanged = BoundIdentity { ..baseline };
        assert!(Some(baseline) == Some(unchanged));
        let mut changed = baseline;
        changed.desktop = "AnotherDesktop";
        assert!(changed != baseline);
        changed = baseline;
        changed.window.foreground = std::ptr::dangling_mut::<c_void>();
        assert!(changed != baseline);
        changed = baseline;
        changed.window.focused = std::ptr::dangling_mut::<c_void>();
        assert!(changed != baseline);
        changed = baseline;
        changed.window.process_id += 1;
        assert!(changed != baseline);
        changed = baseline;
        changed.window.thread_id += 1;
        assert!(changed != baseline);
        changed=baseline;changed.window.dpi=144;
        assert!(changed!=baseline);
        changed = baseline;
        changed.created += 1;
        assert!(changed != baseline);
    }

    #[test]
    fn cleanup_detaches_callbacks_before_releasing_or_poisoning_the_slot() {
        let state = Callbacks {
            signal: std::ptr::dangling::<Signal>(),
            hooks: [std::ptr::dangling_mut::<c_void>(); EVENTS.len()],
            window: std::ptr::dangling_mut::<c_void>(),
            occupied: true,
            poisoned: false,
        };
        let detached = state.detached();
        assert!(detached.signal.is_null());
        assert!(detached.window.is_null());
        assert!(detached.hooks.iter().all(|hook| hook.is_null()));
        assert!(detached.occupied);
        let clean = detached.released(false);
        assert_eq!(reservation(clean.occupied, clean.poisoned), Ok(()));
        let failed = detached.released(true);
        assert_eq!(
            reservation(failed.occupied, failed.poisoned),
            Err(WatchError::NativeUnavailable)
        );
        assert!(failed.released(false).poisoned);
    }

    #[test]
    fn reservations_reject_overlap_and_failed_cleanup_never_reopens() {
        assert_eq!(reservation(false, false), Ok(()));
        assert_eq!(reservation(true, false), Err(WatchError::Busy));
        assert_eq!(reservation(false, true), Err(WatchError::NativeUnavailable));
        assert_eq!(reservation(true, true), Err(WatchError::NativeUnavailable));
    }

    #[test]
    fn only_confirmed_active_wts_state_is_ready() {
        assert_eq!(active_state(&WTSActive.to_ne_bytes()), Ok(()));
        assert_eq!(active_state(&4i32.to_ne_bytes()), Err(WatchError::NotReady));
        assert_eq!(active_state(&[]), Err(WatchError::NotReady));
        assert_eq!(active_state(&[0, 0, 0]), Err(WatchError::NotReady));
        assert_eq!(active_state(&[0; 8]), Err(WatchError::NotReady));
    }

    #[test]
    fn notifications_cover_session_power_and_environment_without_input_events() {
        for message in [
            WM_WTSSESSION_CHANGE,
            WM_POWERBROADCAST,
            WM_DISPLAYCHANGE,
            WM_SETTINGCHANGE,
            WM_ENDSESSION,
            WM_QUERYENDSESSION,
            WM_DESTROY,
        ] {
            assert!(invalidating_message(message));
        }
        for message in [WM_PAINT, WM_NULL, WM_KEYDOWN, WM_MOUSEMOVE] {
            assert!(!invalidating_message(message));
        }
        assert_eq!(EVENTS.len(), 4);
        assert!(EVENTS.contains(&EVENT_SYSTEM_FOREGROUND));
        assert!(EVENTS.contains(&EVENT_OBJECT_FOCUS));
        assert!(EVENTS.contains(&EVENT_OBJECT_DESTROY));
        assert!(EVENTS.contains(&EVENT_SYSTEM_DESKTOPSWITCH));
    }
}
