//! A sidecar reservation for one v5 Acquire. Local input consent remains mandatory.
use crate::{endpoint_alpn, layout_binding::Binding, verify_peer, Identity};
use anyhow::{ensure, Context, Result};
use peerglide_protocol::{Edge, LEASE_MS};
use quinn::{crypto::rustls::HandshakeData, Connection, Endpoint, RecvStream, SendStream};
use std::{
    net::SocketAddr,
    time::{Duration, Instant},
};

pub(super) const ALPN: &[u8] = b"peerglide-layout/1";
const MAGIC: &[u8; 4] = b"PGL1";
const REQUEST_BYTES: usize = 64;
const REPLY_BYTES: usize = 24;
const IO_TIMEOUT: Duration = Duration::from_secs(4);
const RESERVATION_TIMEOUT: Duration = Duration::from_millis(LEASE_MS);

#[derive(Clone, Copy)]
pub struct Request {
    pub lease: u64,
    pub display: u64,
    pub edge: Edge,
    pub fraction: f64,
}

impl Request {
    fn valid(self) -> bool {
        self.lease != 0 && self.display != 0 && (0.0..=1.0).contains(&self.fraction)
    }

    fn encode(self, proof: [u8; 32]) -> Result<[u8; REQUEST_BYTES]> {
        ensure!(self.valid(), "invalid display reservation");
        let mut bytes = [0; REQUEST_BYTES];
        bytes[..4].copy_from_slice(MAGIC);
        bytes[4] = 1;
        bytes[5] = edge_to_byte(self.edge);
        bytes[8..16].copy_from_slice(&self.lease.to_le_bytes());
        bytes[16..24].copy_from_slice(&self.display.to_le_bytes());
        bytes[24..32].copy_from_slice(&self.fraction.to_bits().to_le_bytes());
        bytes[32..].copy_from_slice(&proof);
        Ok(bytes)
    }

    fn decode(bytes: [u8; REQUEST_BYTES]) -> Result<(Self, [u8; 32])> {
        ensure!(
            &bytes[..4] == MAGIC && bytes[4] == 1 && bytes[6..8] == [0; 2],
            "invalid display request header"
        );
        let request = Self {
            lease: read_word(&bytes[8..16])?,
            display: read_word(&bytes[16..24])?,
            edge: edge_from_byte(bytes[5])?,
            fraction: f64::from_bits(read_word(&bytes[24..32])?),
        };
        ensure!(request.valid(), "invalid display reservation");
        Ok((request, bytes[32..].try_into()?))
    }
}

/// A local single-use reply. The v5 receiver still checks peer permission,
/// the current input owner and per-connection lease replay before injection.
pub struct Granted {
    request: Request,
    binding: Binding,
    proof: [u8; 32],
    generation: u64,
    expires: Instant,
}

impl Granted {
    pub fn consume(
        self,
        input: &Connection,
        pinned_peer: &[u8],
        lease: u64,
        layout_generation: u64,
    ) -> Result<Option<Request>> {
        if self.request.lease != lease
            || self.generation != layout_generation
            || Instant::now() >= self.expires
            || !self.binding.matches(input, pinned_peer, &self.proof)?
        {
            return Ok(None);
        }
        Ok(Some(self.request))
    }
}

pub fn endpoint(address: SocketAddr, local: &Identity, pinned_peer: &[u8]) -> Result<Endpoint> {
    ensure!(
        local.certificate.as_slice() != pinned_peer,
        "layout peer must be distinct"
    );
    endpoint_alpn(address, local, pinned_peer, ALPN)
}

/// A positive result describes the target's current generation. It does not
/// grant input. The existing v5 Acquire follows on the bound input connection.
pub async fn reserve(
    sidecar: &Connection,
    input: &Connection,
    pinned_peer: &[u8],
    request: Request,
) -> Result<Option<u64>> {
    verify_sidecar(sidecar, pinned_peer)?;
    let binding = Binding::from_v5(input, pinned_peer)?;
    let bytes = request.encode(binding.proof())?;
    tokio::time::timeout(IO_TIMEOUT, async {
        let (mut send, mut receive) = sidecar.open_bi().await?;
        require_first(&send, &receive)?;
        send.write_all(&bytes).await?;
        send.finish()?;
        let mut reply = [0; REPLY_BYTES];
        receive.read_exact(&mut reply).await?;
        require_end(&mut receive).await?;
        ensure!(
            &reply[..4] == MAGIC && reply[4] == 2 && reply[6..8] == [0; 2],
            "invalid display reply header"
        );
        ensure!(
            read_word(&reply[8..16])? == request.lease,
            "display reply lease differs"
        );
        let generation = read_word(&reply[16..])?;
        match (reply[5], generation) {
            (0, 0) => Ok(None),
            (1, generation) if generation != 0 => Ok(Some(generation)),
            _ => anyhow::bail!("invalid display reply result"),
        }
    })
    .await
    .context("display reservation timed out")?
}

