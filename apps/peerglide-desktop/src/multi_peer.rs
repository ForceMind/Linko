//! A bounded v5 listener for multiple pins and at most one native session.
use crate::{exchange_hello, session, transport, InputReporter, Side, WorkerInputState};
use anyhow::{Context, Result};
use peerglide_protocol::{Edge, InputCapabilities, Message};
use std::{
    future::{pending, Future},
    net::SocketAddr,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::{
    sync::watch,
    task::{JoinHandle, JoinSet, LocalSet},
};
use transport::trusted_listener::{Listener, MAX_TRUSTED_PEERS};

#[derive(clap::Args)]
pub(super) struct Options {
    /// Select a native peer. Repeat only with --serial-native.
    #[arg(long)]
    native_peer_cert: Vec<PathBuf>,
    /// Permit another selected peer after the prior native session recovers.
    #[arg(long, requires = "native_peer_cert", conflicts_with = "allow_control")]
    serial_native: bool,
    /// In serial mode, individually permit incoming control from this certificate.
    #[arg(long, requires = "serial_native")]
    allow_control_peer_cert: Vec<PathBuf>,
    /// Permit incoming control from the single selected native peer.
    #[arg(long, requires = "native_peer_cert")]
    allow_control: bool,
    #[arg(long, value_enum, default_value = "right")]
    edge: Side,
}

#[derive(Clone)]
struct Selection {
    certificate: Vec<u8>,
    allow: bool,
    edge: Edge,
}

#[derive(Default)]
struct NativeRoutes {
    choices: Vec<Selection>,
    serial: bool,
    blocked: bool,
}

impl NativeRoutes {
    fn load(options: Options, peers: &[Vec<u8>]) -> Result<Self> {
        if options.native_peer_cert.is_empty() {
            return Ok(Self::default());
        }
        anyhow::ensure!(
            cfg!(any(windows, target_os = "macos")),
            "selected native input requires Windows or macOS"
        );
        anyhow::ensure!(
            options.native_peer_cert.len() <= MAX_TRUSTED_PEERS
                && options.allow_control_peer_cert.len() <= MAX_TRUSTED_PEERS
                && (options.serial_native || options.native_peer_cert.len() == 1),
            "multiple native peers require serial mode"
        );
        let selected = read_distinct(&options.native_peer_cert, peers)?;
        let allowed = read_distinct(&options.allow_control_peer_cert, &selected)?;
        let choices = selected
            .into_iter()
            .map(|certificate| Selection {
                allow: options.allow_control || allowed.contains(&certificate),
                certificate,
                edge: options.edge.into(),
            })
            .collect();
        Ok(Self {
            choices,
            serial: options.serial_native,
            blocked: false,
        })
    }

    fn admit(&mut self, connection: &quinn::Connection) -> Option<Selection> {
        if self.blocked {
            return None;
        }
        let index = self
            .choices
            .iter()
            .position(|route| transport::verify_peer(connection, &route.certificate).is_ok())?;
        Some(if self.serial {
            self.choices[index].clone()
        } else {
            self.choices.remove(index)
        })
    }

    fn recovered(&mut self, clean: bool) {
        if !clean {
            self.blocked = true;
            eprintln!(
                "Native recovery was not confirmed. Restart explicitly after checking local input."
            );
        }
    }
}

fn read_distinct(paths: &[PathBuf], candidates: &[Vec<u8>]) -> Result<Vec<Vec<u8>>> {
    let mut chosen = Vec::with_capacity(paths.len());
    for path in paths {
        let certificate = transport::read_certificate(path)?;
        anyhow::ensure!(
            candidates.contains(&certificate) && !chosen.contains(&certificate),
            "native peer certificate must be individually selected and unique"
        );
        chosen.push(certificate);
    }
    Ok(chosen)
}

pub(super) async fn run(
    identity: &Path,
    certificates: &[PathBuf],
    bind: SocketAddr,
    options: Options,
) -> Result<()> {
    anyhow::ensure!(
        (1..=MAX_TRUSTED_PEERS).contains(&certificates.len()),
        "trusted peer count outside listener limits"
    );
    let identity = transport::Identity::load(identity)?;
    let peers = certificates
        .iter()
        .map(|path| transport::read_certificate(path))
        .collect::<Result<Vec<_>>>()?;
    let selection = NativeRoutes::load(options, &peers)?;
    let listener = Listener::bind(bind, &identity, peers)?;
    println!(
        "Listening for authenticated peers on {}.",
        listener.local_addr()?
    );
    LocalSet::new()
        .run_until(listen(&listener, tokio::signal::ctrl_c(), selection))
        .await
}

async fn listen<F>(listener: &Listener, stop: F, mut routes: NativeRoutes) -> Result<()>
where
    F: Future<Output = std::io::Result<()>>,
{
    let mut diagnostics = JoinSet::new();
    let mut native = None;
    let (stop_all, stopped) = watch::channel(false);
    let incoming = listener.accept();
    tokio::pin!(stop, incoming);
    let result = loop {
        let capacity = diagnostics.len() + usize::from(native.is_some()) < MAX_TRUSTED_PEERS;
        tokio::select! {
            biased;
            result = &mut stop => break result,
            finished = native_result(native.as_mut()) => {
                routes.recovered(finished.unwrap_or(false));
                native = None;
            }
            _ = diagnostics.join_next(), if !diagnostics.is_empty() => (),
            incoming_peer = &mut incoming, if capacity => {
                if let Ok(peer) = incoming_peer {
                    let (connection, _) = peer.into_parts();
                    let selected = native.is_none().then(|| routes.admit(&connection)).flatten();
                    if let Some(selected) = selected {
                        let stopped = stopped.clone();
                        // The existing native owner stays on the same thread.
                        native = Some(tokio::task::spawn_local(async move {
                            let clean = serve_selected(&connection, selected, stopped).await;
                            connection.close(0u8.into(), b"selected input connection ended");
                            clean
                        }));
                    } else {
                        diagnostics.spawn(async move {
                            let _ = serve_diagnostic(&connection).await;
                            connection.close(0u8.into(), b"diagnostic connection ended");
                        });
                    }
                }
                incoming.set(listener.accept());
            }
        }
    };
    stop_all.send_replace(true);
    crate::request_native_stop();
    listener.close();
    if let Some(native) = native {
        routes.recovered(native.await.unwrap_or(false));
    }
    while diagnostics.join_next().await.is_some() {}
    result?;
    Ok(())
}

async fn native_result(
    current: Option<&mut JoinHandle<bool>>,
) -> Result<bool, tokio::task::JoinError> {
    match current {
        Some(current) => current.await,
        None => pending().await,
    }
}

async fn open_stream(
    connection: &quinn::Connection,
    local: InputCapabilities,
) -> Result<(quinn::SendStream, quinn::RecvStream, InputCapabilities)> {
    let (mut send, mut receive) =
        tokio::time::timeout(Duration::from_secs(5), connection.accept_bi())
            .await
            .context("trusted input stream timed out")??;
    let peer = exchange_hello(&mut send, &mut receive, local).await?;
    Ok((send, receive, peer))
}

async fn serve_selected(
    connection: &quinn::Connection,
    selected: Selection,
    stopped: watch::Receiver<bool>,
) -> bool {
    let local = peerglide_platform::input_capabilities();
    let (send, receive, peer) = match open_stream(connection, local.clone()).await {
        Ok(streams) => streams,
        Err(error) => {
            eprintln!("Selected session negotiation ended: {error}");
            return true;
        }
    };
    if !local.motion() || !peer.motion() {
        let _ = diagnostic_stream(send, receive).await;
        return true;
    }
    let mut reporter = InputReporter::new(false);
    let result = session::run_with_diagnostics(
        connection.clone(),
        send,
        receive,
        session::Policy {
            allow: selected.allow,
            native: true,
            edge: selected.edge,
            local,
            peer,
        },
        stopped,
        &mut reporter,
        session::SessionControls::default(),
    )
    .await;
    if let Err(error) = result {
        eprintln!("Selected native session ended: {error}");
    }
    matches!(
        reporter.finish(),
        WorkerInputState::Local | WorkerInputState::Disabled
    )
}

async fn serve_diagnostic(connection: &quinn::Connection) -> Result<()> {
    let (send, receive, _) = open_stream(connection, InputCapabilities::default()).await?;
    diagnostic_stream(send, receive).await
}

async fn diagnostic_stream(
    mut send: quinn::SendStream,
    mut receive: quinn::RecvStream,
) -> Result<()> {
    loop {
        let reply = match transport::receive(&mut receive).await? {
            Message::Ping { lease } => Message::Pong { lease },
            Message::Acquire { lease, .. } => Message::Denied { lease },
            Message::Release { .. } => continue,
            _ => anyhow::bail!("unexpected diagnostic message"),
        };
        tokio::time::timeout(Duration::from_secs(3), transport::send(&mut send, &reply)).await??;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::oneshot;

    #[tokio::test]
    async fn one_listener_services_two_pins_and_never_grants_input() -> Result<()> {
        let local = transport::Identity::generate()?;
        let left = transport::Identity::generate()?;
        let right = transport::Identity::generate()?;
        let bind = "127.0.0.1:0".parse()?;
        let listener = Listener::bind(
            bind,
            &local,
            vec![left.certificate.clone(), right.certificate.clone()],
        )?;
        let address = listener.local_addr()?;
        let (stop, stopped) = oneshot::channel::<()>();
        let server = listen(
            &listener,
            async {
                stopped
                    .await
                    .map_err(|_| std::io::Error::other("diagnostic test ended"))
            },
            NativeRoutes::default(),
        );
        let clients = async {
            let (first, second) = tokio::try_join!(
                probe(&left, &local.certificate, address, 7),
                probe(&right, &local.certificate, address, 13),
            )?;
            assert!(first.connection.close_reason().is_none());
            assert!(second.connection.close_reason().is_none());
            stop.send(())
                .map_err(|()| anyhow::anyhow!("diagnostic listener ended"))?;
            tokio::join!(first.connection.closed(), second.connection.closed());
            Ok::<_, anyhow::Error>(())
        };
        tokio::time::timeout(Duration::from_secs(10), async {
            tokio::try_join!(server, clients)
        })
        .await??;
        Ok(())
    }

    #[tokio::test]
    async fn native_selection_is_consumed_only_by_the_exact_pin() -> Result<()> {
        let local = transport::Identity::generate()?;
        let chosen = transport::Identity::generate()?;
        let other = transport::Identity::generate()?;
        let bind = "127.0.0.1:0".parse()?;
        let listener = Listener::bind(
            bind,
            &local,
            vec![chosen.certificate.clone(), other.certificate.clone()],
        )?;
        let address = listener.local_addr()?;
        let mut selection = NativeRoutes {
            choices: vec![Selection {
                certificate: chosen.certificate.clone(),
                allow: false,
                edge: Edge::Right,
            }],
            ..NativeRoutes::default()
        };
        tokio::time::timeout(Duration::from_secs(10), async {
            for (identity, expected) in [(&other, false), (&chosen, true), (&chosen, false)] {
                let endpoint = transport::endpoint(bind, identity, &local.certificate)?;
                let connecting = endpoint.connect(address, "peerglide.local")?;
                let (accepted, client) = tokio::join!(listener.accept(), connecting);
                let _client = client?;
                let (server, _) = accepted?.into_parts();
                let selected = selection.admit(&server);
                assert_eq!(selected.is_some(), expected);
                if let Some(selected) = selected {
                    assert!(!selected.allow);
                }
                server.close(0u8.into(), b"selection fixture complete");
            }
            Ok::<_, anyhow::Error>(())
        })
        .await??;
        Ok(())
    }

    #[tokio::test]
    async fn serial_native_routes_keep_incoming_permissions_per_peer_and_block_on_failed_recovery(
    ) -> Result<()> {
        let local = transport::Identity::generate()?;
        let left = transport::Identity::generate()?;
        let right = transport::Identity::generate()?;
        let bind = "127.0.0.1:0".parse()?;
        let listener = Listener::bind(
            bind,
            &local,
            vec![left.certificate.clone(), right.certificate.clone()],
        )?;
        let address = listener.local_addr()?;
        let mut routes = NativeRoutes {
            choices: vec![
                Selection {
                    certificate: left.certificate.clone(),
                    allow: true,
                    edge: Edge::Right,
                },
                Selection {
                    certificate: right.certificate.clone(),
                    allow: false,
                    edge: Edge::Right,
                },
            ],
            serial: true,
            blocked: false,
        };
        tokio::time::timeout(Duration::from_secs(10), async {
            for (identity, expected, clean) in [
                (&left, Some(true), true),
                (&right, Some(false), false),
                (&left, None, false),
            ] {
                let endpoint = transport::endpoint(bind, identity, &local.certificate)?;
                let (accepted, client) = tokio::join!(
                    listener.accept(),
                    endpoint.connect(address, "peerglide.local")?
                );
                let _client = client?;
                let (server, _) = accepted?.into_parts();
                assert_eq!(routes.admit(&server).map(|route| route.allow), expected);
                if expected.is_some() {
                    routes.recovered(clean);
                }
                server.close(0u8.into(), b"serial route fixture ended");
            }
            Ok::<_, anyhow::Error>(())
        })
        .await??;
        Ok(())
    }

    #[test]
    fn serial_route_rejects_a_global_incoming_permission() {
        use clap::Parser;
        assert!(crate::Cli::try_parse_from([
            "peerglide",
            "listen-peers",
            "--peer-cert",
            "peer.der",
            "--native-peer-cert",
            "peer.der",
            "--serial-native",
            "--allow-control",
        ])
        .is_err());
    }

    struct Probe {
        connection: quinn::Connection,
        _endpoint: quinn::Endpoint,
        _stream: (quinn::SendStream, quinn::RecvStream),
    }

    async fn probe(
        identity: &transport::Identity,
        pin: &[u8],
        address: SocketAddr,
        lease: u64,
    ) -> Result<Probe> {
        let endpoint = transport::endpoint("127.0.0.1:0".parse()?, identity, pin)?;
        let connection = endpoint.connect(address, "peerglide.local")?.await?;
        transport::verify_peer(&connection, pin)?;
        let (mut send, mut receive) = connection.open_bi().await?;
        let capabilities =
            exchange_hello(&mut send, &mut receive, InputCapabilities::default()).await?;
        assert!(!capabilities.motion());
        transport::send(&mut send, &Message::Ping { lease: 0 }).await?;
        assert!(matches!(
            transport::receive(&mut receive).await?,
            Message::Pong { lease: 0 }
        ));
        transport::send(
            &mut send,
            &Message::Acquire {
                lease,
                edge: Edge::Right,
                along: 0.5,
            },
        )
        .await?;
        assert!(matches!(
            transport::receive(&mut receive).await?,
            Message::Denied { lease: denied } if denied == lease
        ));
        Ok(Probe {
            connection,
            _endpoint: endpoint,
            _stream: (send, receive),
        })
    }
}
