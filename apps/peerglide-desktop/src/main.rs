mod input_diagnostics;
mod layout_control;
mod pairing_worker;
mod reconnect;
mod session;
#[cfg(target_os = "linux")]
mod x11_receive;
use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use peerglide_platform::ReceiveControl;
use peerglide_protocol::{
    worker::{
        WorkerEvent, WorkerInputEvent, WorkerInputState, WorkerState, WorkerSwitchEvent,
        WorkerSwitchState,
    },
    Edge, InputCapabilities, Message, VERSION,
};
use peerglide_transport as transport;
use std::{
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

static NATIVE_RECOVERY: Mutex<Option<Arc<ReceiveControl>>> = Mutex::new(None);
struct NativeRecoveryRegistration {
    control: Arc<ReceiveControl>,
    registered: bool,
}
impl NativeRecoveryRegistration {
    fn new(control: ReceiveControl, already_stopping: bool) -> Self {
        let control = Arc::new(control);
        let registered = match NATIVE_RECOVERY.lock() {
            Ok(mut slot) => {
                let previous = slot.replace(control.clone());
                drop(slot);
                drop(previous);
                true
            }
            Err(_) => false,
        };
        if already_stopping || !registered {
            control.request_stop();
            control.wake_loop();
        }
        Self {
            control,
            registered,
        }
    }
}
impl Drop for NativeRecoveryRegistration {
    fn drop(&mut self) {
        if !self.registered {
            return;
        }
        let previous = NATIVE_RECOVERY.lock().ok().and_then(|mut slot| {
            if slot
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, &self.control))
            {
                slot.take()
            } else {
                None
            }
        });
        drop(previous);
    }
}
fn request_native_stop() {
    let control = NATIVE_RECOVERY.lock().ok().and_then(|slot| slot.clone());
    if let Some(control) = control {
        control.request_stop();
        control.wake_loop();
    }
}

