//! A bounded v5 diagnostic listener for multiple explicitly pinned peers.
use crate::{exchange_hello, transport};
use anyhow::{Context, Result};
use peerglide_protocol::{InputCapabilities, Message};
use std::{
    future::Future,
    net::SocketAddr,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::task::JoinSet;
use transport::trusted_listener::{Listener, MAX_TRUSTED_PEERS};

pub(super) async fn run(identity: &Path, certificates: &[PathBuf], bind: SocketAddr) -> Result<()> {
    anyhow::ensure!(
        (1..=MAX_TRUSTED_PEERS).contains(&certificates.len()),
        "trusted peer count outside listener limits"
    );
    let identity = transport::Identity::load(identity)?;
    let peers = certificates
        .iter()
        .map(|path| transport::read_certificate(path))
        .collect::<Result<Vec<_>>>()?;
    let listener = Listener::bind(bind, &identity, peers)?;
    println!(
        "Listening for authenticated diagnostic peers on {}.",
        listener.local_addr()?
    );
    listen(&listener, tokio::signal::ctrl_c()).await
}

async fn listen<F>(listener: &Listener, stop: F) -> Result<()>
where
    F: Future<Output = std::io::Result<()>>,
{
    let mut sessions = JoinSet::new();
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
                    sessions.spawn(async move {
                        let _ = serve(&connection).await;
                        connection.close(0u8.into(), b"diagnostic connection ended");
                    });
                }
            }
        }
    };
    listener.close();
    while sessions.join_next().await.is_some() {}
    result?;
    Ok(())
}

async fn serve(connection: &quinn::Connection) -> Result<()> {
    let (mut send, mut receive) =
        tokio::time::timeout(Duration::from_secs(5), connection.accept_bi())
            .await
            .context("diagnostic stream timed out")??;
    exchange_hello(&mut send, &mut receive, InputCapabilities::default()).await?;
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
    use peerglide_protocol::Edge;
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
        let server = listen(&listener, async {
            stopped
                .await
                .map_err(|_| std::io::Error::other("diagnostic test ended"))
        });
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
