//! Owned private pairing child. Observations here are NOT enrollment authority.
//! No secret line enters the ordinary GUI logger, and no disk write occurs.
use anyhow::{Context, Result};
use peerglide_protocol::pairing_worker::{self as ipc, Failure, PairingCommand, PairingEvent, State, StopReason};
use peerglide_transport::{fingerprint, read_certificate, validate_certificate};
use std::{
    io::{self, BufReader, Write}, net::SocketAddr, path::Path,
    process::{Child, Command, Stdio}, sync::{atomic::{AtomicBool, Ordering}, mpsc, Arc},
    time::{Duration, Instant},
};

pub enum Mode {
    Offer(SocketAddr),
    Connect { address: SocketAddr, code: PairingCommand },
}
impl Mode {
    pub fn connect(address: SocketAddr, code: String) -> Result<Self> {
        Ok(Self::Connect { address, code: PairingCommand::code(code).context("请输入八位数字配对码")? })
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Phase { Idle, Starting, Listening, Verifying, Verified, Stopping, Stopped(StopReason), Failed(Failure) }
pub struct Observation {
    certificate: Vec<u8>,
    context: [u8; 32],
}
impl Observation {
    pub fn certificate(&self) -> &[u8] { &self.certificate }
    pub fn context(&self) -> [u8; 32] { self.context }
}

struct Process {
    child: Child,
    generation: u64,
    input: mpsc::SyncSender<PairingCommand>,
    events: mpsc::Receiver<(u64, PairingEvent)>,
    stop: Arc<AtomicBool>,
    faulty: Arc<AtomicBool>,
    stdout_done: Arc<AtomicBool>,
    stderr_done: Arc<AtomicBool>,
    started: Instant,
    stopping: Option<Instant>,
    local_certificate: Vec<u8>,
    offering: bool,
    remaining: u8,
}
impl Process {
    fn request_stop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = self.input.try_send(PairingCommand::stop());
        self.stopping.get_or_insert_with(Instant::now);
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        self.request_stop();
        // This command family cannot enable native input or perform enrollment.
        // Never use this fallback for the ordinary input worker.
        if !matches!(self.child.try_wait(), Ok(Some(_))) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

pub struct Supervisor {
    generation: u64,
    process: Option<Process>,
    phase: Phase,
    listening: Option<PairingEvent>,
    observed: Option<Observation>,
}
impl Default for Supervisor {
    fn default() -> Self {
        Self { generation: 0, process: None, phase: Phase::Idle, listening: None, observed: None }
    }
}
impl Supervisor {
    pub fn phase(&self) -> Phase { self.phase }
    pub fn generation(&self) -> u64 { self.generation }
    pub fn needs_poll(&self) -> bool { self.process.is_some() }
    pub fn local_certificate_snapshot(&self) -> Option<&[u8]> {
        let process = self.process.as_ref()?;
        (process.generation == self.generation).then_some(process.local_certificate.as_slice())
    }
    pub fn displayed_code(&self) -> Option<(&str, SocketAddr)> {
        let process = self.process.as_ref()?;
        if process.generation != self.generation || process.stop.load(Ordering::Acquire)
            || process.faulty.load(Ordering::Acquire) || process.stdout_done.load(Ordering::Acquire)
            || process.started.elapsed() >= Duration::from_secs(120) { return None; }
        if matches!(self.phase, Phase::Stopping | Phase::Stopped(_) | Phase::Failed(_)) { return None; }
        match &self.listening.as_ref()?.event {
            State::Listening { code, address } => Some((code, *address)), _ => None,
        }
    }
    /// An observation only. Persistent commit needs its own live protocol barrier.
    pub fn observation(&self) -> Option<&Observation> {
        let process = self.process.as_ref()?;
        if self.phase != Phase::Verified || process.generation != self.generation
            || process.stop.load(Ordering::Acquire) || process.faulty.load(Ordering::Acquire)
            || process.stdout_done.load(Ordering::Acquire)
            || process.started.elapsed() >= Duration::from_secs(120) { return None; }
        self.observed.as_ref()
    }
    pub fn start(&mut self, executable: &Path, identity: &Path, mode: Mode, wake: Arc<dyn Fn() + Send + Sync>) -> Result<()> {
        anyhow::ensure!(self.process.is_none(), "请先等待上一次配对进程退出");
        let generation = self.generation.checked_add(1).context("配对代次已用尽，请重新打开")?;
        let local_certificate = read_certificate(&identity.join("node.crt.der"))?;
        let mut command = Command::new(executable);
        let initial = match mode {
            Mode::Offer(bind) => { command.arg("pair-offer").arg("--bind").arg(bind.to_string()); None }
            Mode::Connect { address, code } => {
                anyhow::ensure!(code.entered_code().is_some(), "无效的配对码命令");
                command.arg("pair-connect").arg(address.to_string()); Some(code)
            }
        };
        let offering = initial.is_none();
        command.arg("--identity").arg(identity).arg("--managed")
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let started = Instant::now();
        let mut child = command.spawn().context("无法启动配对进程")?;
        let stop = Arc::new(AtomicBool::new(false));
        let faulty = Arc::new(AtomicBool::new(false));
        let out_done = Arc::new(AtomicBool::new(false));
        let err_done = Arc::new(AtomicBool::new(false));
        let setup = (|| -> Result<_> {
            let mut input = child.stdin.take().context("缺少配对输入管道")?;
            let output = child.stdout.take().context("缺少配对输出管道")?;
            let mut errors = child.stderr.take().context("缺少配对诊断管道")?;
            let (commands, rx) = mpsc::sync_channel::<PairingCommand>(1);
            let (events, received) = mpsc::sync_channel::<(u64, PairingEvent)>(8);
            let halted = stop.clone(); let fault = faulty.clone(); let repaint = wake.clone();
            std::thread::Builder::new().name("pairing-parent-input".into()).spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> io::Result<()> {
                    loop {
                        let command = if halted.load(Ordering::Acquire) { PairingCommand::stop() } else {
                            match rx.recv_timeout(Duration::from_millis(25)) {
                                Ok(command) => command,
                                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                                Err(_) => PairingCommand::stop(),
                            }
                        };
                        let terminal = command.is_stop();
                        let mut line = command.line(); line.push('\n');
                        input.write_all(line.as_bytes())?; input.flush()?;
                        if terminal { return Ok(()); }
                    }
                }));
                if !matches!(result, Ok(Ok(()))) { fault.store(true, Ordering::Release); }
                repaint();
            })?;
            let halted = stop.clone(); let fault = faulty.clone(); let done = out_done.clone(); let repaint = wake.clone();
            std::thread::Builder::new().name("pairing-parent-output".into()).spawn(move || {
                let mut reader = BufReader::new(output);
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> io::Result<()> {
                    while let Some(event) = ipc::read_event(&mut reader)? {
                        if events.try_send((generation, event)).is_err() { return Err(io::Error::other("pairing inbox unavailable")); }
                        repaint();
                    }
                    Ok(())
                }));
                if !matches!(result, Ok(Ok(()))) {
                    fault.store(true, Ordering::Release); halted.store(true, Ordering::Release);
                    repaint();
                    // Continue draining without retaining or logging secret records.
                    let _ = io::copy(&mut reader, &mut io::sink());
                }
                done.store(true, Ordering::Release); repaint();
            })?;
            let fault = faulty.clone(); let done = err_done.clone(); let repaint = wake.clone();
            std::thread::Builder::new().name("pairing-parent-diagnostics".into()).spawn(move || {
                // Never forward raw child diagnostics into the GUI log history.
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| io::copy(&mut errors, &mut io::sink())));
                if !matches!(result, Ok(Ok(_))) { fault.store(true, Ordering::Release); }
                done.store(true, Ordering::Release); repaint();
            })?;
            if let Some(command) = initial { commands.try_send(command).map_err(|_| anyhow::anyhow!("无法提交配对码"))?; }
            Ok((commands, received))
        })();
        let (input, events) = match setup { Ok(value) => value, Err(error) => {
            stop.store(true, Ordering::Release); let _ = child.kill(); let _ = child.wait(); return Err(error);
        }};
        self.generation = generation;
        self.listening = None; self.observed = None; self.phase = Phase::Starting;
        self.process = Some(Process { child, generation, input, events, stop, faulty,
            stdout_done: out_done, stderr_done: err_done, started, stopping: None,
            local_certificate, offering, remaining: 5 });
        Ok(())
    }
    pub fn cancel(&mut self) {
        self.generation = self.generation.saturating_add(1);
        self.listening = None; self.observed = None;
        if let Some(process) = self.process.as_mut() { process.request_stop(); self.phase = Phase::Stopping; }
    }
    pub fn poll(&mut self) {
        let Some(process) = self.process.as_mut() else { return; };
        // Exit/EOF/fault/Stop wins over queued Authenticated records.
        let exited = match process.child.try_wait() {
            Ok(status) => status,
            Err(_) => { process.faulty.store(true, Ordering::Release); None }
        };
        if exited.is_some() || process.faulty.load(Ordering::Acquire)
            || process.stop.load(Ordering::Acquire) || process.stdout_done.load(Ordering::Acquire) {
            self.listening = None; self.observed = None;
        }
        if process.faulty.load(Ordering::Acquire) {
            process.request_stop();
            if !matches!(self.phase, Phase::Failed(_)) { self.phase = Phase::Failed(Failure::Internal); }
        }
        if process.started.elapsed() > Duration::from_secs(122) { process.request_stop(); }
        if process.stop.load(Ordering::Acquire) {
            self.listening = None; self.observed = None;
            if !matches!(self.phase, Phase::Stopped(_) | Phase::Failed(_)) { self.phase = Phase::Stopping; }
        }
        if process.stopping.is_some_and(|at| at.elapsed() > Duration::from_secs(3)) {
            let _ = process.child.kill();
        }
        while let Ok((generation, mut event)) = process.events.try_recv() {
            if generation != self.generation { continue; }
            let terminal = matches!(event.event, State::Stopped { .. } | State::Failed { .. });
            if !terminal && (process.stop.load(Ordering::Acquire) || exited.is_some()
                || process.stdout_done.load(Ordering::Acquire) || process.faulty.load(Ordering::Acquire)) { continue; }
            match &mut event.event {
                State::Listening { .. } if process.offering && self.listening.is_none() => {
                    self.phase = Phase::Listening; self.listening = Some(event);
                }
                State::Authenticating {} if self.observed.is_none() => self.phase = Phase::Verifying,
                State::AttemptRejected { remaining } if process.offering && *remaining + 1 == process.remaining => {
                    process.remaining = *remaining; self.phase = Phase::Listening;
                }
                State::Authenticated { certificate, context, local_fingerprint }
                    if self.phase == Phase::Verifying && self.observed.is_none()
                    && *local_fingerprint == fingerprint(&process.local_certificate)
                    && *certificate != process.local_certificate && validate_certificate(certificate).is_ok() => {
                        self.observed = Some(Observation { certificate: std::mem::take(certificate), context: *context });
                        self.phase = Phase::Verified;
                }
                State::Stopped { reason } if !matches!(self.phase, Phase::Failed(_)) => { self.phase = Phase::Stopped(*reason); self.listening = None; self.observed = None; }
                State::Failed { reason } => { self.phase = Phase::Failed(*reason); self.listening = None; self.observed = None; }
                _ => { process.faulty.store(true, Ordering::Release); process.request_stop(); self.phase = Phase::Failed(Failure::Protocol); self.listening = None; self.observed = None; }
            }
        }
        if let Some(status) = exited {
            if !status.success() && !matches!(self.phase, Phase::Failed(_)) {
                self.phase = Phase::Failed(Failure::Internal);
            } else if !matches!(self.phase, Phase::Failed(_) | Phase::Stopped(_)) {
                self.phase = if process.stopping.is_some() && status.success() { Phase::Stopped(StopReason::Cancelled) }
                    else { Phase::Failed(Failure::Internal) };
            }
            if process.stdout_done.load(Ordering::Acquire) && process.stderr_done.load(Ordering::Acquire) {
                self.process = None;
            }
        }
    }
    pub fn stop_and_reap(&mut self) {
        self.cancel();
        let deadline = Instant::now() + Duration::from_secs(3);
        while self.process.is_some() && Instant::now() < deadline { self.poll(); std::thread::sleep(Duration::from_millis(10)); }
        self.process = None; // Process::drop handles only this non-native child.
    }
}
impl Drop for Supervisor { fn drop(&mut self) { self.stop_and_reap(); } }
