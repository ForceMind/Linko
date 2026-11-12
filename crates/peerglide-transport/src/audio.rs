//! One explicitly selected direction on a pinned audio-only QUIC connection.
use crate::{
    endpoint_alpn,
    trusted_listener::{Fingerprint, Listener},
    verify_peer, Identity,
};
use anyhow::{ensure, Context, Result};
use peerglide_protocol::audio::{Frame, FRAME_BYTES};
use quinn::{crypto::rustls::HandshakeData, Connection, Endpoint};
use std::{
    net::SocketAddr,
    time::{Duration, Instant},
};

pub(super) const ALPN: &[u8] = b"peerglide-audio/1";
const HELLO_MAGIC: &[u8; 4] = b"PGAH";
const HELLO_BYTES: usize = 8;
const HELLO_TIMEOUT: Duration = Duration::from_secs(4);
const READ_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_CONSENT: Duration = Duration::from_secs(60 * 60);
// The source emits a 5 ms frame on its clock. Permit up to 1 ms of
// scheduling jitter without allowing multiple immediately queued frames.
const MIN_SEND_INTERVAL: Duration = Duration::from_millis(4);

#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Direction {
    Send = 1,
    Playback = 2,
}

impl Direction {
    fn hello(self) -> [u8; HELLO_BYTES] {
        [
            HELLO_MAGIC[0],
            HELLO_MAGIC[1],
            HELLO_MAGIC[2],
            HELLO_MAGIC[3],
            1,
            self as u8,
            0,
            0,
        ]
    }

    fn matches(self, peer: [u8; HELLO_BYTES]) -> bool {
        let opposite = match self {
            Self::Send => Self::Playback,
            Self::Playback => Self::Send,
        };
        peer == opposite.hello()
    }
}

pub fn endpoint(address: SocketAddr, local: &Identity, pinned_peer: &[u8]) -> Result<Endpoint> {
    ensure!(
        local.certificate.as_slice() != pinned_peer,
        "audio peer must have a distinct identity"
    );
    endpoint_alpn(address, local, pinned_peer, ALPN)
}

/// A local set of individually authorized playback sources on one audio port.
/// The slot is derived from the configured certificate order, never from a frame.
pub struct PlaybackListener {
    trusted: Listener,
    sources: Vec<Fingerprint>,
}

impl PlaybackListener {
    pub fn bind(address: SocketAddr, local: &Identity, peers: Vec<Vec<u8>>) -> Result<Self> {
        ensure!(
            (1..=crate::trusted_listener::MAX_TRUSTED_PEERS).contains(&peers.len()),
            "playback source count outside listener limits"
        );
        let sources = peers
            .iter()
            .map(|peer| *blake3::hash(peer).as_bytes())
            .collect();
        let trusted = Listener::bind_alpn(address, local, peers, ALPN)?;
        Ok(Self { trusted, sources })
    }

    pub fn local_addr(&self) -> Result<SocketAddr> {
        self.trusted.local_addr()
    }

    pub async fn accept(&self, remaining: Duration) -> Result<(usize, Session)> {
        ensure!(
            !remaining.is_zero() && remaining <= MAX_CONSENT,
            "invalid audio consent duration"
        );
        let deadline = Instant::now() + remaining;
        tokio::time::timeout(remaining, async {
            let (connection, fingerprint) = self.trusted.accept().await?.into_parts();
            let slot = self
                .sources
                .iter()
                .position(|source| *source == fingerprint)
                .context("authenticated audio source has no local slot")?;
            let certificate = self
                .trusted
                .certificate(fingerprint)
                .context("authenticated audio source is unavailable")?;
            let session = accept(
                &connection,
                certificate,
                Direction::Playback,
                deadline.saturating_duration_since(Instant::now()),
            )
            .await?;
            Ok((slot, session))
        })
        .await
        .context("audio listener consent expired")?
    }

    pub fn close(&self) {
        self.trusted.close();
    }
}

/// The UI or local worker supplies the direction and consent duration.
/// The peer's hello confirms compatibility; it cannot create local consent.
pub async fn open(
    connection: &Connection,
    pinned_peer: &[u8],
    direction: Direction,
    consent: Duration,
) -> Result<Session> {
    establish(connection, pinned_peer, direction, consent, true).await
}

