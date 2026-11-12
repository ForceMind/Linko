//! TLS 1.3 mutual certificate authentication, explicit out-of-band certificate enrollment.
//! There is deliberately no accept-any-certificate verifier or network TOFU path.
use anyhow::{bail, Context, Result};
use peerglide_protocol::{decode, encode, Message, MAX_FRAME};
use quinn::{
    crypto::rustls::{QuicClientConfig, QuicServerConfig},
    Endpoint,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use std::{net::SocketAddr, path::Path, sync::Arc, time::Duration};
use tokio::io::AsyncReadExt;

pub mod audio;
pub mod clipboard_worker;
pub mod data;
mod data_frames;
mod data_session;
pub mod file_transfer;
pub mod file_worker;
pub mod pairing;
pub mod trusted_listener;

pub struct Identity {
    pub certificate: Vec<u8>,
    key: Vec<u8>,
}
pub const MAX_CERTIFICATE_BYTES: usize = 16 * 1024;
const MAX_PRIVATE_KEY_BYTES: usize = 16 * 1024;

// Bound both metadata-based fast rejection and the actual read, including files
// that grow after stat. This is not protection against a hostile local filesystem.
fn read_material(path: &Path, limit: usize, kind: &str) -> Result<Vec<u8>> {
    let metadata = std::fs::metadata(path).with_context(|| format!("inspect {kind}"))?;
    anyhow::ensure!(metadata.is_file(), "{kind} must be a regular file");
    anyhow::ensure!(metadata.len() <= limit as u64, "{kind} file too large");
    let file = std::fs::File::open(path).with_context(|| format!("open {kind}"))?;
    anyhow::ensure!(file.metadata()?.is_file(), "{kind} must be a regular file");
    let mut bytes = Vec::new();
    use std::io::Read;
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("read {kind}"))?;
    anyhow::ensure!(bytes.len() <= limit, "{kind} file too large");
    anyhow::ensure!(!bytes.is_empty(), "{kind} file is empty");
    Ok(bytes)
}

/// Bounded DER parsing only. Callers still require explicit fingerprint enrollment
/// and exact TLS leaf verification; reading a valid certificate grants no trust.
pub fn read_certificate(path: &Path) -> Result<Vec<u8>> {
    let bytes = read_material(path, MAX_CERTIFICATE_BYTES, "certificate")?;
    validate_certificate(&bytes)?;
    Ok(bytes)
}
impl Identity {
    pub fn generate() -> Result<Self> {
        let cert = rcgen::generate_simple_self_signed(vec!["peerglide.local".into()])?;
        Ok(Self {
            certificate: cert.cert.der().to_vec(),
            key: cert.key_pair.serialize_der(),
        })
    }
    pub fn load(dir: &Path) -> Result<Self> {
        let id = Self {
            certificate: read_certificate(&dir.join("node.crt.der"))?,
            key: read_material(
                &dir.join("node.key.der"),
                MAX_PRIVATE_KEY_BYTES,
                "private key",
            )?,
        };
        let provider = rustls::crypto::ring::default_provider();
        let certified = rustls::sign::CertifiedKey::from_der(vec![id.cert()], id.key(), &provider)
            .context("invalid identity certificate/private key")?;
        // from_der permits Unknown consistency for some providers; this path
        // requires a positive SPKI match rather than assuming compatibility.
        certified
            .keys_match()
            .context("identity certificate/private key mismatch")?;
        Ok(id)
    }
    pub fn create(dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir)?;
        let id = Self::generate()?;
        // Exclusive creation never silently replaces a previously trusted identity.
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut key = options.open(dir.join("node.key.der"))?;
        use std::io::Write;
        key.write_all(&id.key)?;
        let mut cert = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(dir.join("node.crt.der"))?;
        cert.write_all(&id.certificate)?;
        Ok(id)
    }
    pub fn fingerprint(&self) -> String {
        fingerprint(&self.certificate)
    }
    fn cert(&self) -> CertificateDer<'static> {
        self.certificate.clone().into()
    }
    fn key(&self) -> PrivateKeyDer<'static> {
        PrivatePkcs8KeyDer::from(self.key.clone()).into()
    }
}
pub fn fingerprint(cert: &[u8]) -> String {
    blake3::hash(cert).to_hex().to_string()
}

