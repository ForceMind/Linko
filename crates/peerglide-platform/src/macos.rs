//! CoreGraphics event tap and tagged event injection. No permission bypasses.
use super::*;
use crate::display_layout::{Bounds, Display, DisplayId, Layout, Rotation};
use anyhow::{ensure, Context};
use std::{cell::RefCell, ffi::c_void, ptr::null_mut, sync::Mutex};
type Ref = *mut c_void;
#[repr(C)]
#[derive(Clone, Copy)]
struct Point {
    x: f64,
    y: f64,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct Size {
    width: f64,
    height: f64,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct Rect {
    origin: Point,
    size: Size,
}
const TAG: i64 = 0x50474c44;
const WHEEL_AXIS1: u32 = 11;
const WHEEL_AXIS2: u32 = 12;
const WHEEL_CONTINUOUS: u32 = 88;
const WHEEL_FIXED_AXIS1: u32 = 93;
const WHEEL_FIXED_AXIS2: u32 = 94;
const ONE_LINE: i64 = 1 << 16;
#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn CGMainDisplayID() -> u32;
    fn CGDisplayBounds(display: u32) -> Rect;
    fn CGDisplayIsActive(display: u32) -> i32;
    fn CGGetActiveDisplayList(max_displays: u32, displays: *mut u32, count: *mut u32) -> i32;
    fn CGDisplayRotation(display: u32) -> f64;
    fn CGDisplayCopyDisplayMode(display: u32) -> Ref;
    fn CGDisplayModeGetWidth(mode: Ref) -> usize;
    fn CGDisplayModeGetPixelWidth(mode: Ref) -> usize;
    fn CGEventTapCreate(
        location: u32,
        place: u32,
        options: u32,
        mask: u64,
        callback: unsafe extern "C" fn(Ref, u32, Ref, Ref) -> Ref,
        user: Ref,
    ) -> Ref;
    fn CGEventTapEnable(tap: Ref, enable: bool);
    fn CGEventGetLocation(event: Ref) -> Point;
    fn CGEventGetIntegerValueField(event: Ref, field: u32) -> i64;
    fn CGEventGetDoubleValueField(event: Ref, field: u32) -> f64;
    fn CGEventSetIntegerValueField(event: Ref, field: u32, value: i64);
    fn CGEventSetFlags(event: Ref, flags: u64);
    fn CGEventSourceKeyState(state: i32, key: u16) -> bool;
    fn CGEventSourceButtonState(state: i32, button: u32) -> bool;
    fn CGEventCreateKeyboardEvent(source: Ref, key: u16, down: bool) -> Ref;
    fn CGEventCreateMouseEvent(source: Ref, kind: u32, point: Point, button: u32) -> Ref;
    // Fixed-arity variant. The variadic CGEventCreateScrollWheelEvent names wheel1, and
    // on Apple arm64 a named argument travels in a register but a variadic one on the
    // stack, so declaring wheel1 as variadic delivered a garbage vertical delta.
    fn CGEventCreateScrollWheelEvent2(
        source: Ref,
        unit: u32,
        count: u32,
        wheel1: i32,
        wheel2: i32,
        wheel3: i32,
    ) -> Ref;
    fn CGEventCreate(source: Ref) -> Ref;
    fn CGEventPost(tap: u32, event: Ref);
    // boolean_t is a 32-bit integer on every supported macOS target; CGError is int32_t.
    fn CGAssociateMouseAndMouseCursorPosition(connected: i32) -> i32;
}
extern "C" {
    fn atexit(callback: extern "C" fn()) -> i32;
}
#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFRunLoopCommonModes: Ref;
    fn CFMachPortCreateRunLoopSource(allocator: Ref, port: Ref, order: isize) -> Ref;
    fn CFRunLoopGetCurrent() -> Ref;
    fn CFRunLoopAddSource(loop_: Ref, source: Ref, mode: Ref);
    fn CFRunLoopRemoveSource(loop_: Ref, source: Ref, mode: Ref);
    fn CFRunLoopSourceCreate(allocator: Ref, order: isize, context: *mut SourceContext) -> Ref;
    fn CFRunLoopSourceSignal(source: Ref);
    fn CFRunLoopWakeUp(loop_: Ref);
    fn CFRetain(value: *const c_void) -> Ref;
    fn CFRunLoopRunInMode(mode: Ref, seconds: f64, return_after_source: bool) -> i32;
    static kCFRunLoopDefaultMode: Ref;
    fn CFRelease(value: *const c_void);
}
const MAX_ACTIVE_DISPLAYS: usize = 64;

fn active_display_ids() -> Result<Vec<u32>> {
    // One extra element detects a truncated list instead of accepting a partial desktop.
    let mut ids = [0; MAX_ACTIVE_DISPLAYS + 1];
    let mut count = 0;
    // SAFETY: ids has room for the requested capacity and count is writable.
    let result = unsafe { CGGetActiveDisplayList(ids.len() as u32, ids.as_mut_ptr(), &mut count) };
    ensure!(result == 0, "macOS display enumeration failed ({result})");
    let count = count as usize;
    ensure!(
        (1..=MAX_ACTIVE_DISPLAYS).contains(&count),
        "macOS display count outside the snapshot limit"
    );
    Ok(ids[..count].to_vec())
}