pub async fn accept(
    connection: &Connection,
    pinned_peer: &[u8],
    direction: Direction,
    consent: Duration,
) -> Result<Session> {
    establish(connection, pinned_peer, direction, consent, false).await
}

async fn establish(
    connection: &Connection,
    pinned_peer: &[u8],
    direction: Direction,
    consent: Duration,
    connector: bool,
) -> Result<Session> {
    let started = Instant::now();
    ensure!(
        !consent.is_zero() && consent <= MAX_CONSENT,
        "invalid audio consent duration"
    );
    let handshake = connection
        .handshake_data()
        .context("audio handshake unavailable")?
        .downcast::<HandshakeData>()
        .map_err(|_| anyhow::anyhow!("unexpected audio handshake"))?;
    ensure!(
        handshake.protocol.as_deref() == Some(ALPN),
        "unexpected audio ALPN"
    );
    let result = tokio::time::timeout(HELLO_TIMEOUT.min(consent), async {
        verify_peer(connection, pinned_peer)?;
        ensure!(
            connection
                .max_datagram_size()
                .is_some_and(|limit| limit >= FRAME_BYTES),
            "audio datagram size unavailable"
        );
        let (mut send, mut receive) = if connector {
            connection.open_bi().await?
        } else {
            connection.accept_bi().await?
        };
        ensure!(send.id().index() == 0, "audio hello already used");
        let mut peer = [0; HELLO_BYTES];
        if connector {
            send.write_all(&direction.hello()).await?;
            receive.read_exact(&mut peer).await?;
            ensure!(direction.matches(peer), "incompatible audio direction");
        } else {
            receive.read_exact(&mut peer).await?;
            ensure!(direction.matches(peer), "incompatible audio direction");
            send.write_all(&direction.hello()).await?;
        }
        send.finish()?;
        Ok::<_, anyhow::Error>(())
    })
    .await
    .context("audio hello timed out")
    .and_then(|result| result);
    if let Err(error) = result {
        connection.close(1u8.into(), b"audio hello failed");
        return Err(error);
    }
    Ok(Session {
        connection: connection.clone(),
        direction,
        deadline: started + consent,
        next_send: started,
    })
}

pub struct Session {
    connection: Connection,
    direction: Direction,
    deadline: Instant,
    next_send: Instant,
}

impl Session {
    /// False means the live frame was skipped to avoid buffering a burst.
    pub fn send(&mut self, frame: &Frame) -> Result<bool> {
        self.check(Direction::Send)?;
        let now = Instant::now();
        if now < self.next_send {
            return Ok(false);
        }
        let bytes = frame.encode().context("invalid local audio frame")?;
        self.connection.send_datagram(bytes.to_vec().into())?;
        self.next_send = now + MIN_SEND_INTERVAL;
        Ok(true)
    }

    pub async fn receive(&self) -> Result<Frame> {
        self.check(Direction::Playback)?;
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        let result =
            tokio::time::timeout(READ_TIMEOUT.min(remaining), self.connection.read_datagram())
                .await
                .context("audio receive timed out")
                .and_then(|received| received.map_err(Into::into));
        let decoded =
            result.and_then(|bytes| Frame::decode(&bytes).context("invalid audio datagram"));
        let frame = match decoded {
            Ok(frame) => frame,
            Err(error) => {
                self.stop();
                return Err(error);
            }
        };
        self.check(Direction::Playback)?;
        Ok(frame)
    }

    pub fn stop(&self) {
        self.connection.close(0u8.into(), b"audio session stopped");
    }

