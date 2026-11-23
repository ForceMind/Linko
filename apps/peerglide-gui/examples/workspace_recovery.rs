//! RW09 restart experiment with an independently owned simulated window.
use std::io::{self, BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use anyhow::{bail, ensure, Context, Result};
use linko_workspace_lab::coordinator::{Coordinator, HandoffToken, SurfaceSpec, ViewId};
use linko_workspace_lab::geometry::{Camera, PixelSize, Point, Rect, Rotation, SourceGeometry};
use linko_workspace_lab::handoff::Command as WindowCommand;
use linko_workspace_lab::media::RawFrame;
use linko_workspace_lab::session::{ControlScope, SurfaceKind};
use linko_workspace_lab::{Binding, SurfaceKey};

#[path = "workspace_recovery/journal.rs"]
mod journal;
#[path = "../src/test_fixture.rs"]
mod test_fixture;
use journal::{Journal, WindowInstance, FILENAME};
use test_fixture::Fixture;

const WAIT: Duration = Duration::from_secs(10);
const OWNER: WindowInstance = WindowInstance {
    backend: [1; 16],
    object: 7,
};
const BOUNDS: Rect = Rect {
    x: -96.0,
    y: 18.0,
    width: 750.0,
    height: 550.0,
};

fn handoff() -> Result<(Coordinator, ViewId, HandoffToken)> {
    let mut coordinator = Coordinator::new([2; 32], 1)?;
    let size = PixelSize { width: 2, height: 2 };
    let view = coordinator.register(
        SurfaceSpec {
            key: SurfaceKey {
                binding: Binding {
                    source: [1; 32],
                    viewer: [2; 32],
                    nonce: [3; 16],
                },
                object: 1,
                generation: 1,
            },
            kind: SurfaceKind::Window,
            geometry: SourceGeometry {
                pixels: size,
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
    coordinator.grant_view(view, 0, 1_000)?;
    coordinator.grant_control(view, ControlScope::WholeDesktop, 0, 1_000)?;
    let frame = RawFrame::new(coordinator.frame_stamp(view, 1)?, size, vec![0; 16])?;
    coordinator.receive(view, frame, 0)?;
    let frame = coordinator.take_for_present(view, 0)?.context("fixture frame")?;
    coordinator.acknowledge_presentation(frame.ticket, 0)?;
    coordinator.allow_parking(view, true, 0)?;
    let token = coordinator.begin_handoff(view, BOUNDS, 0, 500)?;
    coordinator.destination_ready(token, 0)?;
    Ok((coordinator, view, token))
}

fn worker_hide(directory: &Path, input: &mut impl BufRead, output: &mut impl Write) -> Result<()> {
    let mut journal = Journal::open(directory)?;
    let (mut coordinator, view, token) = handoff()?;
    let current = coordinator.window_intent(view, 0)?.context("hide intent")?;
    ensure!(current.token == token && current.command == WindowCommand::HideSource);
    send(output, "prepared")?;
    ensure!(read(input)? == "arm");
    let record = journal.arm(OWNER, BOUNDS)?;
    send(output, &format!("armed {}", record.transaction))?;
    ensure!(read(input)? == "hide");
    ensure!(coordinator.window_intent(view, 1)? == Some(current));
    // The simulated native owner validates this request independently and
    // holds the window state even if this process is killed.
    send(output, &format!("hide {}", record.transaction))?;
    ensure!(read(input)? == format!("hidden {}", record.transaction));
    coordinator.acknowledge_hidden(token, 2)?;
    send(output, "parked")?;
    ensure!(read(input)? == "exit");
    Ok(())
}

fn worker_recover(directory: &Path, input: &mut impl BufRead, output: &mut impl Write) -> Result<()> {
    let mut journal = Journal::open(directory)?;
    let Some(pending) = journal.pending()? else {
        send(output, "clean")?;
        return Ok(());
    };
    send(output, &format!("pending {}", pending.transaction))?;
    let observed = match read(input)?.as_str() {
        "same-window" => OWNER,
        "replaced-window" => WindowInstance { object: 8, ..OWNER },
        _ => bail!("invalid simulated window observation"),
    };
    let record = journal.recovery_for(observed)?;
    send(output, &format!("restore {}", record.transaction))?;
    let evidence = read(input)?;
    let confirmed_transaction = evidence
        .strip_prefix("restored ")
        .context("native restore was not confirmed")?
        .parse::<u64>()?;
    send(output, "before-clear")?;
    ensure!(read(input)? == "clear");
    journal.confirm(observed, confirmed_transaction)?;
    send(output, "cleared")
}

fn send(output: &mut impl Write, line: &str) -> Result<()> {
    writeln!(output, "{line}")?;
    output.flush()?;
    Ok(())
}

fn read(input: &mut impl BufRead) -> Result<String> {
    let mut line = String::new();
    ensure!(input.read_line(&mut line)? != 0, "fixture control pipe closed");
    Ok(line.trim_end().to_owned())
}

struct Worker {
    process: Child,
    messages: Receiver<Result<String>>,
}

impl Worker {
    fn start(directory: &Path, action: &str) -> Result<Self> {
        let mut process = Command::new(std::env::current_exe()?)
            .args(["--worker", action])
            .arg(directory)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        let mut output = BufReader::new(process.stdout.take().expect("piped stdout"));
        let (send, messages) = mpsc::sync_channel(1);
        std::thread::spawn(move || loop {
            let message = read(&mut output);
            let ended = message.is_err();
            if send.send(message).is_err() || ended {
                break;
            }
        });
        Ok(Self { process, messages })
    }

    fn write(&mut self, line: &str) -> Result<()> {
        send(
            self.process.stdin.as_mut().context("closed worker input")?,
            line,
        )
    }

    fn expect(&self, expected: &str) -> Result<()> {
        let actual = self.messages.recv_timeout(WAIT)??;
        ensure!(actual == expected, "expected {expected}, received {actual}");
        Ok(())
    }

    fn wait(&mut self, success: bool) -> Result<()> {
        let deadline = Instant::now() + WAIT;
        loop {
            if let Some(status) = self.process.try_wait()? {
                ensure!(status.success() == success, "unexpected fixture exit {status}");
                return Ok(());
            }
            ensure!(Instant::now() < deadline, "fixture did not exit");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn crash(&mut self) -> Result<()> {
        ensure!(self.process.try_wait()?.is_none(), "fixture already exited");
        self.process.kill()?;
        self.wait(false)
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        if self.process.try_wait().ok().flatten().is_none() {
            let _ = self.process.kill();
            let _ = self.process.wait();
        }
    }
}

#[derive(Clone, Copy)]
enum WindowState {
    Untouched,
    Parked(u64),
    Restored(u64),
}

struct Platform {
    state: WindowState,
    bounds: Rect,
    restore_count: u8,
}

impl Platform {
    fn restore(&mut self, transaction: u64, original: Rect) -> Result<()> {
        match self.state {
            WindowState::Untouched => ensure!(self.bounds == original),
            WindowState::Parked(active) if active == transaction => {
                self.bounds = original;
                self.state = WindowState::Restored(transaction);
                self.restore_count += 1;
            }
            WindowState::Restored(previous) if previous == transaction => {}
            _ => bail!("simulated native owner rejected the stale restore"),
        }
        Ok(())
    }
}

fn start_recovery(directory: &Path, transaction: u64) -> Result<Worker> {
    let mut worker = Worker::start(directory, "recover")?;
    worker.expect(&format!("pending {transaction}"))?;
    worker.write("same-window")?;
    worker.expect(&format!("restore {transaction}"))?;
    Ok(worker)
}

fn check_clear(worker: &mut Worker) -> Result<()> {
    worker.expect("before-clear")?;
    worker.write("clear")?;
    worker.expect("cleared")?;
    worker.wait(true)
}

fn smoke() -> Result<()> {
    let fixture = Fixture::new()?;
    let directory = &fixture.path;
    let path = directory.join(FILENAME);
    let mut native = Platform {
        state: WindowState::Untouched,
        bounds: BOUNDS,
        restore_count: 0,
    };

    let mut worker = Worker::start(directory, "hide")?;
    worker.expect("prepared")?;
    ensure!(Journal::open(directory).is_err(), "source owner lock missing");
    worker.crash()?;
    ensure!(!path.exists() && Journal::open(directory)?.pending()?.is_none());
    println!("Recovery journal: crash before commit left no obligation.");

    let mut worker = Worker::start(directory, "hide")?;
    worker.expect("prepared")?;
    worker.write("arm")?;
    worker.expect("armed 1")?;
    worker.crash()?;
    let record = Journal::open(directory)?.recovery_for(OWNER)?;
    ensure!(record.original_bounds() == BOUNDS);
    let mut worker = start_recovery(directory, 1)?;
    native.restore(1, record.original_bounds())?;
    worker.write("restored 1")?;
    check_clear(&mut worker)?;
    ensure!(native.restore_count == 0);
    println!("Recovery journal: committed intent survived without applying a restore.");

    let mut worker = Worker::start(directory, "hide")?;
    worker.expect("prepared")?;
    worker.write("arm")?;
    worker.expect("armed 2")?;
    worker.write("hide")?;
    worker.expect("hide 2")?;
    // This parent plays the native owner. Its state survives worker crashes.
    native.state = WindowState::Parked(2);
    native.bounds.x = 200.0;
    worker.write("hidden 2")?;
    worker.expect("parked")?;
    worker.crash()?;
    ensure!(matches!(native.state, WindowState::Parked(2)));
    let before_failure = std::fs::read(&path)?;

    let mut worker = Worker::start(directory, "recover")?;
    worker.expect("pending 2")?;
    worker.write("replaced-window")?;
    worker.wait(false)?;
    let mut worker = start_recovery(directory, 2)?;
    worker.write("restore-failed")?;
    worker.wait(false)?;
    {
        let mut journal = Journal::open(directory)?;
        ensure!(journal.confirm(OWNER, 1).is_err());
        ensure!(journal.arm(OWNER, BOUNDS).is_err());
    }
    ensure!(std::fs::read(&path)? == before_failure);
    ensure!(matches!(native.state, WindowState::Parked(2)));
    println!("Recovery journal: wrong instance, failure and stale ACK retained the obligation.");

    let record = Journal::open(directory)?.recovery_for(OWNER)?;
    let mut worker = start_recovery(directory, 2)?;
    native.restore(2, record.original_bounds())?;
    worker.write("restored 2")?;
    worker.expect("before-clear")?;
    worker.crash()?;
    ensure!(native.bounds == BOUNDS && native.restore_count == 1);
    ensure!(std::fs::read(&path)? == before_failure);
    // A subsequent user move must survive idempotent startup repair.
    native.bounds.x += 24.0;
    let user_position = native.bounds;
    let mut worker = start_recovery(directory, 2)?;
    native.restore(2, record.original_bounds())?;
    worker.write("restored 2")?;
    check_clear(&mut worker)?;
    ensure!(native.restore_count == 1 && native.bounds == user_position);

    let mut worker = Worker::start(directory, "recover")?;
    worker.expect("clean")?;
    worker.wait(true)?;
    let third = {
        let mut journal = Journal::open(directory)?;
        let record = journal.arm(OWNER, user_position)?;
        ensure!(record.transaction == 3);
        ensure!(journal.confirm(OWNER, 2).is_err());
        record
    };
    native.state = WindowState::Untouched;
    let mut worker = start_recovery(directory, 3)?;
    native.restore(3, third.original_bounds())?;
    worker.write("restored 3")?;
    check_clear(&mut worker)?;
    for file in std::fs::read_dir(directory)? {
        ensure!(!file?.file_name().to_string_lossy().contains(".bak-"));
    }
    let future = br#"{"store_version":2,"data":{}}"#;
    std::fs::write(&path, future)?;
    ensure!(Journal::open(directory).is_err() && std::fs::read(&path)? == future);
    println!("Recovery journal: hard kill after restore, idempotent restart and clean re-open verified.");
    fixture.close()
}

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    match args.as_slice() {
        [mode] if mode == "--smoke" => smoke(),
        [mode, action, path] if mode == "--worker" => {
            let mut input = io::stdin().lock();
            let mut output = io::stdout().lock();
            match action.to_str() {
                Some("hide") => worker_hide(Path::new(path), &mut input, &mut output),
                Some("recover") => worker_recover(Path::new(path), &mut input, &mut output),
                _ => bail!("unknown recovery fixture action"),
            }
        }
        _ => bail!("use --smoke to run the source recovery experiment"),
    }
}
