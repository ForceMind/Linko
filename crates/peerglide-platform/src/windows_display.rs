//! Explicit monitor enumeration outside the keyboard and mouse hooks.
use crate::display_layout::{Bounds, Display, DisplayId, Layout, Rotation};
use anyhow::{ensure, Context, Result};
use std::{
    mem::size_of,
    ptr::{null, null_mut},
};
use windows_sys::{
    core::BOOL,
    Win32::{
        Foundation::{LPARAM, RECT, S_OK},
        Graphics::Gdi::{
            EnumDisplayMonitors, EnumDisplaySettingsW, GetMonitorInfoW, DEVMODEW, DMDO_180,
            DMDO_270, DMDO_90, DMDO_DEFAULT, DM_DISPLAYORIENTATION, ENUM_CURRENT_SETTINGS, HDC,
            HMONITOR, MONITORINFO, MONITORINFOEXW,
        },
        UI::{
            HiDpi::{
                SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT,
                DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
            },
            Shell::GetScaleFactorForMonitor,
            WindowsAndMessaging::MONITORINFOF_PRIMARY,
        },
    },
};

const MAX_DISPLAYS: usize = 64;

struct PhysicalContext(DPI_AWARENESS_CONTEXT);

impl PhysicalContext {
    fn enter() -> Result<Self> {
        // SAFETY: the predefined context changes only this calling thread.
        let previous =
            unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        ensure!(!previous.is_null(), "Windows display DPI context unavailable");
        Ok(Self(previous))
    }
}

impl Drop for PhysicalContext {
    fn drop(&mut self) {
        // SAFETY: the OS supplied this context on the same thread in enter().
        unsafe { SetThreadDpiAwarenessContext(self.0) };
    }
}

struct MonitorList {
    handles: [HMONITOR; MAX_DISPLAYS],
    len: usize,
}

unsafe extern "system" fn collect_monitor(
    monitor: HMONITOR,
    _: HDC,
    _: *mut RECT,
    state: LPARAM,
) -> BOOL {
    // SAFETY: EnumDisplayMonitors invokes the callback synchronously with the
    // live list supplied by monitor_handles(). No other access aliases it.
    let list = unsafe { &mut *(state as *mut MonitorList) };
    if let Some(slot) = list.handles.get_mut(list.len) {
        *slot = monitor;
        list.len += 1;
        1
    } else {
        0
    }
}

fn monitor_handles() -> Result<Vec<HMONITOR>> {
    let mut list = MonitorList {
        handles: [null_mut(); MAX_DISPLAYS],
        len: 0,
    };
    // SAFETY: no clip/DC, and the callback cannot outlive this call.
    let result = unsafe {
        EnumDisplayMonitors(
            null_mut(),
            null(),
            Some(collect_monitor),
            (&mut list as *mut MonitorList) as LPARAM,
        )
    };
    ensure!(result != 0 && list.len != 0, "Windows monitor enumeration unavailable");
    let mut handles = list.handles[..list.len].to_vec();
    handles.sort_unstable_by_key(|handle| *handle as usize);
    Ok(handles)
}

fn display_metrics(
    id: DisplayId,
    rect: RECT,
    scale_percent: i32,
    orientation: u32,
) -> Result<Display> {
    // DEVMODE describes counterclockwise orientation.
    let rotation = match orientation {
        DMDO_DEFAULT => Rotation::Upright,
        DMDO_90 => Rotation::Clockwise270,
        DMDO_180 => Rotation::Clockwise180,
        DMDO_270 => Rotation::Clockwise90,
        _ => anyhow::bail!("unsupported Windows display orientation"),
    };
    ensure!(scale_percent > 0, "invalid Windows monitor scale");
    Ok(Display {
        id,
        bounds: Bounds {
            left: f64::from(rect.left),
            top: f64::from(rect.top),
            right: f64::from(rect.right),
            bottom: f64::from(rect.bottom),
        },
        scale: f64::from(scale_percent) / 100.0,
        rotation,
    })
}

fn read_monitor(handle: HMONITOR) -> Result<(Display, bool)> {
    let mut info = MONITORINFOEXW {
        monitorInfo: MONITORINFO {
            cbSize: size_of::<MONITORINFOEXW>() as u32,
            ..MONITORINFO::default()
        },
        ..MONITORINFOEXW::default()
    };
    // SAFETY: cbSize advertises the live larger structure. The pointer comes
    // from the whole allocation because Win32 also writes the device name.
    let read = unsafe { GetMonitorInfoW(handle, (&raw mut info).cast()) };
    ensure!(read != 0, "Windows monitor disappeared during enumeration");
    ensure!(
        info.szDevice[0] != 0 && info.szDevice.contains(&0),
        "invalid Windows monitor device name"
    );
    let mut mode = DEVMODEW {
        dmSize: size_of::<DEVMODEW>() as u16,
        ..DEVMODEW::default()
    };
    // SAFETY: szDevice is NUL-terminated and the mode has the documented size.
    let read = unsafe {
        EnumDisplaySettingsW(info.szDevice.as_ptr(), ENUM_CURRENT_SETTINGS, &mut mode)
    };
    ensure!(read != 0, "Windows monitor mode unavailable");
    let orientation = if mode.dmFields & DM_DISPLAYORIENTATION != 0 {
        // SAFETY: this mode came from the display API with the field flag set.
        unsafe { mode.Anonymous1.Anonymous2.dmDisplayOrientation }
    } else {
        DMDO_DEFAULT
    };
    let mut scale_percent = 0;
    // SAFETY: handle is from this snapshot and the result storage is writable.
    let result = unsafe { GetScaleFactorForMonitor(handle, &mut scale_percent) };
    ensure!(result == S_OK, "Windows monitor scale unavailable");
    // HMONITOR is a local opaque handle. Preserve the full value and keep it
    // inside the process instead of sending or persisting it as identity.
    let display = display_metrics(
        DisplayId(handle as usize as u64),
        info.monitorInfo.rcMonitor,
        scale_percent,
        orientation,
    )?;
    Ok((display, info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0))
}

pub(super) fn read_display_layout() -> Result<Layout> {
    let _context = PhysicalContext::enter()?;
    let handles = monitor_handles()?;
    let mut displays = Vec::with_capacity(handles.len());
    let mut primary = None;
    for handle in &handles {
        let (display, is_primary) = read_monitor(*handle)?;
        if is_primary {
            ensure!(primary.replace(display.id).is_none(), "multiple Windows primary displays");
        }
        displays.push(display);
    }
    ensure!(handles == monitor_handles()?, "Windows display set changed during enumeration");
    Layout::new(primary.context("Windows primary display unavailable")?, displays)
        .map_err(|error| anyhow::anyhow!("invalid Windows layout: {error:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_scale_does_not_rescale_oriented_virtual_desktop_rect() -> Result<()> {
        let rect = RECT {
            left: -1920,
            top: 20,
            right: 0,
            bottom: 1100,
        };
        let display = display_metrics(DisplayId(7), rect, 175, DMDO_270)?;
        assert!((display.bounds.left + 1920.0).abs() < f64::EPSILON);
        assert!((display.bounds.bottom - 1100.0).abs() < f64::EPSILON);
        assert!((display.scale - 1.75).abs() < f64::EPSILON);
        assert_eq!(display.rotation, Rotation::Clockwise90);
        assert!(display_metrics(DisplayId(7), rect, 0, DMDO_DEFAULT).is_err());
        assert!(display_metrics(DisplayId(7), rect, 100, u32::MAX).is_err());
        Ok(())
    }
}
