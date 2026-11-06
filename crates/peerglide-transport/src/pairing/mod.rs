//! Opt-in ephemeral short-code authentication over TCP, then in-place strict
//! mutual TLS. This returns a temporary peer proposal, never persistent trust or
//! input/data/action authority. The production GUI is not wired to this yet.
mod crypto;
mod wire;

use crate::{Identity, MAX_CERTIFICATE_BYTES};
use anyhow::{Context, Result};
use rand::{rngs::OsRng, Rng, RngCore};
use std::{future::Future, net::SocketAddr, sync::Arc, time::Duration};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::watch,
    time::Instant,
};
use tokio_rustls::{TlsAcceptor, TlsConnector, TlsStream};
use zeroize::Zeroizing;

pub const CODE_DIGITS: usize = 8;
pub const OFFER_LIFETIME: Duration = Duration::from_secs(120);
pub const ATTEMPT_LIMIT: u8 = 5;
const ATTEMPT_LIFETIME: Duration = Duration::from_secs(20);
const MAX_FRAME: usize = MAX_CERTIFICATE_BYTES + 64;
const ALPN: &[u8] = b"peerglide-pairing/1";
const DOMAIN: &[u8] = b"PeerGlide/pairing/1\0OPAQUE-RFC9807/Ristretto255/TripleDh/SHA512/Argon2id-65536-3-1\0input=5\0client=input-code;server=display-code\0";

/// Internal computation failure must stop the child, never become a code retry.
#[derive(Debug)]
pub struct ComputationFailed;
impl std::fmt::Display for ComputationFailed {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("pairing computation stopped")
    }
}
impl std::error::Error for ComputationFailed {}

/// Clone only this cancellation handle, never an offer/code or PAKE state.
#[derive(Clone)]
pub struct Cancellation(watch::Sender<bool>);
impl Cancellation {
    pub fn cancel(&self) {
        self.0.send_replace(true);
    }
}
#[derive(Clone)]
struct Lifetime {
    deadline: Instant,
    cancelled: watch::Receiver<bool>,
}
impl Lifetime {
    fn new(deadline: Instant) -> (Self, Cancellation) {
        let (tx, rx) = watch::channel(false);
        (
            Self {
                deadline,
                cancelled: rx,
            },
            Cancellation(tx),
        )
    }
    fn check(&self) -> Result<()> {
        anyhow::ensure!(!*self.cancelled.borrow(), "pairing cancelled");
        anyhow::ensure!(Instant::now() < self.deadline, "pairing expired");
        Ok(())
    }
    async fn run<T>(&self, future: impl Future<Output = Result<T>>) -> Result<T> {
        self.check()?;
        let mut cancelled = self.cancelled.clone();
        let wait = async {
            loop {
                if *cancelled.borrow_and_update() {
                    break;
                }
                if cancelled.changed().await.is_err() {
                    break;
                }
            }
        };
        let result = tokio::select! {
            biased;
            _ = wait => anyhow::bail!("pairing cancelled"),
            result = tokio::time::timeout_at(self.deadline, future) => result.context("pairing expired")??,
        };
        self.check()?;
        Ok(result)
    }
    fn attempt(&self) -> Self {
        Self {
            deadline: self.deadline.min(Instant::now() + ATTEMPT_LIFETIME),
            cancelled: self.cancelled.clone(),
        }
    }
}

#[derive(Clone)]
struct Binding {
    client: [u8; 32],
    server: [u8; 32],
    context: Vec<u8>,
}
impl Binding {
    fn new(client: &[u8], server: &[u8], offer: [u8; 32], attempt: [u8; 32]) -> Result<Self> {
        let client = *blake3::hash(client).as_bytes();
        let server = *blake3::hash(server).as_bytes();
        anyhow::ensure!(client != server, "cannot pair local identity with itself");
        let mut context = DOMAIN.to_vec();
        for field in [&client, &server, &offer, &attempt] {
            context.extend_from_slice(field);
        }
        Ok(Self {
            client,
            server,
            context,
        })
    }
}
fn random_id() -> [u8; 32] {
    let mut id = [0; 32];
    OsRng.fill_bytes(&mut id);
    id
}

