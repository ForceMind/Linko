//! Read-only preflight for a sealed short-click recovery; no injection here.
use super::Target;
use crate::context::ContextError;
use peerglide_actions::execution::NativeFailure;
use windows_sys::Win32::{
    Foundation::{HWND, POINT, RECT},
    Graphics::{
        Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS},
        Gdi::{GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST},
    },
    UI::{
        HiDpi::{
            GetDpiForWindow, SetThreadDpiAwarenessContext,
            DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        },
        Input::KeyboardAndMouse::IsWindowEnabled,
        WindowsAndMessaging::{
            GetAncestor, GetGUIThreadInfo, GetPhysicalCursorPos, GetSystemMetrics, IsWindow,
            WindowFromPhysicalPoint, GA_ROOT, GUITHREADINFO, GUI_INMENUMODE, GUI_INMOVESIZE,
            GUI_POPUPMENUMODE, GUI_SYSTEMMENUMODE, SM_SWAPBUTTON,
        },
    },
};

fn context_error(error: ContextError) -> NativeFailure {
    if error == ContextError::PermissionDenied {
        NativeFailure::PermissionLost
    } else {
        NativeFailure::TargetLost
    }
}
trait WindowOps {
    fn target(&mut self) -> Result<HWND, NativeFailure>;
    fn dpi(&mut self, window: HWND) -> u32;
    fn enabled(&mut self, window: HWND) -> bool;
    fn cursor(&mut self) -> Option<(i32, i32)>;
    fn root_at(&mut self, x: i32, y: i32) -> Option<HWND>;
    fn route_idle(&mut self) -> bool;
    fn swapped(&mut self) -> bool;
    fn fullscreen(&mut self, window: HWND) -> Result<bool, NativeFailure>;
}
struct Native<'a>(&'a Target);
impl WindowOps for Native<'_> {
    fn target(&mut self) -> Result<HWND, NativeFailure> {
        let target = self.0;
        if !target.matches_current().map_err(context_error)?
            || unsafe { IsWindow(target.window.foreground) } == 0
            || super::owner(target.window.foreground).map_err(context_error)?
                != (target.window.process_id, target.window.thread_id)
        {
            return Err(NativeFailure::TargetLost);
        }
        Ok(target.window.foreground)
    }
    fn dpi(&mut self, window: HWND) -> u32 {
        unsafe { GetDpiForWindow(window) }
    }
    fn enabled(&mut self, window: HWND) -> bool {
        unsafe { IsWindowEnabled(window) != 0 }
    }
    fn cursor(&mut self) -> Option<(i32, i32)> {
        let mut point = POINT { x: 0, y: 0 };
        if unsafe { GetPhysicalCursorPos(&mut point) } == 0 {
            None
        } else {
            Some((point.x, point.y))
        }
    }
    fn root_at(&mut self, x: i32, y: i32) -> Option<HWND> {
        let hit = unsafe { WindowFromPhysicalPoint(POINT { x, y }) };
        if hit.is_null() {
            return None;
        }
        let owner = super::owner(hit).ok()?;
        if owner != (self.0.window.process_id, self.0.window.thread_id) {
            return None;
        }
        let root = unsafe { GetAncestor(hit, GA_ROOT) };
        if root.is_null()
            || super::owner(root).ok()? != (self.0.window.process_id, self.0.window.thread_id)
        {
            None
        } else {
            Some(root)
        }
    }
    fn route_idle(&mut self) -> bool {
        let mut info: GUITHREADINFO = unsafe { std::mem::zeroed() };
        info.cbSize = std::mem::size_of::<GUITHREADINFO>() as u32;
        (unsafe { GetGUIThreadInfo(self.0.window.thread_id, &mut info) != 0 })
            && info.hwndCapture.is_null()
            && info.hwndMenuOwner.is_null()
            && info.hwndMoveSize.is_null()
            && info.flags
                & (GUI_INMENUMODE | GUI_INMOVESIZE | GUI_POPUPMENUMODE | GUI_SYSTEMMENUMODE)
                == 0
    }
    fn swapped(&mut self) -> bool {
        unsafe { GetSystemMetrics(SM_SWAPBUTTON) != 0 }
    }
    fn fullscreen(&mut self, window: HWND) -> Result<bool, NativeFailure> {
        // Only this worker's coordinate-query context changes, then is restored.
        // No display configuration, focus, permission or user setting changes.
        let old =
            unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        if old.is_null() {
            return Err(NativeFailure::BeforeSubmission);
        }
        let result = (|| {
            let mut bounds: RECT = unsafe { std::mem::zeroed() };
            if unsafe {
                DwmGetWindowAttribute(
                    window,
                    DWMWA_EXTENDED_FRAME_BOUNDS as u32,
                    (&mut bounds as *mut RECT).cast(),
                    std::mem::size_of::<RECT>() as u32,
                )
            } < 0
            {
                return Err(NativeFailure::BeforeSubmission);
            }
            let monitor = unsafe { MonitorFromWindow(window, MONITOR_DEFAULTTONEAREST) };
            if monitor.is_null() {
                return Err(NativeFailure::BeforeSubmission);
            }
            let mut info: MONITORINFO = unsafe { std::mem::zeroed() };
            info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
            if unsafe { GetMonitorInfoW(monitor, &mut info) } == 0 {
                return Err(NativeFailure::BeforeSubmission);
            }
            covers_monitor(bounds, info.rcMonitor)
        })();
        if unsafe { SetThreadDpiAwarenessContext(old) }.is_null() {
            return Err(NativeFailure::BeforeSubmission);
        }
        result
    }
}
fn covers_monitor(window: RECT, monitor: RECT) -> Result<bool, NativeFailure> {
    if window.left >= window.right
        || window.top >= window.bottom
        || monitor.left >= monitor.right
        || monitor.top >= monitor.bottom
    {
        return Err(NativeFailure::BeforeSubmission);
    }
    Ok(window.left <= monitor.left
        && window.top <= monitor.top
        && window.right >= monitor.right
        && window.bottom >= monitor.bottom)
}
fn read_dpi(ops: &mut impl WindowOps) -> Result<u32, NativeFailure> {
    let window = ops.target()?;
    let dpi = ops.dpi(window);
    if !ops.enabled(window)
        || ops.swapped()
        || !ops.route_idle()
        || ops.fullscreen(window)?
        || !(48..=768).contains(&dpi)
    {
        return Err(NativeFailure::BeforeSubmission);
    }
    if ops.target()? != window {
        return Err(NativeFailure::TargetLost);
    }
    Ok(dpi)
}
fn validate(ops: &mut impl WindowOps, x: i32, y: i32, dpi: u32) -> Result<(), NativeFailure> {
    let window = ops.target()?;
    if !(48..=768).contains(&dpi)
        || ops.dpi(window) != dpi
        || !ops.enabled(window)
        || ops.swapped()
        || !ops.route_idle()
        || ops.fullscreen(window)?
    {
        return Err(NativeFailure::BeforeSubmission);
    }
    if ops.cursor() != Some((x, y)) || ops.root_at(x, y) != Some(window) {
        return Err(NativeFailure::TargetLost);
    }
    if ops.target()? != window {
        return Err(NativeFailure::TargetLost);
    }
    Ok(())
}
pub(in crate::context) fn recovery_dpi(target: &Target) -> Result<u32, NativeFailure> {
    read_dpi(&mut Native(target))
}
pub(in crate::context) fn validate_recovery(
    target: &Target,
    x: i32,
    y: i32,
    dpi: u32,
) -> Result<(), NativeFailure> {
    validate(&mut Native(target), x, y, dpi)
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fake {
        current: HWND,
        next: Option<HWND>,
        dpi: u32,
        enabled: bool,
        cursor: Option<(i32, i32)>,
        hit: Option<HWND>,
        idle: bool,
        swapped: bool,
        calls: usize,
        fullscreen: Result<bool, NativeFailure>,
    }
    impl WindowOps for Fake {
        fn target(&mut self) -> Result<HWND, NativeFailure> {
            self.calls += 1;
            Ok(if self.calls > 1 {
                self.next.unwrap_or(self.current)
            } else {
                self.current
            })
        }
        fn dpi(&mut self, _: HWND) -> u32 {
            self.dpi
        }
        fn enabled(&mut self, _: HWND) -> bool {
            self.enabled
        }
        fn cursor(&mut self) -> Option<(i32, i32)> {
            self.cursor
        }
        fn root_at(&mut self, _: i32, _: i32) -> Option<HWND> {
            self.hit
        }
        fn route_idle(&mut self) -> bool {
            self.idle
        }
        fn swapped(&mut self) -> bool {
            self.swapped
        }
        fn fullscreen(&mut self, _: HWND) -> Result<bool, NativeFailure> {
            self.fullscreen
        }
    }
    // Opaque test-only handles never enter Native or authorize input.
    fn fake() -> Fake {
        let window = 1usize as HWND;
        Fake {
            current: window,
            next: None,
            dpi: 144,
            enabled: true,
            cursor: Some((4, 8)),
            hit: Some(window),
            idle: true,
            swapped: false,
            calls: 0,
            fullscreen: Ok(false),
        }
    }
    #[test]
    fn fullscreen_unknown_geometry_and_scope_changes_reject_capture_admission() {
        for fullscreen in [Ok(true), Err(NativeFailure::BeforeSubmission)] {
            let mut ops = fake();
            ops.fullscreen = fullscreen;
            assert_eq!(read_dpi(&mut ops), Err(NativeFailure::BeforeSubmission));
        }
        for kind in 0..3 {
            let mut ops = fake();
            match kind {
                0 => ops.enabled = false,
                1 => ops.idle = false,
                _ => ops.swapped = true,
            }
            assert_eq!(read_dpi(&mut ops), Err(NativeFailure::BeforeSubmission));
        }
        let monitor = RECT {
            left: -1920,
            top: 0,
            right: 0,
            bottom: 1080,
        };
        assert_eq!(
            covers_monitor(
                RECT {
                    left: -1920,
                    top: 0,
                    right: 0,
                    bottom: 1080
                },
                monitor
            ),
            Ok(true)
        );
        assert_eq!(
            covers_monitor(
                RECT {
                    left: -1900,
                    top: 0,
                    right: 0,
                    bottom: 1080
                },
                monitor
            ),
            Ok(false)
        );
        assert_eq!(
            covers_monitor(
                RECT {
                    left: 0,
                    top: 0,
                    right: 0,
                    bottom: 0
                },
                monitor
            ),
            Err(NativeFailure::BeforeSubmission)
        );
    }
    #[test]
    fn complete_preflight_accepts_only_original_point_and_dpi() {
        let mut ops = fake();
        assert_eq!(validate(&mut ops, 4, 8, 144), Ok(()));
        assert_eq!(read_dpi(&mut fake()), Ok(144));
    }
    #[test]
    fn changed_or_unavailable_position_and_hit_reject() {
        for kind in 0..4 {
            let mut ops = fake();
            match kind {
                0 => ops.cursor = None,
                1 => ops.cursor = Some((5, 8)),
                2 => ops.hit = None,
                _ => ops.hit = Some(2usize as HWND),
            }
            assert_eq!(
                validate(&mut ops, 4, 8, 144),
                Err(NativeFailure::TargetLost)
            );
        }
    }
    #[test]
    fn disabled_swapped_captured_menu_and_dpi_changes_reject() {
        for kind in 0..5 {
            let mut ops = fake();
            match kind {
                0 => ops.enabled = false,
                1 => ops.swapped = true,
                2 => ops.idle = false,
                3 => ops.dpi = 0,
                _ => ops.dpi = 192,
            }
            assert_eq!(
                validate(&mut ops, 4, 8, 144),
                Err(NativeFailure::BeforeSubmission)
            );
        }
    }
    #[test]
    fn late_target_change_and_invalid_dpi_never_become_fallback_target() {
        let mut ops = fake();
        ops.next = Some(2usize as HWND);
        assert_eq!(
            validate(&mut ops, 4, 8, 144),
            Err(NativeFailure::TargetLost)
        );
        for dpi in [0, 47, 769, u32::MAX] {
            let mut ops = fake();
            ops.dpi = dpi;
            assert_eq!(read_dpi(&mut ops), Err(NativeFailure::BeforeSubmission));
        }
    }
}
