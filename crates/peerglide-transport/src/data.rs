//! Independent data negotiation over pinned mutual TLS. Capability is not consent.
use super::{endpoint_alpn, verify_peer, Identity};
use anyhow::{ensure, Context, Result};
use quinn::{crypto::rustls::HandshakeData, Connection, Endpoint, RecvStream, SendStream};
use std::{net::SocketAddr, time::Duration};

pub(super) const ALPN: &[u8] = b"peerglide-data/1";
const MAGIC: &[u8; 4] = b"PGDH";
const VERSION: u8 = 1;
const TEXT: u8 = 1;
const FILES: u8 = 2;
const HELLO_BYTES: usize = 8;
const NEGOTIATION_TIMEOUT: Duration = Duration::from_secs(4);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Capabilities {
    pub text: bool,
    pub files: bool,
}

impl Capabilities {
    fn hello(self) -> [u8; HELLO_BYTES] {
        let flags = (u8::from(self.text) * TEXT) | (u8::from(self.files) * FILES);
        [MAGIC[0], MAGIC[1], MAGIC[2], MAGIC[3], VERSION, flags, 0, 0]
    }

    fn from_hello(hello: [u8; HELLO_BYTES]) -> Result<Self> {
        ensure!(
            &hello[..4] == MAGIC
                && hello[4] == VERSION
                && hello[5] & !(TEXT | FILES) == 0
                && hello[6..] == [0, 0],
            "invalid or unsupported data hello"
        );
        Ok(Self {
            text: hello[5] & TEXT != 0,
            files: hello[5] & FILES != 0,
        })
    }

    fn shared(self, peer: Self) -> Self {
        Self {
            text: self.text && peer.text,
            files: self.files && peer.files,
        }
    }
}

/// A single negotiated stream on a data connection. Callers still need
/// independent direction consent before any clipboard or file side effect.
pub struct Stream {
    pub send: SendStream,
    pub receive: RecvStream,
    pub capabilities: Capabilities,
}

#[derive(Clone, Copy)]
enum Side {
    Connector,
    Listener,
}

/// Opens an endpoint with a data ALPN only, independently of the input endpoint.
pub fn endpoint(address: SocketAddr, local: &Identity, pinned_peer: &[u8]) -> Result<Endpoint> {
    ensure!(
        local.certificate.as_slice() != pinned_peer,
        "data peer must have a distinct identity"
    );
    endpoint_alpn(address, local, pinned_peer, ALPN)
}

pub async fn open(
    connection: &Connection,
    pinned_peer: &[u8],
    supported: Capabilities,
) -> Result<Stream> {
    negotiate(connection, pinned_peer, supported, Side::Connector).await
}

pub async fn accept(
    connection: &Connection,
    pinned_peer: &[u8],
    supported: Capabilities,
) -> Result<Stream> {
    negotiate(connection, pinned_peer, supported, Side::Listener).await
}

async fn negotiate(
    connection: &Connection,
    pinned_peer: &[u8],
    supported: Capabilities,
    side: Side,
) -> Result<Stream> {
    let handshake = connection
        .handshake_data()
        .context("data TLS handshake is unavailable")?
        .downcast::<HandshakeData>()
        .map_err(|_| anyhow::anyhow!("unexpected data TLS handshake"))?;
    ensure!(
        handshake.protocol.as_deref() == Some(ALPN),
        "unexpected data ALPN"
    );
    verify_peer(connection, pinned_peer)?;

    let result = tokio::time::timeout(NEGOTIATION_TIMEOUT, async {
        let (mut send, mut receive) = match side {
            Side::Connector => connection.open_bi().await?,
            Side::Listener => connection.accept_bi().await?,
        };
        ensure!(
            send.id().index() == 0,
            "data connection already used a stream"
        );
        let mut hello = [0; HELLO_BYTES];
        let peer = match side {
            Side::Connector => {
                send.write_all(&supported.hello()).await?;
                receive.read_exact(&mut hello).await?;
                Capabilities::from_hello(hello)?
            }
            Side::Listener => {
                receive.read_exact(&mut hello).await?;
                let peer = Capabilities::from_hello(hello)?;
                send.write_all(&supported.hello()).await?;
                peer
            }
        };
        Ok(Stream {
            send,
            receive,
            capabilities: supported.shared(peer),
        })
    })
    .await
    .context("data negotiation timed out")
    .and_then(|result| result);
    if result.is_err() {
        connection.close(1u8.into(), b"data negotiation failed");
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT_ONLY: Capabilities = Capabilities {
        text: true,
        files: false,
    };
    const TEXT_AND_FILES: Capabilities = Capabilities {
        text: true,
        files: true,
    };

    async fn connect(client: &Endpoint, server: &Endpoint) -> Result<(Connection, Connection)> {
        tokio::time::timeout(Duration::from_secs(5), async {
            let outbound = async {
                let connection = client.connect(server.local_addr()?, "peerglide.local")?;
                Ok::<_, anyhow::Error>(connection.await?)
            };
            let inbound = async {
                let connection = server.accept().await.context("no incoming connection")?;
                Ok::<_, anyhow::Error>(connection.await?)
            };
            tokio::try_join!(outbound, inbound)
        })
        .await?
    }

    #[test]
    fn unknown_hello_fields_fail_closed() {
        let hello = TEXT_AND_FILES.hello();
        assert_eq!(Capabilities::from_hello(hello).unwrap(), TEXT_AND_FILES);
        for (index, value) in [(0, b'X'), (4, 2), (5, 0x80), (7, 1)] {
            let mut invalid = hello;
            invalid[index] = value;
            assert!(Capabilities::from_hello(invalid).is_err());
        }
    }

    #[tokio::test]
    async fn data_negotiates_only_shared_capabilities_with_exact_pins() -> Result<()> {
        let local = Identity::generate()?;
        let peer = Identity::generate()?;
        let other = Identity::generate()?;
        let bind = "127.0.0.1:0".parse()?;
        let left = endpoint(bind, &local, &peer.certificate)?;
        let right = endpoint(bind, &peer, &local.certificate)?;
        let (connector, listener) = connect(&left, &right).await?;
        assert!(open(&connector, &other.certificate, TEXT_ONLY)
            .await
            .is_err());
        assert!(connector.max_datagram_size().is_none());
        let (client, server) = tokio::try_join!(
            open(&connector, &peer.certificate, TEXT_ONLY),
            accept(&listener, &local.certificate, TEXT_AND_FILES)
        )?;
        assert_eq!(client.capabilities, TEXT_ONLY);
        assert_eq!(server.capabilities, TEXT_ONLY);
        Ok(())
    }

    #[tokio::test]
    async fn input_and_data_connections_cannot_be_crossed() -> Result<()> {
        let local = Identity::generate()?;
        let peer = Identity::generate()?;
        let bind = "127.0.0.1:0".parse()?;
        let data = endpoint(bind, &local, &peer.certificate)?;
        let incompatible = super::super::endpoint(bind, &peer, &local.certificate)?;
        assert!(connect(&data, &incompatible).await.is_err());

        let input = super::super::endpoint(bind, &peer, &local.certificate)?;
        let input_client = super::super::endpoint(bind, &local, &peer.certificate)?;
        let (connector, listener) = connect(&input_client, &input).await?;
        assert!(open(&connector, &peer.certificate, TEXT_ONLY)
            .await
            .is_err());
        assert!(accept(&listener, &local.certificate, TEXT_ONLY)
            .await
            .is_err());
        assert!(connector.close_reason().is_none() && listener.close_reason().is_none());
        Ok(())
    }
}