#[cfg(test)]
mod identity_file_tests {
    use super::*;
    struct Fixture(std::path::PathBuf);
    impl Fixture {
        fn new() -> Result<Self> {
            let marker = Identity::generate()?;
            let path =
                std::env::temp_dir().join(format!("peerglide-identity-{}", marker.fingerprint()));
            std::fs::create_dir(&path)?;
            Ok(Self(path))
        }
        fn identity(&self) -> std::path::PathBuf {
            self.0.join("identity")
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn mismatched_identity_is_rejected_at_load_before_gui_or_endpoint() -> Result<()> {
        let fixture = Fixture::new()?;
        Identity::create(&fixture.identity())?;
        let other = Identity::generate()?;
        std::fs::write(fixture.identity().join("node.key.der"), &other.key)?;
        assert!(
            Identity::load(&fixture.identity()).is_err(),
            "mismatched key/certificate accepted"
        );
        Ok(())
    }

    #[test]
    fn persisted_identity_roundtrips_and_existing_files_are_never_replaced() -> Result<()> {
        let fixture = Fixture::new()?;
        let original = Identity::create(&fixture.identity())?;
        let loaded = Identity::load(&fixture.identity())?;
        assert!(original.certificate == loaded.certificate && original.key == loaded.key);
        assert!(Identity::create(&fixture.identity()).is_err());
        let unchanged = Identity::load(&fixture.identity())?;
        assert!(original.certificate == unchanged.certificate && original.key == unchanged.key);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(fixture.identity().join("node.key.der"))?
                .permissions()
                .mode();
            assert_eq!(
                mode & 0o077,
                0,
                "private key must not grant group/other access"
            );
        }
        Ok(())
    }

    #[test]
    fn certificate_import_rejects_empty_malformed_oversized_and_directory() -> Result<()> {
        let fixture = Fixture::new()?;
        let file = fixture.0.join("import.der");
        for bytes in [
            vec![],
            b"not DER".to_vec(),
            vec![0; MAX_CERTIFICATE_BYTES + 1],
        ] {
            std::fs::write(&file, &bytes)?;
            assert!(read_certificate(&file).is_err());
            assert!(validate_certificate(&bytes).is_err());
        }
        assert!(read_certificate(&fixture.0).is_err());
        let id = Identity::generate()?;
        std::fs::write(&file, &id.certificate)?;
        assert!(read_certificate(&file)? == id.certificate);
        // Exact byte bound is allowed by the reader; DER parsing is a separate gate.
        std::fs::write(&file, vec![0; MAX_CERTIFICATE_BYTES])?;
        assert_eq!(
            read_material(&file, MAX_CERTIFICATE_BYTES, "fixture")?.len(),
            MAX_CERTIFICATE_BYTES
        );
        assert!(read_certificate(&file).is_err());
        Ok(())
    }