fn display_metrics(
    id: u32,
    rect: Rect,
    angle: f64,
    width: usize,
    pixel_width: usize,
) -> Result<Display> {
    let rotation = match angle.rem_euclid(360.0) {
        0.0 => Rotation::Upright,
        90.0 => Rotation::Clockwise90,
        180.0 => Rotation::Clockwise180,
        270.0 => Rotation::Clockwise270,
        _ => anyhow::bail!("unsupported macOS display rotation"),
    };
    ensure!(width > 0 && pixel_width > 0, "invalid macOS display mode");
    Ok(Display {
        id: DisplayId(u64::from(id)),
        bounds: Bounds {
            left: rect.origin.x,
            top: rect.origin.y,
            right: rect.origin.x + rect.size.width,
            bottom: rect.origin.y + rect.size.height,
        },
        scale: pixel_width as f64 / width as f64,
        rotation,
    })
}

fn active_display(id: u32) -> Result<Display> {
    // SAFETY: the display ID came from CoreGraphics. Copy returns either a
    // retained mode or null. Release the retained mode after querying it.
    let (rect, angle, width, pixel_width) = unsafe {
        let mode = CGDisplayCopyDisplayMode(id);
        ensure!(
            !mode.is_null(),
            "macOS display mode changed during enumeration"
        );
        let metrics = (
            CGDisplayBounds(id),
            CGDisplayRotation(id),
            CGDisplayModeGetWidth(mode),
            CGDisplayModeGetPixelWidth(mode),
        );
        CFRelease(mode);
        metrics
    };
    display_metrics(id, rect, angle, width, pixel_width)
}

pub(super) fn read_display_layout() -> Result<Layout> {
    let ids = active_display_ids()?;
    let primary = DisplayId(u64::from(*ids.first().context("no active macOS display")?));
    let displays = ids
        .iter()
        .map(|id| active_display(*id))
        .collect::<Result<Vec<_>>>()?;
    ensure!(
        ids == active_display_ids()?,
        "macOS display set changed during enumeration"
    );
    Layout::new(primary, displays)
        .map_err(|error| anyhow::anyhow!("invalid macOS layout: {error:?}"))
}

// ABI from Apple's CFRunLoop.h, version-0 source. The perform callback has
// no input/state work; it just makes RunInMode(returnAfterSource=true) return.
#[repr(C)]
struct SourceContext {
    version: isize,
    info: Ref,
    retain: Option<unsafe extern "C" fn(*const c_void) -> *const c_void>,
    release: Option<unsafe extern "C" fn(*const c_void)>,
    description: Option<unsafe extern "C" fn(*const c_void) -> Ref>,
    equal: Option<unsafe extern "C" fn(*const c_void, *const c_void) -> bool>,
    hash: Option<unsafe extern "C" fn(*const c_void) -> usize>,
    schedule: Option<unsafe extern "C" fn(Ref, Ref, Ref)>,
    cancel: Option<unsafe extern "C" fn(Ref, Ref, Ref)>,
    perform: Option<unsafe extern "C" fn(Ref)>,
}
unsafe extern "C" fn wake_perform(_: Ref) {}
#[derive(Default)]
pub(super) struct ReceiveWake {
    refs: Mutex<Option<(usize, usize)>>,
}
impl ReceiveWake {
    pub(super) fn wake(&self) {
        if let Ok(refs) = self.refs.lock() {
            if let Some((loop_, source)) = *refs {
                // Registration owns retained refs while this mutex is held.
                // Signal+WakeUp are CoreFoundation's cross-thread wake path.
                unsafe {
                    CFRunLoopSourceSignal(source as Ref);
                    CFRunLoopWakeUp(loop_ as Ref);
                }
            }
        }
    }
    unsafe fn register(self: &Arc<Self>) -> Option<WakeRegistration> {
        let mut context = SourceContext {
            version: 0,
            info: null_mut(),
            retain: None,
            release: None,
            description: None,
            equal: None,
            hash: None,
            schedule: None,
            cancel: None,
            perform: Some(wake_perform),
        };
        let source = CFRunLoopSourceCreate(null_mut(), 0, &mut context);
        if source.is_null() {
            return None;
        }
        let loop_ = CFRetain(CFRunLoopGetCurrent());
        CFRunLoopAddSource(loop_, source, kCFRunLoopCommonModes);
        match self.refs.lock() {
            Ok(mut refs) => {
                *refs = Some((loop_ as usize, source as usize));
            }
            Err(_) => {
                CFRunLoopRemoveSource(loop_, source, kCFRunLoopCommonModes);
                CFRelease(source);
                CFRelease(loop_);
                return None;
            }
        }
        Some(WakeRegistration {
            wake: self.clone(),
            loop_,
            source,
        })
    }
}
struct WakeRegistration {
    wake: Arc<ReceiveWake>,
    loop_: Ref,
    source: Ref,
}
impl Drop for WakeRegistration {
    fn drop(&mut self) {
        // First detach cross-thread users, then release on the owning thread.
        *self
            .wake
            .refs
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = None;
        unsafe {
            CFRunLoopRemoveSource(self.loop_, self.source, kCFRunLoopCommonModes);
            CFRelease(self.source);
            CFRelease(self.loop_);
        }
    }
}
pub(super) fn suppressed_modifiers_held() -> bool {
    STATE.with_borrow(|state| state.as_ref().is_none_or(HookState::suppressed_modifiers))
}
struct NativeCursor;
impl source_cursor::CursorBackend for NativeCursor {
    fn associate(&mut self, connected: bool) -> bool {
        // Synchronous WindowServer request: never called from the tap callback.
        unsafe { CGAssociateMouseAndMouseCursorPosition(i32::from(connected)) == 0 }
    }
}
static SOURCE_CURSOR: Mutex<source_cursor::FreezeState> =
    Mutex::new(source_cursor::FreezeState::new());
