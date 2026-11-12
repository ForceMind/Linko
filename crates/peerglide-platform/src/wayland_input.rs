//! A portal-authorized EIS sender for one local Wayland input lease.
mod ffi;
mod keyboard;

use crate::{capture_thread::Reservation, ReceivePoint};
use anyhow::{ensure, Context as _, Result};
use ffi::{Device, Ei, EiApi, Oeffis, Ping, PortalApi};
use keyboard::Keys;
use peerglide_protocol::{Input, InputCapabilities};
use std::{
    ffi::{c_char, c_int, c_void},
    os::fd::{FromRawFd, IntoRawFd, OwnedFd},
    ptr::{null, null_mut, NonNull},
    time::Duration,
};

const INTERVAL: Duration = Duration::from_millis(5);
const PORTAL_WAIT: Duration = Duration::from_secs(60);
const DEVICE_WAIT: Duration = Duration::from_secs(3);
const FENCE_WAIT: Duration = Duration::from_millis(500);
const MAX_EVENTS: usize = 128;
const MAX_DEVICES: usize = 8;
const MAX_REGIONS: usize = 32;
const BUTTONS: [u32; 3] = [0x110, 0x111, 0x112]; // Linux BTN_LEFT, BTN_RIGHT, BTN_MIDDLE.

#[derive(Clone, Copy)]
pub struct Options {
    pub buttons: bool,
    pub scroll: bool,
    pub keyboard: bool,
    pub region: Option<(u32, u32)>,
}

struct Portal {
    ptr: NonNull<Oeffis>,
    api: PortalApi,
}

impl Portal {
    async fn open(keyboard: bool) -> Result<(Self, OwnedFd)> {
        let api = PortalApi::load()?;
        // SAFETY: the ABI creates an independent context with no caller data.
        let ptr = NonNull::new(unsafe { (api.oeffis_new)(null_mut()) })
            .context("could not create the RemoteDesktop portal owner")?;
        let portal = Self { ptr, api };
        let devices = ffi::PORTAL_POINTER | if keyboard { ffi::PORTAL_KEYBOARD } else { 0 };
        // SAFETY: request only explicitly selected devices on this live context.
        unsafe { (portal.api.oeffis_create_session)(portal.ptr.as_ptr(), devices) };
        let fd = tokio::time::timeout(PORTAL_WAIT, async {
            loop {
                if portal.event()? == ffi::PORTAL_CONNECTED {
                    // SAFETY: ConnectedToEIS provides a new, owned duplicate.
                    let fd = unsafe { (portal.api.oeffis_get_eis_fd)(portal.ptr.as_ptr()) };
                    ensure!(fd >= 0, "RemoteDesktop did not provide an EIS connection");
                    // SAFETY: this duplicate has not been transferred or closed.
                    return Ok::<_, anyhow::Error>(unsafe { OwnedFd::from_raw_fd(fd) });
                }
                tokio::time::sleep(INTERVAL).await;
            }
        })
        .await
        .context("RemoteDesktop approval timed out")??;
        Ok((portal, fd))
    }

    fn event(&self) -> Result<c_int> {
        // SAFETY: the portal owns this descriptor until its context is dropped.
        let fd = unsafe { (self.api.oeffis_get_fd)(self.ptr.as_ptr()) };
        if readable(fd)? {
            // SAFETY: the live descriptor signaled pending portal traffic.
            unsafe { (self.api.oeffis_dispatch)(self.ptr.as_ptr()) };
        }
        // SAFETY: event retrieval is local to the live portal context.
        let event = unsafe { (self.api.oeffis_get_event)(self.ptr.as_ptr()) };
        ensure!(
            matches!(event, 0 | ffi::PORTAL_CONNECTED),
            "RemoteDesktop permission was denied or its session ended"
        );
        Ok(event)
    }
}

impl Drop for Portal {
    fn drop(&mut self) {
        // SAFETY: release the original reference before the library unloads.
        unsafe { (self.api.oeffis_unref)(self.ptr.as_ptr()) };
    }
}

struct Context {
    ptr: NonNull<Ei>,
    api: EiApi,
}