    #[test]
    fn malformed_or_oversized_private_key_does_not_load() -> Result<()> {
        let fixture = Fixture::new()?;
        Identity::create(&fixture.identity())?;
        let file = fixture.identity().join("node.key.der");
        for bytes in [
            vec![],
            b"not PKCS8".to_vec(),
            vec![0; MAX_PRIVATE_KEY_BYTES + 1],
        ] {
            std::fs::write(&file, &bytes)?;
            assert!(Identity::load(&fixture.identity()).is_err());
        }
        std::fs::remove_file(&file)?;
        std::fs::create_dir(&file)?;
        assert!(Identity::load(&fixture.identity()).is_err());
        Ok(())
    }
}
fn roots(peer: &[u8]) -> Result<rustls::RootCertStore> {
    anyhow::ensure!(
        !peer.is_empty() && peer.len() <= MAX_CERTIFICATE_BYTES,
        "certificate size outside allowed bounds"
    );
    let mut roots = rustls::RootCertStore::empty();
    roots.add(CertificateDer::from(peer.to_vec()))?;
    Ok(roots)
}
pub fn validate_certificate(certificate: &[u8]) -> Result<()> {
    roots(certificate)?;
    Ok(())
}
fn transport(alpn: &[u8]) -> Arc<quinn::TransportConfig> {
    let mut t = quinn::TransportConfig::default();
    t.max_concurrent_bidi_streams(1u8.into())
        .max_concurrent_uni_streams(0u8.into());
    t.max_idle_timeout(Some(
        Duration::from_secs(4).try_into().expect("constant timeout"),
    ));
    t.keep_alive_interval(Some(Duration::from_millis(400)));
    if alpn == audio::ALPN || alpn == audio::MICROPHONE_ALPN {
        let frame = peerglide_protocol::audio::FRAME_BYTES;
        t.datagram_receive_buffer_size(Some(16 * frame));
        t.datagram_send_buffer_size(4 * frame);
    } else if alpn == data::ALPN {
        t.datagram_receive_buffer_size(None);
        t.datagram_send_buffer_size(0);
    } else {
        t.datagram_receive_buffer_size(Some(64 * 1024));
        // Motion carries an absolute cursor position. Bound the queue so a
        // stalled link discards older positions before accumulating a backlog.
        t.datagram_send_buffer_size(16 * 1024);
    }
    t.receive_window((64 * 1024u32).into());
    t.stream_receive_window((16 * 1024u32).into());
    Arc::new(t)
}
pub fn endpoint(addr: SocketAddr, identity: &Identity, peer: &[u8]) -> Result<Endpoint> {
    endpoint_alpn(addr, identity, peer, peerglide_protocol::ALPN)
}
fn endpoint_alpn(
    addr: SocketAddr,
    identity: &Identity,
    peer: &[u8],
    alpn: &[u8],
) -> Result<Endpoint> {
    let server = server_config(identity, roots(peer)?, alpn)?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut client = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_root_certificates(roots(peer)?)
        .with_client_auth_cert(vec![identity.cert()], identity.key())?;
    client.alpn_protocols = vec![alpn.to_vec()];
    let mut c = quinn::ClientConfig::new(Arc::new(QuicClientConfig::try_from(client)?));
    c.transport_config(transport(alpn));
    let mut e = Endpoint::server(server, addr)?;
    e.set_default_client_config(c);
    Ok(e)
}
fn server_config(
    identity: &Identity,
    roots: rustls::RootCertStore,
    alpn: &[u8],
) -> Result<quinn::ServerConfig> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier = rustls::server::WebPkiClientVerifier::builder_with_provider(
        Arc::new(roots),
        provider.clone(),
    )
    .build()?;
    let mut server = rustls::ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_client_cert_verifier(verifier)
        .with_single_cert(vec![identity.cert()], identity.key())?;
    server.alpn_protocols = vec![alpn.to_vec()];
    let mut config =
        quinn::ServerConfig::with_crypto(Arc::new(QuicServerConfig::try_from(server)?));
    config.transport_config(transport(alpn));
    Ok(config)
}

fn peer_certificate(connection: &quinn::Connection) -> Result<CertificateDer<'static>> {
    let certs = connection
        .peer_identity()
        .context("missing TLS identity")?
        .downcast::<Vec<CertificateDer<'static>>>()
        .map_err(|_| anyhow::anyhow!("unexpected TLS identity"))?;
    (*certs)
        .into_iter()
        .next()
        .context("missing TLS certificate")
}

