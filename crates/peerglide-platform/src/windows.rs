use super::*;
use anyhow::ensure;
use std::{cell::RefCell, mem::size_of, ptr::null_mut};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::{GetMonitorInfoW, HMONITOR, MONITORINFO},
    System::Threading::GetCurrentThreadId,
    UI::{HiDpi::*, Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
};

const TAG: usize = 0x50474c44;
/// Receiver injection queue; adjacent pointer moves coalesce into one slot.
const INJECT_CAPACITY: usize = 256;

/// Runs once on the receiver injection thread, matching the capture thread's
/// coordinate context. It installs no hook.
pub(super) fn prepare_inject_thread() {
    unsafe {
        SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LocalMouseEvent {
    Right(bool),
    Motion,
    Button(u8, bool),
    BothExtraButtons(bool),
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MouseOrigin {
    Recovery(bool),
    Injected,
    Physical,
}

fn right_transition(message: u32) -> Option<bool> {
    match message {
        WM_RBUTTONDOWN => Some(true),
        WM_RBUTTONUP => Some(false),
        _ => None,
    }
}

fn mouse_origin(recovery: Option<bool>, flags: u32, tag: usize) -> MouseOrigin {
    if let Some(block) = recovery {
        // The exact current recovery decision precedes generic injected/TAG
        // bypass. A matched malformed event is faulted/blocked by the broker.
        MouseOrigin::Recovery(block)
    } else if flags & LLMHF_INJECTED != 0 || tag == TAG {
        MouseOrigin::Injected
    } else {
        MouseOrigin::Physical
    }
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

#[derive(Default)]
struct WheelState {
    route: Option<CaptureRoute>,
    remainder: [i32; 2],
}
impl WheelState {
    fn steps(&mut self, route: CaptureRoute, axis: usize, delta: i16) -> i16 {
        if self.route != Some(route) {
            self.remainder = [0; 2];
            self.route = Some(route);
        }
        wheel_steps(&mut self.remainder[axis], delta)
    }
}
thread_local! { static WHEEL:RefCell<WheelState>=const {RefCell::new(WheelState{route:None,remainder:[0;2]})}; }
#[derive(Default)]
pub(super) struct ReceiveWake {
    thread: std::sync::Mutex<Option<u32>>,
}
impl ReceiveWake {
    pub(super) fn wake(&self) {
        if let Ok(thread) = self.thread.lock() {
            if let Some(thread) = *thread {
                // The lock synchronizes registration teardown and prevents an
                // old command from targeting a subsequently reused thread ID.
                unsafe {
                    PostThreadMessageW(thread, WM_APP + 41, 0, 0);
                }
            }
        }
    }
}
struct WakeRegistration(Arc<ReceiveWake>);
impl Drop for WakeRegistration {
    fn drop(&mut self) {
        if let Ok(mut thread) = self.0.thread.lock() {
            *thread = None;
        }
    }
}
pub(super) fn suppressed_modifiers_held() -> bool {
    STATE.with_borrow(|state| state.as_ref().is_none_or(HookState::suppressed_modifiers))
}
thread_local! { static STATE:RefCell<Option<HookState>>=const {RefCell::new(None)}; }
pub fn capture(
    tx: CaptureEventSender,
    gate: CaptureGate,
    target: InputCapabilities,
    local_gestures: Option<gesture_capture::CallbackBroker>,
) -> Result<CaptureThread> {
    capture_impl(tx, gate, target, local_gestures, None)
}
pub(super) fn capture_receiving(
    tx: CaptureEventSender,
    gate: CaptureGate,
    target: InputCapabilities,
    engine: receive_driver::ReceiveEngine,
    wake: Arc<ReceiveWake>,
) -> Result<CaptureThread> {
    capture_impl(tx, gate, target, None, Some((engine, wake)))
}
fn capture_impl(
    tx: CaptureEventSender,
    gate: CaptureGate,
    target: InputCapabilities,
    local_gestures: Option<gesture_capture::CallbackBroker>,
    receive: Option<(receive_driver::ReceiveEngine, Arc<ReceiveWake>)>,
) -> Result<CaptureThread> {
    let reservation = capture_thread::Reservation::acquire()?;
    let (ready, rx) = std::sync::mpsc::sync_channel(1);
    let thread = CaptureThread::spawn_reserved(
        move |stop| unsafe {
            SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            let receive_gate = gate.clone();
            let mut receive = receive;
            let mut source_health = SourceHealth::new();
            let mut queue: MSG = std::mem::zeroed();
            PeekMessageW(&mut queue, null_mut(), 0, 0, PM_NOREMOVE);
            let _wake_registration = receive.as_ref().map(|(_, wake)| {
                if let Ok(mut thread) = wake.thread.lock() {
                    *thread = Some(GetCurrentThreadId());
                }
                WakeRegistration(wake.clone())
            });
            // SendInput leaves this hook-owning thread; see CAPTURE_LIFECYCLE.
            // Created before the hooks, owned and stopped by this loop.
            let published = Arc::new(std::sync::Mutex::new(source_health.observation));
            let inject = match receive.as_ref() {
                Some((_, wake)) => {
                    let wake = wake.clone();
                    let sink = ReceiveSink {
                        gate: receive_gate.clone(),
                        observation: published.clone(),
                    };
                    let wake: inject_thread::Wake = Arc::new(move || wake.wake());
                    match inject_thread::spawn(sink, INJECT_CAPACITY, wake) {
                        Ok(inject) => Some(inject),
                        Err(_) => {
                            let _ = ready.send(false);
                            return;
                        }
                    }
                }
                None => None,
            };
            let mut receive_backend = NativeReceiveBackend {
                gate: receive_gate.clone(),
                observation: source_health.observation,
                inject,
                published,
            };
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
                pairs: PairLedger::default(),
                stopping: false,
                toggle_route: None,
                local_gestures,
                tx,
                gate,
                keys: keys.into_iter().collect(),
                buttons: buttons.into_iter().collect(),
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
            let mut last_turn = std::time::Instant::now();
            let unexpected = loop {
                super::note_loop_gap(&receive_gate, &mut last_turn);
                if receive.is_some() {
                    let debt = STATE
                        .with_borrow(|state| state.as_ref().is_some_and(|s| s.pairs.debt != 0))
                        || receive_gate.receiving_epoch().is_some();
                    source_health.sample(&mut receive_backend, debt);
                }
                // Native effects may synchronously reenter our hooks. This
                // engine and its backend live outside the STATE RefCell borrow.
                let immediate = receive.as_mut().is_some_and(|(engine, _)| {
                    engine.pump(&mut receive_backend, now_ms(), stop.load(Ordering::Acquire))
                });
                let cleanup = STATE.with_borrow_mut(|state| {
                    state
                        .as_mut()
                        .and_then(|s| s.local_gestures.as_mut())
                        .and_then(|broker| broker.cleanup_request())
                });
                if let Some(request) = cleanup {
                    // SendInput may synchronously reenter mouse_hook. No STATE
                    // borrow survives this call. Cleanup never emits a down.
                    let count = send_recovery_up(request.tag);
                    STATE.with_borrow_mut(|state| {
                        if let Some(broker) = state.as_mut().and_then(|s| s.local_gestures.as_mut())
                        {
                            broker.cleanup_result(request, count);
                        }
                    });
                }
                let stopping = stop.load(Ordering::Acquire)
                    || receive
                        .as_ref()
                        .is_some_and(|(engine, _)| engine.control().stop_requested());
                let keep_running = STATE.with_borrow_mut(|state| {
                    state.as_mut().is_some_and(|s| {
                        if let Some(broker) = s.local_gestures.as_mut() {
                            // Stop cancels admission immediately. The same hook
                            // remains installed until its owed physical up is
                            // received; no competing hook or synthetic release.
                            broker.keep_running(stopping)
                        } else {
                            s.keep_running(stopping)
                        }
                    })
                });
                if !keep_running && receive.as_ref().is_none_or(|(engine, _)| engine.can_exit()) {
                    break false;
                }
                if !receive_gate.0.healthy.load(Ordering::Acquire) {
                    break true;
                }
                let has_message = PeekMessageW(&mut msg, null_mut(), 0, 0, PM_REMOVE) != 0;
                if has_message {
                    if msg.message == WM_QUIT {
                        break true;
                    }
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                // PeekMessage can dispatch a sent hook callback even when it
                // returns no posted message. Publish before sleeping either way.
                STATE.with_borrow_mut(|state| {
                    if let Some(state) = state {
                        state.tx.notify_after_callbacks();
                    }
                });
                if !has_message && !immediate {
                    // Input/typed commands wake immediately; the timeout bounds cooperative shutdown.
                    if MsgWaitForMultipleObjectsEx(
                        0,
                        std::ptr::null(),
                        50,
                        QS_ALLINPUT,
                        MWMO_INPUTAVAILABLE,
                    ) == WAIT_FAILED
                    {
                        break true;
                    }
                }
            };
            STATE.with_borrow_mut(|state| {
                if let Some(s) = state {
                    s.gate.disable();
                    if unexpected {
                        s.source_lost();
                        if let Some(broker) = s.local_gestures.as_mut() {
                            broker.lost();
                        }
                    }
                }
            });
            if let Some((engine, _)) = receive.as_mut() {
                engine.pump(&mut receive_backend, now_ms(), true);
                let _ = engine.outcome(); // Preserve failure in the shared control.
            }
            // Unstarted effects never run after this capture; an in-flight call
            // may still need hooks serviced, so unhook before the bounded join.
            receive_backend.request_inject_stop();
            UnhookWindowsHookEx(mouse);
            UnhookWindowsHookEx(keyboard);
            if !receive_backend.join_inject(std::time::Duration::from_millis(500)) {
                if let Some((engine, _)) = receive.as_mut() {
                    engine.mark_unconfirmed();
                }
                // Drop still joins: the injector never outlives this thread.
            }
            drop(receive_backend);
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
                    let route = _callback
                        .as_ref()
                        .map(|callback| callback.route)
                        .unwrap_or_else(|| s.gate.snapshot());
                    let down = w as u32 == WM_KEYDOWN || w as u32 == WM_SYSKEYDOWN;
                    physical_key_routed(
                        s,
                        e.vkCode as u16,
                        e.scanCode as u16,
                        e.flags & LLKHF_EXTENDED != 0,
                        down,
                        route,
                    )
                })
            });
            if block {
                return 1;
            }
        } else {
            STATE.with_borrow_mut(|state| {
                if let Some(broker) = state.as_mut().and_then(|s| s.local_gestures.as_mut()) {
                    broker.foreign_input(false, false);
                }
            });
        }
    }
    CallNextHookEx(null_mut(), code, w, l)
}
unsafe extern "system" fn mouse_hook(code: i32, w: WPARAM, l: LPARAM) -> LRESULT {
    if code >= 0 {
        let e = &*(l as *const MSLLHOOKSTRUCT);
        let recovery = STATE.with_borrow_mut(|state| {
            state
                .as_mut()
                .and_then(|s| s.local_gestures.as_mut())
                .and_then(|broker| {
                    broker.tagged_right(
                        e.dwExtraInfo,
                        right_transition(w as u32),
                        gesture_capture::RawPoint {
                            x: f64::from(e.pt.x),
                            y: f64::from(e.pt.y),
                            units: gesture_capture::CoordinateUnits::WindowsPhysicalPixels,
                        },
                    )
                })
        });
        match mouse_origin(recovery, e.flags, e.dwExtraInfo) {
            MouseOrigin::Recovery(true) => return 1,
            MouseOrigin::Recovery(false) => return CallNextHookEx(null_mut(), code, w, l),
            MouseOrigin::Injected => {
                let block = STATE.with_borrow_mut(|state| {
                    // Atomic tallies only. Injected wheel events (smooth-scroll
                    // drivers) bypass forwarding, so they are counted apart.
                    let wheel = matches!(w as u32, WM_MOUSEWHEEL | WM_MOUSEHWHEEL);
                    if w as u32 == WM_MOUSEMOVE || wheel {
                        if let Some(diagnostics) =
                            state.as_ref().and_then(|s| s.gate.0.diagnostics.get())
                        {
                            diagnostics.increment(if wheel {
                                input_diagnostics::Count::WheelInjected
                            } else {
                                input_diagnostics::Count::Filtered
                            });
                        }
                    }
                    state
                        .as_mut()
                        .and_then(|s| s.local_gestures.as_mut())
                        .is_some_and(|broker| {
                            broker.foreign_input(
                                right_transition(w as u32).is_some(),
                                w as u32 == WM_MOUSEMOVE,
                            )
                        })
                });
                return if block {
                    1
                } else {
                    CallNextHookEx(null_mut(), code, w, l)
                };
            }
            MouseOrigin::Physical => (),
        }
        {
            let block = STATE.with_borrow_mut(|s| {
                let Some(s) = s else {
                    return false;
                };
                if matches!(w as u32, WM_MOUSEWHEEL | WM_MOUSEHWHEEL) {
                    if let Some(diagnostics) = s.gate.0.diagnostics.get() {
                        diagnostics.increment(input_diagnostics::Count::WheelPhysical);
                    }
                }
                if let Some(broker) = s.local_gestures.as_mut() {
                    let remote = s.gate.active();
                    // Fixed native short path only: no KVM channel, cursor query,
                    // wheel state or recognition here.
                    let point = gesture_capture::RawPoint {
                        x: f64::from(e.pt.x),
                        y: f64::from(e.pt.y),
                        units: gesture_capture::CoordinateUnits::WindowsPhysicalPixels,
                    };
                    match local_mouse_event(w as u32, e.mouseData) {
                        LocalMouseEvent::Right(down) => return broker.right(down, point, remote),
                        LocalMouseEvent::Motion => return broker.motion(point),
                        LocalMouseEvent::Button(button, down) => broker.button(button, down),
                        LocalMouseEvent::BothExtraButtons(down) => {
                            broker.button(4, down);
                            broker.button(5, down);
                        }
                        LocalMouseEvent::Other => broker.other(),
                    }
                    return false;
                }
                let callback = s.gate.begin_event();
                let route = callback.route;
                match w as u32 {
                    WM_MOUSEMOVE => {
                        let event = if route.remote() {
                            let mut p = POINT { x: 0, y: 0 };
                            GetCursorPos(&mut p);
                            Captured::Motion {
                                dx: (e.pt.x - p.x) as f64,
                                dy: (e.pt.y - p.y) as f64,
                                route,
                            }
                        } else {
                            Captured::Position {
                                x: e.pt.x as f64,
                                y: e.pt.y as f64,
                                width: GetSystemMetrics(SM_CXSCREEN) as f64,
                                height: GetSystemMetrics(SM_CYSCREEN) as f64,
                                route,
                            }
                        };
                        s.movement(event, route)
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
                        s.button_routed(button, down, route)
                    }
                    WM_MOUSEWHEEL => s.scroll_routed(
                        0,
                        WHEEL.with_borrow_mut(|r| r.steps(route, 1, (e.mouseData >> 16) as i16)),
                        route,
                    ),
                    WM_MOUSEHWHEEL => s.scroll_routed(
                        WHEEL.with_borrow_mut(|r| r.steps(route, 0, (e.mouseData >> 16) as i16)),
                        0,
                        route,
                    ),
                    WM_XBUTTONDOWN | WM_XBUTTONUP => match local_mouse_event(w as u32, e.mouseData)
                    {
                        LocalMouseEvent::Button(button, down) => {
                            s.unsupported_button(button, down, route)
                        }
                        LocalMouseEvent::BothExtraButtons(down) => {
                            let a = s.unsupported_button(4, down, route);
                            let b = s.unsupported_button(5, down, route);
                            a || b
                        }
                        _ => {
                            s.gate.disable();
                            s.emit(Captured::UnsupportedInput);
                            false
                        }
                    },
                    _ => false,
                }
            });
            if block {
                return 1;
            }
        }
    }
    CallNextHookEx(null_mut(), code, w, l)
}

fn recovery_right_input(tag: usize, down: bool) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 0,
                dy: 0,
                mouseData: 0,
                dwFlags: if down {
                    MOUSEEVENTF_RIGHTDOWN
                } else {
                    MOUSEEVENTF_RIGHTUP
                },
                time: 0,
                dwExtraInfo: tag,
            },
        },
    }
}

