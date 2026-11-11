//! Explicit local playback with a bounded AudioQueue and no callback I/O.
use crate::audio_output::scale_sample;
use anyhow::{ensure, Result};
use peerglide_protocol::audio::{CHANNELS, SAMPLE_RATE, SAMPLES};
use std::{
    ffi::c_void,
    mem::size_of,
    ptr::{null, null_mut},
    sync::atomic::{AtomicU8, Ordering},
};

type Queue = *mut c_void;
type CFRef = *const c_void;

const BUFFERS: usize = 4;
const BUFFER_BYTES: u32 = (SAMPLES * size_of::<i16>()) as u32;
const CURRENT_DEVICE: u32 = u32::from_be_bytes(*b"aqcd");
const UTF8: u32 = 0x0800_0100;

#[repr(C)]
struct Description {
    sample_rate: f64,
    format: u32,
    flags: u32,
    bytes_per_packet: u32,
    frames_per_packet: u32,
    bytes_per_frame: u32,
    channels: u32,
    bits_per_channel: u32,
    reserved: u32,
}

#[repr(C)]
struct Buffer {
    capacity: u32,
    data: *mut c_void,
    length: u32,
    user_data: *mut c_void,
    packet_capacity: u32,
    packets: *mut c_void,
    packet_count: u32,
}

#[link(name = "AudioToolbox", kind = "framework")]
extern "C" {
    fn AudioQueueNewOutput(
        format: *const Description,
        callback: unsafe extern "C" fn(*mut c_void, Queue, *mut Buffer),
        user: *mut c_void,
        run_loop: CFRef,
        run_loop_mode: CFRef,
        flags: u32,
        queue: *mut Queue,
    ) -> i32;
    fn AudioQueueAllocateBuffer(queue: Queue, bytes: u32, buffer: *mut *mut Buffer) -> i32;
    fn AudioQueueEnqueueBuffer(
        queue: Queue,
        buffer: *mut Buffer,
        packets: u32,
        descriptions: *const c_void,
    ) -> i32;
    fn AudioQueueStart(queue: Queue, start: *const c_void) -> i32;
    fn AudioQueueDispose(queue: Queue, immediate: u8) -> i32;
    fn AudioQueueSetProperty(queue: Queue, id: u32, data: *const c_void, size: u32) -> i32;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFStringCreateWithBytes(
        allocator: CFRef,
        bytes: *const u8,
        length: isize,
        encoding: u32,
        external: u8,
    ) -> CFRef;
    fn CFStringGetCString(
        value: CFRef,
        output: *mut std::ffi::c_char,
        capacity: isize,
        encoding: u32,
    ) -> u8;
    fn CFRelease(object: CFRef);
}

#[repr(C)]
struct Property {
    selector: u32,
    scope: u32,
    element: u32,
}

impl Property {
    const fn new(selector: [u8; 4], scope: [u8; 4]) -> Self {
        Self {
            selector: u32::from_be_bytes(selector),
            scope: u32::from_be_bytes(scope),
            element: 0,
        }
    }
}

#[link(name = "CoreAudio", kind = "framework")]
extern "C" {
    fn AudioObjectGetPropertyDataSize(
        id: u32,
        property: *const Property,
        qualifier_size: u32,
        qualifier: *const c_void,
        size: *mut u32,
    ) -> i32;
    fn AudioObjectGetPropertyData(
        id: u32,
        property: *const Property,
        qualifier_size: u32,
        qualifier: *const c_void,
        size: *mut u32,
        value: *mut c_void,
    ) -> i32;
}

pub struct OutputDevice {
    pub uid: String,
    pub name: String,
}

pub fn output_devices() -> Result<Vec<OutputDevice>> {
    const SYSTEM: u32 = 1;
    const LIMIT: usize = 64;
    let property = Property::new(*b"dev#", *b"glob");
    let mut ids = [0u32; LIMIT];
    let capacity = size_of::<[u32; LIMIT]>() as u32;
    let mut bytes = capacity;
    // SAFETY: ids has room for capacity bytes. The property describes a list
    // of AudioDeviceIDs on the system object and has no qualifier.
    check(
        unsafe {
            AudioObjectGetPropertyData(
                SYSTEM,
                &property,
                0,
                null(),
                &mut bytes,
                ids.as_mut_ptr().cast(),
            )
        },
        "list devices",
    )?;
    ensure!(
        bytes <= capacity && bytes.is_multiple_of(size_of::<u32>() as u32),
        "macOS audio device list outside bounds"
    );
    let streams = Property::new(*b"stm#", *b"outp");
    Ok(ids[..bytes as usize / size_of::<u32>()]
        .iter()
        .filter_map(|&id| {
            let mut output_bytes = 0;
            // SAFETY: the size slot is writable. This does not start a device.
            let status =
                unsafe { AudioObjectGetPropertyDataSize(id, &streams, 0, null(), &mut output_bytes) };
            if status != 0 || output_bytes == 0 {
                return None;
            }
            let uid = device_string(id, Property::new(*b"uid ", *b"glob"))?;
            if uid.chars().any(char::is_control) {
                return None;
            }
            let name = device_string(id, Property::new(*b"lnam", *b"glob"))
                .unwrap_or_else(|| "macOS audio output".into());
            Some(OutputDevice { uid, name })
        })
        .collect())
}

