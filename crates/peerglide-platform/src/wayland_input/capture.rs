//! EIS receiver owned by the single local input-capture lifecycle.
use super::{
    ensure_wayland_session,
    ffi::{self, CaptureApi, Device, Ei, Event},
    keyboard, quiet_log, readable, BUTTONS,
};
use crate::capture_thread::Reservation;
use anyhow::{ensure, Context, Result};
use peerglide_protocol::{Input, InputCapabilities};
use std::{
    ffi::c_void,
    os::fd::{IntoRawFd, OwnedFd},
    ptr::{null, null_mut, NonNull},
};

const MAX_EVENTS: usize = 256;
const MAX_DEVICES: usize = 16;
const MAX_FRAME: usize = 64;

#[derive(Clone, Copy)]
pub struct CaptureOptions {
    pub buttons: bool,
    pub scroll: bool,
    pub keyboard: bool,
}

impl CaptureOptions {
    pub fn capabilities(self) -> Result<InputCapabilities> {
        let keys = if self.keyboard {
            keyboard::usages().collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        InputCapabilities::new(&keys, true, if self.buttons { 7 } else { 0 }, self.scroll)
            .map_err(anyhow::Error::msg)
    }
}

pub enum Sample {
    Motion(f64, f64),
    Input(Input),
    Wheel(i32, i32),
    Unsupported,
}

impl Sample {
    fn discrete(&self) -> bool {
        !matches!(self, Self::Motion(..))
    }
}

#[derive(Default)]
pub struct Batch {
    pub ready: bool,
    pub pointer_started: bool,
    pub discrete: bool,
    pub samples: Vec<Sample>,
}

struct DeviceState {
    ptr: NonNull<Device>,
    pointer: bool,
    sequence: Option<u32>,
    pending: Vec<Sample>,
}

impl DeviceState {
    fn matches(&self, expected: Option<u32>) -> bool {
        self.sequence
            .is_some_and(|sequence| expected.is_none_or(|expected| expected == sequence))
    }

    fn frame(&mut self, expected: Option<u32>, batch: &mut Batch) -> Result<()> {
        if self.matches(expected) {
            ensure!(
                batch.samples.len() + self.pending.len() <= MAX_EVENTS,
                "too much captured input in one EIS batch"
            );
            batch.discrete |= self.pending.iter().any(Sample::discrete);
            batch.samples.append(&mut self.pending);
        } else {
            self.pending.clear();
        }
        Ok(())
    }
}

pub struct Capture {
    api: CaptureApi,
    ptr: NonNull<Ei>,
    _reservation: Reservation,
    options: CaptureOptions,
    connected: bool,
    seat_seen: bool,
    devices: Vec<DeviceState>,
}

impl Capture {
    pub fn reserve(options: CaptureOptions) -> Result<Self> {
        ensure_wayland_session()?;
        let reservation = Reservation::acquire()?;
        let api = CaptureApi::load()?;
        // SAFETY: create a receiver without any caller context or callbacks.
        let ptr = NonNull::new(unsafe { (api.ei_new_receiver)(null_mut()) })
            .context("could not create the Wayland capture receiver")?;
        // SAFETY: configure a fresh context before its backend is set.
        unsafe {
            (api.ei_log_set_handler)(ptr.as_ptr(), quiet_log);
            (api.ei_configure_name)(ptr.as_ptr(), c"Linko".as_ptr());
        }
        Ok(Self {
            api,
            ptr,
            _reservation: reservation,
            options,
            connected: false,
            seat_seen: false,
            devices: Vec::new(),
        })
    }

    pub fn connect(&mut self, fd: OwnedFd) -> Result<()> {
        ensure!(!self.connected, "the Wayland capture is already connected");
        self.connected = true;
        // SAFETY: ownership transfers to the EIS context, including on error.
        let result = unsafe { (self.api.ei_setup_backend_fd)(self.ptr.as_ptr(), fd.into_raw_fd()) };
        ensure!(result == 0, "the Wayland capture backend could not start");
        Ok(())
    }