fn recovery_click_inputs(down_tag: usize, up_tag: usize) -> [INPUT; 2] {
    [
        recovery_right_input(down_tag, true),
        recovery_right_input(up_tag, false),
    ]
}

/// Worker-only fixed right pair. The protocol owns authorization and receipts;
/// this raw SendInput count never proves a delivered prefix or application action.
pub(crate) fn send_recovery_click(down_tag: usize, up_tag: usize) -> u32 {
    let inputs = recovery_click_inputs(down_tag, up_tag);
    unsafe { SendInput(2, inputs.as_ptr(), size_of::<INPUT>() as i32) }
}

/// Capture-loop/worker-only cleanup of the protocol's recorded synthetic down.
/// Never replay down, move the cursor, query global button state or infer success.
pub(crate) fn send_recovery_up(tag: usize) -> u32 {
    let input = recovery_right_input(tag, false);
    unsafe { SendInput(1, &input, size_of::<INPUT>() as i32) }
}

// Preserve sub-detent wheel increments rather than dropping high-resolution input.
fn wheel_steps(remainder: &mut i32, delta: i16) -> i16 {
    let total = *remainder + i32::from(delta);
    *remainder = total % 120;
    (total / 120) as i16
}
pub fn inject(event: Input) -> Result<()> {
    receive_inject(event).map_err(|error| anyhow::anyhow!("native input request failed: {error:?}"))
}
pub(super) fn receive_inject(
    event: Input,
) -> std::result::Result<(), peerglide_actions::execution::NativeFailure> {
    let (inputs, count) = input_packets(event)?;
    submit_inputs(&inputs[..count])
}
fn input_packets(
    event: Input,
) -> std::result::Result<([INPUT; 2], usize), peerglide_actions::execution::NativeFailure> {
    use peerglide_actions::execution::NativeFailure;
    if !event.valid() {
        return Err(NativeFailure::BeforeSubmission);
    }
    // Fully initialized fixed packets, constructed on the Native owner thread.
    let mut inputs: [INPUT; 2] = unsafe { std::mem::zeroed() };
    let mut count = 1;
    match event {
        Input::Key { usage, down } => {
            inputs[0].r#type = INPUT_KEYBOARD;
            inputs[0].Anonymous.ki =
                key_input(usage, down).map_err(|_| NativeFailure::BeforeSubmission)?;
        }
        Input::Button { button, down } => {
            let flags = match (button, down) {
                (1, true) => MOUSEEVENTF_LEFTDOWN,
                (1, false) => MOUSEEVENTF_LEFTUP,
                (2, true) => MOUSEEVENTF_RIGHTDOWN,
                (2, false) => MOUSEEVENTF_RIGHTUP,
                (3, true) => MOUSEEVENTF_MIDDLEDOWN,
                (3, false) => MOUSEEVENTF_MIDDLEUP,
                _ => return Err(NativeFailure::BeforeSubmission),
            };
            inputs[0] = mouse_packet(flags, 0);
        }
        Input::Scroll { x, y } => {
            inputs[0] = mouse_packet(
                if y != 0 {
                    MOUSEEVENTF_WHEEL
                } else {
                    MOUSEEVENTF_HWHEEL
                },
                (i32::from(if y != 0 { y } else { x }) * 120) as u32,
            );
            if x != 0 && y != 0 {
                inputs[1] = mouse_packet(MOUSEEVENTF_HWHEEL, (i32::from(x) * 120) as u32);
                count = 2;
            }
        }
    }
    Ok((inputs, count))
}
fn mouse_packet(flags: u32, data: u32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 0,
                dy: 0,
                mouseData: data,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: TAG,
            },
        },
    }
}
fn submit_inputs(
    inputs: &[INPUT],
) -> std::result::Result<(), peerglide_actions::execution::NativeFailure> {
    use peerglide_actions::execution::NativeFailure;
    // The result counts SDK submissions only. UIPI/secure-desktop restrictions
    // are not bypassed and GetLastError cannot reliably identify UIPI failure.
    let count = unsafe {
        SendInput(
            inputs.len() as u32,
            inputs.as_ptr(),
            size_of::<INPUT>() as i32,
        )
    };
    let _error = unsafe { GetLastError() };
    if count == inputs.len() as u32 {
        Ok(())
    } else if count == 0 {
        Err(NativeFailure::BeforeSubmission)
    } else {
        Err(NativeFailure::UnknownOutcome)
    }
}
pub fn pointer(x: f64, y: f64) -> Result<()> {
    receive_pointer(x, y)
        .map_err(|error| anyhow::anyhow!("native pointer request failed: {error:?}"))
}
pub(super) fn diagnostic_cursor_position() -> Option<(i32, i32)> {
    let mut point = POINT { x: 0, y: 0 };
    (unsafe { GetCursorPos(&mut point) } != 0).then_some((point.x, point.y))
}
pub(super) fn receive_pointer(
    x: f64,
    y: f64,
) -> std::result::Result<(), peerglide_actions::execution::NativeFailure> {
    if !(ReceivePoint { x, y }).valid() {
        return Err(peerglide_actions::execution::NativeFailure::BeforeSubmission);
    }
    let mut input = mouse_packet(MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE, 0);
    // The active union member is initialized by mouse_packet above.
    input.Anonymous.mi.dx = (x * 65535.0).round() as i32;
    input.Anonymous.mi.dy = (y * 65535.0).round() as i32;
    submit_inputs(&[input])
}
pub(super) fn receive_on_display(
    expected: display_layout::Display,
    desktop: display_layout::Bounds,
    placement: display_layout::Placement,
) -> std::result::Result<(), peerglide_actions::execution::NativeFailure> {
    use peerglide_actions::execution::NativeFailure;
    let handle = expected.id.0 as usize as HMONITOR;
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..MONITORINFO::default()
    };
    // SAFETY: monitor information is written into a live struct on the
    // existing DPI-aware worker. Stale handles fail the documented query.
    if unsafe { GetMonitorInfoW(handle, &mut info) } == 0 {
        return Err(NativeFailure::TargetLost);
    }
    let rect = info.rcMonitor;
    let actual = display_layout::Bounds {
        left: f64::from(rect.left),
        top: f64::from(rect.top),
        right: f64::from(rect.right),
        bottom: f64::from(rect.bottom),
    };
    // SAFETY: these read only documented metrics, in the same DPI context.
    let current_desktop = unsafe {
        let left = f64::from(GetSystemMetrics(SM_XVIRTUALSCREEN));
        let top = f64::from(GetSystemMetrics(SM_YVIRTUALSCREEN));
        display_layout::Bounds {
            left,
            top,
            right: left + f64::from(GetSystemMetrics(SM_CXVIRTUALSCREEN)),
            bottom: top + f64::from(GetSystemMetrics(SM_CYVIRTUALSCREEN)),
        }
    };
    if actual != expected.bounds || current_desktop != desktop {
        return Err(NativeFailure::TargetLost);
    }
    let mut input = mouse_packet(MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK, 0);
    input.Anonymous.mi.dx = placement.virtual_absolute[0];
    input.Anonymous.mi.dy = placement.virtual_absolute[1];
    submit_inputs(&[input])
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
    (0x68, 0x64, false),
    (0x69, 0x65, false),
    (0x6a, 0x66, false),
    (0x6b, 0x67, false),
    (0x6c, 0x68, false),
    (0x6d, 0x69, false),
    (0x6e, 0x6a, false),
    (0x6f, 0x6b, false),
    (0x70, 0x6c, false),
    (0x71, 0x6d, false),
    (0x72, 0x6e, false),
    (0x73, 0x76, false),
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
fn captured_usage(vk: u16, scan: u16, extended: bool) -> Option<u16> {
    if vk == VK_PAUSE || vk == VK_SNAPSHOT {
        None
    } else if vk == VK_NUMLOCK {
        Some(0x53)
    } else {
        // Scan 0x76 is also documented for the Japanese LANG5 usage.
        from_scan(scan, extended).filter(|usage| *usage != 0x73 || vk == VK_F24)
    }
}