extern "C" fn release_source_cursor_at_exit() {
    match SOURCE_CURSOR.try_lock() {
        Ok(mut state) => {
            state.release(&mut NativeCursor);
        }
        Err(std::sync::TryLockError::Poisoned(error)) => {
            error.into_inner().release(&mut NativeCursor);
        }
        // Another thread is mid-request; reconnecting is the only safe exit state.
        Err(std::sync::TryLockError::WouldBlock) => {
            source_cursor::CursorBackend::associate(&mut NativeCursor, true);
        }
    }
}
/// Session-side handoff path, outside callbacks. `current` is evaluated under
/// the same lock as every release, so a stale phase can never disconnect late.
pub(super) fn freeze_source_cursor(current: impl FnOnce() -> bool) {
    static EXIT_HOOK: std::sync::Once = std::sync::Once::new();
    EXIT_HOOK.call_once(|| unsafe {
        let _ = atexit(release_source_cursor_at_exit);
    });
    let mut state = SOURCE_CURSOR
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    state.freeze(current(), &mut NativeCursor);
}
pub(super) fn release_source_cursor() -> Result<()> {
    let mut state = SOURCE_CURSOR
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    ensure!(
        state.release(&mut NativeCursor),
        "source cursor association could not be restored"
    );
    Ok(())
}
/// Capture loop only, between run-loop slices. Never waits for the session.
/// `remote` is read under the same lock as freeze, so a phase committed and frozen
/// between an early read and this lock can never be reconnected by a stale answer.
fn reconcile_source_cursor(remote: impl FnOnce() -> bool) {
    let mut state = match SOURCE_CURSOR.try_lock() {
        Ok(state) => state,
        Err(std::sync::TryLockError::Poisoned(error)) => error.into_inner(),
        Err(std::sync::TryLockError::WouldBlock) => return,
    };
    state.reconcile(remote(), &mut NativeCursor);
}
struct FractionalWheel {
    route: Option<CaptureRoute>,
    remainder: [i64; 2],
}

impl FractionalWheel {
    const fn new() -> Self {
        Self {
            route: None,
            remainder: [0; 2],
        }
    }

    fn steps(
        &mut self,
        route: CaptureRoute,
        continuous: bool,
        integer: [i16; 2],
        fixed: [i64; 2],
    ) -> [i16; 2] {
        if !continuous || !route.remote() {
            self.route = None;
            self.remainder = [0; 2];
            return integer;
        }
        if self.route != Some(route) {
            self.route = Some(route);
            self.remainder = [0; 2];
        }
        std::array::from_fn(|axis| {
            let remainder = &mut self.remainder[axis];
            // Keep native whole-line events and ignore ambiguous fixed-point
            // values that are not fractional when the whole-line field is zero.
            if integer[axis] != 0 || fixed[axis].unsigned_abs() >= ONE_LINE as u64 {
                *remainder = 0;
                return integer[axis];
            }
            let total = *remainder + fixed[axis];
            *remainder = total % ONE_LINE;
            (total / ONE_LINE) as i16
        })
    }
}