pub fn verify_peer(connection: &quinn::Connection, expected: &[u8]) -> Result<()> {
    if peer_certificate(connection)?.as_ref() != expected {
        bail!("peer certificate differs from enrolled identity");
    }
    Ok(())
}
pub async fn send(stream: &mut quinn::SendStream, msg: &Message) -> Result<()> {
    let bytes = encode(msg)?;
    if bytes.len() > MAX_FRAME {
        bail!("frame too large");
    }
    let mut frame = [0; 4 + MAX_FRAME];
    let used = &mut frame[..4 + bytes.len()];
    used[..4].copy_from_slice(&(bytes.len() as u32).to_be_bytes());
    used[4..].copy_from_slice(&bytes);
    stream.write_all(used).await?;
    Ok(())
}
#[derive(Debug)]
pub struct MessageStreamEnded;
impl std::fmt::Display for MessageStreamEnded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("message stream ended at a frame boundary")
    }
}
impl std::error::Error for MessageStreamEnded {}
#[derive(Debug)]
pub struct InvalidMessageFrame;
impl std::fmt::Display for InvalidMessageFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("invalid or incomplete message frame")
    }
}
impl std::error::Error for InvalidMessageFrame {}
fn frame_read_error(error: std::io::Error) -> anyhow::Error {
    if error.kind() == std::io::ErrorKind::UnexpectedEof {
        anyhow::Error::from(error).context(InvalidMessageFrame)
    } else {
        // A connection loss is still a network failure, never a decoded frame.
        error.into()
    }
}
pub async fn receive(stream: &mut quinn::RecvStream) -> Result<Message> {
    let mut header = [0u8; 4];
    if AsyncReadExt::read(stream, &mut header[..1]).await? == 0 {
        return Err(MessageStreamEnded.into());
    }
    // A FIN inside a frame is malformed; transport failures keep their class.
    AsyncReadExt::read_exact(stream, &mut header[1..])
        .await
        .map_err(frame_read_error)?;
    let size = u32::from_be_bytes(header) as usize;
    if size == 0 || size > MAX_FRAME {
        return Err(anyhow::anyhow!("frame length invalid").context(InvalidMessageFrame));
    }
    let mut buffer = [0; MAX_FRAME];
    let body = &mut buffer[..size];
    AsyncReadExt::read_exact(stream, body)
        .await
        .map_err(frame_read_error)?;
    decode(body).map_err(|error| anyhow::Error::msg(error).context(InvalidMessageFrame))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn legacy_protocol_peers_are_rejected_before_input_session() -> Result<()> {
        for alpn in [b"peerglide/3".as_slice(), b"peerglide/4".as_slice()] {
            let a = Identity::generate()?;
            let b = Identity::generate()?;
            let old = endpoint_alpn("127.0.0.1:0".parse()?, &a, &b.certificate, alpn)?;
            let current = endpoint("127.0.0.1:0".parse()?, &b, &a.certificate)?;
            let server = async { current.accept().await.unwrap().await.is_err() };
            let client = async {
                old.connect(current.local_addr().unwrap(), "peerglide.local")
                    .unwrap()
                    .await
                    .is_err()
            };
            let (server, client) = tokio::time::timeout(Duration::from_secs(5), async {
                tokio::join!(server, client)
            })
            .await?;
            assert!(server && client, "legacy peers must not silently downgrade");
        }
        Ok(())
    }
    #[tokio::test]
    async fn mutual_tls_loopback_and_motion() -> Result<()> {
        let a = Identity::generate()?;
        let b = Identity::generate()?;
        let ea = endpoint("127.0.0.1:0".parse()?, &a, &b.certificate)?;
        let eb = endpoint("127.0.0.1:0".parse()?, &b, &a.certificate)?;
        let addr = eb.local_addr()?;
        let server = async {
            let c = eb.accept().await.context("no incoming")?.await?;
            verify_peer(&c, &a.certificate)?;
            let (mut tx, mut rx) = c.accept_bi().await?;
            assert!(matches!(
                receive(&mut rx).await?,
                Message::Hello {
                    version: peerglide_protocol::VERSION,
                    ..
                }
            ));
            send(&mut tx, &Message::Granted { lease: 7 }).await?;
            let d = c.read_datagram().await?;
            assert_eq!(&d[..], b"motion");
            send(&mut tx, &Message::Pong { lease: 7 }).await?;
            receive(&mut rx).await?;
            Ok::<_, anyhow::Error>(())
        };
        let client = async {
            let c = ea.connect(addr, "peerglide.local")?.await?;
            verify_peer(&c, &b.certificate)?;
            let (mut tx, mut rx) = c.open_bi().await?;
            send(
                &mut tx,
                &Message::Hello {
                    version: peerglide_protocol::VERSION,
                    capabilities: Default::default(),
                },
            )
            .await?;
            assert!(matches!(
                receive(&mut rx).await?,
                Message::Granted { lease: 7 }
            ));
            c.send_datagram(b"motion".to_vec().into())?;
            assert!(matches!(
                receive(&mut rx).await?,
                Message::Pong { lease: 7 }
            ));
            send(
                &mut tx,
                &Message::Release {
                    lease: 7,
                    cursor: None,
                },
            )
            .await?;
            // Keep connection alive until the other future has consumed the release.
            tokio::time::sleep(Duration::from_millis(30)).await;
            Ok::<_, anyhow::Error>(())
        };
        tokio::time::timeout(Duration::from_secs(5), async {
            tokio::try_join!(server, client)
        })
        .await??;
        Ok(())
    }
    #[tokio::test]
    async fn rejects_unenrolled_client() -> Result<()> {
        let a = Identity::generate()?;
        let b = Identity::generate()?;
        let stranger = Identity::generate()?;
        let ea = endpoint("127.0.0.1:0".parse()?, &stranger, &b.certificate)?;
        let eb = endpoint("127.0.0.1:0".parse()?, &b, &a.certificate)?;
        let server = async { eb.accept().await.unwrap().await.is_err() };
        let client = async {
            if let Ok(c) = ea
                .connect(eb.local_addr().unwrap(), "peerglide.local")
                .unwrap()
                .await
            {
                c.closed().await;
            }
        };
        let (rejected, _) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(server, client)
        })
        .await?;
        assert!(rejected);
        Ok(())
    }
}

