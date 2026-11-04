mod reconnect;
mod session;
use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use peerglide_protocol::{
    worker::{WorkerEvent, WorkerInputEvent, WorkerInputState, WorkerState},
    Edge, InputCapabilities, Message, VERSION,
};
use peerglide_transport as transport;
use std::{net::SocketAddr, path::PathBuf, time::Duration};

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
    /// Accept a stop command on stdin from the local GUI supervisor.
    #[arg(long, hide = true)]
    managed: bool,
    #[arg(long, default_value = "identity")]
    identity: PathBuf,
    /// Explicitly enrolled remote public certificate; verify its BLAKE3 fingerprint out of band.
    #[arg(long)]
    peer_cert: PathBuf,
    #[arg(long, default_value = "0.0.0.0:24870")]
    bind: SocketAddr,
    /// Explicit consent to receive remote input (independent of connecting/listening).
    #[arg(long)]
    allow_control: bool,
    /// Enable real OS capture/injection; without this flag the endpoint provides connection diagnostics only.
    #[arg(long)]
    native: bool,
    /// Reconnect only the same enrolled peer with bounded backoff; disabled by default.
    #[arg(long)]
    reconnect: bool,
    #[arg(long, value_enum, default_value = "right")]
    edge: Side,
}
#[tokio::main]
async fn main() -> Result<()> {
    match Cli::parse().command {
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
    last: Option<WorkerInputState>,
    recovery_unconfirmed: bool,
    terminal_sent: bool,
    #[cfg(test)]
    observed: Vec<WorkerInputState>,
    #[cfg(test)]
    emitted: Vec<WorkerInputEvent>,
}
impl InputReporter {
    fn new(managed: bool) -> Self {
        Self {
            managed, last: None, recovery_unconfirmed: false, terminal_sent: false,
            #[cfg(test)] observed: Vec::new(),
            #[cfg(test)] emitted: Vec::new(),
        }
    }
    fn set(&mut self, state: WorkerInputState) {
        if self.terminal_sent { return; }
        if state == WorkerInputState::RecoveryUnconfirmed { self.recovery_unconfirmed = true; }
        let state = if self.recovery_unconfirmed { WorkerInputState::RecoveryUnconfirmed } else { state };
        if self.last == Some(state) { return; }
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
            std::thread::spawn(move || {
                use std::io::BufRead;
                for line in std::io::stdin().lock().lines() {
                    match line {
                        Ok(s) if s.trim() == "stop" => break,
                        Ok(_) => (),
                        Err(_) => break,
                    }
                }
                let _ = request.send(());
            });
            tokio::select! {
                result = &mut work => result,
                _ = requested => { let _ = stop.send(true); work.await },
                _ = tokio::signal::ctrl_c() => { let _ = stop.send(true); work.await },
            }
        } else {
            tokio::select! {
                result = &mut work => result,
                _ = tokio::signal::ctrl_c() => { let _ = stop.send(true); work.await },
            }
        }
    };
    let terminal = input_status.finish();
    let result = if result.is_ok() && terminal == WorkerInputState::RecoveryUnconfirmed {
        Err(anyhow::anyhow!("native input recovery remains unconfirmed"))
    } else { result };
    worker_event(managed, if result.is_ok() { WorkerState::Stopped } else { WorkerState::Failed });
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
    anyhow::ensure!(
        peer != id.certificate,
        "a peer must have a distinct identity"
    );
    let mut backoff = reconnect::Backoff::default();
    loop {
        if *stop.borrow() { return Ok(()); }
        reconnect::verify_pins(&o.identity, &o.peer_cert, &id.certificate, &peer)?;
        let result = run_inner(o, address, probe, &id, &peer, stop.clone(), input_status).await;
        if *stop.borrow() { return result; }
        if input_status.last == Some(WorkerInputState::RecoveryUnconfirmed) {
            return match result {
                Err(error) => Err(error),
                Ok(()) => Err(anyhow::anyhow!("native input recovery remains unconfirmed")),
            };
        }
        if !o.reconnect || probe {
            return result;
        }
        match result {
            Ok(()) => (),
            Err(error) if reconnect::retryable(&error) => (),
            Err(error) => return Err(error),
        }
        worker_event(o.managed, WorkerState::Reconnecting);
        eprintln!("Connection ended; retrying the same enrolled peer after backoff.");
        tokio::select! {
            _ = tokio::time::sleep(backoff.next_delay()) => (),
            _ = stop_requested(&mut stop) => return Ok(()),
        }
    }
}
async fn run_inner(
    o: &Options,
    address: Option<SocketAddr>,
    probe: bool,
    id: &transport::Identity,
    peer: &[u8],
    mut stop: tokio::sync::watch::Receiver<bool>,
    input_status: &mut InputReporter,
) -> Result<()> {
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
    // Before the native session is installed, canceling negotiation owns no
    // captured/injected input. Once installed, stop is handled cooperatively by
    // session::run_with_status so its explicit recovery tail cannot be dropped.
    let connected = tokio::select! {
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
    transport::send(
        &mut tx,
        &Message::Hello {
            version: VERSION,
            capabilities: local.clone(),
        },
    )
    .await?;
    let hello = tokio::time::timeout(Duration::from_secs(3), transport::receive(&mut rx)).await??;
    let peer_capabilities = match hello {
        Message::Hello {
            version: VERSION,
            capabilities,
        } if capabilities.valid() => capabilities,
        _ => anyhow::bail!("unsupported protocol version or invalid input capabilities"),
    };
    Ok::<_, anyhow::Error>((c, tx, rx, local, peer_capabilities))
        } => Some(result),
        _ = stop_requested(&mut stop) => None,
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
    println!("Authenticated peer connected. Ctrl+C exits; Ctrl+Alt+Esc restores local input.");
    worker_event(o.managed, WorkerState::Authenticated);
    if probe {
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
    let result = session::run_with_status(
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
    )
    .await;
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

#[cfg(test)]
mod context_cli_tests {
    use super::*;
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
        assert_eq!(reporter.observed, [
            WorkerInputState::Disabled, WorkerInputState::ReceivingRemote,
            WorkerInputState::Recovering, WorkerInputState::RecoveryUnconfirmed,
        ]);
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
        assert_eq!(reporter.emitted.iter().filter(|event| event.terminal).count(), 1);

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
    fn terminal_input_fence_maps_missing_or_inflight_state_to_unconfirmed() {
        for state in [None, Some(WorkerInputState::ControllingRemote),
            Some(WorkerInputState::ReceivingRemote), Some(WorkerInputState::Recovering),
            Some(WorkerInputState::Unavailable)] {
            let mut reporter = InputReporter::new(false);
            if let Some(state) = state { reporter.set(state); }
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
        tokio::time::timeout(Duration::from_secs(1), stop_requested(&mut stopped)).await.unwrap();
        let (stop, mut stopped) = tokio::sync::watch::channel(false);
        drop(stop);
        tokio::time::timeout(Duration::from_secs(1), stop_requested(&mut stopped)).await.unwrap();
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