    fn check(&self, direction: Direction) -> Result<()> {
        ensure!(
            self.direction == direction,
            "audio direction is not authorized"
        );
        if Instant::now() >= self.deadline {
            self.stop();
            anyhow::bail!("audio consent expired");
        }
        ensure!(
            self.connection.close_reason().is_none(),
            "audio session closed"
        );
        Ok(())
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peerglide_protocol::audio::SAMPLES;

    #[tokio::test]
    async fn pinned_audio_session_sends_pcm_only_in_the_selected_direction() -> Result<()> {
        let source = Identity::generate()?;
        let sink = Identity::generate()?;
        let bind = "127.0.0.1:0".parse()?;
        let left = endpoint(bind, &source, &sink.certificate)?;
        let right = endpoint(bind, &sink, &source.certificate)?;
        let (client, server) = tokio::try_join!(
            async {
                Ok::<_, anyhow::Error>(
                    left.connect(right.local_addr()?, "peerglide.local")?
                        .await?,
                )
            },
            async {
                Ok::<_, anyhow::Error>(right.accept().await.context("missing audio peer")?.await?)
            }
        )?;
        let duration = Duration::from_secs(30);
        let (mut outgoing, mut incoming) = tokio::try_join!(
            open(&client, &sink.certificate, Direction::Send, duration),
            accept(&server, &source.certificate, Direction::Playback, duration)
        )?;
        let frame = Frame {
            sequence: 1,
            samples: [-4; SAMPLES],
        };
        assert!(incoming.send(&frame).is_err());
        assert!(outgoing.receive().await.is_err());
        assert!(outgoing.send(&frame)?);
        let received = incoming.receive().await?;
        assert_eq!(received.sequence, 1);
        assert_eq!(received.samples, frame.samples);
        incoming.stop();
        assert!(incoming.receive().await.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn one_audio_port_binds_each_pinned_source_to_its_local_slot() -> Result<()> {
        let output = Identity::generate()?;
        let left = Identity::generate()?;
        let right = Identity::generate()?;
        let bind = "127.0.0.1:0".parse()?;
        assert!(PlaybackListener::bind(
            bind,
            &output,
            vec![left.certificate.clone(), left.certificate.clone()],
        )
        .is_err());
        let listener = PlaybackListener::bind(
            bind,
            &output,
            vec![left.certificate.clone(), right.certificate.clone()],
        )?;
        let address = listener.local_addr()?;
        let consent = Duration::from_secs(10);
        let receive = async {
            let first = listener.accept(consent).await?;
            let second = listener.accept(consent).await?;
            let mut slots = [first.0, second.0];
            slots.sort_unstable();
            assert_eq!(slots, [0, 1]);
            Ok::<_, anyhow::Error>((first, second))
        };
        let sink_pin = &output.certificate;
        let connect = |identity: Identity| async move {
            let endpoint = endpoint(bind, &identity, sink_pin)?;
            let connection = endpoint.connect(address, "peerglide.local")?.await?;
            let session = open(&connection, sink_pin, Direction::Send, consent).await?;
            Ok::<_, anyhow::Error>((endpoint, session))
        };
        let ((first, second), (left_endpoint, mut left), (right_endpoint, mut right)) =
            tokio::time::timeout(consent, async {
                tokio::try_join!(receive, connect(left), connect(right))
            })
            .await??;
        for (source, value) in [(&mut left, 5), (&mut right, 7)] {
            assert!(source.send(&Frame {
                sequence: 1,
                samples: [value; SAMPLES]
            })?);
        }
        for (slot, session) in [first, second] {
            assert_eq!(
                session.receive().await?.samples,
                [[5; SAMPLES], [7; SAMPLES]][slot]
            );
        }
        listener.close();
        drop((left_endpoint, right_endpoint));
        Ok(())
    }

    #[tokio::test]
    async fn audio_cannot_establish_on_the_input_alpn() -> Result<()> {
        let source = Identity::generate()?;
        let sink = Identity::generate()?;
        let bind = "127.0.0.1:0".parse()?;
        let left = endpoint(bind, &source, &sink.certificate)?;
        let right = crate::endpoint(bind, &sink, &source.certificate)?;
        let connection = left.connect(right.local_addr()?, "peerglide.local")?;
        let (outbound, inbound) = tokio::join!(connection, async {
            let incoming = right
                .accept()
                .await
                .context("missing rejected audio peer")?;
            Ok::<_, anyhow::Error>(incoming.await?)
        });
        assert!(outbound.is_err() && inbound.is_err());
        Ok(())
    }
}