/// One explicitly displayed code, with a single absolute lifetime and a budget
/// shared by ALL accepted sockets/IPs/certificates. Not Clone or Debug.
pub struct Offer {
    code: Zeroizing<String>,
    id: [u8; 32],
    local_certificate: Vec<u8>,
    lifetime: Lifetime,
    cancellation: Cancellation,
    attempts: u8,
    completed: bool,
}
impl Offer {
    pub fn new(identity: &Identity) -> Result<Self> {
        crate::validate_certificate(&identity.certificate)?;
        let (lifetime, cancellation) = Lifetime::new(Instant::now() + OFFER_LIFETIME);
        Ok(Self {
            code: Zeroizing::new(format!("{:08}", OsRng.gen_range(0..100_000_000u32))),
            id: random_id(),
            local_certificate: identity.certificate.clone(),
            lifetime,
            cancellation,
            attempts: 0,
            completed: false,
        })
    }
    /// Display locally only. Never log, persist, discover, or send this value.
    pub fn code(&self) -> Result<&str> {
        self.lifetime.check()?;
        anyhow::ensure!(
            !self.completed && self.attempts < ATTEMPT_LIMIT,
            "pairing offer exhausted"
        );
        Ok(&self.code)
    }
    pub fn cancellation(&self) -> Cancellation {
        self.cancellation.clone()
    }
    pub fn attempts_remaining(&self) -> u8 {
        ATTEMPT_LIMIT.saturating_sub(self.attempts)
    }

    pub async fn accept(
        &mut self,
        listener: &TcpListener,
        identity: Arc<Identity>,
    ) -> Result<PairingSession> {
        self.code()?;
        anyhow::ensure!(
            identity.certificate == self.local_certificate,
            "pairing identity changed"
        );
        let (socket, _) = self
            .lifetime
            .run(async { Ok(listener.accept().await?) })
            .await?;
        self.accept_connected(socket, identity).await
    }

    /// A supervisor may accept a socket to report real incoming progress. The
    /// same offer budget and absolute deadline still apply before parsing/KSF.
    pub async fn accept_connected(
        &mut self,
        mut socket: TcpStream,
        identity: Arc<Identity>,
    ) -> Result<PairingSession> {
        self.code()?;
        anyhow::ensure!(
            identity.certificate == self.local_certificate,
            "pairing identity changed"
        );
        // Charge before reading any attacker-controlled bytes or running KSF.
        // Malformed frames, disconnect after KE2 and changed IP all consume it.
        self.attempts += 1;
        let attempt_lifetime = self.lifetime.attempt();
        let result = attempt_lifetime
            .run(async {
                socket.set_nodelay(true)?;
                let hello = wire::read(&mut socket, wire::CLIENT_HELLO, MAX_FRAME).await?;
                let (attempt, peer) = wire::parse_hello(&hello)?;
                let binding = Binding::new(&peer, &identity.certificate, self.id, attempt)?;
                wire::write(
                    &mut socket,
                    wire::SERVER_HELLO,
                    &wire::hello(&self.id, &identity.certificate)?,
                )
                .await?;
                let ke1 = wire::read(&mut socket, wire::KE1, 1024).await?;
                let code = self.code.clone();
                let bound = binding.clone();
                let (state, ke2) =
                    crypto::compute(move || crypto::server_start(code.as_bytes(), &bound, &ke1))
                        .await?;
                wire::write(&mut socket, wire::KE2, &ke2).await?;
                let ke3 = wire::read(&mut socket, wire::KE3, 1024).await?;
                crypto::server_finish(state, &binding, &ke3)?;
                // Exact reads above leave any pipelined ClientHello in this socket.
                let tls = TlsAcceptor::from(Arc::new(server_config(&identity, &peer)?))
                    .accept(socket)
                    .await?;
                let mut stream = TlsStream::Server(tls);
                verify_tls(&stream, &peer)?;
                wire::write(&mut stream, wire::SERVER_READY, &binding.context).await?;
                let ready = wire::read(&mut stream, wire::CLIENT_READY, MAX_FRAME).await?;
                wire::verify_ready(&ready, &binding)?;
                Ok(PairingSession {
                    stream,
                    peer_certificate: peer,
                    binding,
                    lifetime: self.lifetime.clone(),
                    cancellation: self.cancellation.clone(),
                })
            })
            .await;
        if result.is_ok() {
            self.completed = true;
        }
        result
    }
}
impl Drop for Offer {
    fn drop(&mut self) {
        // A successful return transfers lifetime ownership to PairingSession.
        // Failure/abandonment still invalidates pending work and proposals.
        if !self.completed {
            self.cancellation.cancel();
        }
    }
}

