//! Exercises OS process exit with synthetic coordinator fixtures.
use std::ffi::OsStr;
use std::io::{self, Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use linko_workspace_lab::coordinator::{Coordinator, Fault, SurfaceSpec, ViewId};
use linko_workspace_lab::geometry::{Camera, PixelSize, Point, Rect, Rotation, SourceGeometry};
use linko_workspace_lab::handoff::Command as WindowCommand;
use linko_workspace_lab::media::RawFrame;
use linko_workspace_lab::session::{ControlScope, SurfaceKind};
use linko_workspace_lab::{Binding, Error, SurfaceKey};

const CHILD_ARG: &str = "--fixture-child";
const READY: u8 = 1;
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

struct FixtureChild {
    process: Child,
}

impl FixtureChild {
    fn start() -> io::Result<Self> {
        let mut child = Self {
            process: Command::new(std::env::current_exe()?)
                .arg(CHILD_ARG)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()?,
        };
        let mut stdout = child.process.stdout.take().expect("fixture stdout is piped");
        let (send, receive) = mpsc::sync_channel(1);
        let reader = thread::spawn(move || {
            let mut ready = [0];
            let result = stdout.read_exact(&mut ready).map(|()| ready[0]);
            let _ = send.send(result);
        });
        let ready = receive
            .recv_timeout(Duration::from_secs(10))
            .map_err(io::Error::other)??;
        reader.join().expect("fixture stdout reader panicked");
        if ready != READY {
            return Err(io::Error::other("invalid fixture readiness byte"));
        }
        Ok(child)
    }
}

impl Drop for FixtureChild {
    fn drop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}

fn run_child() -> io::Result<()> {
    let mut stdout = io::stdout().lock();
    stdout.write_all(&[READY])?;
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

fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::args_os().nth(1).as_deref() == Some(OsStr::new(CHILD_ARG)) {
        run_child()?;
        return Ok(());
    }

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

    queue(&mut c, first, 2, 1)?;
    let waiting = c.take_for_present(first, 1)?.ok_or(Error::StaleFrame)?;
    queue(&mut c, second, 1, 1)?;
    queue(&mut c, survivor, 1, 1)?;
    let mut child = FixtureChild::start()?;
    assert!(child.process.try_wait()?.is_none(), "fixture exited before stop");
    assert!(c.snapshot(first, 1)?.presentation_pending);
    assert!(c.snapshot(second, 1)?.frame_queued);
    child.process.kill()?;
    assert!(!child.process.wait()?.success());

    assert_eq!(c.close_binding(binding, Fault::MediaWorkerExited, 2)?, 2);
    for view in [first, second] {
        let state = c.snapshot(view, 2)?;
        assert_eq!(state.fault, Some(Fault::MediaWorkerExited));
        assert!(!state.connected && !state.viewing_allowed);
        assert!(!state.frame_queued && !state.presentation_pending && !state.input_active);
    }
    let late_ack = c.acknowledge_presentation(waiting.ticket, 2);
    assert_eq!(late_ack, Err(Error::WrongAcknowledgement));
    assert!(c.pending_release().is_some());
    let restore = c.window_intent(first, 2)?.ok_or(Error::InvalidState)?;
    assert_eq!(restore.token, handoff);
    assert_eq!(restore.command, WindowCommand::RestoreSource(BOUNDS));
    assert_eq!(c.retire(first, 2), Err(Error::ReleasePending));
    assert!(c.snapshot(survivor, 2)?.viewing_allowed);
    let other = c.take_for_present(survivor, 2)?.ok_or(Error::StaleFrame)?;
    c.acknowledge_presentation(other.ticket, 2)?;

    println!("Worker exit: reaped child; two views cleared, recovery and second connection kept.");
    Ok(())
}
