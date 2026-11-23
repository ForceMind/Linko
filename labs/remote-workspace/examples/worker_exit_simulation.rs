//! Routes a local worker's OS exit through the workspace coordinator.
use std::ffi::OsStr;
use std::io::{self, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use linko_workspace_lab::coordinator::{Coordinator, Fault, SurfaceSpec, ViewId};
use linko_workspace_lab::frame_binding::{FrameMatch, FrameMatcher, MediaFrame, MediaTrack};
use linko_workspace_lab::geometry::{Camera, PixelSize, Point, Rect, Rotation, SourceGeometry};
use linko_workspace_lab::handoff::Command as WindowCommand;
use linko_workspace_lab::media::RawFrame;
use linko_workspace_lab::session::{ControlScope, SurfaceKind};
use linko_workspace_lab::{Binding, Error, SurfaceKey};

const CHILD_ARG: &str = "--fixture-child";
const CODEC_ARG: &str = "--native-codec";
const WINDOW_ARG: &str = "--native-window";
const FIXTURE_READY: u8 = 1;
const WORKER_VERSION: u8 = 1;
const WORKER_START: u8 = 1;
const WORKER_READY: u8 = 2;
#[cfg(windows)]
const WORKER_STOP: u8 = 3;
#[cfg(windows)]
const WORKER_STOPPED: u8 = 4;
const WORKER_FRAME: u8 = 5;
const WORKER_PRESENTED: u8 = 6;
// Include LF and Ctrl-Z so the Windows test also checks binary pipe mode.
const WORKER_GENERATION: u64 = 0x0102_0a04_051a_0708;
const MEDIA_TRACK: MediaTrack = MediaTrack {
    generation: WORKER_GENERATION,
    track: 1,
    ssrc: 2,
};

type SimulationResult<T> = Result<T, Box<dyn std::error::Error>>;

const FIXTURE_PIXELS: PixelSize = PixelSize {
    width: 2,
    height: 2,
};
const NATIVE_PIXELS: PixelSize = PixelSize {
    width: 128,
    height: 96,
};
const NATIVE_WIDTH: usize = NATIVE_PIXELS.width as usize;
const NATIVE_HEIGHT: usize = NATIVE_PIXELS.height as usize;
const NV12_BYTES: usize = NATIVE_WIDTH * NATIVE_HEIGHT * 3 / 2;

#[derive(Clone, Copy)]
enum FixturePixels {
    Stripes,
    Window,
}
const BOUNDS: Rect = Rect {
    x: 0.0,
    y: 0.0,
    width: 100.0,
    height: 100.0,
};

struct CoordinatedWorker {
    process: Child,
    binding: Option<Binding>,
}

impl CoordinatedWorker {
    fn spawn(mut command: Command, binding: Binding) -> io::Result<Self> {
        command.stdin(Stdio::piped()).stdout(Stdio::piped());
        let process = command.spawn()?;
        Ok(Self {
            process,
            binding: Some(binding),
        })
    }

    fn start_fixture(binding: Binding) -> io::Result<Self> {
        let mut command = Command::new(std::env::current_exe()?);
        command.arg(CHILD_ARG);
        let mut worker = Self::spawn(command, binding)?;
        worker.expect_event([FIXTURE_READY], Duration::from_secs(10))?;
        Ok(worker)
    }

    fn start_codec(
        binary: &Path,
        binding: Binding,
        generation: u64,
        fixture: FixturePixels,
    ) -> io::Result<(Self, [Vec<u8>; 2])> {
        // The Actions launcher provides the path to its freshly built fixture.
        let mut command = Command::new(binary);
        command.arg("--worker-pixels");
        let mut worker = Self::spawn(command, binding)?;
        worker.send_command(WORKER_START, generation)?;
        let frames = worker.read_event(Duration::from_secs(90), move |stdout| {
            let first = read_worker_frame(stdout, generation, 2, fixture)?;
            let second = read_worker_frame(stdout, generation, 3, fixture)?;
            expect_record(stdout, worker_record(WORKER_READY, generation))?;
            Ok([first, second])
        })?;
        println!("Native worker: verified two decoded frames and codec READY.");
        Ok((worker, frames))
    }

    fn send_command(&mut self, kind: u8, generation: u64) -> io::Result<()> {
        let input = self.process.stdin.as_mut().expect("worker stdin is piped");
        input.write_all(&worker_record(kind, generation))?;
        input.flush()
    }

    fn expect_event<const N: usize>(
        &mut self,
        expected: [u8; N],
        timeout: Duration,
    ) -> io::Result<()> {
        self.read_event(timeout, move |stdout| expect_record(stdout, expected))
    }

    fn read_event<T: Send + 'static>(
        &mut self,
        timeout: Duration,
        read: impl FnOnce(&mut ChildStdout) -> io::Result<T> + Send + 'static,
    ) -> io::Result<T> {
        let mut stdout = self.process.stdout.take().expect("worker stdout is piped");
        let (send, receive) = mpsc::sync_channel(1);
        let reader = thread::spawn(move || {
            let result = read(&mut stdout);
            let _ = send.send((stdout, result));
        });
        let (stdout, result) = receive.recv_timeout(timeout).map_err(io::Error::other)?;
        reader.join().expect("worker stdout reader panicked");
        self.process.stdout = Some(stdout);
        result
    }

    fn poll_exit(
        &mut self,
        coordinator: &mut Coordinator,
        now: u64,
    ) -> SimulationResult<Option<(ExitStatus, usize)>> {
        let Some(binding) = self.binding else {
            return Ok(None);
        };
        let Some(status) = self.process.try_wait()? else {
            return Ok(None);
        };
        let closed = coordinator.close_binding(binding, Fault::MediaWorkerExited, now)?;
        self.binding = None;
        Ok(Some((status, closed)))
    }

    fn await_exit(
        &mut self,
        coordinator: &mut Coordinator,
        now: u64,
    ) -> SimulationResult<(ExitStatus, usize)> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(result) = self.poll_exit(coordinator, now)? {
                return Ok(result);
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "worker did not exit").into());
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for CoordinatedWorker {
    fn drop(&mut self) {
        if self.binding.is_some() {
            let _ = self.process.kill();
            let _ = self.process.wait();
        }
    }
}

