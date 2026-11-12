//! Local proof that a display reservation belongs to one authenticated v5 connection.
use crate::verify_peer;
use anyhow::{ensure, Context, Result};
use quinn::{crypto::rustls::HandshakeData, Connection};

const EXPORT_LABEL: &[u8] = b"EXPORTER-Linko-Input-Layout-v1";

/// Never log or persist this binding. The public certificate pin and local
/// receive consent remain separate from this ephemeral connection correlation.
pub struct Binding {
    peer_digest: [u8; 32],
    proof: [u8; 32],
}

impl Binding {
    pub fn from_v5(connection: &Connection, pinned_peer: &[u8]) -> Result<Self> {
        ensure!(
            connection.close_reason().is_none(),
            "input connection already ended"
        );
        let data = connection
            .handshake_data()
            .context("input handshake unavailable")?
            .downcast::<HandshakeData>()
            .map_err(|_| anyhow::anyhow!("unexpected input handshake"))?;
        ensure!(
            data.protocol.as_deref() == Some(peerglide_protocol::ALPN),
            "layout binding requires the v5 input ALPN"
        );
        verify_peer(connection, pinned_peer)?;
        let mut material = [0; 32];
        connection
            .export_keying_material(&mut material, EXPORT_LABEL, b"layout reservation")
            .map_err(|_| anyhow::anyhow!("input connection exporter unavailable"))?;
        let proof = blake3::derive_key("Linko display route binding v1", &material);
        Ok(Self {
            peer_digest: *blake3::hash(pinned_peer).as_bytes(),
            proof,
        })
    }

    /// Share only inside an independently authenticated display negotiation.
    pub fn proof(&self) -> [u8; 32] {
        self.proof
    }

    /// Verify the same peer and the same live input connection after a sidecar
    /// reply. This cannot authorize native input or restore a retired lease.
    pub fn matches(
        &self,
        connection: &Connection,
        pinned_peer: &[u8],
        peer_proof: &[u8; 32],
    ) -> Result<bool> {
        let current = Self::from_v5(connection, pinned_peer)?;
        Ok(self.peer_digest == current.peer_digest
            && self.proof == current.proof
            && self.proof == *peer_proof)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{endpoint, Identity};
    use quinn::Endpoint;
    use std::time::Duration;

    async fn pair(client: &Endpoint, server: &Endpoint) -> Result<(Connection, Connection)> {
        tokio::time::timeout(Duration::from_secs(5), async {
            tokio::try_join!(
                async {
                    let attempt = client.connect(server.local_addr()?, "peerglide.local")?;
                    Ok::<_, anyhow::Error>(attempt.await?)
                },
                async {
                    let incoming = server.accept().await.context("no input connection")?;
                    Ok::<_, anyhow::Error>(incoming.await?)
                },
            )
        })
        .await?
    }

    #[tokio::test]
    async fn display_binding_is_equal_only_for_the_same_live_input_connection() -> Result<()> {
        let left = Identity::generate()?;
        let right = Identity::generate()?;
        let stranger = Identity::generate()?;
        let address = "127.0.0.1:0".parse()?;
        let client = endpoint(address, &left, &right.certificate)?;
        let server = endpoint(address, &right, &left.certificate)?;
        let (first_left, first_right) = pair(&client, &server).await?;
        let (second_left, _second_right) = pair(&client, &server).await?;
        let sent = Binding::from_v5(&first_left, &right.certificate)?;
        let peer = Binding::from_v5(&first_right, &left.certificate)?;
        assert_eq!(sent.proof(), peer.proof());
        assert!(sent.matches(&first_left, &right.certificate, &peer.proof())?);
        assert!(!sent.matches(&second_left, &right.certificate, &peer.proof())?);
        assert!(Binding::from_v5(&first_left, &stranger.certificate).is_err());
        first_left.close(0u8.into(), b"fixture ended");
        assert!(sent
            .matches(&first_left, &right.certificate, &peer.proof())
            .is_err());
        Ok(())
    }
}
