//! CoreGraphics event tap and tagged event injection. No permission bypasses.
use super::*;
use anyhow::ensure;
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
#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn CGMainDisplayID() -> u32;
    fn CGDisplayBounds(display: u32) -> Rect;
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
    fn CGEventCreateScrollWheelEvent(source: Ref, unit: u32, count: u32, ...) -> Ref;
    fn CGEventCreate(source: Ref) -> Ref;
    fn CGEventPost(tap: u32, event: Ref);
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
thread_local! {static STATE:RefCell<Option<HookState>>=const {RefCell::new(None)};}
static MODIFIERS: Mutex<u64> = Mutex::new(0);
static BUTTONS: AtomicU64 = AtomicU64::new(0);
pub(super) fn capture_receiving(
    tx: mpsc::Sender<Captured>,
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
                keys: initial_keys(),
                buttons: initial_buttons(),
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
                    Captured::Position {
                        x: p.x - b.origin.x,
                        y: p.y - b.origin.y,
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
                return s.scroll_routed(
                    CGEventGetIntegerValueField(event, 12).clamp(-1200, 1200) as i16,
                    CGEventGetIntegerValueField(event, 11).clamp(-1200, 1200) as i16,
                    route,
                );
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
    unsafe {
        let b = CGDisplayBounds(CGMainDisplayID());
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
        let event = OwnedEvent::created(CGEventCreateMouseEvent(
            null_mut(),
            kind,
            Point {
                x: b.origin.x + x * (b.size.width - 1.0),
                y: b.origin.y + y * (b.size.height - 1.0),
            },
            button,
        ))?;
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
                let event = OwnedEvent::created(CGEventCreateScrollWheelEvent(
                    null_mut(),
                    1,
                    2,
                    i32::from(y),
                    i32::from(x),
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
            let (tx, mut rx) = mpsc::channel(8);
            let mut state = HookState {
                pairs: PairLedger::default(),
                stopping: false,
                toggle_route: None,
                tx,
                gate: CaptureGate::default(),
                keys: Default::default(),
                buttons: sampled_buttons(1u64 << number),
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