impl Context {
    fn open(api: EiApi, fd: OwnedFd) -> Result<Self> {
        // SAFETY: the ABI creates a sender context owned by this worker.
        let ptr = NonNull::new(unsafe { (api.ei_new_sender)(null_mut()) })
            .context("could not create the EIS sender")?;
        let owner = Self { ptr, api };
        // SAFETY: configure the fresh context and transfer the owned descriptor.
        let result = unsafe {
            (owner.api.ei_log_set_handler)(ptr.as_ptr(), quiet_log);
            (owner.api.ei_configure_name)(ptr.as_ptr(), c"Linko".as_ptr());
            (owner.api.ei_setup_backend_fd)(ptr.as_ptr(), fd.into_raw_fd())
        };
        ensure!(result == 0, "the EIS sender could not start");
        Ok(owner)
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        // SAFETY: the context also closes the backend descriptor it owns.
        unsafe { (self.api.ei_unref)(self.ptr.as_ptr()) };
    }
}

unsafe extern "C" fn quiet_log(_: *mut Ei, _: c_int, _: *const c_char, _: *mut c_void) {}

fn readable(fd: c_int) -> Result<bool> {
    ensure!(fd >= 0, "the Wayland input connection has no descriptor");
    let mut descriptor = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: poll borrows one initialized record and does not retain it.
    let result = unsafe { libc::poll(&mut descriptor, 1, 0) };
    if result < 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::Interrupted {
            return Ok(false);
        }
        return Err(error).context("could not check Wayland input readiness");
    }
    ensure!(
        descriptor.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) == 0,
        "the Wayland input connection ended"
    );
    Ok(descriptor.revents & libc::POLLIN != 0)
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Region {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

impl Region {
    fn position(self, point: ReceivePoint) -> Result<(f64, f64)> {
        ensure!(
            point.valid()
                && self.width > 0
                && self.height > 0
                && self.x.checked_add(self.width).is_some()
                && self.y.checked_add(self.height).is_some(),
            "invalid Wayland pointer region or position"
        );
        Ok((
            f64::from(self.x) + f64::from(self.width - 1) * point.x,
            f64::from(self.y) + f64::from(self.height - 1) * point.y,
        ))
    }
}

#[derive(Clone, Copy)]
struct Selection {
    pointer: usize,
    region: Region,
    buttons: Option<usize>,
    scroll: Option<usize>,
    keyboard: Option<usize>,
}

impl Selection {
    fn devices(self) -> impl Iterator<Item = usize> {
        let buttons = self.buttons.filter(|&device| device != self.pointer);
        let scroll = self
            .scroll
            .filter(|&device| device != self.pointer && Some(device) != buttons);
        let keyboard = self.keyboard.filter(|&device| {
            device != self.pointer && Some(device) != buttons && Some(device) != scroll
        });
        [Some(self.pointer), buttons, scroll, keyboard]
            .into_iter()
            .flatten()
    }
}

struct ReadyDevice {
    ptr: NonNull<Device>,
    regions: Vec<Region>,
    buttons: bool,
    scroll: bool,
    keyboard: bool,
}

fn select(devices: &[ReadyDevice], options: Options) -> Result<Option<Selection>> {
    let mut regions = devices
        .iter()
        .enumerate()
        .flat_map(|(device, info)| {
            info.regions
                .iter()
                .copied()
                .map(move |region| (device, region))
        })
        .filter(|(_, region)| {
            options
                .region
                .is_none_or(|origin| origin == (region.x, region.y))
        });
    let Some((pointer, region)) = regions.next() else {
        return Ok(None);
    };
    ensure!(
        regions.next().is_none(),
        "choose a unique Wayland region with --region X,Y"
    );
    region.position(ReceivePoint { x: 0.0, y: 0.0 })?;
    let unique = |preferred: bool, supports: fn(&ReadyDevice) -> bool| -> Result<Option<usize>> {
        if !preferred {
            return Ok(None);
        }
        if supports(&devices[pointer]) {
            return Ok(Some(pointer));
        }
        let mut matches = devices
            .iter()
            .enumerate()
            .filter(|(_, info)| supports(info));
        let first = matches.next().map(|(index, _)| index);
        ensure!(
            matches.next().is_none(),
            "Wayland input device capability is ambiguous"
        );
        Ok(first)
    };
    let buttons = unique(options.buttons, |device| device.buttons)?;
    let scroll = unique(options.scroll, |device| device.scroll)?;
    let keyboard = unique(options.keyboard, |device| device.keyboard)?;
    if (options.buttons && buttons.is_none())
        || (options.scroll && scroll.is_none())
        || (options.keyboard && keyboard.is_none())
    {
        return Ok(None);
    }
    Ok(Some(Selection {
        pointer,
        region,
        buttons,
        scroll,
        keyboard,
    }))
}

struct Fence {
    ptr: NonNull<Ping>,
    unref: unsafe extern "C" fn(*mut Ping) -> *mut Ping,
}

impl Drop for Fence {
    fn drop(&mut self) {
        // SAFETY: this local reference never outlives its live EIS context.
        unsafe { (self.unref)(self.ptr.as_ptr()) };
    }
}

pub struct WaylandInput {
    ei: Context,
    portal: Portal,
    _reservation: Reservation,
    options: Options,
    seat_seen: bool,
    devices: Vec<ReadyDevice>,
    selected: Option<Selection>,
    offered: InputCapabilities,
    failed: bool,
    active: bool,
    owned: [bool; 3],
    keys: Keys,
    scrolling: [bool; 2],
}

impl WaylandInput {
    pub async fn open(options: Options) -> Result<Self> {
        ensure!(
            std::env::var_os("WAYLAND_DISPLAY").is_some()
                || std::env::var("XDG_SESSION_TYPE").ok().as_deref() == Some("wayland"),
            "Wayland input requires a local Wayland session"
        );
        let reservation = Reservation::acquire()?;
        let api = EiApi::load()?;
        let (portal, fd) = Portal::open(options.keyboard).await?;
        let mut owner = Self {
            ei: Context::open(api, fd)?,
            portal,
            _reservation: reservation,
            options,
            seat_seen: false,
            devices: Vec::new(),
            selected: None,
            offered: InputCapabilities::default(),
            failed: false,
            active: false,
            owned: [false; 3],
            keys: Keys::default(),
            scrolling: [false; 2],
        };
        tokio::time::timeout(DEVICE_WAIT, async {
            loop {
                owner.check()?;
                if let Some(selection) = select(&owner.devices, options)? {
                    let keys = if selection.keyboard.is_some() {
                        keyboard::usages().collect::<Vec<_>>()
                    } else {
                        Vec::new()
                    };
                    owner.offered = InputCapabilities::new(
                        &keys,
                        true,
                        if selection.buttons.is_some() { 7 } else { 0 },
                        selection.scroll.is_some(),
                    )
                    .map_err(anyhow::Error::msg)?;
                    owner.selected = Some(selection);
                    return Ok::<_, anyhow::Error>(());
                }
                tokio::time::sleep(INTERVAL).await;
            }
        })
        .await
        .context(
            "the compositor did not grant the requested Wayland input devices or region",
        )??;
        owner.confirm().await?;
        Ok(owner)
    }

    pub fn capabilities(&self) -> InputCapabilities {
        self.offered.clone()
    }

    fn selection(&self) -> Result<Selection> {
        self.selected
            .context("the compositor has not selected a pointer")
    }

    pub fn check(&mut self) -> Result<()> {
        self.pump(None).map(|_| ())
    }

    fn pump(&mut self, fence: Option<NonNull<Ping>>) -> Result<bool> {
        ensure!(
            !self.failed,
            "the Wayland input owner is no longer available"
        );
        let result = self.pump_events(fence);
        self.failed = result.is_err();
        result
    }

    fn pump_events(&mut self, fence: Option<NonNull<Ping>>) -> Result<bool> {
        self.portal.event()?;
        let ptr = self.ei.ptr.as_ptr();
        // SAFETY: this descriptor is borrowed from the live EIS context.
        let fd = unsafe { (self.ei.api.ei_get_fd)(ptr) };
        if readable(fd)? {
            // SAFETY: the backend is readable and this owner serializes dispatch.
            unsafe { (self.ei.api.ei_dispatch)(ptr) };
        }
        let mut confirmed = false;
        for _ in 0..MAX_EVENTS {
            // SAFETY: each returned event gives this caller one reference.
            let Some(event) = NonNull::new(unsafe { (self.ei.api.ei_get_event)(ptr) }) else {
                // A synthetic PONG can precede disconnect. Drain this batch
                // and recheck the portal before accepting the round trip.
                self.portal.event()?;
                return Ok(confirmed);
            };
            let result = self.handle(event, fence);
            // SAFETY: all borrowed event data was handled before releasing it.
            unsafe { (self.ei.api.ei_event_unref)(event.as_ptr()) };
            confirmed |= result?;
        }
        anyhow::bail!("the compositor exceeded the Wayland input event bound");
    }

    fn handle(&mut self, event: NonNull<ffi::Event>, fence: Option<NonNull<Ping>>) -> Result<bool> {
        let api = &self.ei.api;
        // SAFETY: event is referenced for the duration of this call; a seat or
        // device is queried only for its documented event type.
        match unsafe { (api.ei_event_get_type)(event.as_ptr()) } {
            ffi::DISCONNECT | ffi::SEAT_REMOVED => {
                anyhow::bail!("the compositor ended Wayland input")
            }
            ffi::SEAT_ADDED => {
                ensure!(
                    !self.seat_seen,
                    "more than one Wayland input seat is available"
                );
                self.seat_seen = true;
                let seat = NonNull::new(unsafe { (api.ei_event_get_seat)(event.as_ptr()) })
                    .context("the compositor did not provide an input seat")?;
                self.bind(seat)?;
            }
            ffi::DEVICE_ADDED => {
                ensure!(
                    self.selected.is_none(),
                    "Wayland input device topology changed"
                );
            }
            ffi::DEVICE_RESUMED => {
                let device = NonNull::new(unsafe { (api.ei_event_get_device)(event.as_ptr()) })
                    .context("the compositor did not provide an input device")?;
                ensure!(
                    self.selected.is_none(),
                    "a Wayland input device resumed after selection"
                );
                if !self.devices.iter().any(|known| known.ptr == device) {
                    self.add_device(device)?;
                }
            }
            ffi::DEVICE_REMOVED | ffi::DEVICE_PAUSED => {
                let device = unsafe { (api.ei_event_get_device)(event.as_ptr()) };
                if let Some(index) = self
                    .devices
                    .iter()
                    .position(|known| known.ptr.as_ptr() == device)
                {
                    ensure!(
                        self.selected.is_none(),
                        "a Wayland input device was revoked or paused"
                    );
                    let removed = self.devices.remove(index);
                    unsafe { (api.ei_device_unref)(removed.ptr.as_ptr()) };
                }
            }
            ffi::PONG => {
                let ping = unsafe { (api.ei_event_pong_get_ping)(event.as_ptr()) };
                return Ok(fence.is_some_and(|fence| fence.as_ptr() == ping));
            }
            _ => (),
        }
        Ok(false)
    }

    fn bind(&self, seat: NonNull<ffi::Seat>) -> Result<()> {
        let api = &self.ei.api;
        let seat = seat.as_ptr();
        let requested = [
            Some(ffi::CAP_ABSOLUTE),
            self.options.buttons.then_some(ffi::CAP_BUTTON),
            self.options.scroll.then_some(ffi::CAP_SCROLL),
            self.options.keyboard.then_some(ffi::CAP_KEYBOARD),
        ];
        // SAFETY: seat is borrowed from the current add event. Capabilities
        // are ABI enum values terminated by zero.
        unsafe {
            let mut caps = [0; 4];
            for (slot, capability) in caps.iter_mut().zip(requested.into_iter().flatten()) {
                ensure!(
                    (api.ei_seat_has_capability)(seat, capability),
                    "the Wayland seat did not grant the requested input capabilities"
                );
                *slot = capability;
            }
            (api.ei_seat_bind_capabilities)(
                seat, caps[0], caps[1], caps[2], caps[3], null::<c_void>(),
            );
        }
        Ok(())
    }

    fn add_device(&mut self, device: NonNull<Device>) -> Result<()> {
        ensure!(
            self.devices.len() < MAX_DEVICES,
            "too many Wayland input devices"
        );
        let api = &self.ei.api;
        let device = device.as_ptr();
        let mut regions = Vec::new();
        // SAFETY: the device is borrowed from a resumed event. Region data
        // is copied before the event reference is released.
        unsafe {
            if (api.ei_device_has_capability)(device, ffi::CAP_ABSOLUTE) {
                for index in 0..=MAX_REGIONS {
                    let region = (api.ei_device_get_region)(device, index);
                    if region.is_null() {
                        break;
                    }
                    ensure!(index < MAX_REGIONS, "too many Wayland pointer regions");
                    regions.push(Region {
                        x: (api.ei_region_get_x)(region),
                        y: (api.ei_region_get_y)(region),
                        width: (api.ei_region_get_width)(region),
                        height: (api.ei_region_get_height)(region),
                    });
                }
            }
            self.devices.push(ReadyDevice {
                ptr: NonNull::new((api.ei_device_ref)(device))
                    .context("could not retain the Wayland pointer")?,
                regions,
                buttons: (api.ei_device_has_capability)(device, ffi::CAP_BUTTON),
                scroll: (api.ei_device_has_capability)(device, ffi::CAP_SCROLL),
                keyboard: (api.ei_device_has_capability)(device, ffi::CAP_KEYBOARD),
            });
        }
        Ok(())
    }

    async fn confirm(&mut self) -> Result<()> {
        // SAFETY: retain the ping until matching PONG or cancellation.
        let ptr = NonNull::new(unsafe { (self.ei.api.ei_new_ping)(self.ei.ptr.as_ptr()) })
            .context("could not allocate the compositor round trip")?;
        let fence = Fence {
            ptr,
            unref: self.ei.api.ei_ping_unref,
        };
        // SAFETY: this ping is used only once on the live EIS context.
        unsafe { (self.ei.api.ei_ping)(fence.ptr.as_ptr()) };
        tokio::time::timeout(FENCE_WAIT, async {
            loop {
                if self.pump(Some(fence.ptr))? {
                    return Ok::<_, anyhow::Error>(());
                }
                tokio::time::sleep(INTERVAL).await;
            }
        })
        .await
        .context("the compositor did not confirm the EIS round trip")?
    }

    pub async fn begin(&mut self, point: ReceivePoint) -> Result<()> {
        self.check()?;
        ensure!(!self.active, "Wayland pointer lease is already active");
        let selection = self.selection()?;
        selection.region.position(point)?;
        for device in selection.devices() {
            // SAFETY: each selected device is resumed and this is its only lease.
            unsafe { (self.ei.api.ei_device_start_emulating)(self.device(device), 1) };
        }
        self.active = true;
        self.motion(point)?;
        self.confirm().await
    }

    pub fn motion(&mut self, point: ReceivePoint) -> Result<()> {
        self.check()?;
        ensure!(self.active, "Wayland pointer lease is inactive");
        let selection = self.selection()?;
        let (x, y) = selection.region.position(point)?;
        let device = self.device(selection.pointer);
        // SAFETY: the selected resumed device has absolute pointer capability.
        unsafe { (self.ei.api.ei_device_pointer_motion_absolute)(device, x, y) };
        self.frame(device);
        Ok(())
    }

    pub fn input(&mut self, point: ReceivePoint, event: Input) -> Result<()> {
        ensure!(
            self.offered.supports(event),
            "Wayland input capability was not granted"
        );
        self.motion(point)?;
        let selection = self.selection()?;
        match event {
            Input::Button { button, down } => {
                let index = usize::from(button - 1);
                if self.owned[index] != down {
                    self.owned[index] = down;
                    let device = self.device(selection.buttons.context("no Wayland buttons")?);
                    // SAFETY: this live resumed device has button capability.
                    unsafe { (self.ei.api.ei_device_button_button)(device, BUTTONS[index], down) };
                    self.frame(device);
                }
            }
            Input::Scroll { x, y } if x != 0 || y != 0 => {
                let device = self.device(selection.scroll.context("no Wayland scroll")?);
                let (x, y) = scroll_units(x, y);
                self.scrolling[0] |= x != 0;
                self.scrolling[1] |= y != 0;
                // SAFETY: this live resumed device has scroll capability.
                unsafe { (self.ei.api.ei_device_scroll_discrete)(device, x, y) };
                self.frame(device);
            }
            Input::Scroll { .. } => (),
            Input::Key { usage, down } => {
                if let Some(code) = self.keys.transition(usage, down) {
                    let device = self.device(selection.keyboard.context("no Wayland keyboard")?);
                    // SAFETY: this live resumed device has keyboard capability.
                    unsafe { (self.ei.api.ei_device_keyboard_key)(device, code, down) };
                    self.frame(device);
                }
            }
        }
        Ok(())
    }

    pub async fn finish(&mut self) -> Result<()> {
        if !self.active {
            return Ok(());
        }
        self.check()?;
        let selection = self.selection()?;
        if let Some(index) = selection.buttons.filter(|_| self.owned.contains(&true)) {
            let device = self.device(index);
            for button in owned_releases(&mut self.owned) {
                // SAFETY: release only buttons submitted to this live device.
                unsafe { (self.ei.api.ei_device_button_button)(device, button, false) };
            }
            self.frame(device);
        }
        if let Some(index) = selection.scroll.filter(|_| self.scrolling.contains(&true)) {
            let device = self.device(index);
            // SAFETY: cancel only the axes used by this scroll lease.
            unsafe {
                (self.ei.api.ei_device_scroll_cancel)(device, self.scrolling[0], self.scrolling[1])
            };
            self.frame(device);
        }
        if let Some(index) = selection.keyboard.filter(|_| self.keys.has_held()) {
            let device = self.device(index);
            for code in self.keys.release() {
                // SAFETY: release only keys submitted to this live keyboard.
                unsafe { (self.ei.api.ei_device_keyboard_key)(device, code, false) };
            }
            self.frame(device);
        }
        for index in selection.devices() {
            // SAFETY: finish the same resumed devices started by begin.
            unsafe { (self.ei.api.ei_device_stop_emulating)(self.device(index)) };
        }
        self.active = false;
        self.confirm().await
    }

    fn device(&self, index: usize) -> *mut Device {
        self.devices[index].ptr.as_ptr()
    }

    fn frame(&self, device: *mut Device) {
        // SAFETY: device is live and belongs to this EIS context.
        unsafe {
            (self.ei.api.ei_device_frame)(device, (self.ei.api.ei_now)(self.ei.ptr.as_ptr()))
        };
    }
}

impl Drop for WaylandInput {
    fn drop(&mut self) {
        for device in self.devices.drain(..) {
            // SAFETY: drop all retained device refs before dropping the context.
            unsafe { (self.ei.api.ei_device_unref)(device.ptr.as_ptr()) };
        }
    }
}

fn scroll_units(x: i16, y: i16) -> (i32, i32) {
    (
        i32::from(x.clamp(-12, 12)) * 120,
        -i32::from(y.clamp(-12, 12)) * 120,
    )
}

fn owned_releases(owned: &mut [bool; 3]) -> impl Iterator<Item = u32> + '_ {
    BUTTONS
        .into_iter()
        .zip(owned)
        .filter_map(|(button, down)| std::mem::take(down).then_some(button))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requires_a_unique_region_and_the_explicitly_requested_devices() -> Result<()> {
        let primary = Region {
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
        };
        let secondary = Region {
            x: 1920,
            y: 24,
            width: 800,
            height: 600,
        };
        let mut devices = vec![
            ReadyDevice {
                ptr: NonNull::dangling(),
                regions: vec![primary, secondary],
                buttons: false,
                scroll: false,
                keyboard: false,
            },
            ReadyDevice {
                ptr: NonNull::dangling(),
                regions: Vec::new(),
                buttons: true,
                scroll: false,
                keyboard: false,
            },
        ];
        let mut options = Options {
            buttons: true,
            scroll: false,
            keyboard: false,
            region: None,
        };
        assert!(select(&devices, options).is_err());
        options.region = Some((1920, 24));
        let chosen = select(&devices, options)?.context("region missing")?;
        assert_eq!(chosen.devices().collect::<Vec<_>>(), [0, 1]);
        assert_eq!(
            chosen.region.position(ReceivePoint { x: 1.0, y: 1.0 })?,
            (2719.0, 623.0)
        );
        assert!(chosen
            .region
            .position(ReceivePoint {
                x: f64::NAN,
                y: 0.0
            })
            .is_err());
        options.scroll = true;
        assert!(select(&devices, options)?.is_none());
        devices[1].scroll = true;
        let chosen = select(&devices, options)?.context("scroll device missing")?;
        assert_eq!(chosen.devices().collect::<Vec<_>>(), [0, 1]);
        assert!(chosen.keyboard.is_none());
        options.keyboard = true;
        assert!(select(&devices, options)?.is_none());
        devices[1].keyboard = true;
        let chosen = select(&devices, options)?.context("keyboard device missing")?;
        assert_eq!(chosen.keyboard, Some(1));
        assert_eq!(chosen.devices().collect::<Vec<_>>(), [0, 1]);
        Ok(())
    }

    #[test]
    fn release_plan_owns_only_submitted_buttons_and_scroll_is_bounded() {
        let mut held = [true, false, true];
        assert_eq!(
            owned_releases(&mut held).collect::<Vec<_>>(),
            [0x110, 0x112]
        );
        assert!(owned_releases(&mut held).next().is_none());
        assert_eq!(scroll_units(1, -1), (120, 120));
        assert_eq!(scroll_units(-1200, 1200), (-1440, -1440));
        assert_eq!(scroll_units(0, 0), (0, 0));
    }
}
