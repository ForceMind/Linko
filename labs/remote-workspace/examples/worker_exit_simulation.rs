//! Routes a local worker's OS exit through the workspace coordinator.
use std::ffi::OsStr;
use std::io::{self, Read, Write};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
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
const FIXTURE_READY: u8 = 1;
const WORKER_VERSION: u8 = 1;
const WORKER_START: u8 = 1;
const WORKER_READY: u8 = 2;
#[cfg(windows)]
const WORKER_STOP: u8 = 3;
#[cfg(windows)]
const WORKER_STOPPED: u8 = 4;
// Include LF and Ctrl-Z so the Windows test also checks binary pipe mode.
const WORKER_GENERATION: u64 = 0x0102_0a04_051a_0708;
const MEDIA_TRACK: MediaTrack = MediaTrack {
    generation: WORKER_GENERATION,
    track: 1,
    ssrc: 2,
};

type SimulationResult<T> = Result<T, Box<dyn std::error::Error>>;

const PIXELS: PixelSize = PixelSize {
    width: 2,
    height: 2,
};
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

    fn start_codec(binary: &Path, binding: Binding, generation: u64) -> io::Result<Self> {
        // The Actions launcher provides the path to its freshly built fixture.
        let mut command = Command::new(binary);
        command.args(["--worker-fixture", "80"]);
        let mut worker = Self::spawn(command, binding)?;
        worker.send_command(WORKER_START, generation)?;
        worker.expect_event(
            worker_record(WORKER_READY, generation),
            Duration::from_secs(90),
        )?;
        println!("Native worker: verified codec READY for this generation.");
        Ok(worker)
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
        let mut stdout = self.process.stdout.take().expect("worker stdout is piped");
        let (send, receive) = mpsc::sync_channel(1);
        let reader = thread::spawn(move || {
            let mut event = [0; N];
            let result = stdout.read_exact(&mut event).map(|()| event);
            let _ = send.send((stdout, result));
        });
        let (stdout, result) = receive.recv_timeout(timeout).map_err(io::Error::other)?;
        reader.join().expect("worker stdout reader panicked");
        self.process.stdout = Some(stdout);
        if result? != expected {
            return Err(io::Error::other("mismatched worker readiness event"));
        }
        Ok(())
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

fn worker_record(kind: u8, generation: u64) -> [u8; 16] {
    // This local record matches the native fixture's control channel.
    let mut record = [0; 16];
    record[..4].copy_from_slice(b"LRW4");
    record[4] = WORKER_VERSION;
    record[5] = kind;
    record[8..].copy_from_slice(&generation.to_be_bytes());
    record
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

fn register(c: &mut Coordinator, binding: Binding, object: u64) -> Result<ViewId, Error> {
    let view = c.register(
        SurfaceSpec {
            key: SurfaceKey {
                binding,
                object,
                generation: 1,
            },
            kind: SurfaceKind::Window,
            geometry: SourceGeometry {
                pixels: PIXELS,
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

fn queue(c: &mut Coordinator, view: ViewId, sequence: u64, now: u64) -> Result<(), Error> {
    let frame = RawFrame::new(c.frame_stamp(view, sequence)?, PIXELS, vec![0; 16])?;
    c.receive(view, frame, now)?;
    Ok(())
}

fn match_frame(
    c: &Coordinator,
    view: ViewId,
    matcher: &mut FrameMatcher,
    sequence: u32,
    now: u64,
) -> Result<(RawFrame, FrameMatch), Error> {
    let gate = c.view_gate(view)?;
    let media = MediaFrame {
        track: MEDIA_TRACK,
        timestamp: sequence,
    };
    let stamp = gate.frame_stamp(u64::from(sequence));
    assert!(matcher.decoded(gate, media, now)?.is_none());
    let matched = matcher
        .describe(gate, media, stamp, now)?
        .ok_or(Error::StaleFrame)?;
    let frame = RawFrame::new(stamp, PIXELS, vec![0; 16])?;
    Ok((frame, matched))
}

fn main() -> SimulationResult<()> {
    let mut args = std::env::args_os().skip(1);
    let codec = match args.next().as_deref() {
        None => None,
        Some(arg) if arg == OsStr::new(CHILD_ARG) => {
            run_child()?;
            return Ok(());
        }
        Some(arg) if arg == OsStr::new(CODEC_ARG) => Some(args.next().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "missing native codec fixture path",
            )
        })?),
        Some(_) => {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "unknown fixture mode").into());
        }
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
    let first = register(&mut c, binding, 1)?;
    let second = register(&mut c, binding, 2)?;
    let survivor = register(&mut c, independent, 1)?;
    c.grant_control(first, ControlScope::WholeDesktop, 0, 10_000)?;
    queue(&mut c, first, 1, 0)?;
    let initial = c.take_for_present(first, 0)?.ok_or(Error::StaleFrame)?;
    c.acknowledge_presentation(initial.ticket, 0)?;
    c.focus(first, 0)?;
    c.allow_parking(first, true, 0)?;
    let handoff = c.begin_handoff(first, BOUNDS, 0, 100)?;
    c.destination_ready(handoff, 0)?;
    c.acknowledge_hidden(handoff, 0)?;

    let mut matcher = FrameMatcher::new(c.view_gate(first)?, MEDIA_TRACK, 0, 0)?;
    let (frame, matched) = match_frame(&c, first, &mut matcher, 2, 1)?;
    c.receive_matched(first, frame, matched, 1)?;
    let waiting = c.take_for_present(first, 1)?.ok_or(Error::StaleFrame)?;
    let (late_frame, late_match) = match_frame(&c, first, &mut matcher, 3, 1)?;
    queue(&mut c, second, 1, 1)?;
    queue(&mut c, survivor, 1, 1)?;
    let mut worker = match codec.as_deref() {
        Some(binary) => {
            CoordinatedWorker::start_codec(Path::new(binary), binding, WORKER_GENERATION)?
        }
        None => CoordinatedWorker::start_fixture(binding)?,
    };
    assert!(worker.poll_exit(&mut c, 1)?.is_none());
    assert!(c.snapshot(first, 1)?.presentation_pending);
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
    let late_ack = c.acknowledge_presentation(waiting.ticket, 2);
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
        let mut next = CoordinatedWorker::start_codec(Path::new(binary), replacement, generation)?;
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
