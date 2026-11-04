mod reconnect;
mod session;
use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use peerglide_protocol::{
    worker::{WorkerEvent, WorkerState},
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
        } => diagnose_context(include_application_id),
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

fn diagnose_context(include_application_id: bool) -> Result<()> {
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
async fn run(o: Options, address: Option<SocketAddr>, probe: bool) -> Result<()> {
    let managed = o.managed;
    let result = if o.managed {
        let (stop, stopped) = tokio::sync::oneshot::channel();
        std::thread::spawn(move || {
            use std::io::BufRead;
            for line in std::io::stdin().lock().lines() {
                match line {
                    Ok(s) if s.trim() == "stop" => break,
                    Ok(_) => (),
                    Err(_) => break,
                }
            }
            let _ = stop.send(());
        });
        tokio::select! {result=supervise(&o,address,probe)=>result,_=stopped=>Ok(()),_=tokio::signal::ctrl_c()=>Ok(())}
    } else {
        tokio::select! { result=supervise(&o,address,probe)=>result, _=tokio::signal::ctrl_c()=>Ok(()) }
    };
    worker_event(
        managed,
        if result.is_ok() {
            WorkerState::Stopped
        } else {
            WorkerState::Failed
        },
    );
    result
}
async fn supervise(o: &Options, address: Option<SocketAddr>, probe: bool) -> Result<()> {
    let id = transport::Identity::load(&o.identity)?;
    let peer = transport::read_certificate(&o.peer_cert)?;
    anyhow::ensure!(
        peer != id.certificate,
        "a peer must have a distinct identity"
    );
    let mut backoff = reconnect::Backoff::default();
    loop {
        reconnect::verify_pins(&o.identity, &o.peer_cert, &id.certificate, &peer)?;
        let result = run_inner(o, address, probe, &id, &peer).await;
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
        tokio::time::sleep(backoff.next_delay()).await;
    }
}
async fn run_inner(
    o: &Options,
    address: Option<SocketAddr>,
    probe: bool,
    id: &transport::Identity,
    peer: &[u8],
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
    if !peer_capabilities.motion() {
        eprintln!("Peer native input is unavailable; connection diagnostics remain available.");
    }
    println!("Authenticated peer connected. Ctrl+C exits; Ctrl+Alt+Esc restores local input.");
    worker_event(o.managed, WorkerState::Authenticated);
    if probe {
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
        c.close(0u8.into(), b"probe complete");
        e.wait_idle().await;
        return Ok(());
    }
    let result = session::run(
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
    fn readonly_context_command_needs_no_identity_network_or_input_flags() {
        assert!(matches!(
            Cli::try_parse_from(["peerglide", "diagnose-context"])
                .unwrap()
                .command,
            Command::DiagnoseContext {
                include_application_id: false
            }
        ));
        assert!(matches!(
            Cli::try_parse_from(["peerglide", "diagnose-context", "--include-application-id"])
                .unwrap()
                .command,
            Command::DiagnoseContext {
                include_application_id: true
            }
        ));
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