thread_local! {
    static WHEEL: RefCell<FractionalWheel> = const { RefCell::new(FractionalWheel::new()) };
}
thread_local! {static STATE:RefCell<Option<HookState>>=const {RefCell::new(None)};}
static MODIFIERS: Mutex<u64> = Mutex::new(0);
static BUTTONS: AtomicU64 = AtomicU64::new(0);
pub(super) fn capture_receiving(
    tx: CaptureEventSender,
    gate: CaptureGate,
    target: InputCapabilities,
    mut engine: receive_driver::ReceiveEngine,
    wake: Arc<ReceiveWake>,
) -> Result<CaptureThread> {
    ensure!(unsafe {AXIsProcessTrusted()},"grant Accessibility and Input Monitoring permissions in System Settings, then restart PeerGlide");
    let reservation = capture_thread::Reservation::acquire()?;
    let (ready, rx) = std::sync::mpsc::sync_channel(1);
    let thread = CaptureThread::spawn_reserved(
        move |stop| unsafe {
            let receive_gate = gate.clone();
            let mut source_health = SourceHealth::new();
            let mut backend = NativeReceiveBackend {
                gate: gate.clone(),
                observation: source_health.observation,
            };
            let state = HookState {
                pairs: PairLedger::default(),
                stopping: false,
                toggle_route: None,
                tx,
                gate,
                keys: initial_keys().into_iter().collect(),
                buttons: initial_buttons().into_iter().collect(),
                toggle_armed: false,
                target,
            };
            state.update_held();
            STATE.set(Some(state));
            let mask = [1, 2, 3, 4, 5, 6, 7, 10, 11, 12, 22, 25, 26, 27]
                .iter()
                .fold(0, |a, n| a | 1u64 << n);
            let tap = CGEventTapCreate(0, 0, 0, mask, callback, null_mut());
            if tap.is_null() {
                let _ = ready.send(false);
                return;
            }
            let source = CFMachPortCreateRunLoopSource(null_mut(), tap, 0);
            if source.is_null() {
                CFRelease(tap);
                let _ = ready.send(false);
                return;
            }
            CFRunLoopAddSource(CFRunLoopGetCurrent(), source, kCFRunLoopCommonModes);
            let Some(_wake_registration) = wake.register() else {
                CFRunLoopRemoveSource(CFRunLoopGetCurrent(), source, kCFRunLoopCommonModes);
                CFRelease(source);
                CFRelease(tap);
                let _ = ready.send(false);
                return;
            };
            CGEventTapEnable(tap, true);
            let _ = ready.send(true);
            let mut last_turn = std::time::Instant::now();
            loop {
                super::note_loop_gap(&receive_gate, &mut last_turn);
                // Fail-open path independent of the async session: a watchdog,
                // Emergency, fault or stop that leaves Remote reconnects here.
                reconcile_source_cursor(|| receive_gate.remote_live().is_some());
                let debt = STATE
                    .with_borrow(|state| state.as_ref().is_some_and(|s| s.pairs.debt != 0))
                    || receive_gate.receiving_epoch().is_some();
                source_health.sample(&mut backend, debt);
                // SDK posts can reenter callback; never hold STATE across pump.
                let immediate = engine.pump(&mut backend, now_ms(), stop.load(Ordering::Acquire));
                let stopping = stop.load(Ordering::Acquire) || engine.control().stop_requested();
                let keep = STATE.with_borrow_mut(|state| {
                    state.as_mut().is_some_and(|s| s.keep_running(stopping))
                });
                if !keep && engine.can_exit() {
                    break;
                }
                if !receive_gate.0.healthy.load(Ordering::Acquire) {
                    break;
                }
                let result = CFRunLoopRunInMode(
                    kCFRunLoopDefaultMode,
                    if immediate { 0.0 } else { 0.05 },
                    true,
                );
                STATE.with_borrow_mut(|state| {
                    if let Some(state) = state {
                        state.tx.notify_after_callbacks();
                    }
                });
                if result == 1 || result == 2 {
                    receive_gate.source_lost();
                    break;
                }
            }
            engine.pump(&mut backend, now_ms(), true);
            let _ = engine.outcome();
            CGEventTapEnable(tap, false);
            STATE.with_borrow(|s| {
                if let Some(s) = s {
                    s.gate.disable();
                }
            });
            // The tap is disabled: waiting for an in-flight session request is safe.
            let _ = release_source_cursor();
            CFRunLoopRemoveSource(CFRunLoopGetCurrent(), source, kCFRunLoopCommonModes);
            CFRelease(source);
            CFRelease(tap);
        },
        reservation,
    );
    ensure!(
        rx.recv_timeout(std::time::Duration::from_secs(3))?,
        "CoreGraphics event tap unavailable; check Input Monitoring permission"
    );
    Ok(thread)
}
unsafe extern "C" fn callback(_: Ref, kind: u32, event: Ref, _: Ref) -> Ref {
    if kind >= 0xffff_fffe {
        STATE.with_borrow_mut(|s| {
            if let Some(s) = s {
                if s.stopping && s.pairs.debt == 0 {
                    s.gate.disable();
                } else {
                    s.source_lost();
                }
                s.emit(Captured::Emergency);
            }
        });
        return event;
    }
    if CGEventGetIntegerValueField(event, 42) == TAG {
        if matches!(kind, 5..=7 | 22 | 27) {
            STATE.with(|state| {
                if let Ok(state) = state.try_borrow() {
                    if let Some(diagnostics) =
                        state.as_ref().and_then(|s| s.gate.0.diagnostics.get())
                    {
                        // Our own tagged scroll comes back through the tap as well;
                        // an untagged one would be treated as local physical input.
                        diagnostics.increment(if kind == 22 {
                            input_diagnostics::Count::WheelInjected
                        } else {
                            input_diagnostics::Count::Filtered
                        });
                    }
                }
            });
        }
        return event;
    }
    let block = STATE.with_borrow_mut(|s| {
        let Some(s) = s else {
            return false;
        };
        let callback = s.gate.begin_event();
        let route = callback.route;
        match kind {
            10..=12 => {
                let code = CGEventGetIntegerValueField(event, 9) as u16;
                let usage = from_code(code);
                let down = if kind == 12 {
                    // Own-tagged posts were filtered above. Modifier transition
                    // events do not repeat; combined key state may still contain
                    // our remote synthetic modifier and is not a physical Up.
                    match usage {
                        Some(usage) if (0xe0..=0xe7).contains(&usage) => {
                            !s.physical_key_held(usage)
                        }
                        _ => CGEventSourceKeyState(0, code),
                    }
                } else {
                    kind == 10
                };
                return match usage {
                    Some(usage) => s.key_routed(usage, down, route),
                    None => s.unsupported_key(code, down),
                };
            }
            5..=7 | 27 => {
                let captured = if route.remote() {
                    Captured::Motion {
                        dx: CGEventGetDoubleValueField(event, 4),
                        dy: CGEventGetDoubleValueField(event, 5),
                        route,
                    }
                } else {
                    let p = CGEventGetLocation(event);
                    let b = CGDisplayBounds(CGMainDisplayID());
                    let (x, y) = if s.gate.0.routed_target.get().is_some() {
                        (p.x, p.y)
                    } else {
                        (p.x - b.origin.x, p.y - b.origin.y)
                    };
                    Captured::Position {
                        x,
                        y,
                        width: b.size.width,
                        height: b.size.height,
                        route,
                    }
                };
                return s.movement(captured, route);
            }
            1..=4 | 25 | 26 => {
                if kind == 25 || kind == 26 {
                    let number = CGEventGetIntegerValueField(event, 3);
                    if number != 2 {
                        if (3..=31).contains(&number) {
                            let button = (number + 1) as u8;
                            return s.unsupported_button(button, kind == 25, route);
                        }
                        s.gate.disable();
                        s.emit(Captured::UnsupportedInput);
                        return false;
                    }
                }
                let (button, down) = match kind {
                    1 => (1, true),
                    2 => (1, false),
                    3 => (2, true),
                    4 => (2, false),
                    25 => (3, true),
                    _ => (3, false),
                };
                return s.button_routed(button, down, route);
            }
            22 => {
                if let Some(diagnostics) = s.gate.0.diagnostics.get() {
                    diagnostics.increment(input_diagnostics::Count::WheelPhysical);
                }
                let integer = [
                    CGEventGetIntegerValueField(event, WHEEL_AXIS2).clamp(-1200, 1200) as i16,
                    CGEventGetIntegerValueField(event, WHEEL_AXIS1).clamp(-1200, 1200) as i16,
                ];
                let continuous =
                    route.remote() && CGEventGetIntegerValueField(event, WHEEL_CONTINUOUS) != 0;
                let fixed = if continuous {
                    [
                        CGEventGetIntegerValueField(event, WHEEL_FIXED_AXIS2),
                        CGEventGetIntegerValueField(event, WHEEL_FIXED_AXIS1),
                    ]
                } else {
                    [0; 2]
                };
                let [x, y] =
                    WHEEL.with_borrow_mut(|wheel| wheel.steps(route, continuous, integer, fixed));
                return s.scroll_routed(x, y, route);
            }
            _ => (),
        }
        if matches!(route, CaptureRoute::Receiving(_)) {
            s.gate.request_takeover(route);
            return true;
        }
        route.remote()
    });
    if block {
        null_mut()
    } else {
        event
    }
}
struct OwnedEvent(Ref);
impl OwnedEvent {
    fn created(
        event: Ref,
    ) -> std::result::Result<Self, peerglide_actions::execution::NativeFailure> {
        if event.is_null() {
            Err(peerglide_actions::execution::NativeFailure::BeforeSubmission)
        } else {
            Ok(Self(event))
        }
    }
    unsafe fn submit(&self, flags: u64) {
        CGEventSetFlags(self.0, flags);
        CGEventSetIntegerValueField(self.0, 42, TAG);
        // CGEventPost has no result. This is an SDK request, never confirmation
        // that the OS/app accepted it or that a physical key has been released.
        CGEventPost(0, self.0);
    }
}
impl Drop for OwnedEvent {
    fn drop(&mut self) {
        unsafe {
            CFRelease(self.0);
        }
    }
}
fn next_button_state(current: u64, button: u8, down: bool) -> u64 {
    let bit = 1u64 << (button - 1);
    if down {
        current | bit
    } else {
        current & !bit
    }
}
fn commit_created<T>(
    created: Option<T>,
    commit: impl FnOnce(&T),
) -> std::result::Result<T, peerglide_actions::execution::NativeFailure> {
    let event = created.ok_or(peerglide_actions::execution::NativeFailure::BeforeSubmission)?;
    commit(&event);
    Ok(event)
}
pub fn pointer(x: f64, y: f64) -> Result<()> {
    receive_pointer(x, y)
        .map_err(|error| anyhow::anyhow!("native pointer request failed: {error:?}"))
}
pub(super) fn receive_pointer(
    x: f64,
    y: f64,
) -> std::result::Result<(), peerglide_actions::execution::NativeFailure> {
    use peerglide_actions::execution::NativeFailure;
    if !(ReceivePoint { x, y }).valid() {
        return Err(NativeFailure::BeforeSubmission);
    }
    let b = unsafe { CGDisplayBounds(CGMainDisplayID()) };
    receive_global_pointer(Point {
        x: b.origin.x + x * (b.size.width - 1.0),
        y: b.origin.y + y * (b.size.height - 1.0),
    })
}