/// One explicit code submission, consumed by one connection attempt. No automatic
/// search/retry against candidate endpoints (malicious servers can guess too).
pub struct CodeEntry {
    code: Zeroizing<String>,
    lifetime: Lifetime,
    cancellation: Cancellation,
}
impl CodeEntry {
    pub fn new(code: String) -> Result<Self> {
        let code = Zeroizing::new(code);
        anyhow::ensure!(
            code.len() == CODE_DIGITS && code.bytes().all(|b| b.is_ascii_digit()),
            "expected eight digit pairing code"
        );
        let (lifetime, cancellation) = Lifetime::new(Instant::now() + OFFER_LIFETIME);
        Ok(Self {
            code,
            lifetime,
            cancellation,
        })
    }
    pub fn cancellation(&self) -> Cancellation {
        self.cancellation.clone()
    }
    pub async fn connect(
        self,
        address: SocketAddr,
        identity: Arc<Identity>,
    ) -> Result<PairingSession> {
        let limit = self.lifetime.attempt();
        limit
            .run(async {
                let mut socket = TcpStream::connect(address).await?;
                socket.set_nodelay(true)?;
                let attempt = random_id();
                wire::write(
                    &mut socket,
                    wire::CLIENT_HELLO,
                    &wire::hello(&attempt, &identity.certificate)?,
                )
                .await?;
                let hello = wire::read(&mut socket, wire::SERVER_HELLO, MAX_FRAME).await?;
                let (offer, peer) = wire::parse_hello(&hello)?;
                let binding = Binding::new(&identity.certificate, &peer, offer, attempt)?;
                let (state, ke1) = crypto::client_start(self.code.as_bytes())?;
                wire::write(&mut socket, wire::KE1, &ke1).await?;
                let ke2 = wire::read(&mut socket, wire::KE2, 1024).await?;
                let code = self.code.clone();
                let bound = binding.clone();
                let ke3 = crypto::compute(move || {
                    crypto::client_finish(state, code.as_bytes(), &bound, &ke2)
                })
                .await?;
                wire::write(&mut socket, wire::KE3, &ke3).await?;
                let name = rustls::pki_types::ServerName::try_from("peerglide.local")?;
                let tls = TlsConnector::from(Arc::new(client_config(&identity, &peer)?))
                    .connect(name, socket)
                    .await?;
                let mut stream = TlsStream::Client(tls);
                verify_tls(&stream, &peer)?;
                wire::write(&mut stream, wire::CLIENT_READY, &binding.context).await?;
                let ready = wire::read(&mut stream, wire::SERVER_READY, MAX_FRAME).await?;
                wire::verify_ready(&ready, &binding)?;
                Ok(PairingSession {
                    stream,
                    peer_certificate: peer,
                    binding,
                    lifetime: self.lifetime.clone(),
                    cancellation: self.cancellation.clone(),
                })
            })
            .await
    }
}

