//! Native adapters. Capture is disabled automatically when the network watchdog expires.
use anyhow::Result;
use peerglide_protocol::{Input, InputCapabilities};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, OnceLock,
};
use std::time::Instant;
use tokio::sync::mpsc;
pub mod context;
#[cfg(any(windows, target_os = "macos", test))]
mod capture_thread;
#[cfg(any(windows, target_os = "macos"))]
use capture_thread::CaptureThread;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;
#[cfg(target_os = "macos")]
use macos as native;
#[cfg(windows)]
use windows as native;

pub fn now_ms() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_millis() as u64
}
#[derive(Clone, Copy)]
pub enum Captured {
    Position {
        x: f64,
        y: f64,
        width: f64,
        height: f64,
    },
    Motion {
        dx: f64,
        dy: f64,
    },
    Input(Input),
    Emergency,
    UnsupportedInput,
    Toggle,
}
#[derive(Default)]
struct GateState {
    deadline: AtomicU64,
    control: AtomicU64,
}
#[derive(Clone, Default)]
pub struct CaptureGate(Arc<GateState>);
const ACTIVE: u64 = 1 << 63;
const IN_CALLBACK: u64 = 1 << 62;
const COUNT: u64 = !(ACTIVE | IN_CALLBACK);
impl CaptureGate {
    /// Never enable suppression while a callback or physically held input straddles the boundary.
    pub fn try_activate(&self) -> bool {
        self.0.deadline.store(now_ms() + 1000, Ordering::Release);
        self.0
            .control
            .compare_exchange(0, ACTIVE, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
    pub fn refresh(&self) {
        if self.active() {
            self.0.deadline.store(now_ms() + 1000, Ordering::Release);
        }
    }
    pub fn disable(&self) {
        self.0.control.fetch_and(!ACTIVE, Ordering::AcqRel);
        self.0.deadline.store(0, Ordering::Release);
    }
    pub fn active(&self) -> bool {
        if self.0.deadline.load(Ordering::Acquire) <= now_ms() {
            self.0.control.fetch_and(!ACTIVE, Ordering::AcqRel);
            return false;
        }
        self.0.control.load(Ordering::Acquire) & ACTIVE != 0
    }
    pub fn idle(&self) -> bool {
        self.0.control.load(Ordering::Acquire) & COUNT == 0
    }
    #[cfg(any(windows, target_os = "macos", test))]
    fn begin_event(&self) -> CallbackGuard {
        self.0.control.fetch_or(IN_CALLBACK, Ordering::AcqRel);
        CallbackGuard(self.clone())
    }
    #[cfg(any(windows, target_os = "macos", test))]
    fn set_held(&self, count: u64) {
        let mut old = self.0.control.load(Ordering::Acquire);
        loop {
            let new = (old & !COUNT) | (count & COUNT);
            match self.0.control.compare_exchange_weak(
                old,
                new,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(current) => old = current,
            }
        }
    }
}
#[cfg(any(windows, target_os = "macos", test))]
struct CallbackGuard(CaptureGate);
#[cfg(any(windows, target_os = "macos", test))]
impl Drop for CallbackGuard {
    fn drop(&mut self) {
        self.0 .0.control.fetch_and(!IN_CALLBACK, Ordering::AcqRel);
    }
}
impl Drop for CaptureSession {
    fn drop(&mut self) {
        if self.shutdown().is_err() {
            eprintln!("Native capture shutdown incomplete; new capture remains blocked until the old thread exits.");
        }
    }
}
pub struct CaptureSession {
    pub gate: CaptureGate,
    #[cfg(any(windows, target_os = "macos"))]
    _thread: CaptureThread,
    return_position: std::sync::Mutex<Option<(f64, f64)>>,
}
impl CaptureSession {
    pub fn shutdown(&mut self) -> Result<()> {
        self.restore();
        #[cfg(any(windows, target_os = "macos"))]
        anyhow::ensure!(
            self._thread.shutdown(std::time::Duration::from_millis(500)),
            "native capture shutdown timed out or panicked"
        );
        Ok(())
    }
    pub fn begin(&self, return_x: f64, return_y: f64) -> Result<bool> {
        if !self.gate.try_activate() {
            return Ok(false);
        }
        *self
            .return_position
            .lock()
            .map_err(|_| anyhow::anyhow!("capture position lock poisoned"))? =
            Some((return_x, return_y));
        // Windows clips low-level hook coordinates at display boundaries. Park
        // the physical cursor inside the screen so blocked moves retain deltas.
        pointer(0.5, 0.5)?;
        Ok(true)
    }
    pub fn restore(&self) {
        self.gate.disable();
        if let Ok(mut position) = self.return_position.lock() {
            if let Some((x, y)) = position.take() {
                let _ = pointer(x, y);
            }
        }
    }
}
/// Compiled backend support, independent of OS permissions and peer grants.
pub fn input_capabilities() -> InputCapabilities {
    #[cfg(any(windows, target_os = "macos"))]
    {
        InputCapabilities::new(&native::key_usages(), true, 7, true)
            .expect("static backend capabilities")
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        InputCapabilities::default()
    }
}
pub fn capture(tx: mpsc::Sender<Captured>, target: InputCapabilities) -> Result<CaptureSession> {
    let gate = CaptureGate::default();
    #[cfg(any(windows, target_os = "macos"))]
    {
        let thread = native::capture(tx, gate.clone(), target)?;
        Ok(CaptureSession {
            gate,
            _thread: thread,
            return_position: std::sync::Mutex::new(None),
        })
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = (tx, gate, target);
        anyhow::bail!("native input currently requires Windows or macOS; use probe on Linux")
    }
}
pub fn inject(event: Input) -> Result<()> {
    #[cfg(any(windows, target_os = "macos"))]
    {
        native::inject(event)
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = event;
        anyhow::bail!("native injection unavailable")
    }
}
pub fn pointer(x: f64, y: f64) -> Result<()> {
    #[cfg(any(windows, target_os = "macos"))]
    {
        native::pointer(x, y)
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = (x, y);
        anyhow::bail!("native injection unavailable")
    }
}
#[cfg(any(windows, target_os = "macos"))]
struct HookState {
    tx: mpsc::Sender<Captured>,
    gate: CaptureGate,
    keys: std::collections::BTreeSet<u16>,
    buttons: std::collections::BTreeSet<u8>,
    toggle_armed: bool,
    target: InputCapabilities,
}
#[cfg(any(windows, target_os = "macos"))]
impl HookState {
    fn button(&mut self, button: u8, down: bool) {
        if down {
            self.buttons.insert(button);
        } else {
            self.buttons.remove(&button);
        }
        self.update_held();
        if self.reject_target(Input::Button { button, down }) {
            return;
        }
        self.emit(Captured::Input(Input::Button { button, down }));
    }
    fn reject_target(&mut self, event: Input) -> bool {
        if self.gate.active() && !self.target.supports(event) {
            self.gate.disable();
            self.toggle_armed = false;
            self.emit(Captured::UnsupportedInput);
            true
        } else {
            false
        }
    }
    fn scroll(&mut self, x: i16, y: i16) {
        if !self.reject_target(Input::Scroll { x, y }) {
            self.emit(Captured::Input(Input::Scroll { x, y }));
        }
    }
    fn update_held(&self) {
        self.gate
            .set_held((self.keys.len() + self.buttons.len()) as u64);
    }
    fn emit(&self, e: Captured) {
        if self.tx.try_send(e).is_err() {
            self.gate.disable();
        }
    }
    // Unsupported physical keys remain local and cancel control. Track their
    // held state in a disjoint namespace so they cannot straddle acquisition.
    fn unsupported_key(&mut self, code: u16, down: bool) -> bool {
        let token = 0x8000 | (code & 0x7fff);
        if down {
            self.keys.insert(token);
        } else {
            self.keys.remove(&token);
        }
        self.update_held();
        self.gate.disable();
        self.toggle_armed = false;
        self.emit(Captured::UnsupportedInput);
        false
    }
    fn key(&mut self, usage: u16, down: bool) -> bool {
        let fresh_down = if down {
            self.keys.insert(usage)
        } else {
            self.keys.remove(&usage);
            false
        };
        self.update_held();
        let ctrl = self.keys.contains(&0xe0) || self.keys.contains(&0xe4);
        let alt = self.keys.contains(&0xe2) || self.keys.contains(&0xe6);
        if ctrl && alt && down && usage == 0x29 {
            self.gate.disable();
            self.toggle_armed = false;
            self.emit(Captured::Emergency);
            return false;
        }
        if ctrl && alt && fresh_down && usage == 0x4f && self.target.motion() {
            self.toggle_armed = true;
        }
        let swallow_toggle = self.toggle_armed && usage == 0x4f;
        if !swallow_toggle {
            if self.reject_target(Input::Key { usage, down }) {
                return false;
            }
            self.emit(Captured::Input(Input::Key { usage, down }));
        }
        if self.toggle_armed && self.keys.is_empty() {
            self.toggle_armed = false;
            self.emit(Captured::Toggle);
        }
        swallow_toggle || self.gate.active()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compiled_backend_capabilities_are_valid_and_do_not_claim_linux_input() {
        let caps = input_capabilities();
        assert!(caps.valid());
        assert_eq!(caps.motion(), cfg!(any(windows, target_os = "macos")));
        #[cfg(any(windows, target_os = "macos"))]
        for usage in native::key_usages() {
            assert!(caps.supports(Input::Key { usage, down: true }));
            assert!(caps.supports(Input::Key { usage, down: false }));
        }
    }
    #[test]
    fn watchdog_is_fail_open_and_explicitly_releasable() {
        let gate = CaptureGate::default();
        assert!(!gate.active());
        assert!(gate.try_activate());
        assert!(gate.active());
        gate.disable();
        assert!(!gate.active());
        gate.0.deadline.store(now_ms(), Ordering::Release);
        assert!(!gate.active());
        gate.set_held(1);
        assert!(!gate.idle());
    }
}

#[cfg(all(test, any(windows, target_os = "macos")))]
mod hook_tests {
    use super::*;
    fn state(capacity: usize) -> (HookState, mpsc::Receiver<Captured>) {
        let (tx, rx) = mpsc::channel(capacity);
        (
            HookState {
                tx,
                gate: CaptureGate::default(),
                keys: Default::default(),
                buttons: Default::default(),
                toggle_armed: false,
                target: InputCapabilities::new(
                    &(4..=0x73).chain(0xe0..=0xe7).collect::<Vec<_>>(),
                    true,
                    7,
                    true,
                )
                .unwrap(),
            },
            rx,
        )
    }
    #[test]
    fn target_key_rejection_is_local_and_does_not_swallow_the_press() {
        let (mut s, mut rx) = state(20);
        s.target = InputCapabilities::new(&[], true, 7, true).unwrap();
        assert!(s.gate.try_activate());
        assert!(!s.key(4, true));
        assert!(!s.gate.active());
        assert!(matches!(rx.try_recv().unwrap(), Captured::UnsupportedInput));
        assert!(!s.gate.try_activate());
        assert!(!s.key(4, false));
        assert!(s.gate.idle());
    }
    #[test]
    fn target_button_and_scroll_rejections_fail_open() {
        let (mut s, mut rx) = state(20);
        s.target = InputCapabilities::new(&[], true, 0, false).unwrap();
        assert!(s.gate.try_activate());
        s.scroll(0, 1);
        assert!(!s.gate.active());
        assert!(matches!(rx.try_recv().unwrap(), Captured::UnsupportedInput));
        assert!(s.gate.try_activate());
        s.button(1, true);
        assert!(!s.gate.active());
        assert!(matches!(rx.try_recv().unwrap(), Captured::UnsupportedInput));
        s.button(1, false);
        assert!(s.gate.idle());
    }
    #[test]
    fn unavailable_target_does_not_reserve_the_switch_arrow() {
        let (mut s, mut rx) = state(20);
        s.target = InputCapabilities::default();
        for (usage, down) in [
            (0xe0, true),
            (0xe2, true),
            (0x4f, true),
            (0x4f, false),
            (0xe0, false),
            (0xe2, false),
        ] {
            assert!(!s.key(usage, down));
        }
        while let Ok(event) = rx.try_recv() {
            assert!(!matches!(event, Captured::Toggle));
        }
    }
    #[test]
    fn held_switch_chord_cannot_resume_control_on_autorepeat() {
        let (mut s, mut rx) = state(20);
        s.keys.extend([0xe0, 0xe2, 0x4f]);
        s.update_held();
        assert!(!s.key(0x4f, true));
        for usage in [0x4f, 0xe0, 0xe2] {
            s.key(usage, false);
        }
        while let Ok(event) = rx.try_recv() {
            assert!(!matches!(event, Captured::Toggle));
        }
        assert!(s.gate.idle());
    }
    #[test]
    fn hotkey_waits_for_release_and_never_forwards_arrow() {
        let (mut s, mut rx) = state(20);
        s.key(0xe0, true);
        s.key(0xe2, true);
        assert!(s.key(0x4f, true));
        assert!(!s.gate.idle());
        assert!(s.key(0x4f, false));
        s.key(0xe0, false);
        s.key(0xe2, false);
        assert!(s.gate.idle());
        let mut toggles = 0;
        while let Ok(e) = rx.try_recv() {
            match e {
                Captured::Toggle => toggles += 1,
                Captured::Input(Input::Key { usage: 0x4f, .. }) => panic!("switch arrow leaked"),
                _ => (),
            }
        }
        assert_eq!(toggles, 1);
    }
    #[test]
    fn unsupported_key_cancels_control_and_blocks_acquisition_until_release() {
        let (mut s, mut rx) = state(20);
        assert!(s.gate.try_activate());
        assert!(!s.unsupported_key(0x100, true));
        assert!(!s.gate.active());
        assert!(!s.gate.try_activate());
        assert!(!s.unsupported_key(0x100, true)); // autorepeat is not a second hold
        assert!(!s.unsupported_key(0x100, false));
        assert!(s.gate.try_activate());
        while let Ok(event) = rx.try_recv() {
            assert!(matches!(event, Captured::UnsupportedInput));
        }
    }
    #[test]
    fn unsupported_key_cancels_an_armed_switch_and_fails_open_when_queue_full() {
        let (mut s, _rx) = state(1);
        s.toggle_armed = true;
        assert!(s.gate.try_activate());
        s.unsupported_key(0x100, true);
        s.unsupported_key(0x100, false);
        assert!(!s.toggle_armed);
        assert!(!s.gate.active());
        assert!(s.gate.idle());
    }
    #[test]
    fn queue_overflow_disables_capture() {
        let (s, _rx) = state(1);
        assert!(s.gate.try_activate());
        s.emit(Captured::Emergency);
        s.emit(Captured::Emergency);
        assert!(!s.gate.active());
    }
}

#[cfg(test)]
mod handoff_tests {
    use super::*;
    #[test]
    fn activation_cannot_split_a_local_callback_or_held_key() {
        let gate = CaptureGate::default();
        {
            let _callback = gate.begin_event();
            assert!(!gate.try_activate());
            gate.set_held(1);
        }
        assert!(!gate.try_activate());
        {
            let _callback = gate.begin_event();
            gate.set_held(0);
            assert!(!gate.try_activate());
        }
        assert!(gate.try_activate());
        gate.0.deadline.store(now_ms(), Ordering::Release);
        assert!(!gate.active());
        gate.refresh();
        assert!(!gate.active());
    }
}
