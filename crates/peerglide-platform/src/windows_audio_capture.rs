//! Explicit WASAPI loopback owned by its creating worker thread.
use anyhow::{ensure, Result};
use peerglide_protocol::audio::{CHANNELS, SAMPLES};
use std::{ffi::c_void, ptr::null_mut};

extern "C" {
    fn linko_wasapi_open(output: *mut *mut c_void) -> i32;
    fn linko_wasapi_read(
        owner: *mut c_void,
        output: *mut i16,
        capacity: u32,
        written: *mut u32,
    ) -> i32;
    fn linko_wasapi_stop(owner: *mut c_void) -> i32;
    fn linko_wasapi_drop(owner: *mut c_void);
}

pub struct SystemAudio {
    owner: *mut c_void,
}

impl SystemAudio {
    pub fn open_after_explicit_consent() -> Result<Self> {
        let mut owner = null_mut();
        // SAFETY: the writable slot receives an owned, thread-bound capture.
        let result = unsafe { linko_wasapi_open(&mut owner) };
        check(result, "open")?;
        ensure!(!owner.is_null(), "WASAPI capture owner unavailable");
        Ok(Self { owner })
    }

    /// Returns the number of interleaved samples written, at most one frame.
    pub fn read(&mut self, output: &mut [i16]) -> Result<usize> {
        ensure!(
            !output.is_empty() && output.len() <= SAMPLES && output.len().is_multiple_of(CHANNELS),
            "invalid WASAPI output buffer"
        );
        let capacity = (output.len() / CHANNELS) as u32;
        let mut frames = 0;
        // SAFETY: the live capture belongs to this thread. output is sized for
        // capacity stereo frames, and the native adapter honors that bound.
        let result =
            unsafe { linko_wasapi_read(self.owner, output.as_mut_ptr(), capacity, &mut frames) };
        check(result, "read")?;
        ensure!(frames <= capacity, "WASAPI returned oversized audio");
        Ok(frames as usize * CHANNELS)
    }

    pub fn stop(&mut self) -> Result<()> {
        // SAFETY: this owner is live and stays on the creating COM thread.
        check(unsafe { linko_wasapi_stop(self.owner) }, "stop")
    }
}

impl Drop for SystemAudio {
    fn drop(&mut self) {
        // SAFETY: this is the sole owner. Native destruction closes COM handles.
        unsafe { linko_wasapi_drop(self.owner) };
    }
}

fn check(result: i32, operation: &'static str) -> Result<()> {
    ensure!(
        result >= 0,
        "WASAPI loopback {operation} failed ({result:#010x})"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_pcm_buffer_is_rejected_before_entering_wasapi() {
        let mut inert = SystemAudio { owner: null_mut() };
        for samples in [0, 1, SAMPLES + 2] {
            let mut output = vec![0; samples];
            assert_eq!(
                inert.read(&mut output).unwrap_err().to_string(),
                "invalid WASAPI output buffer"
            );
        }
    }
}
