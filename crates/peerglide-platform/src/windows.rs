use super::*;
use anyhow::{bail, ensure};
use std::{cell::RefCell, mem::size_of, ptr::null_mut};
use windows_sys::Win32::{
    Foundation::*,
    UI::{HiDpi::*, Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
};

const TAG: usize = 0x50474c44;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LocalMouseEvent {
    Right(bool),
    Motion,
    Button(u8, bool),
    BothExtraButtons(bool),
    Other,
}

fn local_mouse_event(message: u32, data: u32) -> LocalMouseEvent {
    match message {
        WM_RBUTTONDOWN => LocalMouseEvent::Right(true),
        WM_RBUTTONUP => LocalMouseEvent::Right(false),
        WM_MOUSEMOVE => LocalMouseEvent::Motion,
        WM_LBUTTONDOWN => LocalMouseEvent::Button(1, true),
        WM_LBUTTONUP => LocalMouseEvent::Button(1, false),
        WM_MBUTTONDOWN => LocalMouseEvent::Button(3, true),
        WM_MBUTTONUP => LocalMouseEvent::Button(3, false),
        WM_XBUTTONDOWN | WM_XBUTTONUP => {
            let button = match (data >> 16) as u16 {
                XBUTTON1 => 4,
                XBUTTON2 => 5,
                both if both == (XBUTTON1 | XBUTTON2) => {
                    return LocalMouseEvent::BothExtraButtons(message == WM_XBUTTONDOWN);
                }
                _ => return LocalMouseEvent::Other,
            };
            LocalMouseEvent::Button(button, message == WM_XBUTTONDOWN)
        }
        _ => LocalMouseEvent::Other,
    }
}

thread_local! { static WHEEL:RefCell<[i32; 2]>=const {RefCell::new([0; 2])}; }
thread_local! { static STATE:RefCell<Option<HookState>>=const {RefCell::new(None)}; }
pub fn capture(
    tx: mpsc::Sender<Captured>,
    gate: CaptureGate,
    target: InputCapabilities,
    local_gestures: Option<gesture_capture::CallbackBroker>,
) -> Result<CaptureThread> {
    let reservation = capture_thread::Reservation::acquire()?;
    let (ready, rx) = std::sync::mpsc::sync_channel(1);
    let thread = CaptureThread::spawn_reserved(
        move |stop| unsafe {
            SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            let keys = initial_keys();
            let buttons = initial_buttons();
            let mut local_gestures = local_gestures;
            if let Some(broker) = local_gestures.as_mut() {
                // Startup sampling/collection precedes installation, never in
                // callbacks. KVM keeps its existing three-button capability.
                let extra_buttons = [(4, VK_XBUTTON1), (5, VK_XBUTTON2)]
                    .into_iter()
                    .filter(|(_, vk)| GetAsyncKeyState(i32::from(*vk)) < 0)
                    .map(|(button, _)| button);
                broker.initialize(
                    keys.iter().copied(),
                    buttons.iter().copied().chain(extra_buttons),
                );
            }
            let state = HookState {
                local_gestures,
                tx,
                gate,
                keys,
                buttons,
                toggle_armed: false,
                target,
            };
            state.update_held();
            STATE.set(Some(state));
            let mouse = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook), null_mut(), 0);
            let keyboard = SetWindowsHookExW(WH_KEYBOARD_LL, Some(key_hook), null_mut(), 0);
            if mouse.is_null() || keyboard.is_null() {
                if !mouse.is_null() {
                    UnhookWindowsHookEx(mouse);
                }
                if !keyboard.is_null() {
                    UnhookWindowsHookEx(keyboard);
                }
                STATE.with_borrow_mut(|state| {
                    if let Some(broker) = state.as_mut().and_then(|s| s.local_gestures.as_mut()) {
                        broker.lost();
                    }
                });
                let _ = ready.send(false);
                return;
            }
            let _ = ready.send(true);
            let mut msg: MSG = std::mem::zeroed();
            let unexpected = loop {
                let stopping = stop.load(Ordering::Acquire);
                let keep_running = STATE.with_borrow_mut(|state| {
                    state.as_mut().is_some_and(|s| {
                        if let Some(broker) = s.local_gestures.as_mut() {
                            // Stop cancels admission immediately. The same hook
                            // remains installed until its owed physical up is
                            // received; no competing hook or synthetic release.
                            broker.keep_running(stopping)
                        } else {
                            !stopping
                        }
                    })
                });
                if !keep_running {
                    break false;
                }
                if PeekMessageW(&mut msg, null_mut(), 0, 0, PM_REMOVE) != 0 {
                    if msg.message == WM_QUIT {
                        break true;
                    }
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                } else {
                    // Input wakes immediately; the timeout bounds cooperative shutdown.
                    if MsgWaitForMultipleObjectsEx(
                        0,
                        std::ptr::null(),
                        50,
                        QS_ALLINPUT,
                        MWMO_INPUTAVAILABLE,
                    ) == WAIT_FAILED {
                        break true;
                    }
                }
            };
            STATE.with_borrow_mut(|state| {
                if let Some(s) = state {
                    s.gate.disable();
                    if unexpected {
                        if let Some(broker) = s.local_gestures.as_mut() {
                            broker.lost();
                        }
                    }
                }
            });
            UnhookWindowsHookEx(mouse);
            UnhookWindowsHookEx(keyboard);
        },
        reservation,
    );
    ensure!(
        rx.recv_timeout(std::time::Duration::from_secs(3))?,
        "unable to install input hooks"
    );
    Ok(thread)
}
unsafe extern "system" fn key_hook(code: i32, w: WPARAM, l: LPARAM) -> LRESULT {
    if code >= 0 {
        let e = &*(l as *const KBDLLHOOKSTRUCT);
        if e.flags & LLKHF_INJECTED == 0 && e.dwExtraInfo != TAG {
            let block = STATE.with_borrow_mut(|s| {
                s.as_mut().is_some_and(|s| {
                    let _callback = if s.local_gestures.is_none() {
                        Some(s.gate.begin_event())
                    } else {
                        None
                    };
                    let down = w as u32 == WM_KEYDOWN || w as u32 == WM_SYSKEYDOWN;
                    physical_key(
                        s,
                        e.vkCode as u16,
                        e.scanCode as u16,
                        e.flags & LLKHF_EXTENDED != 0,
                        down,
                    )
                })
            });
            if block {
                return 1;
            }
        }
    }
    CallNextHookEx(null_mut(), code, w, l)
}
unsafe extern "system" fn mouse_hook(code: i32, w: WPARAM, l: LPARAM) -> LRESULT {
    if code >= 0 {
        let e = &*(l as *const MSLLHOOKSTRUCT);
        if e.flags & LLMHF_INJECTED == 0 && e.dwExtraInfo != TAG {
            let block = STATE.with_borrow_mut(|s| {
                let Some(s) = s else {
                    return false;
                };
                if let Some(broker) = s.local_gestures.as_mut() {
                    let remote = s.gate.active();
                    // Fixed native short path only: no legacy BTreeSet, tokio
                    // channel, cursor query, wheel state or recognition here.
                    let point = gesture_capture::RawPoint {
                        x: f64::from(e.pt.x),
                        y: f64::from(e.pt.y),
                        units: gesture_capture::CoordinateUnits::WindowsPhysicalPixels,
                    };
                    match local_mouse_event(w as u32, e.mouseData) {
                        LocalMouseEvent::Right(down) => return broker.right(down, point, remote),
                        LocalMouseEvent::Motion => broker.motion(point),
                        LocalMouseEvent::Button(button, down) => broker.button(button, down),
                        LocalMouseEvent::BothExtraButtons(down) => {
                            broker.button(4, down);
                            broker.button(5, down);
                        }
                        LocalMouseEvent::Other => broker.other(),
                    }
                    return false;
                }
                let _callback = s.gate.begin_event();
                let active = s.gate.active();
                match w as u32 {
                    WM_MOUSEMOVE => {
                        if active {
                            let mut p = POINT { x: 0, y: 0 };
                            GetCursorPos(&mut p);
                            s.emit(Captured::Motion {
                                dx: (e.pt.x - p.x) as f64,
                                dy: (e.pt.y - p.y) as f64,
                            });
                        } else {
                            s.emit(Captured::Position {
                                x: e.pt.x as f64,
                                y: e.pt.y as f64,
                                width: GetSystemMetrics(SM_CXSCREEN) as f64,
                                height: GetSystemMetrics(SM_CYSCREEN) as f64,
                            });
                        }
                    }
                    WM_LBUTTONDOWN | WM_LBUTTONUP | WM_RBUTTONDOWN | WM_RBUTTONUP
                    | WM_MBUTTONDOWN | WM_MBUTTONUP => {
                        let (button, down) = match w as u32 {
                            WM_LBUTTONDOWN => (1, true),
                            WM_LBUTTONUP => (1, false),
                            WM_RBUTTONDOWN => (2, true),
                            WM_RBUTTONUP => (2, false),
                            WM_MBUTTONDOWN => (3, true),
                            _ => (3, false),
                        };
                        s.button(button, down);
                    }
                    WM_MOUSEWHEEL => s.scroll(
                        0,
                        WHEEL.with_borrow_mut(|r| {
                            wheel_steps(&mut r[1], (e.mouseData >> 16) as i16)
                        }),
                    ),
                    WM_MOUSEHWHEEL => s.scroll(
                        WHEEL.with_borrow_mut(|r| {
                            wheel_steps(&mut r[0], (e.mouseData >> 16) as i16)
                        }),
                        0,
                    ),
                    _ => (),
                }
                active && s.gate.active()
            });
            if block {
                return 1;
            }
        }
    }
    CallNextHookEx(null_mut(), code, w, l)
}
// Preserve sub-detent wheel increments rather than dropping high-resolution input.
fn wheel_steps(remainder: &mut i32, delta: i16) -> i16 {
    let total = *remainder + i32::from(delta);
    *remainder = total % 120;
    (total / 120) as i16
}
pub fn inject(event: Input) -> Result<()> {
    unsafe {
        let mut input: INPUT = std::mem::zeroed();
        match event {
            Input::Key { usage, down } => {
                input.r#type = INPUT_KEYBOARD;
                input.Anonymous.ki = key_input(usage, down)?;
            }
            Input::Button { button, down } => {
                input.r#type = INPUT_MOUSE;
                let flags = match (button, down) {
                    (1, true) => MOUSEEVENTF_LEFTDOWN,
                    (1, false) => MOUSEEVENTF_LEFTUP,
                    (2, true) => MOUSEEVENTF_RIGHTDOWN,
                    (2, false) => MOUSEEVENTF_RIGHTUP,
                    (3, true) => MOUSEEVENTF_MIDDLEDOWN,
                    (3, false) => MOUSEEVENTF_MIDDLEUP,
                    _ => bail!("unsupported mouse button"),
                };
                input.Anonymous.mi = MOUSEINPUT {
                    dx: 0,
                    dy: 0,
                    mouseData: 0,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: TAG,
                };
            }
            Input::Scroll { x, y } => {
                if x != 0 && y != 0 {
                    inject(Input::Scroll { x, y: 0 })?;
                }
                input.r#type = INPUT_MOUSE;
                input.Anonymous.mi = MOUSEINPUT {
                    dx: 0,
                    dy: 0,
                    mouseData: (if y != 0 { y } else { x } as i32 * 120) as u32,
                    dwFlags: if y != 0 {
                        MOUSEEVENTF_WHEEL
                    } else {
                        MOUSEEVENTF_HWHEEL
                    },
                    time: 0,
                    dwExtraInfo: TAG,
                };
            }
        }
        ensure!(
            SendInput(1, &input, size_of::<INPUT>() as i32) == 1,
            "SendInput rejected (integrity level / secure desktop)"
        );
        Ok(())
    }
}
pub fn pointer(x: f64, y: f64) -> Result<()> {
    unsafe {
        let input = INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx: (x * 65535.0).round() as i32,
                    dy: (y * 65535.0).round() as i32,
                    mouseData: 0,
                    dwFlags: MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE,
                    time: 0,
                    dwExtraInfo: TAG,
                },
            },
        };
        ensure!(
            SendInput(1, &input, size_of::<INPUT>() as i32) == 1,
            "pointer injection rejected"
        );
        Ok(())
    }
}
// Set-1 physical scan positions; E0 distinguishes keypad/navigation and modifiers.
const KEYS: &[(u16, u16, bool)] = &[
    (0x04, 0x1e, false),
    (0x05, 0x30, false),
    (0x06, 0x2e, false),
    (0x07, 0x20, false),
    (0x08, 0x12, false),
    (0x09, 0x21, false),
    (0x0a, 0x22, false),
    (0x0b, 0x23, false),
    (0x0c, 0x17, false),
    (0x0d, 0x24, false),
    (0x0e, 0x25, false),
    (0x0f, 0x26, false),
    (0x10, 0x32, false),
    (0x11, 0x31, false),
    (0x12, 0x18, false),
    (0x13, 0x19, false),
    (0x14, 0x10, false),
    (0x15, 0x13, false),
    (0x16, 0x1f, false),
    (0x17, 0x14, false),
    (0x18, 0x16, false),
    (0x19, 0x2f, false),
    (0x1a, 0x11, false),
    (0x1b, 0x2d, false),
    (0x1c, 0x15, false),
    (0x1d, 0x2c, false),
    (0x1e, 0x02, false),
    (0x1f, 0x03, false),
    (0x20, 0x04, false),
    (0x21, 0x05, false),
    (0x22, 0x06, false),
    (0x23, 0x07, false),
    (0x24, 0x08, false),
    (0x25, 0x09, false),
    (0x26, 0x0a, false),
    (0x27, 0x0b, false),
    (0x28, 0x1c, false),
    (0x29, 0x01, false),
    (0x2a, 0x0e, false),
    (0x2b, 0x0f, false),
    (0x2c, 0x39, false),
    (0x2d, 0x0c, false),
    (0x2e, 0x0d, false),
    (0x2f, 0x1a, false),
    (0x30, 0x1b, false),
    (0x31, 0x2b, false),
    (0x33, 0x27, false),
    (0x34, 0x28, false),
    (0x35, 0x29, false),
    (0x36, 0x33, false),
    (0x37, 0x34, false),
    (0x38, 0x35, false),
    (0x39, 0x3a, false),
    (0x3a, 0x3b, false),
    (0x3b, 0x3c, false),
    (0x3c, 0x3d, false),
    (0x3d, 0x3e, false),
    (0x3e, 0x3f, false),
    (0x3f, 0x40, false),
    (0x40, 0x41, false),
    (0x41, 0x42, false),
    (0x42, 0x43, false),
    (0x43, 0x44, false),
    (0x44, 0x57, false),
    (0x45, 0x58, false),
    (0x49, 0x52, true),
    (0x4a, 0x47, true),
    (0x4b, 0x49, true),
    (0x4c, 0x53, true),
    (0x4d, 0x4f, true),
    (0x4e, 0x51, true),
    (0x4f, 0x4d, true),
    (0x50, 0x4b, true),
    (0x51, 0x50, true),
    (0x52, 0x48, true),
    (0x53, 0x45, true),
    (0x54, 0x35, true),
    (0x58, 0x1c, true),
    (0x55, 0x37, false),
    (0x56, 0x4a, false),
    (0x57, 0x4e, false),
    (0x59, 0x4f, false),
    (0x5a, 0x50, false),
    (0x5b, 0x51, false),
    (0x5c, 0x4b, false),
    (0x5d, 0x4c, false),
    (0x5e, 0x4d, false),
    (0x5f, 0x47, false),
    (0x60, 0x48, false),
    (0x61, 0x49, false),
    (0x62, 0x52, false),
    (0x63, 0x53, false),
    (0x64, 0x56, false),
    (0xe0, 0x1d, false),
    (0xe1, 0x2a, false),
    (0xe2, 0x38, false),
    (0xe5, 0x36, false),
    (0xe3, 0x5b, true),
    (0xe4, 0x1d, true),
    (0xe6, 0x38, true),
    (0xe7, 0x5c, true),
];
pub(super) fn key_usages() -> Vec<u16> {
    KEYS.iter().map(|k| k.0).collect()
}
fn to_scan(usage: u16) -> Option<(u16, bool)> {
    KEYS.iter().find(|k| k.0 == usage).map(|k| (k.1, k.2))
}
fn from_scan(scan: u16, extended: bool) -> Option<u16> {
    KEYS.iter()
        .find(|k| k.1 == scan && k.2 == extended)
        .map(|k| k.0)
}
const STARTUP_VK: u16 = 0x4000;
fn physical_key(s: &mut HookState, vk: u16, scan: u16, extended: bool, down: bool) -> bool {
    let usage = if vk == VK_PAUSE || vk == VK_SNAPSHOT {
        None
    } else if vk == VK_NUMLOCK {
        Some(0x53)
    } else {
        from_scan(scan, extended)
    };
    if let Some(broker) = s.local_gestures.as_mut() {
        broker.clear_startup_key(STARTUP_VK | vk);
        // HookState's G1 branches use only the broker's fixed key bitset.
        return match usage {
            Some(usage) => s.key(usage, down),
            None => s.unsupported_key(vk, down),
        };
    }
    // Startup state is sampled as VKs; the first physical callback replaces that
    // sample with its unambiguous scan identity (including keypad Enter).
    let startup_held = s.keys.remove(&(STARTUP_VK | vk));
    match usage {
        Some(usage) => {
            if startup_held && down {
                s.keys.insert(usage);
            }
            s.key(usage, down)
        }
        None => s.unsupported_key(vk, down),
    }
}
fn key_input(usage: u16, down: bool) -> Result<KEYBDINPUT> {
    let (scan, extended) = to_scan(usage).ok_or_else(|| anyhow::anyhow!("unsupported HID key"))?;
    Ok(KEYBDINPUT {
        wVk: 0,
        wScan: scan,
        dwFlags: KEYEVENTF_SCANCODE
            | if extended { KEYEVENTF_EXTENDEDKEY } else { 0 }
            | if down { 0 } else { KEYEVENTF_KEYUP },
        time: 0,
        dwExtraInfo: TAG,
    })
}

