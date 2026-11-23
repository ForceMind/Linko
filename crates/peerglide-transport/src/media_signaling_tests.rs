//! CI transport for the offline viewing model. Test media fields model the
//! independently parsed description the adapter would apply.
use std::io::Read;
use std::time::Duration;

use anyhow::{bail, ensure, Context, Result};
use linko_workspace_lab::session::{SessionGate, SurfaceKind};
use linko_workspace_lab::signaling::{
    ControlDescription, Direction, MediaParameters, Negotiation, Role,
};
use linko_workspace_lab::{Binding, Error, SurfaceKey};
use quinn::crypto::rustls::HandshakeData;
use quinn::{Connection, Endpoint};
use tokio::time::timeout;

use super::{endpoint, endpoint_alpn, verify_peer, Identity};

const TEST_ALPN: &[u8] = b"linko-rw04-ci/1";
const EXPORTER: &[u8] = b"EXPORTER-Linko-Test-RW04";
const MAGIC: &[u8; 5] = b"LTSG\x01";
const WAIT: Duration = Duration::from_secs(5);
const RECORD_BYTES: usize = 5 + 1 + 16 + 4 * 8 + 2 * (1 + 32 + 32);
const OFFER: MediaParameters = MediaParameters {
    direction: Direction::SendOnly,
    dtls_sha256: [10; 32],
    ice_credentials_digest: [11; 32],
};
const ANSWER: MediaParameters = MediaParameters {
    direction: Direction::ReceiveOnly,
    dtls_sha256: [12; 32],
    ice_credentials_digest: [13; 32],
};

fn test_endpoint(identity: &Identity, peer: &Identity) -> Result<Endpoint> {
    endpoint_alpn(
        "127.0.0.1:0".parse()?,
        identity,
        &peer.certificate,
        TEST_ALPN,
    )
}

fn authenticated_peer(connection: &Connection, enrolled: &Identity) -> Result<[u8; 32]> {
    let negotiated = connection
        .handshake_data()
        .context("missing QUIC handshake data")?
        .downcast::<HandshakeData>()
        .map_err(|_| anyhow::anyhow!("wrong QUIC handshake data"))?;
    ensure!(negotiated.protocol.as_deref() == Some(TEST_ALPN));
    verify_peer(connection, &enrolled.certificate)?;
    // verify_peer has compared the actual TLS leaf to these exact DER bytes.
    Ok(*blake3::hash(&enrolled.certificate).as_bytes())
}

struct Pair {
    source: Connection,
    viewer: Connection,
    binding: Binding,
}

impl Pair {
    async fn connect(
        source: &Identity,
        viewer: &Identity,
        source_endpoint: &Endpoint,
        viewer_endpoint: &Endpoint,
    ) -> Result<Self> {
        let addr = source_endpoint.local_addr()?;
        let incoming = async {
            Ok::<_, anyhow::Error>(
                source_endpoint
                    .accept()
                    .await
                    .context("no incoming")?
                    .await?,
            )
        };
        let outgoing = async {
            Ok::<_, anyhow::Error>(viewer_endpoint.connect(addr, "peerglide.local")?.await?)
        };
        let (at_source, at_viewer) =
            timeout(WAIT, async { tokio::try_join!(incoming, outgoing) }).await??;
        let viewer_id = authenticated_peer(&at_source, viewer)?;
        let source_id = authenticated_peer(&at_viewer, source)?;
        let mut context = [0; 64];
        context[..32].copy_from_slice(&source_id);
        context[32..].copy_from_slice(&viewer_id);
        let mut nonce = [0; 16];
        let mut peer_nonce = [0; 16];
        at_source
            .export_keying_material(&mut nonce, EXPORTER, &context)
            .map_err(|_| anyhow::anyhow!("source could not derive connection binding"))?;
        at_viewer
            .export_keying_material(&mut peer_nonce, EXPORTER, &context)
            .map_err(|_| anyhow::anyhow!("viewer could not derive connection binding"))?;
        ensure!(nonce != [0; 16] && nonce == peer_nonce);
        Ok(Self {
            source: at_source,
            viewer: at_viewer,
            binding: Binding {
                source: source_id,
                viewer: viewer_id,
                nonce,
            },
        })
    }

