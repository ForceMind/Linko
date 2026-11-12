//! A bounded v5 listener for multiple pins and at most one native session.
use crate::{exchange_hello, session, transport, InputReporter, Side};
use anyhow::{Context, Result};
use peerglide_protocol::{Edge, InputCapabilities, Message};
use std::{
    future::Future,
    net::SocketAddr,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::{sync::watch, task::JoinSet};
use transport::trusted_listener::{Listener, MAX_TRUSTED_PEERS};

#[derive(clap::Args)]
pub(super) struct Options {
    /// Start native input for the first connection from this enrolled certificate.
    #[arg(long)]
    native_peer_cert: Option<PathBuf>,
    /// Explicit permission for the selected computer to control this one.
    #[arg(long, requires = "native_peer_cert")]
    allow_control: bool,
    #[arg(long, value_enum, default_value = "right")]
    edge: Side,
}

struct Selection {
    certificate: Vec<u8>,
    allow: bool,
    edge: Edge,
}

fn take_selected(
    selection: &mut Option<Selection>,
    connection: &quinn::Connection,
) -> Option<Selection> {
    let route = selection.as_ref()?;
    transport::verify_peer(connection, &route.certificate).ok()?;
    selection.take()
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
    let selection = if let Some(path) = &options.native_peer_cert {
        anyhow::ensure!(
            cfg!(any(windows, target_os = "macos")),
            "selected native input requires Windows or macOS"
        );
        let certificate = transport::read_certificate(path)?;
        anyhow::ensure!(
            peers.contains(&certificate),
            "selected native certificate must be in the enrolled set"
        );
        Some(Selection {
            certificate,
            allow: options.allow_control,
            edge: options.edge.into(),
        })
    } else {
        None
    };
    let listener = Listener::bind(bind, &identity, peers)?;
    println!(
        "Listening for authenticated peers on {}.",
        listener.local_addr()?
    );
    listen(&listener, tokio::signal::ctrl_c(), selection).await
}

async fn listen<F>(listener: &Listener, stop: F, mut selection: Option<Selection>) -> Result<()>
where
    F: Future<Output = std::io::Result<()>>,
{
    let mut sessions = JoinSet::new();
    let (stop_all, stopped) = watch::channel(false);
    tokio::pin!(stop);
    let result = loop {
        while sessions.try_join_next().is_some() {}
        if sessions.len() >= MAX_TRUSTED_PEERS {
            tokio::select! {
                biased;
                result = &mut stop => break result,
                _ = sessions.join_next() => (),
            }
            continue;
        }
        tokio::select! {
            biased;
            result = &mut stop => break result,
            incoming = listener.accept() => {
                if let Ok(peer) = incoming {
                    let (connection, _) = peer.into_parts();
                    let selected = take_selected(&mut selection, &connection);
                    let stopped = stopped.clone();
                    sessions.spawn(async move {
                        let selected_native = selected.is_some();
                        let result = serve(&connection, selected, stopped).await;
                        if selected_native {
                            match result {
                                Ok(()) => println!("Selected session ended. Restart the listener to select another native session."),
                                Err(error) => eprintln!("Selected native session ended: {error}"),
                            }
                        }
                        connection.close(0u8.into(), b"trusted connection ended");
                    });
                }
            }
        }
    };
    stop_all.send_replace(true);
    crate::request_native_stop();
    listener.close();
    while sessions.join_next().await.is_some() {}
    result?;
    Ok(())
}

async fn serve(
    connection: &quinn::Connection,
    selected: Option<Selection>,
    stopped: watch::Receiver<bool>,
) -> Result<()> {
    let (mut send, mut receive) =
        tokio::time::timeout(Duration::from_secs(5), connection.accept_bi())
            .await
            .context("trusted input stream timed out")??;
    let local = if selected.is_some() {
        peerglide_platform::input_capabilities()
    } else {
        InputCapabilities::default()
    };
    let peer = exchange_hello(&mut send, &mut receive, local.clone()).await?;
    if let Some(selected) = selected.filter(|_| local.motion() && peer.motion()) {
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
        reporter.finish();
        return result;
    }
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
            None,
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
        let mut selection = Some(Selection {
            certificate: chosen.certificate.clone(),
            allow: false,
            edge: Edge::Right,
        });
        tokio::time::timeout(Duration::from_secs(10), async {
            for (identity, expected) in [(&other, false), (&chosen, true), (&chosen, false)] {
                let endpoint = transport::endpoint(bind, identity, &local.certificate)?;
                let connecting = endpoint.connect(address, "peerglide.local")?;
                let (accepted, client) = tokio::join!(listener.accept(), connecting);
                let _client = client?;
                let (server, _) = accepted?.into_parts();
                let selected = take_selected(&mut selection, &server);
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