pub(super) fn receive_on_display(
    expected: display_layout::Display,
    _desktop: display_layout::Bounds,
    placement: display_layout::Placement,
) -> std::result::Result<(), peerglide_actions::execution::NativeFailure> {
    use peerglide_actions::execution::NativeFailure;
    let id = u32::try_from(expected.id.0).map_err(|_| NativeFailure::TargetLost)?;
    // SAFETY: CoreGraphics checks the scalar ID, and the pointer coordinates
    // are only sent if the selected monitor still occupies that region.
    let (rect, rotation) = unsafe {
        if CGDisplayIsActive(id) == 0 {
            return Err(NativeFailure::TargetLost);
        }
        (CGDisplayBounds(id), CGDisplayRotation(id))
    };
    let actual = display_layout::Bounds {
        left: rect.origin.x,
        top: rect.origin.y,
        right: rect.origin.x + rect.size.width,
        bottom: rect.origin.y + rect.size.height,
    };
    let angle = match expected.rotation {
        display_layout::Rotation::Upright => 0.0,
        display_layout::Rotation::Clockwise90 => 90.0,
        display_layout::Rotation::Clockwise180 => 180.0,
        display_layout::Rotation::Clockwise270 => 270.0,
    };
    if actual != expected.bounds || rotation.rem_euclid(360.0) != angle {
        return Err(NativeFailure::TargetLost);
    }
    receive_global_pointer(Point {
        x: placement.desktop_x,
        y: placement.desktop_y,
    })
}