unsafe fn initial_keys() -> std::collections::BTreeSet<u16> {
    // Exclude mouse VKs and generic modifier aliases (the side-specific VKs
    // below own their releases). Unknown keys held at startup also block capture.
    (7u16..=255)
        .filter(|v| ![0x10, 0x11, 0x12].contains(v))
        .filter(|v| GetAsyncKeyState(i32::from(*v)) < 0)
        .map(|v| STARTUP_VK | v)
        .collect()
}
unsafe fn initial_buttons() -> std::collections::BTreeSet<u8> {
    [(1, 1), (2, 2), (3, 4)]
        .into_iter()
        .filter(|(_, v)| GetAsyncKeyState(*v) < 0)
        .map(|(b, _)| b)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_mouse_classification_preserves_right_pairs_and_all_other_buttons() {
        assert_eq!(local_mouse_event(WM_RBUTTONDOWN, 0), LocalMouseEvent::Right(true));
        assert_eq!(local_mouse_event(WM_RBUTTONUP, 0), LocalMouseEvent::Right(false));
        assert_eq!(local_mouse_event(WM_MOUSEMOVE, 0), LocalMouseEvent::Motion);
        for (down, up, button, data) in [
            (WM_LBUTTONDOWN, WM_LBUTTONUP, 1, 0),
            (WM_MBUTTONDOWN, WM_MBUTTONUP, 3, 0),
            (WM_XBUTTONDOWN, WM_XBUTTONUP, 4, u32::from(XBUTTON1) << 16),
            (WM_XBUTTONDOWN, WM_XBUTTONUP, 5, u32::from(XBUTTON2) << 16),
        ] {
            assert_eq!(local_mouse_event(down, data), LocalMouseEvent::Button(button, true));
            assert_eq!(local_mouse_event(up, data), LocalMouseEvent::Button(button, false));
        }
        for message in [WM_MOUSEWHEEL, WM_MOUSEHWHEEL, WM_NULL, WM_XBUTTONDOWN, WM_XBUTTONUP] {
            assert_eq!(local_mouse_event(message, 0), LocalMouseEvent::Other);
        }
        assert_eq!(local_mouse_event(WM_XBUTTONDOWN, 3 << 16), LocalMouseEvent::BothExtraButtons(true));
        assert_eq!(local_mouse_event(WM_XBUTTONUP, 3 << 16), LocalMouseEvent::BothExtraButtons(false));
        assert_eq!(local_mouse_event(WM_XBUTTONDOWN, 4 << 16), LocalMouseEvent::Other);
    }

    #[test]
    fn high_resolution_wheel_preserves_remainder_and_direction() {
        let mut remainder = 0;
        assert_eq!(wheel_steps(&mut remainder, 30), 0);
        assert_eq!(wheel_steps(&mut remainder, 30), 0);
        assert_eq!(wheel_steps(&mut remainder, 90), 1);
        assert_eq!(remainder, 30);
        assert_eq!(wheel_steps(&mut remainder, -60), 0);
        assert_eq!(wheel_steps(&mut remainder, -90), -1);
        assert_eq!(remainder, 0);
        assert_eq!(wheel_steps(&mut remainder, 240), 2);
    }
    #[test]
    fn key_positions_roundtrip() {
        for &(usage, scan, extended) in KEYS {
            assert_eq!(to_scan(usage), Some((scan, extended)));
            assert_eq!(from_scan(scan, extended), Some(usage));
            let down = key_input(usage, true).unwrap();
            let up = key_input(usage, false).unwrap();
            assert_eq!(down.wVk, 0);
            assert_eq!(down.wScan, scan);
            assert_eq!(up.dwFlags, down.dwFlags | KEYEVENTF_KEYUP);
            assert_eq!(down.dwFlags & KEYEVENTF_SCANCODE, KEYEVENTF_SCANCODE);
            assert_eq!(down.dwFlags & KEYEVENTF_EXTENDEDKEY != 0, extended);
        }
    }
    #[test]
    fn keypad_navigation_and_modifier_sides_stay_distinct() {
        assert_eq!(from_scan(0x1c, false), Some(0x28));
        assert_eq!(from_scan(0x1c, true), Some(0x58));
        assert_eq!(from_scan(0x4f, false), Some(0x59));
        assert_eq!(from_scan(0x4f, true), Some(0x4d));
        assert_eq!(from_scan(0x1d, false), Some(0xe0));
        assert_eq!(from_scan(0x1d, true), Some(0xe4));
    }
    #[test]
    fn startup_virtual_sample_repeat_is_not_a_fresh_switch_press() {
        let (tx, _rx) = mpsc::channel(16);
        let mut s = HookState {
            local_gestures: None,
            tx,
            gate: CaptureGate::default(),
            keys: [STARTUP_VK | VK_RIGHT, 0xe0, 0xe2].into(),
            buttons: Default::default(),
            toggle_armed: false,
            target: InputCapabilities::new(&key_usages(), true, 7, true).unwrap(),
        };
        s.update_held();
        assert!(!physical_key(&mut s, VK_RIGHT, 0x4d, true, true));
        assert!(!s.toggle_armed);
    }
    #[test]
    fn keypad_enter_release_clears_ambiguous_startup_sample() {
        let (tx, mut rx) = mpsc::channel(8);
        let mut s = HookState {
            local_gestures: None,
            tx,
            gate: CaptureGate::default(),
            keys: [STARTUP_VK | VK_RETURN].into(),
            buttons: Default::default(),
            toggle_armed: false,
            target: InputCapabilities::new(&key_usages(), true, 7, true).unwrap(),
        };
        s.update_held();
        assert!(!s.gate.try_activate());
        assert!(!physical_key(&mut s, VK_RETURN, 0x1c, true, false));
        assert!(s.gate.try_activate());
        assert!(matches!(
            rx.try_recv().unwrap(),
            Captured::Input(Input::Key {
                usage: 0x58,
                down: false
            })
        ));
    }
}