    pub fn poll(&mut self, expected: Option<u32>) -> Result<Batch> {
        let ptr = self.ptr.as_ptr();
        // SAFETY: this descriptor is borrowed from the live EIS context.
        let fd = unsafe { (self.api.ei_get_fd)(ptr) };
        if readable(fd)? {
            // SAFETY: this owner serializes dispatch on its readable backend.
            unsafe { (self.api.ei_dispatch)(ptr) };
        }
        let mut batch = Batch::default();
        for _ in 0..MAX_EVENTS {
            // SAFETY: each returned event gives this caller one reference.
            let Some(event) = NonNull::new(unsafe { (self.api.ei_get_event)(ptr) }) else {
                batch.ready = self.seat_seen;
                batch.pointer_started = self.devices.iter().any(|device| {
                    device.pointer && device.matches(expected)
                });
                batch.discrete |= self.devices.iter().any(|device| {
                    device.matches(expected) && device.pending.iter().any(Sample::discrete)
                });
                return Ok(batch);
            };
            let result = self.handle(event, expected, &mut batch);
            // SAFETY: handle does not retain the event or its borrowed data.
            unsafe { (self.api.ei_event_unref)(event.as_ptr()) };
            result?;
        }
        anyhow::bail!("the compositor exceeded the capture event bound");
    }