const STARTUP_VK: u16 = 0x4000;
#[cfg(test)]
fn physical_key(s: &mut HookState, vk: u16, scan: u16, extended: bool, down: bool) -> bool {
    let route = s.gate.snapshot();
    physical_key_routed(s, vk, scan, extended, down, route)
}
fn physical_key_routed(
    s: &mut HookState,
    vk: u16,
    scan: u16,
    extended: bool,
    down: bool,
    route: CaptureRoute,
) -> bool {
    let usage = captured_usage(vk, scan, extended);
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
            s.key_routed(usage, down, route)
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
fn sampled_buttons(mut down: impl FnMut(i32) -> bool) -> std::collections::BTreeSet<u8> {
    // Startup admission must include unsupported side buttons too. Observing a
    // hold does not add those buttons to the peer's three-button receive map.
    [
        (1, VK_LBUTTON),
        (2, VK_RBUTTON),
        (3, VK_MBUTTON),
        (4, VK_XBUTTON1),
        (5, VK_XBUTTON2),
    ]
    .into_iter()
    .filter(|(_, vk)| down(i32::from(*vk)))
    .map(|(button, _)| button)
    .collect()
}
unsafe fn initial_buttons() -> std::collections::BTreeSet<u8> {
    sampled_buttons(|vk| GetAsyncKeyState(vk) < 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_inputs_are_only_a_tagged_right_pair_or_one_right_up() {
        // Opaque test tags test field propagation, never consent/authentication.
        let down_tag = 0x1234usize;
        let up_tag = 0x5678usize;
        let pair = recovery_click_inputs(down_tag, up_tag);
        for (input, tag, flags) in [
            (&pair[0], down_tag, MOUSEEVENTF_RIGHTDOWN),
            (&pair[1], up_tag, MOUSEEVENTF_RIGHTUP),
        ] {
            assert_eq!(input.r#type, INPUT_MOUSE);
            // SAFETY: the pure constructor initializes INPUT_MOUSE's mi field.
            let mouse = unsafe { input.Anonymous.mi };
            assert_eq!(mouse.dwExtraInfo, tag);
            assert_eq!(mouse.dwFlags, flags);
            assert_eq!(
                (mouse.dx, mouse.dy, mouse.mouseData, mouse.time),
                (0, 0, 0, 0)
            );
            assert_eq!(mouse.dwFlags & MOUSEEVENTF_MOVE, 0);
        }
        let cleanup = recovery_right_input(up_tag, false);
        assert_eq!(cleanup.r#type, INPUT_MOUSE);
        // SAFETY: the same pure INPUT_MOUSE constructor initializes this union.
        let mouse = unsafe { cleanup.Anonymous.mi };
        assert_eq!(mouse.dwFlags, MOUSEEVENTF_RIGHTUP);
        assert_eq!(mouse.dwExtraInfo, up_tag);
        assert_eq!(
            (mouse.dx, mouse.dy, mouse.mouseData, mouse.time),
            (0, 0, 0, 0)
        );
    }

    #[test]
    fn current_recovery_decision_precedes_legacy_injected_origin_filtering() {
        for flags in [0, LLMHF_INJECTED] {
            for tag in [0, TAG] {
                assert_eq!(
                    mouse_origin(Some(true), flags, tag),
                    MouseOrigin::Recovery(true)
                );
                assert_eq!(
                    mouse_origin(Some(false), flags, tag),
                    MouseOrigin::Recovery(false)
                );
            }
        }
        assert_eq!(mouse_origin(None, LLMHF_INJECTED, 0), MouseOrigin::Injected);
        assert_eq!(mouse_origin(None, 0, TAG), MouseOrigin::Injected);
        assert_eq!(mouse_origin(None, 0, 0), MouseOrigin::Physical);
        assert_eq!(right_transition(WM_RBUTTONDOWN), Some(true));
        assert_eq!(right_transition(WM_RBUTTONUP), Some(false));
        for message in [
            WM_MOUSEMOVE,
            WM_LBUTTONDOWN,
            WM_MBUTTONUP,
            WM_MOUSEWHEEL,
            WM_NULL,
        ] {
            // The broker receives None and must block/fault a matching recovery
            // tag on any message other than the exact right-down/right-up pair.
            assert_eq!(right_transition(message), None);
        }
    }

    #[test]
    fn local_mouse_classification_preserves_right_pairs_and_all_other_buttons() {
        assert_eq!(
            local_mouse_event(WM_RBUTTONDOWN, 0),
            LocalMouseEvent::Right(true)
        );
        assert_eq!(
            local_mouse_event(WM_RBUTTONUP, 0),
            LocalMouseEvent::Right(false)
        );
        assert_eq!(local_mouse_event(WM_MOUSEMOVE, 0), LocalMouseEvent::Motion);
        for (down, up, button, data) in [
            (WM_LBUTTONDOWN, WM_LBUTTONUP, 1, 0),
            (WM_MBUTTONDOWN, WM_MBUTTONUP, 3, 0),
            (WM_XBUTTONDOWN, WM_XBUTTONUP, 4, u32::from(XBUTTON1) << 16),
            (WM_XBUTTONDOWN, WM_XBUTTONUP, 5, u32::from(XBUTTON2) << 16),
        ] {
            assert_eq!(
                local_mouse_event(down, data),
                LocalMouseEvent::Button(button, true)
            );
            assert_eq!(
                local_mouse_event(up, data),
                LocalMouseEvent::Button(button, false)
            );
        }
        for message in [
            WM_MOUSEWHEEL,
            WM_MOUSEHWHEEL,
            WM_NULL,
            WM_XBUTTONDOWN,
            WM_XBUTTONUP,
        ] {
            assert_eq!(local_mouse_event(message, 0), LocalMouseEvent::Other);
        }
        assert_eq!(
            local_mouse_event(WM_XBUTTONDOWN, 3 << 16),
            LocalMouseEvent::BothExtraButtons(true)
        );
        assert_eq!(
            local_mouse_event(WM_XBUTTONUP, 3 << 16),
            LocalMouseEvent::BothExtraButtons(false)
        );
        assert_eq!(
            local_mouse_event(WM_XBUTTONDOWN, 4 << 16),
            LocalMouseEvent::Other
        );
    }

    #[test]
    fn wheel_remainders_do_not_cross_route_or_epoch_boundaries() {
        let gate = CaptureGate::default();
        let local = gate.route().unwrap();
        let mut wheel = WheelState::default();
        assert_eq!(wheel.steps(local, 1, 60), 0);
        let remote = CaptureRoute::Remote(gate.publish(true).unwrap());
        assert_eq!(wheel.steps(remote, 1, 60), 0);
        assert_eq!(wheel.steps(remote, 1, 60), 1);
        assert_eq!(wheel.steps(remote, 0, 60), 0);
        gate.disable();
        let next = CaptureRoute::Remote(gate.publish(true).unwrap());
        assert_eq!(wheel.steps(next, 0, 60), 0);
        assert_eq!(wheel.steps(next, 0, 60), 1);
    }
    #[test]
    fn receiver_packets_keep_owned_tags_and_two_axis_submission_is_one_finite_batch() {
        let (inputs, count) = input_packets(Input::Scroll { x: 1, y: -2 }).unwrap();
        assert_eq!(count, 2);
        unsafe {
            assert_eq!(inputs[0].Anonymous.mi.dwFlags, MOUSEEVENTF_WHEEL);
            assert_eq!(inputs[1].Anonymous.mi.dwFlags, MOUSEEVENTF_HWHEEL);
            for input in inputs {
                assert_eq!(input.Anonymous.mi.dwExtraInfo, TAG);
            }
        }
        let (inputs, count) = input_packets(Input::Key {
            usage: 4,
            down: false,
        })
        .unwrap();
        assert_eq!(count, 1);
        unsafe {
            assert_eq!(inputs[0].Anonymous.ki.dwExtraInfo, TAG);
            assert_ne!(inputs[0].Anonymous.ki.dwFlags & KEYEVENTF_KEYUP, 0);
        }
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
    fn extended_function_keys_keep_scan_identity_and_balanced_release() {
        let scans = [
            0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6a, 0x6b, 0x6c, 0x6d, 0x6e, 0x76,
        ];
        assert_eq!(captured_usage(VK_F24, 0x76, false), Some(0x73));
        assert_eq!(captured_usage(VK_KANA, 0x76, false), None);
        for (usage, scan) in (0x68..=0x73).zip(scans) {
            assert_eq!(to_scan(usage), Some((scan, false)));
            assert_eq!(from_scan(scan, false), Some(usage));
            for down in [true, false] {
                let (packets, count) = input_packets(Input::Key { usage, down }).unwrap();
                assert_eq!(count, 1);
                // SAFETY: the test supplied a keyboard input.
                let key = unsafe { packets[0].Anonymous.ki };
                assert_eq!(key.wScan, scan);
                assert_eq!(key.dwFlags & KEYEVENTF_KEYUP != 0, !down);
            }
        }
    }

    #[test]
    fn startup_side_button_hold_blocks_receiving_until_its_local_up() {
        for (button, vk) in [(4, VK_XBUTTON1), (5, VK_XBUTTON2)] {
            let (tx, mut rx) = capture_events::channel(8).unwrap();
            let mut state = HookState {
                pairs: PairLedger::default(),
                stopping: false,
                toggle_route: None,
                local_gestures: None,
                tx,
                gate: CaptureGate::default(),
                keys: Default::default(),
                buttons: sampled_buttons(|observed| observed == i32::from(vk))
                    .into_iter()
                    .collect(),
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
                "side-button input was never forwarded"
            );
        }
    }
    #[test]
    fn startup_virtual_sample_repeat_is_not_a_fresh_switch_press() {
        let (tx, _rx) = capture_events::channel(16).unwrap();
        let mut s = HookState {
            pairs: PairLedger::default(),
            stopping: false,
            toggle_route: None,
            local_gestures: None,
            tx,
            gate: CaptureGate::default(),
            keys: [STARTUP_VK | VK_RIGHT, 0xe0, 0xe2].into_iter().collect(),
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
        let (tx, mut rx) = capture_events::channel(8).unwrap();
        let mut s = HookState {
            pairs: PairLedger::default(),
            stopping: false,
            toggle_route: None,
            local_gestures: None,
            tx,
            gate: CaptureGate::default(),
            keys: [STARTUP_VK | VK_RETURN].into_iter().collect(),
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
            Captured::Input {
                event: Input::Key {
                    usage: 0x58,
                    down: false
                },
                ..
            }
        ));
    }
}