fn prove_window_pixels(
    binary: &Path,
    binding: Binding,
    generation: u64,
    rgba: &[u8],
) -> io::Result<()> {
    if rgba.len() != NATIVE_WIDTH * NATIVE_HEIGHT * 4 {
        return Err(invalid_worker_data("invalid renderer frame size"));
    }
    let mut command = Command::new(binary);
    command.arg("--present-pixels");
    let mut presenter = CoordinatedWorker::spawn(command, binding)?;
    presenter.send_command(WORKER_START, generation)?;
    let mut input = presenter
        .process
        .stdin
        .take()
        .expect("renderer stdin is piped");
    input.write_all(rgba)?;
    drop(input);
    presenter.expect_event(
        worker_record(WORKER_PRESENTED, generation),
        Duration::from_secs(15),
    )?;
    if !presenter.process.wait()?.success() {
        return Err(io::Error::other(
            "native presenter did not exit successfully",
        ));
    }
    Ok(())
}

fn worker_record(kind: u8, generation: u64) -> [u8; 16] {
    // This local record matches the native fixture's control channel.
    let mut record = [0; 16];
    record[..4].copy_from_slice(b"LRW4");
    record[4] = WORKER_VERSION;
    record[5] = kind;
    record[8..].copy_from_slice(&generation.to_be_bytes());
    record
}