/// Private construction after PAKE, strict TLS and peer ready. A temporary
/// proposal only; durable two-peer enrollment/receipts are a separate operation.
pub struct PairingSession {
    stream: TlsStream<TcpStream>,
    peer_certificate: Vec<u8>,
    binding: Binding,
    lifetime: Lifetime,
    cancellation: Cancellation,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PeerEnd {
    Disconnected,
    UnexpectedData,
}
impl Drop for PairingSession {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}
impl PairingSession {
    pub fn cancellation(&self) -> Cancellation {
        self.cancellation.clone()
    }
    pub fn peer_certificate(&self) -> Result<&[u8]> {
        self.lifetime.check()?;
        verify_tls(&self.stream, &self.peer_certificate)?;
        Ok(&self.peer_certificate)
    }
    pub fn context_id(&self) -> Result<[u8; 32]> {
        self.peer_certificate()?;
        Ok(*blake3::hash(&self.binding.context).as_bytes())
    }
    /// While awaiting a local decision, detect closure or unexpected application
    /// data. There must be exactly one stream reader; a future commit protocol
    /// must replace this idle read, never run beside it. Every outcome invalidates
    /// this temporary proposal; observing readiness is not persistent trust.
    pub async fn wait_peer_end(&mut self) -> Result<PeerEnd> {
        use tokio::io::AsyncReadExt;
        let lifetime = self.lifetime.clone();
        let result = lifetime
            .run(async {
                let mut byte = [0; 1];
                match self.stream.read(&mut byte).await {
                    Ok(0) => Ok(PeerEnd::Disconnected),
                    Ok(_) => Ok(PeerEnd::UnexpectedData),
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::UnexpectedEof
                                | std::io::ErrorKind::ConnectionReset
                                | std::io::ErrorKind::ConnectionAborted
                                | std::io::ErrorKind::BrokenPipe
                        ) =>
                    {
                        Ok(PeerEnd::Disconnected)
                    }
                    Err(error) => Err(error.into()),
                }
            })
            .await;
        self.cancellation.cancel();
        result
    }
}

fn server_config(identity: &Identity, peer: &[u8]) -> Result<rustls::ServerConfig> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier = rustls::server::WebPkiClientVerifier::builder_with_provider(
        Arc::new(crate::roots(peer)?),
        provider.clone(),
    )
    .build()?;
    let mut config = rustls::ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_client_cert_verifier(verifier)
        .with_single_cert(vec![identity.cert()], identity.key())?;
    config.alpn_protocols = vec![ALPN.to_vec()];
    config.session_storage = Arc::new(rustls::server::NoServerSessionStorage {});
    config.send_tls13_tickets = 0;
    config.max_early_data_size = 0;
    Ok(config)
}
fn client_config(identity: &Identity, peer: &[u8]) -> Result<rustls::ClientConfig> {
    let mut config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])?
    .with_root_certificates(crate::roots(peer)?)
    .with_client_auth_cert(vec![identity.cert()], identity.key())?;
    config.alpn_protocols = vec![ALPN.to_vec()];
    config.resumption = rustls::client::Resumption::disabled();
    config.enable_early_data = false;
    Ok(config)
}
fn verify_tls(stream: &TlsStream<TcpStream>, peer: &[u8]) -> Result<()> {
    let (_, connection) = stream.get_ref();
    anyhow::ensure!(
        connection.protocol_version() == Some(rustls::ProtocolVersion::TLSv1_3)
            && connection.alpn_protocol() == Some(ALPN),
        "pairing TLS version/ALPN mismatch"
    );
    anyhow::ensure!(
        connection
            .peer_certificates()
            .and_then(|chain| chain.first())
            .map(|cert| cert.as_ref())
            == Some(peer),
        "pairing TLS certificate mismatch"
    );
    Ok(())
}

#[cfg(test)]
mod tests;