#[derive(Parser)]
#[command(
    version,
    about = "PeerGlide — Multiple Computers. One Seamless Experience."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Private GUI child: display a short code; never enable input or persist trust.
    #[command(hide = true)]
    PairOffer {
        #[arg(long)]
        identity: PathBuf,
        #[arg(long, default_value = "127.0.0.1:0")]
        bind: SocketAddr,
        #[arg(long, required = true)]
        managed: bool,
    },
    /// Private GUI child: code arrives only through bounded stdin, never argv.
    #[command(hide = true)]
    PairConnect {
        address: SocketAddr,
        #[arg(long)]
        identity: PathBuf,
        #[arg(long, required = true)]
        managed: bool,
    },
    /// 只读检查本进程的前台/焦点与系统权限，不创建身份或启用输入。
    DiagnoseContext {
        /// 明确输出应用标识（Windows 可能包含个人安装路径）；默认不输出。
        #[arg(long)]
        include_application_id: bool,
        /// 显式观察焦点/会话通知，1–5000毫秒；不捕获输入或签发动作票据。
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..=5000))]
        observe_ms: Option<u64>,
    },
    /// Create an identity. Exchange ONLY node.crt.der over an independently trusted channel.
    Init {
        #[arg(long, default_value = "identity")]
        identity: PathBuf,
    },
    /// Accept a connection from exactly the enrolled certificate. No input permission by default.
    Listen(Options),
    /// Receive pointer motion from one verified peer on the local Xorg desktop.
    #[cfg(target_os = "linux")]
    ListenX11 {
        #[arg(long, default_value = "identity")]
        identity: PathBuf,
        #[arg(long)]
        peer_cert: PathBuf,
        #[arg(long, default_value = "0.0.0.0:24870")]
        bind: SocketAddr,
        /// Explicitly allow the selected computer to move the local pointer.
        #[arg(long, required = true)]
        allow_pointer_control: bool,
        /// Also receive supported keys and the three mouse buttons.
        #[arg(long, requires = "allow_pointer_control")]
        allow_discrete_input: bool,
        /// Also receive wheel steps on a standard X11 pointer mapping.
        #[arg(long, requires = "allow_pointer_control")]
        allow_scroll: bool,
    },
    /// Establish a peer connection. Network roles do not determine input roles.
    Connect {
        address: SocketAddr,
        #[command(flatten)]
        options: Options,
        #[arg(long)]
        probe: bool,
    },
}
#[derive(Clone, Copy, ValueEnum)]
enum Side {
    Left,
    Right,
    Top,
    Bottom,
}
impl From<Side> for Edge {
    fn from(s: Side) -> Self {
        match s {
            Side::Left => Self::Left,
            Side::Right => Self::Right,
            Side::Top => Self::Top,
            Side::Bottom => Self::Bottom,
        }
    }
}
#[derive(clap::Args)]
struct Options {
    #[arg(skip)]
    layout: std::sync::Arc<layout_control::Controller>,
    /// Accept a stop command on stdin from the local GUI supervisor.
    #[arg(long, hide = true)]
    managed: bool,
    #[arg(long, default_value = "identity")]
    identity: PathBuf,
    /// Explicitly enrolled remote public certificate; verify its BLAKE3 fingerprint out of band.
    #[arg(long)]
    peer_cert: PathBuf,
    #[arg(long, hide = true, requires = "managed")]
    expected_local_fingerprint: Option<String>,
    #[arg(long, hide = true, requires = "managed")]
    expected_peer_fingerprint: Option<String>,
    #[arg(long, default_value = "0.0.0.0:24870")]
    bind: SocketAddr,
    /// Explicit consent to receive remote input (independent of connecting/listening).
    #[arg(long)]
    allow_control: bool,
    /// Enable real OS capture/injection; without this flag the endpoint provides connection diagnostics only.
    #[arg(long)]
    native: bool,
    /// Emit bounded input pipeline counts for the first 120 seconds.
    #[arg(long, requires_all = ["native", "managed"], conflicts_with = "reconnect")]
    input_diagnostics: bool,
    /// Reconnect only the same enrolled peer with bounded backoff; disabled by default.
    #[arg(long)]
    reconnect: bool,
    /// Private GUI first-connection grace; never reconnects a completed session.
    #[arg(long, hide = true, requires = "managed", conflicts_with = "reconnect", value_parser = clap::value_parser!(u64).range(1..=120))]
    wait_for_peer_secs: Option<u64>,
    #[arg(long, value_enum, default_value = "right")]
    edge: Side,
}
#[tokio::main]
async fn main() -> Result<()> {
    match Cli::parse().command {
        Command::PairOffer {
            identity,
            bind,
            managed,
        } => pairing_worker::run(identity, pairing_worker::Mode::Offer(bind), managed).await,
        Command::PairConnect {
            address,
            identity,
            managed,
        } => pairing_worker::run(identity, pairing_worker::Mode::Connect(address), managed).await,
        Command::DiagnoseContext {
            include_application_id,
            observe_ms,
        } => diagnose_context(include_application_id, observe_ms),
        Command::Init { identity } => {
            let id = transport::Identity::create(&identity)?;
            println!(
                "Identity created. Public certificate: {}\nBLAKE3: {}\nKeep node.key.der private.",
                identity.join("node.crt.der").display(),
                id.fingerprint()
            );
            Ok(())
        }
        Command::Listen(o) => run(o, None, false).await,
        #[cfg(target_os = "linux")]
        Command::ListenX11 {
            identity,
            peer_cert,
            bind,
            allow_pointer_control,
            allow_discrete_input,
            allow_scroll,
        } => {
            x11_receive::run(
                &identity,
                &peer_cert,
                bind,
                allow_pointer_control,
                allow_discrete_input,
                allow_scroll,
            )
            .await
        }
        Command::Connect {
            address,
            options,
            probe,
        } => run(options, Some(address), probe).await,
    }
}