fn receive_global_pointer(
    point: Point,
) -> std::result::Result<(), peerglide_actions::execution::NativeFailure> {
    use peerglide_actions::execution::NativeFailure;
    unsafe {
        let buttons = BUTTONS.load(Ordering::Acquire);
        let (kind, button) = if buttons & 1 != 0 {
            (6, 0)
        } else if buttons & 2 != 0 {
            (7, 1)
        } else if buttons & 4 != 0 {
            (27, 2)
        } else {
            (5, 0)
        };
        let event = OwnedEvent::created(CGEventCreateMouseEvent(null_mut(), kind, point, button))?;
        let modifiers = MODIFIERS
            .lock()
            .map_err(|_| NativeFailure::BeforeSubmission)?;
        event.submit(modifier_flags(*modifiers));
        Ok(())
    }
}
pub fn inject(event: Input) -> Result<()> {
    receive_inject(event).map_err(|error| anyhow::anyhow!("native input request failed: {error:?}"))
}
pub(super) fn receive_inject(
    event: Input,
) -> std::result::Result<(), peerglide_actions::execution::NativeFailure> {
    use peerglide_actions::execution::NativeFailure;
    if !event.valid() {
        return Err(NativeFailure::BeforeSubmission);
    }
    unsafe {
        // The RAII event exists before any state commit, so allocation failure
        // or a poisoned modifier lock cannot pollute BUTTONS/MODIFIERS or leak.
        match event {
            Input::Key { usage, down } => {
                let code = to_code(usage).ok_or(NativeFailure::BeforeSubmission)?;
                let event =
                    OwnedEvent::created(CGEventCreateKeyboardEvent(null_mut(), code, down))?;
                let mut modifiers = MODIFIERS
                    .lock()
                    .map_err(|_| NativeFailure::BeforeSubmission)?;
                let mut next = *modifiers;
                if (0xe0..=0xe7).contains(&usage) {
                    let bit = 1u64 << (usage - 0xe0);
                    if down {
                        next |= bit;
                    } else {
                        next &= !bit;
                    }
                }
                *modifiers = next;
                event.submit(modifier_flags(next));
            }
            Input::Button { button, down } => {
                let (kind, b) = match (button, down) {
                    (1, true) => (1, 0),
                    (1, false) => (2, 0),
                    (2, true) => (3, 1),
                    (2, false) => (4, 1),
                    (3, true) => (25, 2),
                    (3, false) => (26, 2),
                    _ => return Err(NativeFailure::BeforeSubmission),
                };
                let pos = OwnedEvent::created(CGEventCreate(null_mut()))?;
                let point = CGEventGetLocation(pos.0);
                let modifiers = MODIFIERS
                    .lock()
                    .map_err(|_| NativeFailure::BeforeSubmission)?;
                let next = next_button_state(BUTTONS.load(Ordering::Acquire), button, down);
                let event =
                    OwnedEvent::created(CGEventCreateMouseEvent(null_mut(), kind, point, b)).ok();
                let event = commit_created(event, |_| BUTTONS.store(next, Ordering::Release))?;
                event.submit(modifier_flags(*modifiers));
            }
            Input::Scroll { x, y } => {
                let event = OwnedEvent::created(CGEventCreateScrollWheelEvent2(
                    null_mut(),
                    1,
                    2,
                    i32::from(y),
                    i32::from(x),
                    0,
                ))?;
                let modifiers = MODIFIERS
                    .lock()
                    .map_err(|_| NativeFailure::BeforeSubmission)?;
                event.submit(modifier_flags(*modifiers));
            }
        }
        Ok(())
    }
}
// USB HID usages to documented macOS virtual key positions; intentionally finite MVP coverage.
const KEYS: &[(u16, u16)] = &[
    (4, 0),
    (5, 11),
    (6, 8),
    (7, 2),
    (8, 14),
    (9, 3),
    (10, 5),
    (11, 4),
    (12, 34),
    (13, 38),
    (14, 40),
    (15, 37),
    (16, 46),
    (17, 45),
    (18, 31),
    (19, 35),
    (20, 12),
    (21, 15),
    (22, 1),
    (23, 17),
    (24, 32),
    (25, 9),
    (26, 13),
    (27, 7),
    (28, 16),
    (29, 6),
    (30, 18),
    (31, 19),
    (32, 20),
    (33, 21),
    (34, 23),
    (35, 22),
    (36, 26),
    (37, 28),
    (38, 25),
    (39, 29),
    (40, 36),
    (41, 53),
    (42, 51),
    (43, 48),
    (44, 49),
    (45, 27),
    (46, 24),
    (47, 33),
    (48, 30),
    (49, 42),
    (51, 41),
    (52, 39),
    (53, 50),
    (54, 43),
    (55, 47),
    (56, 44),
    (57, 57),
    (58, 122),
    (59, 120),
    (60, 99),
    (61, 118),
    (62, 96),
    (63, 97),
    (64, 98),
    (65, 100),
    (66, 101),
    (67, 109),
    (68, 103),
    (69, 111),
    (73, 114),
    (74, 115),
    (75, 116),
    (76, 117),
    (77, 119),
    (78, 121),
    (79, 124),
    (80, 123),
    (81, 125),
    (82, 126),
    (0x53, 71),
    (0x54, 75),
    (0x55, 67),
    (0x56, 78),
    (0x57, 69),
    (0x58, 76),
    (0x59, 83),
    (0x5a, 84),
    (0x5b, 85),
    (0x5c, 86),
    (0x5d, 87),
    (0x5e, 88),
    (0x5f, 89),
    (0x60, 91),
    (0x61, 92),
    (0x62, 82),
    (0x63, 65),
    (0x64, 10),
    (0x68, 105),
    (0x69, 107),
    (0x6a, 113),
    (0x6b, 106),
    (0x6c, 64),
    (0x6d, 79),
    (0x6e, 80),
    (0x6f, 90),
    (0xe0, 59),
    (0xe1, 56),
    (0xe2, 58),
    (0xe3, 55),
    (0xe4, 62),
    (0xe5, 60),
    (0xe6, 61),
    (0xe7, 54),
];
pub(super) fn key_usages() -> Vec<u16> {
    KEYS.iter().map(|k| k.0).collect()
}
fn to_code(u: u16) -> Option<u16> {
    KEYS.iter().find(|p| p.0 == u).map(|p| p.1)
}
fn from_code(c: u16) -> Option<u16> {
    KEYS.iter().find(|p| p.1 == c).map(|p| p.0)
}

