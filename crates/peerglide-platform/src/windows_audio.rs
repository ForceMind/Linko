//! Local WinMM output owner. Opens only on explicit local playback selection.
use anyhow::{ensure, Result};
use peerglide_protocol::audio::{CHANNELS, SAMPLE_RATE, SAMPLES};
use std::{cell::UnsafeCell, mem::size_of, ptr::null_mut};
use windows_sys::Win32::Media::{
    Audio::{
        waveOutClose, waveOutGetDevCapsW, waveOutGetNumDevs, waveOutOpen, waveOutPrepareHeader,
        waveOutReset, waveOutUnprepareHeader, waveOutWrite, CALLBACK_NULL, HWAVEOUT, WAVEFORMATEX,
        WAVEHDR, WAVEOUTCAPSW, WAVERR_STILLPLAYING, WAVE_FORMAT_PCM,
    },
    MMSYSERR_NOERROR,
};

const BUFFERS: usize = 4;
const MAX_OUTPUTS: u32 = 64;
const HEADER_BYTES: u32 = size_of::<WAVEHDR>() as u32;
const DEFAULT_OUTPUT: u32 = u32::MAX; // WAVE_MAPPER in the platform SDK.

pub struct OutputDevice {
    pub id: u32,
    pub name: String,
}

/// Device IDs are a current local snapshot. Callers refresh on device change.
pub fn output_devices() -> Vec<OutputDevice> {
    // SAFETY: this query receives no pointers and opens no stream.
    let count = unsafe { waveOutGetNumDevs() }.min(MAX_OUTPUTS);
    (0..count)
        .filter_map(|id| {
            let mut caps = WAVEOUTCAPSW::default();
            // SAFETY: caps has the exact SDK size and is writable for the call.
            let result = unsafe {
                waveOutGetDevCapsW(id as usize, &mut caps, size_of::<WAVEOUTCAPSW>() as u32)
            };
            if result != MMSYSERR_NOERROR {
                return None;
            }
            let name = caps.szPname;
            let end = name.iter().position(|word| *word == 0).unwrap_or(name.len());
            Some(OutputDevice {
                id,
                name: String::from_utf16_lossy(&name[..end]),
            })
        })
        .collect()
}

struct Buffer {
    header: UnsafeCell<WAVEHDR>,
    samples: UnsafeCell<[i16; SAMPLES]>,
    prepared: bool,
}

impl Buffer {
    fn new() -> Box<Self> {
        Box::new(Self {
            header: UnsafeCell::new(WAVEHDR::default()),
            samples: UnsafeCell::new([0; SAMPLES]),
            prepared: false,
        })
    }

    fn retire(&mut self, output: HWAVEOUT) -> Result<bool> {
        if !self.prepared {
            return Ok(true);
        }
        // SAFETY: the device owns the header while queued. Only WinMM checks
        // and releases that ownership; Rust never reads its live flags.
        let result = unsafe { waveOutUnprepareHeader(output, self.header.get(), HEADER_BYTES) };
        if result == WAVERR_STILLPLAYING {
            return Ok(false);
        }
        check(result, "unprepare")?;
        self.prepared = false;
        Ok(true)
    }

    fn submit(&mut self, output: HWAVEOUT, samples: &[i16; SAMPLES], volume: u8) -> Result<()> {
        // SAFETY: retire() established that WinMM owns neither buffer. Both
        // addresses remain in the same allocation until unprepare succeeds.
        unsafe {
            *self.samples.get() = samples.map(|sample| scale_sample(sample, volume));
            self.header.get().write(WAVEHDR {
                lpData: self.samples.get().cast(),
                dwBufferLength: (SAMPLES * size_of::<i16>()) as u32,
                ..WAVEHDR::default()
            });
        }
        // SAFETY: the fixed buffer and initialized header live until retired.
        check(
            unsafe { waveOutPrepareHeader(output, self.header.get(), HEADER_BYTES) },
            "prepare",
        )?;
        self.prepared = true;
        // SAFETY: preparation succeeded and this owner retains both addresses.
        check(
            unsafe { waveOutWrite(output, self.header.get(), HEADER_BYTES) },
            "write",
        )
    }
}