fn diagnose_context(include_application_id: bool, observe_ms: Option<u64>) -> Result<()> {
    use peerglide_platform::context::{observe_foreground, permissions};
    let permissions = permissions();
    let mut report = serde_json::json!({
        "schema_version": 1,
        "platform": std::env::consts::OS,
        "observation_scope": "sampled_no_notifications",
        "action_execution_supported": false,
        "native_capture_started": false,
        "permissions": {
            "accessibility": permissions.accessibility.code(),
            "input_monitoring": permissions.input_monitoring.code(),
            "input_desktop": permissions.input_desktop.code(),
            "secure_input": permissions.secure_input.code(),
        },
    });
    match observe_foreground() {
        Ok(foreground) => {
            report["foreground_status"] = "available".into();
            report["focus_status"] = foreground.focus().code().into();
            report["recheck_status"] = match foreground.matches_current() {
                Ok(true) => "matched_at_recheck",
                Ok(false) => "changed",
                Err(error) => error.code(),
            }
            .into();
            if include_application_id {
                report["application_id"] = foreground.application_id().into();
            }
        }
        Err(error) => {
            report["foreground_status"] = error.code().into();
            report["focus_status"] = "unavailable".into();
            report["recheck_status"] = "unavailable".into();
        }
    }
    if let Some(milliseconds) = observe_ms {
        report["observation_scope"] = "sampled_with_observer_attempt".into();
        use peerglide_platform::context::watch::LocalContexts;
        let started = std::time::Instant::now();
        let deadline = started + Duration::from_millis(milliseconds);
        let mut changes = 0;
        let observer_status = match LocalContexts::new() {
            Err(error) => error.code(),
            Ok(mut contexts) => {
                let mut epoch = contexts.epoch();
                let mut status = "ready";
                while std::time::Instant::now() < deadline {
                    let result = contexts.poll();
                    let current = contexts.epoch();
                    changes += current.saturating_sub(epoch);
                    epoch = current;
                    if let Err(error) = result {
                        status = error.code();
                        break;
                    }
                    std::thread::sleep(
                        Duration::from_millis(5)
                            .min(deadline.saturating_duration_since(std::time::Instant::now())),
                    );
                }
                if contexts.shutdown().is_err() {
                    status = "cleanup_failed";
                }
                status
            }
        };
        report["observer"] = serde_json::json!({
            "requested_ms": milliseconds, "status": observer_status,
            "coalesced_invalidations": changes, "context_minted": false,
            "delivery_verified": false,
        });
    }
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

fn worker_event(managed: bool, state: WorkerState) {
    if managed {
        use std::io::Write;
        let mut out = std::io::stdout().lock();
        let _ = serde_json::to_writer(&mut out, &WorkerEvent::new(state));
        let _ = writeln!(out);
        let _ = out.flush();
    }
}
/// Written only by the async supervisor/session, never a native callback. The
/// bounded wire record contains no peer/input payload, and repeats are coalesced.
pub(crate) struct InputReporter {
    managed: bool,
    last_switch: Option<WorkerSwitchState>,
    peer_unavailable: bool,
    last: Option<WorkerInputState>,
    recovery_unconfirmed: bool,
    terminal_sent: bool,
    #[cfg(test)]
    observed: Vec<WorkerInputState>,
    #[cfg(test)]
    emitted: Vec<WorkerInputEvent>,
    #[cfg(test)]
    observed_switches: Vec<WorkerSwitchState>,
}
impl InputReporter {
    fn new(managed: bool) -> Self {
        Self {
            managed,
            last_switch: None,
            peer_unavailable: false,
            last: None,
            recovery_unconfirmed: false,
            terminal_sent: false,
            #[cfg(test)]
            observed: Vec::new(),
            #[cfg(test)]
            emitted: Vec::new(),
            #[cfg(test)]
            observed_switches: Vec::new(),
        }
    }
    fn switch(&mut self, state: WorkerSwitchState) {
        let state = if state == WorkerSwitchState::Idle && self.peer_unavailable {
            WorkerSwitchState::PeerUnavailable
        } else {
            state
        };
        if self.terminal_sent || self.last_switch == Some(state) {
            return;
        }
        self.last_switch = Some(state);
        #[cfg(test)]
        self.observed_switches.push(state);
        if self.managed {
            use std::io::Write;
            let mut out = std::io::stdout().lock();
            let _ = writeln!(out, "{}", WorkerSwitchEvent::new(state).line());
            let _ = out.flush();
        }
    }
    fn peer_motion(&mut self, available: bool) {
        self.peer_unavailable = !available;
        self.switch(WorkerSwitchState::Idle);
    }
    fn clear_switch(&mut self) {
        self.peer_unavailable = false;
        self.switch(WorkerSwitchState::Idle);
    }
    fn set(&mut self, state: WorkerInputState) {
        if self.terminal_sent {
            return;
        }
        if state == WorkerInputState::RecoveryUnconfirmed {
            self.recovery_unconfirmed = true;
        }
        let state = if self.recovery_unconfirmed {
            WorkerInputState::RecoveryUnconfirmed
        } else {
            state
        };
        if self.last == Some(state) {
            return;
        }
        self.last = Some(state);
        #[cfg(test)]
        self.observed.push(state);
        self.emit(WorkerInputEvent::new(state));
    }
    fn emit(&mut self, event: WorkerInputEvent) {
        #[cfg(test)]
        self.emitted.push(event);
        if self.managed {
            use std::io::Write;
            let mut out = std::io::stdout().lock();
            let _ = writeln!(out, "{}", event.line());
            let _ = out.flush();
        }
    }
    /// Called once after the entire supervisor future and its reconnect loop
    /// returned. Ordinary Local/Disabled records and EOF are never this fence.
    fn finish(&mut self) -> WorkerInputState {
        if self.terminal_sent {
            return self.last.unwrap_or(WorkerInputState::RecoveryUnconfirmed);
        }
        self.clear_switch();
        let state = if self.recovery_unconfirmed {
            WorkerInputState::RecoveryUnconfirmed
        } else {
            match self.last {
                Some(WorkerInputState::Local) => WorkerInputState::Local,
                Some(WorkerInputState::Disabled) => WorkerInputState::Disabled,
                _ => WorkerInputState::RecoveryUnconfirmed,
            }
        };
        self.last = Some(state);
        self.recovery_unconfirmed = state == WorkerInputState::RecoveryUnconfirmed;
        self.terminal_sent = true;
        self.emit(WorkerInputEvent::new_terminal(state).expect("bounded terminal input state"));
        state
    }
}

async fn stop_requested(stop: &mut tokio::sync::watch::Receiver<bool>) {
    // A closed supervisor channel also means no owner remains to authorize work.
    let requested = *stop.borrow();
    if !requested {
        let _ = stop.wait_for(|requested| *requested).await;
    }
}

async fn run(o: Options, address: Option<SocketAddr>, probe: bool) -> Result<()> {
    anyhow::ensure!(
        !probe || !o.input_diagnostics,
        "input diagnostics require a native session"
    );
    anyhow::ensure!(
        o.wait_for_peer_secs.is_none()
            || (o.managed && address.is_some() && !probe && !o.reconnect),
        "initial wait requires a managed non-probe connector without reconnect"
    );
    let managed = o.managed;
    let mut input_status = InputReporter::new(managed);
    // No native capture/input effect exists during identity/network setup.
    input_status.set(WorkerInputState::Disabled);
    let (stop, stopped) = tokio::sync::watch::channel(false);
    let result = {
        let work = supervise(&o, address, probe, stopped, &mut input_status);
        tokio::pin!(work);
        if managed {
            let (request, requested) = tokio::sync::oneshot::channel();
            let direct_stop = stop.clone();
            let layout = o.layout.clone();
            std::thread::spawn(move || {
                let stdin = std::io::stdin();
                let mut reader = stdin.lock();
                loop {
                    match layout_control::read_command(&mut reader) {
                        Ok(Some(line)) if line.trim() == "stop" => break,
                        Ok(Some(line)) => {
                            if let Some(update) =
                                peerglide_protocol::layout_ipc::LayoutUpdate::parse(&line)
                            {
                                layout.submit(update);
                            }
                        }
                        Ok(None) | Err(_) => break,
                    }
                }
                // This thread must also wake native recovery when the async
                // session itself is stalled. No OS input is injected here.
                let _ = direct_stop.send(true);
                request_native_stop();
                let _ = request.send(());
            });
            tokio::select! {
                result = &mut work => result,
                _ = requested => { let _ = stop.send(true); request_native_stop(); work.await },
                _ = tokio::signal::ctrl_c() => { let _ = stop.send(true); request_native_stop(); work.await },
            }
        } else {
            tokio::select! {
                result = &mut work => result,
                _ = tokio::signal::ctrl_c() => { let _ = stop.send(true); request_native_stop(); work.await },
            }
        }
    };
    let terminal = input_status.finish();
    let result = if result.is_ok() && terminal == WorkerInputState::RecoveryUnconfirmed {
        Err(anyhow::anyhow!("native input recovery remains unconfirmed"))
    } else {
        result
    };
    worker_event(
        managed,
        if result.is_ok() {
            WorkerState::Stopped
        } else if result
            .as_ref()
            .err()
            .is_some_and(|error| error.is::<reconnect::InitialWaitExpired>())
            && terminal != WorkerInputState::RecoveryUnconfirmed
        {
            WorkerState::PeerWaitExpired
        } else {
            WorkerState::Failed
        },
    );
    result
}
async fn supervise(
    o: &Options,
    address: Option<SocketAddr>,
    probe: bool,
    mut stop: tokio::sync::watch::Receiver<bool>,
    input_status: &mut InputReporter,
) -> Result<()> {
    let id = transport::Identity::load(&o.identity)?;
    let peer = transport::read_certificate(&o.peer_cert)?;
    reconnect::verify_launch_fingerprints(
        &id.certificate,
        &peer,
        o.expected_local_fingerprint.as_deref(),
        o.expected_peer_fingerprint.as_deref(),
    )?;
    anyhow::ensure!(
        peer != id.certificate,
        "a peer must have a distinct identity"
    );
    let mut backoff = reconnect::Backoff::default();
    let mut initial = reconnect::InitialWait::new(o.wait_for_peer_secs);
    loop {
        if *stop.borrow() {
            return Ok(());
        }
        reconnect::verify_pins(&o.identity, &o.peer_cert, &id.certificate, &peer)?;
        initial.check()?;
        let result = run_inner(
            Attempt {
                o,
                address,
                probe,
                id: &id,
                peer: &peer,
                initial: &mut initial,
            },
            stop.clone(),
            input_status,
        )
        .await;
        if *stop.borrow() {
            return result;
        }
        if input_status.last == Some(WorkerInputState::RecoveryUnconfirmed) {
            return match result {
                Err(error) => Err(error),
                Ok(()) => Err(anyhow::anyhow!("native input recovery remains unconfirmed")),
            };
        }
        if (!o.reconnect && !initial.waiting()) || probe {
            return result;
        }
        match result {
            Ok(()) => (),
            Err(error) if reconnect::retryable(&error) => (),
            Err(error) => return Err(error),
        }
        initial.check()?;
        worker_event(
            o.managed,
            if initial.waiting() {
                WorkerState::WaitingForPeer
            } else {
                WorkerState::Reconnecting
            },
        );
        eprintln!("Connection ended; retrying the same enrolled peer after backoff.");
        tokio::select! {
            biased;
            _ = stop_requested(&mut stop) => return Ok(()),
            _ = initial_wait_deadline(initial.deadline()) => return Err(reconnect::InitialWaitExpired.into()),
            _ = tokio::time::sleep(backoff.next_delay()) => (),
        }
    }
}
struct Attempt<'a> {
    o: &'a Options,
    address: Option<SocketAddr>,
    probe: bool,
    id: &'a transport::Identity,
    peer: &'a [u8],
    initial: &'a mut reconnect::InitialWait,
}
async fn initial_wait_deadline(deadline: Option<tokio::time::Instant>) {
    if let Some(deadline) = deadline {
        tokio::time::sleep_until(deadline).await;
    } else {
        std::future::pending::<()>().await;
    }
}
async fn run_inner(
    attempt: Attempt<'_>,
    mut stop: tokio::sync::watch::Receiver<bool>,
    input_status: &mut InputReporter,
) -> Result<()> {
    let Attempt {
        o,
        address,
        probe,
        id,
        peer,
        initial,
    } = attempt;
    input_status.clear_switch();
    println!(
        "Local fingerprint: {}\nEnrolled peer: {}",
        id.fingerprint(),
        transport::fingerprint(peer)
    );
    let e = transport::endpoint(o.bind, id, peer)?;
    println!(
        "Listening on {}. Incoming control: {}. Native input: {}.",
        e.local_addr()?,
        o.allow_control,
        o.native
    );
    worker_event(o.managed, WorkerState::Listening);
    if initial.waiting() {
        worker_event(o.managed, WorkerState::WaitingForPeer);
    }
    // Before the native session is installed, canceling negotiation owns no
    // captured/injected input. Once installed, stop is handled cooperatively by
    // session::run_with_status so its explicit recovery tail cannot be dropped.
    let connected = tokio::select! {
        biased;
        _ = stop_requested(&mut stop) => None,
        _ = initial_wait_deadline(initial.deadline()) => Some(Err(reconnect::InitialWaitExpired.into())),
        result = async {
    let (c, mut tx, mut rx) = if let Some(addr) = address {
        let c = tokio::time::timeout(Duration::from_secs(5), e.connect(addr, "peerglide.local")?)
            .await??;
        transport::verify_peer(&c, peer)?;
        let (tx, rx) = c.open_bi().await?;
        (c, tx, rx)
    } else {
        let incoming = e.accept().await.context("endpoint closed")?;
        let c = tokio::time::timeout(Duration::from_secs(5), incoming).await??;
        transport::verify_peer(&c, peer)?;
        let (tx, rx) = tokio::time::timeout(Duration::from_secs(5), c.accept_bi()).await??;
        (c, tx, rx)
    };
    let local = if o.native {
        peerglide_platform::input_capabilities()
    } else {
        InputCapabilities::default()
    };
    let peer_capabilities = exchange_hello(&mut tx, &mut rx, local.clone()).await?;
    Ok::<_, anyhow::Error>((c, tx, rx, local, peer_capabilities))
        } => Some(result),
    };
    let Some(connected) = connected else {
        e.close(0u8.into(), b"supervisor stopped");
        e.wait_idle().await;
        return Ok(());
    };
    let (c, mut tx, mut rx, local, peer_capabilities) = connected?;
    if *stop.borrow() {
        c.close(0u8.into(), b"supervisor stopped");
        e.wait_idle().await;
        return Ok(());
    }
    if !peer_capabilities.motion() {
        eprintln!("Peer native input is unavailable; connection diagnostics remain available.");
    }
    reconnect::verify_pins(&o.identity, &o.peer_cert, &id.certificate, peer)?;
    initial.negotiated()?;
    println!("Authenticated peer connected. Ctrl+C exits; Ctrl+Alt+Esc restores local input.");
    if probe {
        worker_event(o.managed, WorkerState::Authenticated);
        input_status.set(WorkerInputState::Disabled);
        let result = tokio::select! {
            result = async {
                transport::send(&mut tx, &Message::Ping { lease: 0 }).await?;
                anyhow::ensure!(
                    matches!(
                        tokio::time::timeout(Duration::from_secs(3), transport::receive(&mut rx)).await??,
                        Message::Pong { lease: 0 }
                    ),
                    "probe response mismatch"
                );
                println!("PASS: mutual TLS, protocol negotiation, reliable roundtrip.");
                worker_event(o.managed, WorkerState::ProbePassed);
                Ok::<_, anyhow::Error>(())
            } => result,
            _ = stop_requested(&mut stop) => Ok(()),
        };
        c.close(0u8.into(), b"probe complete");
        e.wait_idle().await;
        return result;
    }
    let diagnostics = o
        .input_diagnostics
        .then(input_diagnostics::DiagnosticWindow::start)
        .transpose()?;
    let result = session::run_with_diagnostics(
        c.clone(),
        tx,
        rx,
        session::Policy {
            allow: o.allow_control,
            native: o.native,
            edge: o.edge.into(),
            local,
            peer: peer_capabilities,
        },
        stop,
        input_status,
        session::SessionControls {
            diagnostics: diagnostics.as_ref().map(|window| &window.counters),
            layout: Some(&o.layout),
        },
    )
    .await;
    if let Some(diagnostics) = diagnostics {
        diagnostics.finish().await;
    }
    worker_event(o.managed, WorkerState::Disconnected);
    let reason = c.close_reason();
    c.close(0u8.into(), b"session ended");
    e.wait_idle().await;
    result?;
    if let Some(reason) = reason {
        if !matches!(&reason, quinn::ConnectionError::ApplicationClosed(closed) if closed.error_code == 0u8.into())
        {
            return Err(reason.into());
        }
    }
    Ok(())
}