fn device_string(id: u32, property: Property) -> Option<String> {
    let mut value: CFRef = null();
    let mut size = size_of::<CFRef>() as u32;
    // SAFETY: the selected property returns one retained CFString.
    let status = unsafe {
        AudioObjectGetPropertyData(id, &property, 0, null(), &mut size, (&raw mut value).cast())
    };
    if status != 0 || value.is_null() {
        return None;
    }
    let mut bytes = [0u8; 513];
    // SAFETY: a successful CoreAudio string property gives a live CFString.
    let copied = unsafe {
        let copied = size == size_of::<CFRef>() as u32
            && CFStringGetCString(value, bytes.as_mut_ptr().cast(), bytes.len() as isize, UTF8) != 0;
        CFRelease(value);
        copied
    };
    if copied {
        bounded_utf8(&bytes)
    } else {
        None
    }
}

fn bounded_utf8(bytes: &[u8]) -> Option<String> {
    let end = bytes.iter().position(|byte| *byte == 0)?;
    if end == 0 {
        return None;
    }
    std::str::from_utf8(&bytes[..end]).ok().map(str::to_owned)
}

#[derive(Default)]
struct Available(AtomicU8);

impl Available {
    fn mark(&self, index: usize) {
        if index < BUFFERS {
            self.0.fetch_or(1 << index, Ordering::Release);
        }
    }

    /// Only the application owner takes slots. The callback only marks them.
    fn take(&self) -> Option<usize> {
        let bits = self.0.load(Ordering::Acquire);
        if bits == 0 {
            return None;
        }
        let index = bits.trailing_zeros() as usize;
        self.0.fetch_and(!(1 << index), Ordering::AcqRel);
        Some(index)
    }
}

unsafe extern "C" fn returned(user: *mut c_void, _: Queue, buffer: *mut Buffer) {
    if user.is_null() || buffer.is_null() {
        return;
    }
    // SAFETY: the context remains alive through synchronous disposal. The
    // buffer's user slot was fixed before it was ever handed to AudioQueue.
    let (available, slot) = unsafe { (&*user.cast::<Available>(), (*buffer).user_data as usize) };
    if let Some(index) = slot.checked_sub(1) {
        available.mark(index);
    }
}

pub struct Player {
    queue: Queue,
    available: Option<Box<Available>>,
    buffers: [*mut Buffer; BUFFERS],
    started: bool,
    stopping: bool,
}

impl Player {
    /// The optional UID selects an output device local to this Mac.
    pub fn open_after_explicit_consent(device_uid: Option<&str>) -> Result<Self> {
        let mut player = Self {
            queue: null_mut(),
            available: Some(Box::new(Available::default())),
            buffers: [null_mut(); BUFFERS],
            started: false,
            stopping: false,
        };
        let user = player.available.as_deref_mut().unwrap() as *mut Available;
        let format = pcm_format();
        // SAFETY: user refers to a fixed allocation kept through disposal.
        // The callback only marks availability on the queue's own thread.
        check(
            unsafe {
                AudioQueueNewOutput(
                    &format,
                    returned,
                    user.cast(),
                    null(),
                    null(),
                    0,
                    &mut player.queue,
                )
            },
            "open",
        )?;
        ensure!(!player.queue.is_null(), "macOS audio output unavailable");
        if let Some(uid) = device_uid {
            player.select_device(uid)?;
        }
        for (index, slot) in player.buffers.iter_mut().enumerate() {
            // SAFETY: slot is writable and AudioQueue owns the returned buffer.
            check(
                unsafe { AudioQueueAllocateBuffer(player.queue, BUFFER_BYTES, slot) },
                "allocate",
            )?;
            ensure!(!slot.is_null(), "macOS audio buffer unavailable");
            // SAFETY: no buffer has been queued; this user-data value is immutable.
            unsafe { (**slot).user_data = (index + 1) as *mut c_void };
            player.available.as_ref().unwrap().mark(index);
        }
        Ok(player)
    }

