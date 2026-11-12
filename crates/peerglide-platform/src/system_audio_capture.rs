//! Explicit audio capture owned by its creating worker thread.
use anyhow::{ensure, Result};
use peerglide_protocol::audio::{CHANNELS, SAMPLES};
use std::{
    ffi::c_void,
    ptr::null_mut,
    sync::atomic::{AtomicBool, Ordering},
};

type AllowMicrophone = unsafe extern "C" fn(*const c_void) -> u8;

#[cfg(all(windows, target_env = "msvc"))]
extern "C" {
    #[link_name = "linko_wasapi_open"]
    fn native_open(output: *mut *mut c_void) -> i32;
    #[link_name = "linko_wasapi_mic_open"]
    fn native_microphone(
        output: *mut *mut c_void,
        allow: AllowMicrophone,
        token: *const c_void,
    ) -> i32;
    #[link_name = "linko_wasapi_read"]
    fn native_read(owner: *mut c_void, output: *mut i16, capacity: u32, written: *mut u32) -> i32;
    #[link_name = "linko_wasapi_stop"]
    fn native_stop(owner: *mut c_void) -> i32;
    #[link_name = "linko_wasapi_drop"]
    fn native_drop(owner: *mut c_void);
}

#[cfg(target_os = "macos")]
extern "C" {
    #[link_name = "linko_mac_audio_open"]
    fn native_open(output: *mut *mut c_void) -> i32;
    #[link_name = "linko_mac_mic_open"]
    fn native_microphone(
        output: *mut *mut c_void,
        allow: AllowMicrophone,
        token: *const c_void,
    ) -> i32;
    #[link_name = "linko_mac_audio_read"]
    fn native_read(owner: *mut c_void, output: *mut i16, capacity: u32, written: *mut u32) -> i32;
    #[link_name = "linko_mac_audio_stop"]
    fn native_stop(owner: *mut c_void) -> i32;
    #[link_name = "linko_mac_audio_drop"]
    fn native_drop(owner: *mut c_void);
}

pub struct AudioCapture {
    owner: *mut c_void,
    source: &'static str,
}

pub type SystemAudio = AudioCapture;

type Open = unsafe extern "C" fn(*mut *mut c_void) -> i32;

impl AudioCapture {
    pub fn open_after_explicit_consent() -> Result<Self> {
        Self::open(native_open, "system audio")
    }

    pub fn open_microphone_after_explicit_consent(cancelled: &AtomicBool) -> Result<Self> {
        let mut owner = null_mut();
        // SAFETY: the native open calls `allow_microphone` synchronously, never
        // retaining its token. Both referenced slots live through this call.
        let result = unsafe {
            native_microphone(&mut owner, allow_microphone, std::ptr::from_ref(cancelled).cast())
        };
        Self::from_open(result, owner, "microphone")
    }

    fn open(open: Open, source: &'static str) -> Result<Self> {
        let mut owner = null_mut();
        // SAFETY: the writable slot receives an owned, thread-bound capture.
        let result = unsafe { open(&mut owner) };
        Self::from_open(result, owner, source)
    }

    fn from_open(result: i32, owner: *mut c_void, source: &'static str) -> Result<Self> {
        check(result, source, "open")?;
        ensure!(!owner.is_null(), "{source} capture owner unavailable");
        Ok(Self { owner, source })
    }

    /// Returns the number of interleaved samples written, at most one frame.
    pub fn read(&mut self, output: &mut [i16]) -> Result<usize> {
        ensure!(
            !output.is_empty() && output.len() <= SAMPLES && output.len().is_multiple_of(CHANNELS),
            "invalid {} output buffer",
            self.source
        );
        let capacity = (output.len() / CHANNELS) as u32;
        let mut frames = 0;
        // SAFETY: the live capture belongs to this thread. output is sized for
        // capacity stereo frames, and the native adapter honors that bound.
        let result = unsafe { native_read(self.owner, output.as_mut_ptr(), capacity, &mut frames) };
        check(result, self.source, "read")?;
        ensure!(
            frames <= capacity,
            "{} capture returned oversized audio",
            self.source
        );
        Ok(frames as usize * CHANNELS)
    }

    pub fn stop(&mut self) -> Result<()> {
        // SAFETY: this owner is live and stays on the creating capture thread.
        check(unsafe { native_stop(self.owner) }, self.source, "stop")
    }
}

unsafe extern "C" fn allow_microphone(token: *const c_void) -> u8 {
    // SAFETY: only the synchronous open call uses this callback with a pointer
    // to its live cancellation flag.
    let cancelled = unsafe { &*token.cast::<AtomicBool>() };
    u8::from(!cancelled.load(Ordering::Acquire))
}

impl Drop for AudioCapture {
    fn drop(&mut self) {
        // SAFETY: this is the sole owner. Native destruction closes capture resources.
        unsafe { native_drop(self.owner) };
    }
}

fn check(result: i32, source: &'static str, operation: &'static str) -> Result<()> {
    ensure!(result >= 0, "{source} {operation} failed ({result:#010x})");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_pcm_buffer_is_rejected_before_entering_native_capture() {
        for source in ["system audio", "microphone"] {
            let mut inert = AudioCapture {
                owner: null_mut(),
                source,
            };
            for samples in [0, 1, SAMPLES + 2] {
                let mut output = vec![0; samples];
                assert_eq!(
                    inert.read(&mut output).unwrap_err().to_string(),
                    format!("invalid {source} output buffer")
                );
            }
        }
    }
}