pub struct Player {
    output: HWAVEOUT,
    buffers: [Option<Box<Buffer>>; BUFFERS],
    stopping: bool,
}

impl Player {
    pub fn open_after_explicit_consent(device: Option<u32>) -> Result<Self> {
        let mut output = null_mut();
        let format = pcm_format();
        // SAFETY: output and the local format live for the call. No callback,
        // window messages or OS-wide volume setting is requested.
        let result = unsafe {
            waveOutOpen(
                &mut output,
                device.unwrap_or(DEFAULT_OUTPUT),
                &format,
                0,
                0,
                CALLBACK_NULL,
            )
        };
        check(result, "open")?;
        ensure!(!output.is_null(), "Windows audio output handle unavailable");
        Ok(Self {
            output,
            buffers: std::array::from_fn(|_| Some(Buffer::new())),
            stopping: false,
        })
    }

    /// False means all four buffers are still busy. The caller drops this
    /// period instead of queuing delayed audio or writing a live buffer.
    pub fn submit(&mut self, samples: &[i16; SAMPLES], volume_percent: u8) -> Result<bool> {
        ensure!(!self.output.is_null() && !self.stopping, "Windows audio output stopped");
        ensure!(volume_percent <= 100, "audio volume outside limits");
        for buffer in self.buffers.iter_mut().flatten() {
            if buffer.retire(self.output)? {
                buffer.submit(self.output, samples, volume_percent)?;
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub fn stop(&mut self) -> Result<()> {
        self.stopping = true;
        if self.output.is_null() {
            return Ok(());
        }
        // SAFETY: this owner holds the output handle and all its queued memory.
        check(unsafe { waveOutReset(self.output) }, "reset")?;
        for buffer in self.buffers.iter_mut().flatten() {
            ensure!(buffer.retire(self.output)?, "Windows audio buffer still owned by device");
        }
        // SAFETY: every successfully prepared buffer has been retired.
        check(unsafe { waveOutClose(self.output) }, "close")?;
        self.output = null_mut();
        Ok(())
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        if self.stop().is_err() {
            // A failed device can retain native pointers. Only leak prepared
            // buffers in that terminal case instead of freeing live memory.
            for buffer in &mut self.buffers {
                if buffer.as_ref().is_some_and(|buffer| buffer.prepared) {
                    std::mem::forget(buffer.take());
                }
            }
        }
    }
}

fn pcm_format() -> WAVEFORMATEX {
    let block = (CHANNELS * size_of::<i16>()) as u16;
    WAVEFORMATEX {
        wFormatTag: WAVE_FORMAT_PCM as u16,
        nChannels: CHANNELS as u16,
        nSamplesPerSec: SAMPLE_RATE,
        nAvgBytesPerSec: SAMPLE_RATE * u32::from(block),
        nBlockAlign: block,
        wBitsPerSample: 16,
        cbSize: 0,
    }
}

fn scale_sample(sample: i16, volume_percent: u8) -> i16 {
    (i32::from(sample) * i32::from(volume_percent) / 100) as i16
}

fn check(result: u32, operation: &'static str) -> Result<()> {
    ensure!(result == MMSYSERR_NOERROR, "Windows audio {operation} failed ({result})");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_format_matches_the_wire_and_gain_is_local() {
        let format = pcm_format();
        let (channels, sample_rate, alignment, throughput) = (
            format.nChannels,
            format.nSamplesPerSec,
            format.nBlockAlign,
            format.nAvgBytesPerSec,
        );
        assert_eq!(channels, 2);
        assert_eq!(sample_rate, 48_000);
        assert_eq!(alignment, 4);
        assert_eq!(throughput, 192_000);
        assert_eq!(scale_sample(i16::MIN, 100), i16::MIN);
        assert_eq!(scale_sample(i16::MAX, 0), 0);
        assert_eq!(scale_sample(-120, 25), -30);
    }
}