async fn exchange_hello(
    tx: &mut quinn::SendStream,
    rx: &mut quinn::RecvStream,
    local: InputCapabilities,
) -> Result<InputCapabilities> {
    transport::send(
        tx,
        &Message::Hello {
            version: VERSION,
            capabilities: local,
        },
    )
    .await?;
    let hello = tokio::time::timeout(Duration::from_secs(3), transport::receive(rx)).await??;
    match hello {
        Message::Hello {
            version: VERSION,
            capabilities,
        } if capabilities.valid() => Ok(capabilities),
        _ => anyhow::bail!("unsupported protocol version or invalid input capabilities"),
    }
}

#[cfg(test)]
mod context_cli_tests {
    use super::*;
    #[tokio::test]
    async fn initial_wait_invalid_modes_reject_before_identity_io() {
        for args in [
            vec![
                "peerglide",
                "listen",
                "--peer-cert",
                "missing",
                "--managed",
                "--wait-for-peer-secs",
                "2",
            ],
            vec![
                "peerglide",
                "connect",
                "127.0.0.1:1",
                "--peer-cert",
                "missing",
                "--managed",
                "--probe",
                "--wait-for-peer-secs",
                "2",
            ],
        ] {
            let cli = Cli::try_parse_from(args).unwrap();
            let error = match cli.command {
                Command::Listen(options) => run(options, None, false).await.unwrap_err(),
                Command::Connect {
                    address,
                    options,
                    probe,
                } => run(options, Some(address), probe).await.unwrap_err(),
                _ => unreachable!(),
            };
            assert!(error.to_string().contains("initial wait requires"));
        }
        for extra in [
            vec!["--wait-for-peer-secs", "0", "--managed"],
            vec!["--wait-for-peer-secs", "121", "--managed"],
            vec!["--wait-for-peer-secs", "2"],
            vec!["--wait-for-peer-secs", "2", "--managed", "--reconnect"],
        ] {
            let mut args = vec![
                "peerglide",
                "connect",
                "127.0.0.1:1",
                "--peer-cert",
                "missing",
            ];
            args.extend(extra);
            assert!(Cli::try_parse_from(args).is_err());
        }
    }
    #[test]
    fn input_reporter_coalesces_changes_independently_of_connection_authentication() {
        let mut reporter = InputReporter::new(false);
        reporter.set(WorkerInputState::Disabled);
        reporter.set(WorkerInputState::Disabled);
        worker_event(false, WorkerState::Authenticated);
        assert_eq!(reporter.last, Some(WorkerInputState::Disabled));
        reporter.set(WorkerInputState::ReceivingRemote);
        reporter.set(WorkerInputState::ReceivingRemote);
        reporter.set(WorkerInputState::Recovering);
        reporter.set(WorkerInputState::RecoveryUnconfirmed);
        reporter.set(WorkerInputState::RecoveryUnconfirmed);
        assert_eq!(
            reporter.observed,
            [
                WorkerInputState::Disabled,
                WorkerInputState::ReceivingRemote,
                WorkerInputState::Recovering,
                WorkerInputState::RecoveryUnconfirmed,
            ]
        );
    }