fn modifier_flags(bits: u64) -> u64 {
    let mut flags = 0;
    for (mask, flag) in [
        (0x11, 1 << 18),
        (0x22, 1 << 17),
        (0x44, 1 << 19),
        (0x88, 1 << 20),
    ] {
        if bits & mask != 0 {
            flags |= flag;
        }
    }
    flags
}
unsafe fn initial_keys() -> std::collections::BTreeSet<u16> {
    (0u16..128)
        .filter(|code| CGEventSourceKeyState(0, *code))
        .map(|code| from_code(code).unwrap_or(0x8000 | code))
        .collect()
}
fn sampled_buttons(held: u64) -> std::collections::BTreeSet<u8> {
    // Match the existing callback's 0..31 native button numbers. No additional
    // remote button capability or physical-origin proof is created here.
    (0u8..32)
        .filter(|number| held & (1u64 << number) != 0)
        .map(|number| number + 1)
        .collect()
}
unsafe fn initial_buttons() -> std::collections::BTreeSet<u8> {
    // NSEvent's one-time snapshot can include synthesized state. It is used only
    // conservatively to refuse admission while held, never for event tracking or
    // physical provenance. Preserve the original three CG source observations.
    let mut held =
        objc2::rc::autoreleasepool(|_| objc2_app_kit::NSEvent::pressedMouseButtons() as u64);
    for number in 0..3 {
        if CGEventSourceButtonState(0, number) {
            held |= 1u64 << number;
        }
    }
    sampled_buttons(held)
}

#[cfg(test)]
mod tests {
    #[test]
    fn failed_button_event_creation_does_not_commit_drag_state() {
        use std::cell::Cell;
        let buttons = Cell::new(1u64);
        let next = super::next_button_state(buttons.get(), 2, true);
        let result = super::commit_created::<()>(None, |_| buttons.set(next));
        assert_eq!(
            result,
            Err(peerglide_actions::execution::NativeFailure::BeforeSubmission)
        );
        assert_eq!(buttons.get(), 1);
        super::commit_created(Some(()), |_| buttons.set(next)).unwrap();
        assert_eq!(buttons.get(), 3);
        let next = super::next_button_state(buttons.get(), 1, false);
        assert_eq!(next, 2, "releasing one button preserves another owned drag");
    }