    fn select_device(&self, uid: &str) -> Result<()> {
        ensure!(!uid.is_empty() && uid.len() <= 512 && !uid.contains('\0'), "invalid audio device UID");
        // SAFETY: the byte slice and its explicit length are valid for the call.
        let value = unsafe {
            CFStringCreateWithBytes(null(), uid.as_ptr(), uid.len() as isize, UTF8, 0)
        };
        ensure!(!value.is_null(), "macOS audio device UID unavailable");
        // SAFETY: AudioQueue accepts a pointer to the retained CFStringRef value.
        let result = unsafe {
            let property = (&raw const value).cast();
            let result = AudioQueueSetProperty(self.queue, CURRENT_DEVICE, property, size_of::<CFRef>() as u32);
            CFRelease(value);
            result
        };
        check(result, "select device")
    }

    pub fn submit(&mut self, samples: &[i16; SAMPLES], volume_percent: u8) -> Result<bool> {
        ensure!(!self.queue.is_null() && !self.stopping, "macOS audio output stopped");
        ensure!(volume_percent <= 100, "audio volume outside limits");
        let Some(index) = self.available.as_ref().and_then(|available| available.take()) else {
            return Ok(false);
        };
        let buffer = self.buffers[index];
        // SAFETY: the callback returned this live buffer to the sole owner.
        // AudioQueue cannot access the payload again until enqueue succeeds.
        unsafe {
            ensure!((*buffer).capacity >= BUFFER_BYTES && !(*buffer).data.is_null(), "macOS audio buffer invalid");
            let pcm = samples.map(|sample| scale_sample(sample, volume_percent));
            std::ptr::copy_nonoverlapping(
                pcm.as_ptr().cast::<u8>(),
                (*buffer).data.cast::<u8>(),
                BUFFER_BYTES as usize,
            );
            (*buffer).length = BUFFER_BYTES;
            check(AudioQueueEnqueueBuffer(self.queue, buffer, 0, null()), "queue")?;
        }
        if !self.started {
            // SAFETY: the owned queue has an initial PCM buffer.
            check(unsafe { AudioQueueStart(self.queue, null()) }, "start")?;
            self.started = true;
        }
        Ok(true)
    }

    pub fn stop(&mut self) -> Result<()> {
        self.stopping = true;
        if self.queue.is_null() {
            return Ok(());
        }
        // SAFETY: immediate disposal synchronously stops output and returns
        // ownership of callback context. On failure keep it for recovery.
        check(unsafe { AudioQueueDispose(self.queue, 1) }, "dispose")?;
        self.queue = null_mut();
        Ok(())
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        if self.stop().is_err() {
            // A failing driver may still hold the callback context.
            std::mem::forget(self.available.take());
        }
    }
}

fn pcm_format() -> Description {
    let bytes = (CHANNELS * size_of::<i16>()) as u32;
    Description {
        sample_rate: f64::from(SAMPLE_RATE),
        format: u32::from_be_bytes(*b"lpcm"),
        flags: (1 << 2) | (1 << 3), // signed integer and packed
        bytes_per_packet: bytes,
        frames_per_packet: 1,
        bytes_per_frame: bytes,
        channels: CHANNELS as u32,
        bits_per_channel: 16,
        reserved: 0,
    }
}

fn check(status: i32, operation: &'static str) -> Result<()> {
    ensure!(status == 0, "macOS audio {operation} failed ({status})");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn callback_returns_only_existing_slots_once() {
        let available = Available::default();
        available.mark(BUFFERS);
        assert!(available.take().is_none());
        available.mark(1);
        available.mark(1);
        available.mark(3);
        assert_eq!(available.take(), Some(1));
        assert_eq!(available.take(), Some(3));
        assert!(available.take().is_none());
    }

    #[test]
    fn device_names_must_fit_and_decode_without_unterminated_bytes() {
        assert_eq!(bounded_utf8(b"Desk audio\0unused").as_deref(), Some("Desk audio"));
        assert!(bounded_utf8(b"not terminated").is_none());
        assert!(bounded_utf8(b"\xff\0").is_none());
        assert!(bounded_utf8(b"\0").is_none());
    }

    #[test]
    fn format_is_interleaved_pcm_for_the_wire_samples() {
        let format = pcm_format();
        assert!((format.sample_rate - 48_000.0).abs() < f64::EPSILON);
        assert_eq!(format.channels, 2);
        assert_eq!(format.bytes_per_frame, 4);
        assert_eq!(format.frames_per_packet, 1);
        assert_eq!(format.bits_per_channel, 16);
    }
}
