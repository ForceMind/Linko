//! Public libei 1.x and liboeffis 1.x ABI used by the local input owner.
use anyhow::{Context, Result};
use libloading::Library;
use std::ffi::{c_char, c_int, c_uint, c_void};

macro_rules! opaque {
    ($($name:ident),+ $(,)?) => {$(
        #[repr(C)]
        pub(super) struct $name {
            _private: [u8; 0],
        }
    )+};
}
opaque!(Oeffis, Ei, Event, Seat, Device, Region, Ping);

macro_rules! api {
    ($name:ident, $soname:literal, {$($symbol:ident: $signature:ty),+ $(,)?}) => {
        pub(super) struct $name {
            $(
                pub(super) $symbol: $signature,
            )+
            _library: Library,
        }

        impl $name {
            pub(super) fn load() -> Result<Self> {
                // SAFETY: resolve public ABI symbols in the named system
                // library and keep the handle alive through every call.
                let library = unsafe { Library::new($soname) }
                    .with_context(|| format!("Wayland input requires {}", $soname))?;
                Ok(Self {
                    $(
                        $symbol: unsafe { library.get::<$signature>(stringify!($symbol).as_bytes()) }
                            .map(|symbol| *symbol)
                            .with_context(|| concat!("Wayland input requires ", stringify!($symbol)))?,
                    )+
                    _library: library,
                })
            }
        }
    };
}

pub(super) const PORTAL_KEYBOARD: u32 = 1;
pub(super) const PORTAL_POINTER: u32 = 1 << 1;
pub(super) const PORTAL_CONNECTED: c_int = 1;
pub(super) const CAP_ABSOLUTE: c_uint = 1 << 1;
pub(super) const CAP_KEYBOARD: c_uint = 1 << 2;
pub(super) const CAP_SCROLL: c_uint = 1 << 4;
pub(super) const CAP_BUTTON: c_uint = 1 << 5;
pub(super) const DISCONNECT: c_int = 2;
pub(super) const SEAT_ADDED: c_int = 3;
pub(super) const SEAT_REMOVED: c_int = 4;
pub(super) const DEVICE_ADDED: c_int = 5;
pub(super) const DEVICE_REMOVED: c_int = 6;
pub(super) const DEVICE_PAUSED: c_int = 7;
pub(super) const DEVICE_RESUMED: c_int = 8;
pub(super) const PONG: c_int = 90;

type LogHandler = unsafe extern "C" fn(*mut Ei, c_int, *const c_char, *mut c_void);

api!(PortalApi, "liboeffis.so.1", {
    oeffis_new: unsafe extern "C" fn(*mut c_void) -> *mut Oeffis,
    oeffis_unref: unsafe extern "C" fn(*mut Oeffis) -> *mut Oeffis,
    oeffis_get_fd: unsafe extern "C" fn(*mut Oeffis) -> c_int,
    oeffis_create_session: unsafe extern "C" fn(*mut Oeffis, u32),
    oeffis_dispatch: unsafe extern "C" fn(*mut Oeffis),
    oeffis_get_event: unsafe extern "C" fn(*mut Oeffis) -> c_int,
    oeffis_get_eis_fd: unsafe extern "C" fn(*mut Oeffis) -> c_int,
});

api!(EiApi, "libei.so.1", {
    ei_new_sender: unsafe extern "C" fn(*mut c_void) -> *mut Ei,
    ei_unref: unsafe extern "C" fn(*mut Ei) -> *mut Ei,
    ei_configure_name: unsafe extern "C" fn(*mut Ei, *const c_char),
    ei_log_set_handler: unsafe extern "C" fn(*mut Ei, LogHandler),
    ei_setup_backend_fd: unsafe extern "C" fn(*mut Ei, c_int) -> c_int,
    ei_get_fd: unsafe extern "C" fn(*mut Ei) -> c_int,
    ei_dispatch: unsafe extern "C" fn(*mut Ei),
    ei_get_event: unsafe extern "C" fn(*mut Ei) -> *mut Event,
    ei_event_unref: unsafe extern "C" fn(*mut Event) -> *mut Event,
    ei_event_get_type: unsafe extern "C" fn(*mut Event) -> c_int,
    ei_event_get_device: unsafe extern "C" fn(*mut Event) -> *mut Device,
    ei_event_get_seat: unsafe extern "C" fn(*mut Event) -> *mut Seat,
    ei_seat_has_capability: unsafe extern "C" fn(*mut Seat, c_uint) -> bool,
    ei_seat_bind_capabilities: unsafe extern "C" fn(*mut Seat, ...),
    ei_device_ref: unsafe extern "C" fn(*mut Device) -> *mut Device,
    ei_device_unref: unsafe extern "C" fn(*mut Device) -> *mut Device,
    ei_device_has_capability: unsafe extern "C" fn(*mut Device, c_uint) -> bool,
    ei_device_get_region: unsafe extern "C" fn(*mut Device, usize) -> *mut Region,
    ei_region_get_x: unsafe extern "C" fn(*mut Region) -> u32,
    ei_region_get_y: unsafe extern "C" fn(*mut Region) -> u32,
    ei_region_get_width: unsafe extern "C" fn(*mut Region) -> u32,
    ei_region_get_height: unsafe extern "C" fn(*mut Region) -> u32,
    ei_device_start_emulating: unsafe extern "C" fn(*mut Device, u32),
    ei_device_stop_emulating: unsafe extern "C" fn(*mut Device),
    ei_device_pointer_motion_absolute: unsafe extern "C" fn(*mut Device, f64, f64),
    ei_device_button_button: unsafe extern "C" fn(*mut Device, u32, bool),
    ei_device_keyboard_key: unsafe extern "C" fn(*mut Device, u32, bool),
    ei_device_scroll_discrete: unsafe extern "C" fn(*mut Device, i32, i32),
    ei_device_scroll_cancel: unsafe extern "C" fn(*mut Device, bool, bool),
    ei_device_frame: unsafe extern "C" fn(*mut Device, u64),
    ei_now: unsafe extern "C" fn(*mut Ei) -> u64,
    ei_new_ping: unsafe extern "C" fn(*mut Ei) -> *mut Ping,
    ei_ping: unsafe extern "C" fn(*mut Ping),
    ei_ping_unref: unsafe extern "C" fn(*mut Ping) -> *mut Ping,
    ei_event_pong_get_ping: unsafe extern "C" fn(*mut Event) -> *mut Ping,
});