    fn handle(
        &mut self,
        event: NonNull<Event>,
        expected: Option<u32>,
        batch: &mut Batch,
    ) -> Result<()> {
        let event = event.as_ptr();
        // SAFETY: event is referenced by the caller for this dispatch.
        let kind = unsafe { (self.api.ei_event_get_type)(event) };
        match kind {
            ffi::DISCONNECT | ffi::SEAT_REMOVED => anyhow::bail!("Wayland input capture ended"),
            ffi::SEAT_ADDED => self.bind(event),
            ffi::DEVICE_ADDED => {
                ensure!(self.devices.len() < MAX_DEVICES, "too many capture devices");
                let device = self.event_device(event)?;
                ensure!(
                    !self.devices.iter().any(|known| known.ptr == device),
                    "the compositor repeated a capture device"
                );
                // SAFETY: this device is borrowed from its add event.
                let pointer = unsafe {
                    (self.api.ei_device_has_capability)(device.as_ptr(), ffi::CAP_RELATIVE)
                };
                self.devices.push(DeviceState {
                    ptr: device,
                    pointer,
                    sequence: None,
                    pending: Vec::new(),
                });
                Ok(())
            }
            ffi::DEVICE_PAUSED | ffi::DEVICE_REMOVED | ffi::STOP_EMULATING => {
                let device = self.event_device(event)?;
                if let Some(index) = self.devices.iter().position(|known| known.ptr == device) {
                    ensure!(
                        !self.devices[index].matches(expected),
                        "the compositor stopped an active capture device"
                    );
                    if kind == ffi::DEVICE_REMOVED {
                        self.devices.remove(index);
                    } else {
                        self.devices[index].sequence = None;
                        self.devices[index].pending.clear();
                    }
                }
                Ok(())
            }
            ffi::START_EMULATING => {
                let ptr = self.event_device(event)?;
                let device = self.devices.iter_mut().find(|known| known.ptr == ptr)
                    .context("emulation started on an unknown capture device")?;
                ensure!(device.sequence.is_none(), "capture activation changed unexpectedly");
                // SAFETY: sequence is defined on StartEmulating events.
                device.sequence = Some(unsafe { (self.api.ei_event_emulating_get_sequence)(event) });
                Ok(())
            }
            ffi::FRAME => {
                let ptr = self.event_device(event)?;
                let device = self.devices.iter_mut().find(|known| known.ptr == ptr)
                    .context("frame arrived from an unknown capture device")?;
                device.frame(expected, batch)
            }
            ffi::POINTER_MOTION | ffi::BUTTON_BUTTON | ffi::SCROLL_DISCRETE | ffi::KEYBOARD_KEY => {
                let ptr = self.event_device(event)?;
                let sample = self.sample(event, kind)?;
                let device = self.devices.iter_mut().find(|known| known.ptr == ptr)
                    .context("input arrived from an unknown capture device")?;
                if device.matches(expected) {
                    ensure!(device.pending.len() < MAX_FRAME, "capture frame is too large");
                    device.pending.push(sample);
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn bind(&mut self, event: *mut Event) -> Result<()> {
        ensure!(!self.seat_seen, "the compositor supplied more than one capture seat");
        // SAFETY: seat is borrowed from its add event.
        let seat = NonNull::new(unsafe { (self.api.ei_event_get_seat)(event) })
            .context("the compositor did not provide a capture seat")?;
        let requested = [
            Some(ffi::CAP_RELATIVE),
            self.options.buttons.then_some(ffi::CAP_BUTTON),
            self.options.scroll.then_some(ffi::CAP_SCROLL),
            self.options.keyboard.then_some(ffi::CAP_KEYBOARD),
        ];
        let mut caps = [0; 4];
        for (slot, capability) in caps.iter_mut().zip(requested.into_iter().flatten()) {
            // SAFETY: capability is from the public ABI and the seat is live.
            ensure!(
                unsafe { (self.api.ei_seat_has_capability)(seat.as_ptr(), capability) },
                "the compositor did not grant the requested capture capability"
            );
            *slot = capability;
        }
        // SAFETY: pass only selected ABI enum values with a terminating zero.
        unsafe {
            (self.api.ei_seat_bind_capabilities)(
                seat.as_ptr(), caps[0], caps[1], caps[2], caps[3], null::<c_void>(),
            );
        }
        self.seat_seen = true;
        Ok(())
    }

    fn event_device(&self, event: *mut Event) -> Result<NonNull<Device>> {
        // SAFETY: caller uses this only on documented device event variants.
        NonNull::new(unsafe { (self.api.ei_event_get_device)(event) })
            .context("the compositor did not provide a capture device")
    }

    fn sample(&self, event: *mut Event, kind: i32) -> Result<Sample> {
        // SAFETY: each getter is selected by the documented event type.
        unsafe {
            match kind {
                ffi::POINTER_MOTION => {
                    let dx = (self.api.ei_event_pointer_get_dx)(event);
                    let dy = (self.api.ei_event_pointer_get_dy)(event);
                    ensure!(dx.is_finite() && dy.is_finite(), "invalid captured pointer motion");
                    Ok(Sample::Motion(dx, dy))
                }
                ffi::BUTTON_BUTTON => {
                    let button = (self.api.ei_event_button_get_button)(event);
                    let down = (self.api.ei_event_button_get_is_press)(event);
                    Ok(BUTTONS.iter().position(|&code| code == button).map_or(
                        Sample::Unsupported,
                        |index| Sample::Input(Input::Button { button: index as u8 + 1, down }),
                    ))
                }
                ffi::SCROLL_DISCRETE => Ok(Sample::Wheel(
                    (self.api.ei_event_scroll_get_discrete_dx)(event),
                    (self.api.ei_event_scroll_get_discrete_dy)(event),
                )),
                ffi::KEYBOARD_KEY => {
                    let code = (self.api.ei_event_keyboard_get_key)(event);
                    let down = (self.api.ei_event_keyboard_get_key_is_press)(event);
                    Ok(keyboard::usage_from_evdev(code).map_or(
                        Sample::Unsupported,
                        |usage| Sample::Input(Input::Key { usage, down }),
                    ))
                }
                _ => Ok(Sample::Unsupported),
            }
        }
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        // SAFETY: no event or backend descriptor outlives this context.
        unsafe { (self.api.ei_unref)(self.ptr.as_ptr()) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_complete_frames_for_the_requested_activation_reach_the_batch() -> Result<()> {
        let mut device = DeviceState {
            ptr: NonNull::dangling(),
            pointer: true,
            sequence: Some(14),
            pending: vec![Sample::Motion(1.0, 2.0), Sample::Input(Input::Key { usage: 4, down: true })],
        };
        let mut batch = Batch::default();
        device.frame(Some(15), &mut batch)?;
        assert!(batch.samples.is_empty() && !batch.discrete);
        device.sequence = Some(15);
        device.pending.push(Sample::Input(Input::Key { usage: 4, down: true }));
        device.frame(Some(15), &mut batch)?;
        assert!(batch.discrete);
        assert_eq!(batch.samples.len(), 1);
        assert!(device.pending.is_empty());
        Ok(())
    }
}
