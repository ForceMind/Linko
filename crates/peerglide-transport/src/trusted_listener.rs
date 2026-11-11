//! Server-only v5 endpoint with exact locally configured certificate pins.
use crate::{peer_certificate, server_config, Identity, MAX_CERTIFICATE_BYTES};
use anyhow::{ensure, Context, Result};
use quinn::{Connection, Endpoint};
use rustls::{pki_types::CertificateDer, RootCertStore};
use std::{collections::BTreeMap, net::SocketAddr, time::Duration};

pub const MAX_TRUSTED_PEERS: usize = 16;
pub type Fingerprint = [u8; 32];

pub struct Listener {
    endpoint: Endpoint,
    pins: BTreeMap<Fingerprint, Vec<u8>>,
}

/// Authentication only. Receiving input still requires local authorization and
/// ownership of the current native admission lease.
pub struct TrustedConnection {
    connection: Connection,
    peer: Fingerprint,
}

impl TrustedConnection {
    pub fn into_parts(self) -> (Connection, Fingerprint) {
        (self.connection, self.peer)
    }
}

impl Listener {
    /// A frozen set from locally trusted settings. Callers replace the listener
    /// and close revoked connections when trust changes.
    pub fn bind(address: SocketAddr, local: &Identity, peers: Vec<Vec<u8>>) -> Result<Self> {
        ensure!(
            (1..=MAX_TRUSTED_PEERS).contains(&peers.len()),
            "trusted peer count outside listener limits"
        );
        let mut roots = RootCertStore::empty();
        let mut pins = BTreeMap::new();
        for certificate in peers {
            ensure!(
                !certificate.is_empty()
                    && certificate.len() <= MAX_CERTIFICATE_BYTES
                    && certificate != local.certificate,
                "invalid trusted listener certificate"
            );
            roots.add(CertificateDer::from(certificate.clone()))?;
            let digest = *blake3::hash(&certificate).as_bytes();
            ensure!(
                pins.insert(digest, certificate).is_none(),
                "duplicate trusted listener certificate"
            );
        }
        let server = server_config(local, roots, peerglide_protocol::ALPN)?;
        let endpoint = Endpoint::server(server, address)?;
        Ok(Self { endpoint, pins })
    }

    pub fn local_addr(&self) -> Result<SocketAddr> {
        Ok(self.endpoint.local_addr()?)
    }

    pub async fn accept(&self) -> Result<TrustedConnection> {
        let incoming = self
            .endpoint
            .accept()
            .await
            .context("input listener closed")?;
        let connection = tokio::time::timeout(Duration::from_secs(5), incoming)
            .await
            .context("trusted input handshake timed out")??;
        match self.exact_pin(&connection) {
            Ok(peer) => Ok(TrustedConnection { connection, peer }),
            Err(error) => {
                connection.close(1u8.into(), b"input identity not enrolled");
                Err(error)
            }
        }
    }

    pub fn close(&self) {
        self.endpoint.close(0u8.into(), b"trusted listener stopped");
    }

    fn exact_pin(&self, connection: &Connection) -> Result<Fingerprint> {
        let certificate = peer_certificate(connection)?;
        let digest = *blake3::hash(certificate.as_ref()).as_bytes();
        ensure!(
            self.pins.get(&digest).map(Vec::as_slice) == Some(certificate.as_ref()),
            "peer certificate differs from enrolled identities"
        );
        Ok(digest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn multiple_pinned_peers_share_one_v5_listener() -> Result<()> {
        let local = Identity::generate()?;
        let first = Identity::generate()?;
        let second = Identity::generate()?;
        let address = "127.0.0.1:0".parse()?;
        let listener = Listener::bind(
            address,
            &local,
            vec![first.certificate.clone(), second.certificate.clone()],
        )?;
        let left = crate::endpoint(address, &first, &local.certificate)?;
        let right = crate::endpoint(address, &second, &local.certificate)?;
        let bound = listener.local_addr()?;
        let accepting = async {
            let (first, second) = tokio::try_join!(listener.accept(), listener.accept())?;
            Ok::<_, anyhow::Error>((first.into_parts(), second.into_parts()))
        };
        let connecting = async {
            let first = left.connect(bound, "peerglide.local")?;
            let second = right.connect(bound, "peerglide.local")?;
            Ok::<_, anyhow::Error>(tokio::try_join!(first, second)?)
        };
        let (((server_left, left_pin), (server_right, right_pin)), (client_left, client_right)) =
            tokio::time::timeout(std::time::Duration::from_secs(8), async {
                tokio::try_join!(accepting, connecting)
            })
            .await??;
        let mut admitted = [left_pin, right_pin];
        admitted.sort_unstable();
        let mut expected = [
            *blake3::hash(&first.certificate).as_bytes(),
            *blake3::hash(&second.certificate).as_bytes(),
        ];
        expected.sort_unstable();
        assert_eq!(admitted, expected);
        crate::verify_peer(&client_left, &local.certificate)?;
        crate::verify_peer(&client_right, &local.certificate)?;
        listener.close();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::join!(server_left.closed(), server_right.closed())
        })
        .await?;
        Ok(())
    }

    #[tokio::test]
    async fn unregistered_client_and_invalid_pin_sets_are_rejected() -> Result<()> {
        let local = Identity::generate()?;
        let allowed = Identity::generate()?;
        let outsider = Identity::generate()?;
        let address = "127.0.0.1:0".parse()?;
        assert!(Listener::bind(address, &local, Vec::new()).is_err());
        assert!(Listener::bind(address, &local, vec![local.certificate.clone()]).is_err());
        assert!(Listener::bind(
            address,
            &local,
            vec![allowed.certificate.clone(), allowed.certificate.clone()],
        )
        .is_err());
        let listener = Listener::bind(address, &local, vec![allowed.certificate])?;
        let peer = crate::endpoint(address, &outsider, &local.certificate)?;
        let connecting = peer.connect(listener.local_addr()?, "peerglide.local")?;
        let rejected =
            tokio::time::timeout(std::time::Duration::from_secs(8), listener.accept()).await?;
        assert!(rejected.is_err());
        // TLS client completion can precede server verification, so its local
        // success is never proof of admission on the accepting end.
        drop(connecting);
        listener.close();
        Ok(())
    }
}