#[cfg(test)]
mod negative_tests {
    use super::*;
    #[tokio::test]
    async fn stream_fin_distinguishes_frame_boundaries_from_partial_headers_and_bodies(
    ) -> Result<()> {
        for tail in [
            vec![],
            vec![0],
            vec![0, 0, 0],
            vec![0, 0, 0, 2, b'{'],
            vec![0, 0, 0, 0],
        ] {
            let boundary = tail.is_empty();
            let a = Identity::generate()?;
            let b = Identity::generate()?;
            let ea = endpoint("127.0.0.1:0".parse()?, &a, &b.certificate)?;
            let eb = endpoint("127.0.0.1:0".parse()?, &b, &a.certificate)?;
            let (seen, observed) = tokio::sync::oneshot::channel();
            let server = async {
                let c = eb.accept().await.unwrap().await?;
                let (mut tx, mut rx) = c.accept_bi().await?;
                assert!(matches!(
                    receive(&mut rx).await?,
                    Message::Ping { lease: 0 }
                ));
                let error = receive(&mut rx)
                    .await
                    .err()
                    .context("FIN accepted as a message")?;
                assert_eq!(error.is::<MessageStreamEnded>(), boundary);
                assert_eq!(error.is::<InvalidMessageFrame>(), !boundary);
                send(&mut tx, &Message::Pong { lease: 0 }).await?;
                observed.await?;
                c.close(0u8.into(), b"frame classification checked");
                Ok::<_, anyhow::Error>(())
            };
            let client = async {
                let c = ea.connect(eb.local_addr()?, "peerglide.local")?.await?;
                let (mut tx, mut rx) = c.open_bi().await?;
                send(&mut tx, &Message::Ping { lease: 0 }).await?;
                tx.write_all(&tail).await?;
                tx.finish()?;
                assert!(matches!(
                    receive(&mut rx).await?,
                    Message::Pong { lease: 0 }
                ));
                let _ = seen.send(());
                let _ = c.closed().await;
                Ok::<_, anyhow::Error>(())
            };
            tokio::time::timeout(Duration::from_secs(5), async {
                tokio::try_join!(server, client)
            })
            .await??;
        }
        Ok(())
    }
    #[tokio::test]
    async fn rejects_wrong_server_certificate() -> Result<()> {
        let a = Identity::generate()?;
        let b = Identity::generate()?;
        let other = Identity::generate()?;
        let ea = endpoint("127.0.0.1:0".parse()?, &a, &other.certificate)?;
        let eb = endpoint("127.0.0.1:0".parse()?, &b, &a.certificate)?;
        let server = async {
            let _ = eb.accept().await.unwrap().await;
        };
        let client = async {
            assert!(ea
                .connect(eb.local_addr().unwrap(), "peerglide.local")
                .unwrap()
                .await
                .is_err());
        };
        tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(server, client)
        })
        .await?;
        Ok(())
    }
    #[tokio::test]
    async fn oversized_stream_frame_rejected_without_reading_body() -> Result<()> {
        let a = Identity::generate()?;
        let b = Identity::generate()?;
        let ea = endpoint("127.0.0.1:0".parse()?, &a, &b.certificate)?;
        let eb = endpoint("127.0.0.1:0".parse()?, &b, &a.certificate)?;
        let server = async {
            let c = eb.accept().await.unwrap().await?;
            let (mut tx, mut rx) = c.accept_bi().await?;
            let error = receive(&mut rx)
                .await
                .err()
                .context("oversized frame accepted")?;
            assert!(error.is::<InvalidMessageFrame>());
            send(&mut tx, &Message::Denied { lease: 0 }).await?;
            tokio::time::sleep(Duration::from_millis(50)).await;
            Ok::<_, anyhow::Error>(())
        };
        let client = async {
            let c = ea.connect(eb.local_addr()?, "peerglide.local")?.await?;
            let (mut tx, mut rx) = c.open_bi().await?;
            tx.write_all(&u32::MAX.to_be_bytes()).await?;
            assert!(matches!(
                receive(&mut rx).await?,
                Message::Denied { lease: 0 }
            ));
            Ok::<_, anyhow::Error>(())
        };
        tokio::time::timeout(Duration::from_secs(5), async {
            tokio::try_join!(server, client)
        })
        .await??;
        Ok(())
    }
}