    fn viewing_gate(&self) -> Result<SessionGate> {
        let mut gate = SessionGate::new(
            SurfaceKey {
                binding: self.binding,
                object: 1,
                generation: 1,
            },
            SurfaceKind::Window,
            1,
            false,
        )?;
        assert_eq!(gate.can_view(1), Err(Error::PermissionDenied));
        // A separate local test decision, made after mutual authentication.
        gate.grant_view(1, 100)?;
        Ok(gate)
    }
}

fn append_parameters(bytes: &mut Vec<u8>, parameters: MediaParameters) {
    bytes.push(match parameters.direction {
        Direction::SendOnly => 1,
        Direction::ReceiveOnly => 2,
    });
    bytes.extend_from_slice(&parameters.dtls_sha256);
    bytes.extend_from_slice(&parameters.ice_credentials_digest);
}

fn encode(
    gate: &SessionGate,
    generation: u64,
    role: Role,
    declared: MediaParameters,
    media: MediaParameters,
) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(RECORD_BYTES);
    bytes.extend_from_slice(MAGIC);
    bytes.push(match role {
        Role::Offer => 1,
        Role::Answer => 2,
    });
    let surface = gate.key();
    bytes.extend_from_slice(&surface.binding.nonce);
    for value in [
        surface.object,
        surface.generation,
        gate.view_epoch(),
        generation,
    ] {
        bytes.extend_from_slice(&value.to_be_bytes());
    }
    append_parameters(&mut bytes, declared);
    append_parameters(&mut bytes, media);
    bytes
}

fn field<const N: usize>(fields: &mut &[u8]) -> Result<[u8; N]> {
    let mut bytes = [0; N];
    fields.read_exact(&mut bytes)?;
    Ok(bytes)
}

fn read_parameters(fields: &mut &[u8]) -> Result<MediaParameters> {
    let direction = match field(fields)? {
        [1] => Direction::SendOnly,
        [2] => Direction::ReceiveOnly,
        _ => bail!("invalid test media direction"),
    };
    Ok(MediaParameters {
        direction,
        dtls_sha256: field(fields)?,
        ice_credentials_digest: field(fields)?,
    })
}

fn decode(mut wire: &[u8], binding: Binding) -> Result<(ControlDescription, MediaParameters)> {
    ensure!(wire.len() == RECORD_BYTES, "wrong test description length");
    ensure!(
        &field::<5>(&mut wire)? == MAGIC,
        "unknown test protocol version"
    );
    let role = match field(&mut wire)? {
        [1] => Role::Offer,
        [2] => Role::Answer,
        _ => bail!("invalid test description role"),
    };
    let surface = SurfaceKey {
        binding: Binding {
            nonce: field(&mut wire)?,
            ..binding
        },
        object: u64::from_be_bytes(field(&mut wire)?),
        generation: u64::from_be_bytes(field(&mut wire)?),
    };
    let control = ControlDescription {
        surface,
        view_epoch: u64::from_be_bytes(field(&mut wire)?),
        generation: u64::from_be_bytes(field(&mut wire)?),
        role,
        parameters: read_parameters(&mut wire)?,
    };
    let media = read_parameters(&mut wire)?;
    ensure!(wire.is_empty(), "trailing test description fields");
    Ok((control, media))
}

async fn transfer(sender: &Connection, receiver: &Connection, bytes: &[u8]) -> Result<Vec<u8>> {
    let outgoing = async {
        let (mut tx, _rx) = sender.open_bi().await?;
        tx.write_all(bytes).await?;
        tx.finish()?;
        Ok::<_, anyhow::Error>(())
    };
    let incoming = async {
        let (_tx, mut rx) = receiver.accept_bi().await?;
        Ok::<_, anyhow::Error>(rx.read_to_end(RECORD_BYTES).await?)
    };
    let (_, received) = timeout(WAIT, async { tokio::try_join!(outgoing, incoming) }).await??;
    Ok(received)
}