    #[test]
    fn terminal_input_fence_is_explicit_once_and_prevents_later_or_reversed_snapshots() {
        let mut reporter = InputReporter::new(false);
        reporter.set(WorkerInputState::Disabled);
        reporter.set(WorkerInputState::Local);
        assert!(reporter.emitted.iter().all(|event| !event.terminal));
        assert_eq!(reporter.finish(), WorkerInputState::Local);
        reporter.set(WorkerInputState::ControllingRemote);
        reporter.set(WorkerInputState::Disabled);
        assert_eq!(reporter.finish(), WorkerInputState::Local);
        assert_eq!(reporter.emitted.len(), 3);
        assert!(reporter.emitted.last().unwrap().terminal);
        assert_eq!(
            reporter
                .emitted
                .iter()
                .filter(|event| event.terminal)
                .count(),
            1
        );

        let mut uncertain = InputReporter::new(false);
        uncertain.set(WorkerInputState::ReceivingRemote);
        uncertain.set(WorkerInputState::RecoveryUnconfirmed);
        uncertain.set(WorkerInputState::Local);
        uncertain.set(WorkerInputState::Disabled);
        assert_eq!(uncertain.finish(), WorkerInputState::RecoveryUnconfirmed);
        assert_eq!(uncertain.last, Some(WorkerInputState::RecoveryUnconfirmed));
        assert_eq!(uncertain.emitted.len(), 3);
        assert!(uncertain.emitted.last().unwrap().terminal);
    }
    #[test]
    fn switch_metadata_cannot_change_input_authority_or_outlive_the_terminal_fence() {
        let mut reporter = InputReporter::new(false);
        reporter.set(WorkerInputState::Disabled);
        reporter.peer_motion(false);
        reporter.switch(WorkerSwitchState::Idle);
        assert_eq!(
            reporter.observed_switches,
            [WorkerSwitchState::PeerUnavailable]
        );
        reporter.peer_motion(true);
        reporter.switch(WorkerSwitchState::Pending);
        reporter.switch(WorkerSwitchState::Pending);
        reporter.switch(WorkerSwitchState::Denied);
        assert_eq!(reporter.last, Some(WorkerInputState::Disabled));
        assert!(!reporter.recovery_unconfirmed);
        assert_eq!(reporter.finish(), WorkerInputState::Disabled);
        let count = reporter.observed_switches.len();
        reporter.switch(WorkerSwitchState::Pending);
        assert_eq!(reporter.observed_switches.len(), count);
        assert_eq!(
            reporter.observed_switches.last(),
            Some(&WorkerSwitchState::Idle)
        );
    }

