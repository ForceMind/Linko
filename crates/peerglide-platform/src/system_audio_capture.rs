//! Explicit system audio capture owned by its creating worker thread.
use anyhow::{ensure, Result};
use peerglide_protocol::audio::{CHANNELS, SAMPLES};
use std::{ffi::c_void, ptr::null_mut};

#[cfg(all(windows, target_env = "msvc"))]
extern "C" {
    #[link_name = "linko_wasapi_open"]
    fn native_open(output: *mut *mut c_void) -> i32;
    #[link_name = "linko_wasapi_read"]
    fn native_read(
        owner: *mut c_void,
        output: *mut i16,
        capacity: u32,
        written: *mut u32,
    ) -> i32;
    #[link_name = "linko_wasapi_stop"]
    fn native_stop(owner: *mut c_void) -> i32;
    #[link_name = "linko_wasapi_drop"]
    fn native_drop(owner: *mut c_void);
}

#[cfg(target_os = "macos")]
extern "C" {
    #[link_name = "linko_mac_audio_open"]
    fn native_open(output: *mut *mut c_void) -> i32;
    #[link_name = "linko_mac_audio_read"]
    fn native_read(
        owner: *mut c_void,
        output: *mut i16,
        capacity: u32,
        written: *mut u32,
    ) -> i32;
    #[link_name = "linko_mac_audio_stop"]
    fn native_stop(owner: *mut c_void) -> i32;
    #[link_name = "linko_mac_audio_drop"]
    fn native_drop(owner: *mut c_void);
}

pub struct SystemAudio {
    owner: *mut c_void,
}

impl SystemAudio {
    pub fn open_after_explicit_consent() -> Result<Self> {
        let mut owner = null_mut();
        // SAFETY: the writable slot receives an owned, thread-bound capture.
        let result = unsafe { native_open(&mut owner) };
        check(result, "open")?;
        ensure!(!owner.is_null(), "system audio capture owner unavailable");
        Ok(Self { owner })
    }

    /// Returns the number of interleaved samples written, at most one frame.
    pub fn read(&mut self, output: &mut [i16]) -> Result<usize> {
        ensure!(
            !output.is_empty() && output.len() <= SAMPLES && output.len().is_multiple_of(CHANNELS),
            "invalid system audio output buffer"
        );
        let capacity = (output.len() / CHANNELS) as u32;
        let mut frames = 0;
        // SAFETY: the live capture belongs to this thread. output is sized for
        // capacity stereo frames, and the native adapter honors that bound.
        let result =
            unsafe { native_read(self.owner, output.as_mut_ptr(), capacity, &mut frames) };
        check(result, "read")?;
        ensure!(frames <= capacity, "system capture returned oversized audio");
        Ok(frames as usize * CHANNELS)
    }

    pub fn stop(&mut self) -> Result<()> {
        // SAFETY: this owner is live and stays on the creating capture thread.
        check(unsafe { native_stop(self.owner) }, "stop")
    }
}

impl Drop for SystemAudio {
    fn drop(&mut self) {
        // SAFETY: this is the sole owner. Native destruction closes capture resources.
        unsafe { native_drop(self.owner) };
    }
}

fn check(result: i32, operation: &'static str) -> Result<()> {
    ensure!(result >= 0, "system audio {operation} failed ({result:#010x})");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_pcm_buffer_is_rejected_before_entering_native_capture() {
        let mut inert = SystemAudio { owner: null_mut() };
        for samples in [0, 1, SAMPLES + 2] {
            let mut output = vec![0; samples];
            assert_eq!(
                inert.read(&mut output).unwrap_err().to_string(),
                "invalid system audio output buffer"
            );
        }
    }
}