fn invalid_worker_data(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn expect_record<const N: usize>(stdout: &mut impl Read, expected: [u8; N]) -> io::Result<()> {
    let mut event = [0; N];
    stdout.read_exact(&mut event)?;
    if event != expected {
        return Err(invalid_worker_data("unexpected worker event"));
    }
    Ok(())
}

fn read_worker_frame(
    stdout: &mut impl Read,
    generation: u64,
    sequence: u32,
    fixture: FixturePixels,
) -> io::Result<Vec<u8>> {
    expect_record(stdout, worker_record(WORKER_FRAME, generation))?;
    let mut header = [0; 12];
    stdout.read_exact(&mut header)?;
    if header[..4] != sequence.to_be_bytes()
        || header[4..6] != (NATIVE_WIDTH as u16).to_be_bytes()
        || header[6..8] != (NATIVE_HEIGHT as u16).to_be_bytes()
        || header[8..] != (NV12_BYTES as u32).to_be_bytes()
    {
        return Err(invalid_worker_data("invalid decoded frame header"));
    }
    let mut nv12 = vec![0; NV12_BYTES];
    stdout.read_exact(&mut nv12)?;

    let rgba = nv12_rgba(&nv12);
    let valid = match fixture {
        FixturePixels::Window => matches_window_color(&rgba, sequence == 3),
        FixturePixels::Stripes => {
            let expected = if sequence == 2 { 24 } else { 32 };
            let middle_row = NATIVE_HEIGHT / 2 * NATIVE_WIDTH;
            let left = i32::from(nv12[middle_row + NATIVE_WIDTH / 2]);
            let right = i32::from(nv12[middle_row + NATIVE_WIDTH * 7 / 8]);
            (left - expected).abs() <= 6 && (right - (232 - expected)).abs() <= 6
        }
    };
    if !valid {
        return Err(invalid_worker_data("unexpected decoded fixture pixels"));
    }
    Ok(rgba)
}

fn matches_window_color(rgba: &[u8], resized: bool) -> bool {
    let center = (NATIVE_HEIGHT / 2 * NATIVE_WIDTH + NATIVE_WIDTH / 2) * 4;
    let expected = if resized {
        [30, 70, 190]
    } else {
        [190, 30, 70]
    };
    rgba[center..center + 3]
        .iter()
        .zip(expected)
        .all(|(&decoded, expected)| decoded.abs_diff(expected) <= 24)
        && rgba[center + 3] == 255
}

// The bounded SDR fixture uses the limited-range BT.601 matrix.
fn nv12_rgba(nv12: &[u8]) -> Vec<u8> {
    let (luma, chroma) = nv12.split_at(NATIVE_WIDTH * NATIVE_HEIGHT);
    let mut rgba = Vec::with_capacity(luma.len() * 4);
    let channel = |value: i32| ((value + 128) >> 8).clamp(0, 255) as u8;
    for (index, &y) in luma.iter().enumerate() {
        let row = index / NATIVE_WIDTH;
        let column = index % NATIVE_WIDTH;
        let uv = row / 2 * NATIVE_WIDTH + (column & !1);
        let c = 298 * (i32::from(y) - 16);
        let d = i32::from(chroma[uv]) - 128;
        let e = i32::from(chroma[uv + 1]) - 128;
        rgba.extend_from_slice(&[
            channel(c + 409 * e),
            channel(c - 100 * d - 208 * e),
            channel(c + 516 * d),
            255,
        ]);
    }
    rgba
}

fn run_child() -> io::Result<()> {
    let mut stdout = io::stdout().lock();
    stdout.write_all(&[FIXTURE_READY])?;
    stdout.flush()?;
    if io::stdin().read(&mut [0])? != 0 {
        return Err(io::Error::other("unexpected fixture input"));
    }
    Ok(())
}

fn register(
    c: &mut Coordinator,
    binding: Binding,
    object: u64,
    pixels: PixelSize,
) -> Result<ViewId, Error> {
    let view = c.register(
        SurfaceSpec {
            key: SurfaceKey {
                binding,
                object,
                generation: 1,
            },
            kind: SurfaceKind::Window,
            geometry: SourceGeometry {
                pixels,
                logical: BOUNDS,
                rotation: Rotation::None,
                revision: 1,
            },
            camera: Camera {
                origin: Point { x: 0.0, y: 0.0 },
                zoom: 1.0,
            },
            panel: BOUNDS,
        },
        0,
    )?;
    c.grant_view(view, 0, 10_000)?;
    Ok(view)
}

fn queue(
    c: &mut Coordinator,
    view: ViewId,
    sequence: u64,
    now: u64,
    pixels: PixelSize,
) -> Result<(), Error> {
    let bytes = usize::try_from(pixels.rgba_bytes()?).map_err(|_| Error::Capacity)?;
    let frame = RawFrame::new(c.frame_stamp(view, sequence)?, pixels, vec![0; bytes])?;
    c.receive(view, frame, now)?;
    Ok(())
}

fn match_frame(
    c: &Coordinator,
    view: ViewId,
    matcher: &mut FrameMatcher,
    frame: &RawFrame,
    now: u64,
) -> Result<FrameMatch, Error> {
    let gate = c.view_gate(view)?;
    let media = MediaFrame {
        track: MEDIA_TRACK,
        timestamp: u32::try_from(frame.sequence()).map_err(|_| Error::InvalidSequence)?,
    };
    assert!(matcher.decoded(gate, media, now)?.is_none());
    matcher
        .describe(gate, media, frame.stamp(), now)?
        .ok_or(Error::StaleFrame)
}

fn main() -> SimulationResult<()> {
    let mut args = std::env::args_os().skip(1);
    let (codec, fixture) = match args.next().as_deref() {
        None => (None, FixturePixels::Stripes),
        Some(arg) if arg == OsStr::new(CHILD_ARG) => {
            run_child()?;
            return Ok(());
        }
        Some(arg) if arg == OsStr::new(CODEC_ARG) || arg == OsStr::new(WINDOW_ARG) => {
            let fixture = if arg == OsStr::new(WINDOW_ARG) {
                FixturePixels::Window
            } else {
                FixturePixels::Stripes
            };
            let binary = args
                .next()
                .ok_or_else(|| invalid_worker_data("missing native codec fixture path"))?;
            (Some(binary), fixture)
        }
        Some(_) => {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "unknown fixture mode").into());
        }
    };

    let size = if codec.is_some() {
        NATIVE_PIXELS
    } else {
        FIXTURE_PIXELS
    };
    let binding = Binding {
        source: [1; 32],
        viewer: [2; 32],
        nonce: [3; 16],
    };
    let independent = Binding {
        nonce: [4; 16],
        ..binding
    };
    let mut c = Coordinator::new(binding.viewer, 3)?;
    let first = register(&mut c, binding, 1, size)?;
    let second = register(&mut c, binding, 2, FIXTURE_PIXELS)?;
    let survivor = register(&mut c, independent, 1, FIXTURE_PIXELS)?;
    c.grant_control(first, ControlScope::WholeDesktop, 0, 10_000)?;
    queue(&mut c, first, 1, 0, size)?;
    let initial = c.take_for_present(first, 0)?.ok_or(Error::StaleFrame)?;
    c.acknowledge_presentation(initial.ticket, 0)?;
    c.focus(first, 0)?;
    c.allow_parking(first, true, 0)?;
    let handoff = c.begin_handoff(first, BOUNDS, 0, 100)?;
    c.destination_ready(handoff, 0)?;
    c.acknowledge_hidden(handoff, 0)?;

    let (mut worker, [pixels, late_pixels]) = match codec.as_deref() {
        Some(binary) => {
            CoordinatedWorker::start_codec(Path::new(binary), binding, WORKER_GENERATION, fixture)?
        }
        None => (
            CoordinatedWorker::start_fixture(binding)?,
            [vec![0; 16], vec![0; 16]],
        ),
    };
    let mut matcher = FrameMatcher::new(c.view_gate(first)?, MEDIA_TRACK, 0, 0)?;
    let frame = RawFrame::new(c.frame_stamp(first, 2)?, size, pixels)?;
    let matched = match_frame(&c, first, &mut matcher, &frame, 1)?;
    c.receive_matched(first, frame, matched, 1)?;
    let waiting = c.take_for_present(first, 1)?.ok_or(Error::StaleFrame)?;
    if codec.is_some() {
        let rgba = waiting.frame.pixels();
        match fixture {
            FixturePixels::Window => assert!(matches_window_color(rgba, false)),
            FixturePixels::Stripes => {
                let middle = NATIVE_HEIGHT / 2 * NATIVE_WIDTH;
                let dark = (middle + NATIVE_WIDTH / 2) * 4;
                let bright = (middle + NATIVE_WIDTH * 7 / 8) * 4;
                assert!(rgba[dark] < 32 && rgba[bright] > 175);
                assert_eq!(rgba[bright + 3], 255);
            }
        }
        println!("Native worker: coordinator returned a decoded 128x96 frame.");
    }
    let late_frame = RawFrame::new(c.frame_stamp(first, 3)?, size, late_pixels)?;
    let late_match = match_frame(&c, first, &mut matcher, &late_frame, 1)?;
    queue(&mut c, second, 1, 1, FIXTURE_PIXELS)?;
    queue(&mut c, survivor, 1, 1, FIXTURE_PIXELS)?;
    assert!(worker.poll_exit(&mut c, 1)?.is_none());
    assert!(c.snapshot(first, 1)?.presentation_pending);
    let late_ticket = if let (Some(binary), FixturePixels::Window) = (codec.as_deref(), fixture) {
        prove_window_pixels(
            Path::new(binary),
            binding,
            WORKER_GENERATION,
            waiting.frame.pixels(),
        )?;
        c.acknowledge_presentation(waiting.ticket, 1)?;
        assert!(!c.snapshot(first, 1)?.presentation_pending);
        println!("Native worker: coordinator accepted the verified presentation receipt.");

        let pending = c.take_for_present(second, 1)?.ok_or(Error::StaleFrame)?;
        queue(&mut c, second, 2, 1, FIXTURE_PIXELS)?;
        pending.ticket
    } else {
        waiting.ticket
    };
    assert!(c.snapshot(second, 1)?.frame_queued);
    worker.process.kill()?;
    let (status, closed) = worker.await_exit(&mut c, 2)?;
    assert!(!status.success());
    assert_eq!(closed, 2);
    assert!(worker.poll_exit(&mut c, 2)?.is_none());
    for view in [first, second] {
        let state = c.snapshot(view, 2)?;
        assert_eq!(state.fault, Some(Fault::MediaWorkerExited));
        assert!(!state.connected && !state.viewing_allowed);
        assert!(!state.frame_queued && !state.presentation_pending && !state.input_active);
    }
    let late_ack = c.acknowledge_presentation(late_ticket, 2);
    assert_eq!(late_ack, Err(Error::WrongAcknowledgement));
    let late_delivery = c.receive_matched(first, late_frame, late_match, 2);
    assert_eq!(late_delivery, Err(Error::StaleTarget));
    let matcher_expired = matcher.expire(c.view_gate(first)?, 2);
    assert_eq!(matcher_expired, Err(Error::StaleTarget));
    assert!(c.pending_release().is_some());
    let restore = c.window_intent(first, 2)?.ok_or(Error::InvalidState)?;
    assert_eq!(restore.token, handoff);
    assert_eq!(restore.command, WindowCommand::RestoreSource(BOUNDS));
    assert_eq!(c.retire(first, 2), Err(Error::ReleasePending));
    assert!(c.snapshot(survivor, 2)?.viewing_allowed);
    let other = c.take_for_present(survivor, 2)?.ok_or(Error::StaleFrame)?;
    c.acknowledge_presentation(other.ticket, 2)?;

    println!("Worker exit: views cleared, late frame rejected, recovery and connection kept.");
    #[cfg(windows)]
    if let Some(binary) = codec.as_deref() {
        let replacement = Binding {
            nonce: [5; 16],
            ..binding
        };
        let generation = WORKER_GENERATION + 1;
        let (mut next, _) =
            CoordinatedWorker::start_codec(Path::new(binary), replacement, generation, fixture)?;
        next.send_command(WORKER_STOP, generation)?;
        next.expect_event(
            worker_record(WORKER_STOPPED, generation),
            Duration::from_secs(10),
        )?;
        let (status, closed) = next.await_exit(&mut c, 3)?;
        assert!(status.success());
        assert_eq!(closed, 0);
        assert!(c.snapshot(survivor, 3)?.viewing_allowed);
        println!("Native worker: verified Windows replacement and clean STOPPED.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_wrong_generation_oversize_and_truncation() {
        let mut packet = worker_record(WORKER_FRAME, WORKER_GENERATION).to_vec();
        packet.extend_from_slice(&2_u32.to_be_bytes());
        packet.extend_from_slice(&(NATIVE_WIDTH as u16).to_be_bytes());
        packet.extend_from_slice(&(NATIVE_HEIGHT as u16).to_be_bytes());
        packet.extend_from_slice(&(NV12_BYTES as u32).to_be_bytes());

        let read = |mut bytes: &[u8], generation| {
            read_worker_frame(&mut bytes, generation, 2, FixturePixels::Stripes)
        };
        let wrong_generation = read(&packet, WORKER_GENERATION + 1);
        assert_eq!(
            wrong_generation.unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        let truncated = read(&packet, WORKER_GENERATION);
        assert_eq!(truncated.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
        packet[24] = 0xff;
        let oversized = read(&packet, WORKER_GENERATION);
        assert_eq!(oversized.unwrap_err().kind(), io::ErrorKind::InvalidData);
    }
}