    #[test]
    fn terminal_input_fence_maps_missing_or_inflight_state_to_unconfirmed() {
        for state in [
            None,
            Some(WorkerInputState::ControllingRemote),
            Some(WorkerInputState::ReceivingRemote),
            Some(WorkerInputState::Recovering),
            Some(WorkerInputState::Unavailable),
        ] {
            let mut reporter = InputReporter::new(false);
            if let Some(state) = state {
                reporter.set(state);
            }
            assert_eq!(reporter.finish(), WorkerInputState::RecoveryUnconfirmed);
            assert!(reporter.emitted.last().unwrap().terminal);
        }
        let mut never_native = InputReporter::new(false);
        never_native.set(WorkerInputState::Disabled);
        assert_eq!(never_native.finish(), WorkerInputState::Disabled);
        assert!(never_native.emitted.last().unwrap().terminal);
    }

    #[tokio::test]
    async fn supervisor_stop_signal_is_cooperative_for_request_and_owner_loss() {
        let (stop, mut stopped) = tokio::sync::watch::channel(false);
        stop.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(1), stop_requested(&mut stopped))
            .await
            .unwrap();
        let (stop, mut stopped) = tokio::sync::watch::channel(false);
        drop(stop);
        tokio::time::timeout(Duration::from_secs(1), stop_requested(&mut stopped))
            .await
            .unwrap();
    }

    #[test]
    fn readonly_context_command_needs_no_identity_network_or_input_flags() {
        assert!(matches!(
            Cli::try_parse_from(["peerglide", "diagnose-context"])
                .unwrap()
                .command,
            Command::DiagnoseContext {
                include_application_id: false,
                observe_ms: None
            }
        ));
        assert!(matches!(
            Cli::try_parse_from(["peerglide", "diagnose-context", "--include-application-id"])
                .unwrap()
                .command,
            Command::DiagnoseContext {
                include_application_id: true,
                observe_ms: None
            }
        ));
        assert!(matches!(
            Cli::try_parse_from(["peerglide", "diagnose-context", "--observe-ms", "50"])
                .unwrap()
                .command,
            Command::DiagnoseContext {
                observe_ms: Some(50),
                ..
            }
        ));
        for invalid in ["0", "5001", "-1", "bad"] {
            assert!(Cli::try_parse_from([
                "peerglide",
                "diagnose-context",
                "--observe-ms",
                invalid
            ])
            .is_err());
        }
        for flag in [
            "--native",
            "--allow-control",
            "--identity",
            "--peer-cert",
            "--managed",
        ] {
            assert!(Cli::try_parse_from(["peerglide", "diagnose-context", flag]).is_err());
        }
    }
}