    use super::*;
    #[test]
    fn startup_other_button_hold_blocks_receiving_until_its_local_up() {
        for number in 3u8..32 {
            let button = number + 1;
            let (tx, mut rx) = capture_events::channel(8).unwrap();
            let mut state = HookState {
                pairs: PairLedger::default(),
                stopping: false,
                toggle_route: None,
                tx,
                gate: CaptureGate::default(),
                keys: Default::default(),
                buttons: sampled_buttons(1u64 << number).into_iter().collect(),
                toggle_armed: false,
                target: InputCapabilities::new(&key_usages(), true, 7, true).unwrap(),
            };
            state.update_held();
            assert!(!state.gate.idle());
            assert!(state.gate.enter_receiving().is_none());
            let local = state.gate.route().unwrap();
            assert!(!state.unsupported_button(button, false, local));
            assert_eq!(state.pairs.debt, 0, "startup Down belonged to the local OS");
            assert!(state.gate.idle());
            assert!(state.gate.enter_receiving().is_some());
            assert!(!state.target.supports(Input::Button { button, down: true }));
            assert!(matches!(rx.try_recv().unwrap(), Captured::UnsupportedInput));
            assert!(
                rx.try_recv().is_err(),
                "other-button input was never forwarded"
            );
        }
    }
    #[test]
    fn key_positions_roundtrip() {
        for &(u, c) in KEYS {
            assert_eq!(to_code(u), Some(c));
            assert_eq!(from_code(c), Some(u));
        }
    }
    #[test]
    fn both_modifier_sides_preserve_shared_flag() {
        assert_eq!(modifier_flags(0x11), 1 << 18);
        assert_eq!(modifier_flags(0x10), 1 << 18);
        assert_eq!(modifier_flags(0), 0);
    }
}

#[cfg(test)]
mod display_layout_tests {
    use super::*;

    #[test]
    fn macos_display_metrics_keep_oriented_bounds_and_backing_scale() -> Result<()> {
        let rect = Rect {
            origin: Point { x: -700.0, y: 30.0 },
            size: Size {
                width: 700.0,
                height: 1200.0,
            },
        };
        let display = display_metrics(19, rect, 90.0, 700, 1400)?;
        assert_eq!(display.id, DisplayId(19));
        assert!((display.bounds.left + 700.0).abs() < f64::EPSILON);
        assert!(display.bounds.right.abs() < f64::EPSILON);
        assert!((display.bounds.bottom - 1230.0).abs() < f64::EPSILON);
        assert!((display.scale - 2.0).abs() < f64::EPSILON);
        assert_eq!(display.rotation, Rotation::Clockwise90);
        assert!(display_metrics(19, rect, 45.0, 700, 1400).is_err());
        assert!(display_metrics(19, rect, 0.0, 0, 1400).is_err());
        Ok(())
    }
}

#[cfg(test)]
mod fractional_wheel_tests {
    use super::*;

    fn epoch(generation: u64) -> CaptureEpoch {
        CaptureEpoch {
            instance: 1,
            generation,
        }
    }

    #[test]
    fn sub_line_scroll_accumulates_independent_axes_and_preserves_sign() {
        let mut wheel = FractionalWheel::new();
        let route = CaptureRoute::Remote(epoch(1));
        let fixed = [-ONE_LINE / 2, ONE_LINE / 4];
        assert_eq!(wheel.steps(route, true, [0; 2], fixed), [0, 0]);
        assert_eq!(wheel.steps(route, true, [0; 2], fixed), [-1, 0]);
        assert_eq!(wheel.steps(route, true, [0; 2], fixed), [0, 0]);
        assert_eq!(wheel.steps(route, true, [0; 2], fixed), [-1, 1]);
    }

    #[test]
    fn routing_discrete_input_and_ambiguous_fixed_values_reset_remainders() {
        let mut wheel = FractionalWheel::new();
        let first = CaptureRoute::Remote(epoch(1));
        let next = CaptureRoute::Remote(epoch(2));
        let local = CaptureRoute::Local(epoch(3));
        let fixed = [ONE_LINE / 2, -ONE_LINE / 2];
        assert_eq!(wheel.steps(first, true, [0; 2], fixed), [0, 0]);
        assert_eq!(wheel.steps(next, true, [0; 2], fixed), [0, 0]);
        assert_eq!(wheel.steps(local, true, [2, 3], fixed), [2, 3]);
        assert_eq!(wheel.steps(next, true, [0; 2], fixed), [0, 0]);
        assert_eq!(wheel.steps(next, false, [4, -5], fixed), [4, -5]);
        assert_eq!(wheel.steps(next, true, [0; 2], fixed), [0, 0]);
        assert_eq!(
            wheel.steps(next, true, [0; 2], [i64::MAX, i64::MIN]),
            [0, 0]
        );
        assert_eq!(wheel.steps(next, true, [0; 2], fixed), [0, 0]);
    }
}