#[tokio::test]
async fn authenticates_both_descriptions_and_rejects_old_connection_or_revoked_grant() -> Result<()>
{
    let source = Identity::generate()?;
    let viewer = Identity::generate()?;
    let source_endpoint = test_endpoint(&source, &viewer)?;
    let viewer_endpoint = test_endpoint(&viewer, &source)?;
    let pair = Pair::connect(&source, &viewer, &source_endpoint, &viewer_endpoint).await?;
    let mut gate = pair.viewing_gate()?;
    let mut model = Negotiation::new(&gate, 1)?;
    let generation = model.begin(&mut gate, 1)?;
    let source_peer_at_viewer = authenticated_peer(&pair.viewer, &source)?;
    let viewer_peer_at_source = authenticated_peer(&pair.source, &viewer)?;

    let wrong_media = MediaParameters {
        dtls_sha256: [14; 32],
        ..OFFER
    };
    let substituted = encode(&gate, generation, Role::Offer, OFFER, wrong_media);
    let substituted = transfer(&pair.source, &pair.viewer, &substituted).await?;
    let (description, media) = decode(&substituted, pair.binding)?;
    assert_eq!(
        model.admit(&gate, source_peer_at_viewer, &description, media, 2),
        Err(Error::PermissionDenied)
    );

    let offer = encode(&gate, generation, Role::Offer, OFFER, OFFER);
    let truncated = transfer(&pair.source, &pair.viewer, &offer[..RECORD_BYTES - 1]).await?;
    assert!(decode(&truncated, pair.binding).is_err());
    let offer = transfer(&pair.source, &pair.viewer, &offer).await?;
    let (description, media) = decode(&offer, pair.binding)?;
    model.admit(&gate, source_peer_at_viewer, &description, media, 3)?;
    assert_eq!(
        model.ready(&gate, generation, 3),
        Err(Error::InvalidSequence)
    );

    let answer = encode(&gate, generation, Role::Answer, ANSWER, ANSWER);
    let answer = transfer(&pair.viewer, &pair.source, &answer).await?;
    let (description, media) = decode(&answer, pair.binding)?;
    model.admit(&gate, viewer_peer_at_source, &description, media, 4)?;
    model.ready(&gate, generation, 4)?;
    assert_eq!(gate.can_control(4), Err(Error::PermissionDenied));

    gate.revoke_all(5)?;
    let answer = transfer(&pair.viewer, &pair.source, &answer).await?;
    let (description, media) = decode(&answer, pair.binding)?;
    assert_eq!(
        model.admit(&gate, viewer_peer_at_source, &description, media, 5),
        Err(Error::PermissionDenied)
    );
    pair.viewer.close(0u8.into(), b"test revocation");
    assert!(matches!(
        timeout(WAIT, pair.source.closed()).await?,
        quinn::ConnectionError::ApplicationClosed(_)
    ));

    let next = Pair::connect(&source, &viewer, &source_endpoint, &viewer_endpoint).await?;
    assert_ne!(pair.binding.nonce, next.binding.nonce);
    let mut gate = next.viewing_gate()?;
    let mut model = Negotiation::new(&gate, 1)?;
    let next_generation = model.begin(&mut gate, 1)?;
    assert_eq!(next_generation, generation);
    let replay = transfer(&next.source, &next.viewer, &offer).await?;
    let (description, media) = decode(&replay, next.binding)?;
    let peer = authenticated_peer(&next.viewer, &source)?;
    assert_eq!(
        model.admit(&gate, peer, &description, media, 2),
        Err(Error::StaleTarget)
    );

    let current = encode(&gate, next_generation, Role::Offer, OFFER, OFFER);
    let current = transfer(&next.source, &next.viewer, &current).await?;
    let (description, media) = decode(&current, next.binding)?;
    model.admit(&gate, peer, &description, media, 3)?;
    Ok(())
}

#[tokio::test]
async fn rejects_unknown_certificate_and_v5_before_test_signal() -> Result<()> {
    let source = Identity::generate()?;
    let viewer = Identity::generate()?;
    let unknown = Identity::generate()?;
    let server = test_endpoint(&source, &viewer)?;
    let wrong_cert = test_endpoint(&unknown, &source)?;
    let wrong_alpn = endpoint("127.0.0.1:0".parse()?, &viewer, &source.certificate)?;
    let addr = server.local_addr()?;
    for (client, require_client_rejection) in [(&wrong_cert, false), (&wrong_alpn, true)] {
        let receiving = async {
            let incoming = server.accept().await.context("no incoming")?;
            ensure!(
                incoming.await.is_err(),
                "unexpected test signaling handshake"
            );
            Ok::<_, anyhow::Error>(())
        };
        let connecting = async {
            let result = client.connect(addr, "peerglide.local")?.await;
            if require_client_rejection {
                ensure!(result.is_err(), "v5 negotiated the test ALPN");
            }
            Ok::<_, anyhow::Error>(())
        };
        timeout(WAIT, async { tokio::try_join!(receiving, connecting) }).await??;
    }
    Ok(())
}
