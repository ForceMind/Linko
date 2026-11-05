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
    fn CFRunLoopRunInMode(mode: Ref, seconds: f64, return_after_source: bool) -> i32;
    static kCFRunLoopDefaultMode: Ref;
    fn CFRelease(value: *const c_void);
}
thread_local! {static STATE:RefCell<Option<HookState>>=const {RefCell::new(None)};}
static MODIFIERS: Mutex<u64> = Mutex::new(0);
static BUTTONS: AtomicU64 = AtomicU64::new(0);
pub fn capture(
    tx: mpsc::Sender<Captured>,
    gate: CaptureGate,
    target: InputCapabilities,
) -> Result<CaptureThread> {
    ensure!(unsafe {AXIsProcessTrusted()},"grant Accessibility and Input Monitoring permissions in System Settings, then restart PeerGlide");
    let reservation = capture_thread::Reservation::acquire()?;
    let (ready, rx) = std::sync::mpsc::sync_channel(1);
    let thread = CaptureThread::spawn_reserved(
        move |stop| unsafe {
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
            CGEventTapEnable(tap, true);
            let _ = ready.send(true);
            while STATE.with_borrow_mut(|state| {
                state
                    .as_mut()
                    .is_some_and(|s| s.keep_running(stop.load(Ordering::Acquire)))
            }) {
                CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.05, false);
            }
            CGEventTapEnable(tap, false);
            STATE.with_borrow(|s| {
                if let Some(s) = s {
                    s.gate.disable();
                }
            });
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
                let down = if kind == 12 {
                    CGEventSourceKeyState(0, code)
                } else {
                    kind == 10
                };
                return match from_code(code) {
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
                return s.scroll_routed(
                    CGEventGetIntegerValueField(event, 12).clamp(-1200, 1200) as i16,
                    CGEventGetIntegerValueField(event, 11).clamp(-1200, 1200) as i16,
                    route,
                )
            }
            _ => (),
        }
        route.remote()
    });
    if block {
        null_mut()
    } else {
        event
    }
}
unsafe fn post(event: Ref) -> Result<()> {
    ensure!(!event.is_null(), "CoreGraphics rejected event creation");
    CGEventSetIntegerValueField(event, 42, TAG);
    CGEventPost(0, event);
    CFRelease(event);
    Ok(())
}
pub fn pointer(x: f64, y: f64) -> Result<()> {
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
        post(CGEventCreateMouseEvent(
            null_mut(),
            kind,
            Point {
                x: b.origin.x + x * (b.size.width - 1.0),
                y: b.origin.y + y * (b.size.height - 1.0),
            },
            button,
        ))
    }
}
pub fn inject(event: Input) -> Result<()> {
    unsafe {
        match event {
            Input::Key { usage, down } => {
                let code = to_code(usage).ok_or_else(|| anyhow::anyhow!("unsupported HID key"))?;
                let e = CGEventCreateKeyboardEvent(null_mut(), code, down);
                ensure!(!e.is_null(), "keyboard event unavailable");
                let mut modifiers = MODIFIERS
                    .lock()
                    .map_err(|_| anyhow::anyhow!("modifier lock poisoned"))?;
                if (0xe0..=0xe7).contains(&usage) {
                    let bit = 1u64 << (usage - 0xe0);
                    if down {
                        *modifiers |= bit;
                    } else {
                        *modifiers &= !bit;
                    }
                }
                CGEventSetFlags(e, modifier_flags(*modifiers));
                post(e)
            }
            Input::Button { button, down } => {
                let pos = CGEventCreate(null_mut());
                ensure!(!pos.is_null(), "pointer state unavailable");
                let point = CGEventGetLocation(pos);
                CFRelease(pos);
                let (kind, b) = match (button, down) {
                    (1, true) => (1, 0),
                    (1, false) => (2, 0),
                    (2, true) => (3, 1),
                    (2, false) => (4, 1),
                    (3, true) => (25, 2),
                    (3, false) => (26, 2),
                    _ => anyhow::bail!("unsupported button"),
                };
                let bit = 1u64 << (button - 1);
                if down {
                    BUTTONS.fetch_or(bit, Ordering::Release);
                } else {
                    BUTTONS.fetch_and(!bit, Ordering::Release);
                }
                post(CGEventCreateMouseEvent(null_mut(), kind, point, b))
            }
            Input::Scroll { x, y } => post(CGEventCreateScrollWheelEvent(
                null_mut(),
                1,
                2,
                y as i32,
                x as i32,
            )),
        }
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
unsafe fn initial_buttons() -> std::collections::BTreeSet<u8> {
    [(1, 0), (2, 1), (3, 2)]
        .into_iter()
        .filter(|(_, b)| CGEventSourceButtonState(0, *b))
        .map(|(u, _)| u)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
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