/// The target's local controller returns its current layout generation only if
/// the requested display and direction are explicitly allowed for this peer.
pub async fn accept(
    sidecar: &Connection,
    input: &Connection,
    pinned_peer: &[u8],
    choose: impl FnOnce(Request) -> Option<u64>,
) -> Result<Option<Granted>> {
    verify_sidecar(sidecar, pinned_peer)?;
    let binding = Binding::from_v5(input, pinned_peer)?;
    tokio::time::timeout(IO_TIMEOUT, async {
        let (mut send, mut receive) = sidecar.accept_bi().await?;
        require_first(&send, &receive)?;
        let mut bytes = [0; REQUEST_BYTES];
        receive.read_exact(&mut bytes).await?;
        require_end(&mut receive).await?;
        let (request, proof) = Request::decode(bytes)?;
        let grant = if binding.matches(input, pinned_peer, &proof)? {
            choose(request)
                .filter(|generation| *generation != 0)
                .map(|generation| Granted {
                    request,
                    binding,
                    proof,
                    generation,
                    expires: Instant::now() + RESERVATION_TIMEOUT,
                })
        } else {
            None
        };
        let mut reply = [0; REPLY_BYTES];
        reply[..4].copy_from_slice(MAGIC);
        reply[4] = 2;
        reply[5] = u8::from(grant.is_some());
        reply[8..16].copy_from_slice(&request.lease.to_le_bytes());
        let generation = grant.as_ref().map_or(0, |grant| grant.generation);
        reply[16..].copy_from_slice(&generation.to_le_bytes());
        send.write_all(&reply).await?;
        send.finish()?;
        Ok(grant)
    })
    .await
    .context("display offer timed out")?
}

fn verify_sidecar(connection: &Connection, pinned_peer: &[u8]) -> Result<()> {
    let handshake = connection
        .handshake_data()
        .context("display handshake unavailable")?
        .downcast::<HandshakeData>()
        .map_err(|_| anyhow::anyhow!("unexpected display handshake"))?;
    ensure!(
        handshake.protocol.as_deref() == Some(ALPN),
        "unexpected display ALPN"
    );
    verify_peer(connection, pinned_peer)
}

fn require_first(send: &SendStream, receive: &RecvStream) -> Result<()> {
    ensure!(
        send.id().index() == 0 && receive.id().index() == 0,
        "display stream already used"
    );
    Ok(())
}

async fn require_end(receive: &mut RecvStream) -> Result<()> {
    let mut extra = [0];
    ensure!(
        receive.read(&mut extra).await?.is_none(),
        "unexpected display trailing bytes"
    );
    Ok(())
}

fn read_word(bytes: &[u8]) -> Result<u64> {
    Ok(u64::from_le_bytes(bytes.try_into()?))
}

fn edge_to_byte(edge: Edge) -> u8 {
    match edge {
        Edge::Left => 1,
        Edge::Right => 2,
        Edge::Top => 3,
        Edge::Bottom => 4,
    }
}

fn edge_from_byte(value: u8) -> Result<Edge> {
    match value {
        1 => Ok(Edge::Left),
        2 => Ok(Edge::Right),
        3 => Ok(Edge::Top),
        4 => Ok(Edge::Bottom),
        _ => anyhow::bail!("invalid display edge"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn pair(client: &Endpoint, server: &Endpoint) -> Result<(Connection, Connection)> {
        tokio::time::timeout(Duration::from_secs(5), async {
            tokio::try_join!(
                async {
                    let connecting = client.connect(server.local_addr()?, "peerglide.local")?;
                    Ok::<_, anyhow::Error>(connecting.await?)
                },
                async {
                    let incoming = server.accept().await.context("missing reservation peer")?;
                    Ok::<_, anyhow::Error>(incoming.await?)
                },
            )
        })
        .await?
    }

    #[tokio::test]
    async fn reservation_is_bound_to_current_input_and_layout_generation() -> Result<()> {
        let a = Identity::generate()?;
        let b = Identity::generate()?;
        let bind = "127.0.0.1:0".parse()?;
        let a_input = crate::endpoint(bind, &a, &b.certificate)?;
        let b_input = crate::endpoint(bind, &b, &a.certificate)?;
        let a_layout = endpoint(bind, &a, &b.certificate)?;
        let b_layout = endpoint(bind, &b, &a.certificate)?;
        let (a_v5, b_v5) = pair(&a_input, &b_input).await?;
        let (_, other_v5) = pair(&a_input, &b_input).await?;
        let request = Request {
            lease: 19,
            display: 12,
            edge: Edge::Left,
            fraction: 0.25,
        };

        let (sender, receiver) = pair(&a_layout, &b_layout).await?;
        let (reply, granted) = tokio::try_join!(
            reserve(&sender, &a_v5, &b.certificate, request),
            accept(&receiver, &other_v5, &a.certificate, |_| {
                panic!("wrong input connection must fail before local admission")
            }),
        )?;
        assert!(reply.is_none() && granted.is_none());

        for (generation, expired) in [(8, false), (9, false), (8, true)] {
            let (sender, receiver) = pair(&a_layout, &b_layout).await?;
            assert!(sender.max_datagram_size().is_none());
            let (reply, granted) = tokio::try_join!(
                reserve(&sender, &a_v5, &b.certificate, request),
                accept(&receiver, &b_v5, &a.certificate, |offer| {
                    assert_eq!(offer.display, 12);
                    Some(8)
                }),
            )?;
            assert_eq!(reply, Some(8));
            let mut granted = granted.context("local reservation missing")?;
            if expired {
                granted.expires = Instant::now();
            }
            let consumed = granted.consume(&b_v5, &a.certificate, 19, generation)?;
            if generation == 8 && !expired {
                let resolved = consumed.context("current reservation rejected")?;
                assert_eq!(resolved.edge, Edge::Left);
                assert!((resolved.fraction - 0.25).abs() < f64::EPSILON);
            } else {
                assert!(consumed.is_none());
            }
        }
        Ok(())
    }

    #[test]
    fn request_has_closed_shape_and_finite_fraction() -> Result<()> {
        let request = Request {
            lease: 1,
            display: 2,
            edge: Edge::Top,
            fraction: 0.0,
        };
        let mut frame = request.encode([1; 32])?;
        frame[7] = 1;
        assert!(Request::decode(frame).is_err());
        assert!(Request {
            fraction: f64::NAN,
            ..request
        }
        .encode([1; 32])
        .is_err());
        Ok(())
    }
}
