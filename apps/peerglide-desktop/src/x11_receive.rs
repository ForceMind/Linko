//! Explicit single-peer v5 receive session on local Xorg.
use anyhow::{ensure, Context, Result};
use peerglide_core::Receiver;
use peerglide_platform::{now_ms, x11_activity::Activity, x11_inject::Injector, ReceivePoint};
use peerglide_protocol::{decode, Input, InputCapabilities, Message, Motion, LEASE_MS};
use peerglide_transport as transport;
use std::{
    future::{pending, Future},
    net::SocketAddr,
    path::Path,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    thread::{self, Thread},
    time::Duration,
};
use tokio::sync::{mpsc, oneshot};

const NATIVE_POLL: Duration = Duration::from_millis(5);
const ADMISSION: Duration = Duration::from_millis(250);
const CLEANUP: Duration = Duration::from_secs(1);

struct Mailbox {
    stop: AtomicBool,
    deadline: AtomicU64,
    latest: Mutex<Option<ReceivePoint>>,
    input: Mutex<Option<PendingInput>>,
}

impl Mailbox {
    fn live(&self, now: u64) -> bool {
        !self.stop.load(Ordering::Acquire) && now < self.deadline.load(Ordering::Acquire)
    }
}

struct PendingInput {
    point: ReceivePoint,
    effect: Input,
    expires: u64,
    completed: oneshot::Sender<()>,
}

struct Worker {
    mailbox: Arc<Mailbox>,
    thread: Thread,
    finished: Option<oneshot::Receiver<Result<()>>>,
}

impl Worker {
    fn start(point: ReceivePoint, now: u64) -> Result<(Self, oneshot::Receiver<()>)> {
        let mailbox = Arc::new(Mailbox {
            stop: AtomicBool::new(false),
            deadline: AtomicU64::new(now.saturating_add(LEASE_MS)),
            latest: Mutex::new(Some(point)),
            input: Mutex::new(None),
        });
        let request = mailbox.clone();
        let admit_before = now.saturating_add(ADMISSION.as_millis() as u64);
        let (ready, admitted) = oneshot::channel();
        let (done, finished) = oneshot::channel();
        let owner = thread::Builder::new()
            .name("linko-x11-input".into())
            .spawn(move || {
                let result = serve_native(&request, ready, admit_before);
                request.stop.store(true, Ordering::Release);
                let _ = done.send(result);
            })?;
        let worker = Self {
            mailbox,
            thread: owner.thread().clone(),
            finished: Some(finished),
        };
        Ok((worker, admitted))
    }

    fn replace_motion(&self, point: ReceivePoint) -> Result<()> {
        let mut latest = self
            .mailbox
            .latest
            .lock()
            .map_err(|_| anyhow::anyhow!("X11 position queue unavailable"))?;
        if self.mailbox.live(now_ms()) {
            *latest = Some(point);
            self.thread.unpark();
        }
        Ok(())
    }

    fn submit_input(&self, point: ReceivePoint, effect: Input) -> Result<oneshot::Receiver<()>> {
        let mut queued = self
            .mailbox
            .input
            .lock()
            .map_err(|_| anyhow::anyhow!("X11 input queue unavailable"))?;
        let now = now_ms();
        ensure!(
            self.mailbox.live(now) && queued.is_none(),
            "X11 input lease is no longer active"
        );
        // The reliable cursor anchors the effect. A previously queued motion
        // may not move the pointer after this anchor.
        self.mailbox
            .latest
            .lock()
            .map_err(|_| anyhow::anyhow!("X11 position queue unavailable"))?
            .take();
        let (completed, result) = oneshot::channel();
        *queued = Some(PendingInput {
            point,
            effect,
            expires: now.saturating_add(ADMISSION.as_millis() as u64),
            completed,
        });
        self.thread.unpark();
        Ok(result)
    }

    fn renew(&self, now: u64) {
        self.mailbox
            .deadline
            .store(now.saturating_add(LEASE_MS), Ordering::Release);
    }

    fn stop(&self) {
        self.mailbox.stop.store(true, Ordering::Release);
        self.thread.unpark();
    }

    async fn finish(&mut self) -> Result<()> {
        let Some(finished) = self.finished.as_mut() else {
            return Ok(());
        };
        let result = finished.await;
        self.finished = None;
        result.context("X11 input owner ended unexpectedly")?
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.stop();
    }
}

fn serve_native(mailbox: &Mailbox, ready: oneshot::Sender<()>, admit_before: u64) -> Result<()> {
    let activity = Activity::open_for_local_takeover()?;
    let mut injector = Injector::open_after_explicit_consent()?;
    let mut ready = Some(ready);
    let result = (|| -> Result<()> {
        while mailbox.live(now_ms()) {
            if activity.local_activity()? {
                return Ok(());
            }
            let input = mailbox
                .input
                .lock()
                .map_err(|_| anyhow::anyhow!("X11 input queue unavailable"))?
                .take();
            if let Some(input) = input {
                let admitted = || -> Result<bool> {
                    let local = activity.local_activity()?;
                    let now = now_ms();
                    Ok(!local
                        && mailbox.live(now)
                        && now < input.expires
                        && !input.completed.is_closed())
                };
                if !injector.pointer_if(input.point, admitted)? || !admitted()? {
                    return Ok(());
                }
                match input.effect {
                    Input::Key { usage, down } => {
                        ensure!(injector.key(usage, down)?, "X11 key ownership changed")
                    }
                    Input::Button { button, down } => ensure!(
                        injector.button(button, down)?,
                        "X11 button ownership changed"
                    ),
                    Input::Scroll { x, y } => {
                        if !injector.scroll(x, y, admitted)? {
                            return Ok(());
                        }
                    }
                }
                if input.completed.send(()).is_err() {
                    return Ok(());
                }
                continue;
            }
            let point = mailbox
                .latest
                .lock()
                .map_err(|_| anyhow::anyhow!("X11 position queue unavailable"))?
                .take();
            if let Some(point) = point {
                let placed = injector.pointer_if(point, || {
                    let local = activity.local_activity()?;
                    let now = now_ms();
                    Ok(!local
                        && mailbox.live(now)
                        && ready
                            .as_ref()
                            .is_none_or(|ready| now < admit_before && !ready.is_closed()))
                })?;
                if !placed {
                    return Ok(());
                }
                if let Some(ready) = ready.take() {
                    if ready.send(()).is_err() {
                        return Ok(());
                    }
                }
            }
            thread::park_timeout(NATIVE_POLL);
        }
        Ok(())
    })();
    injector
        .release_owned()
        .context("X11 owned inputs could not be released")?;
    result
}

async fn capabilities(allow_discrete: bool, allow_scroll: bool) -> Result<InputCapabilities> {
    if !allow_discrete && !allow_scroll {
        return InputCapabilities::new(&[], true, 0, false).map_err(anyhow::Error::msg);
    }
    let (ready, result) = oneshot::channel();
    thread::Builder::new()
        .name("linko-x11-keymap".into())
        .spawn(move || {
            let caps = Injector::open_after_explicit_consent().and_then(|injector| {
                ensure!(
                    !allow_scroll || injector.scroll_available()?,
                    "standard X11 wheel mapping unavailable"
                );
                let keys = if allow_discrete {
                    injector.key_usages().collect::<Vec<_>>()
                } else {
                    Vec::new()
                };
                InputCapabilities::new(
                    &keys,
                    true,
                    if allow_discrete { 7 } else { 0 },
                    allow_scroll,
                )
                .map_err(anyhow::Error::msg)
            });
            let _ = ready.send(caps);
        })?;
    tokio::time::timeout(ADMISSION, result)
        .await
        .context("X11 capability query timed out")??
}

pub(super) async fn run(
    identity: &Path,
    certificate: &Path,
    bind: SocketAddr,
    allow: bool,
    allow_discrete: bool,
    allow_scroll: bool,
) -> Result<()> {
    ensure!(allow, "X11 pointer control requires explicit local consent");
    let identity = transport::Identity::load(identity)?;
    let peer = transport::read_certificate(certificate)?;
    ensure!(
        peer != identity.certificate,
        "X11 pointer source must be another computer"
    );
    let endpoint = transport::endpoint(bind, &identity, &peer)?;
    let stop = tokio::signal::ctrl_c();
    tokio::pin!(stop);
    println!(
        "Waiting for one verified computer on {}. Ctrl+C stops input control.",
        endpoint.local_addr()?
    );
    let connection = tokio::select! {
        biased;
        result = &mut stop => return result.map_err(Into::into),
        result = async {
            let incoming = endpoint.accept().await.context("X11 listener closed")?;
            Ok::<_, anyhow::Error>(tokio::time::timeout(Duration::from_secs(5), incoming).await??)
        } => result?,
    };
    serve(&endpoint, &connection, &peer, &mut stop, allow_discrete, allow_scroll).await
}

async fn serve<F>(
    endpoint: &quinn::Endpoint,
    connection: &quinn::Connection,
    peer: &[u8],
    stop: F,
    allow_discrete: bool,
    allow_scroll: bool,
) -> Result<()>
where
    F: Future<Output = std::io::Result<()>>,
{
    let result = async {
        transport::verify_peer(connection, peer)?;
        drive(connection, stop, allow_discrete, allow_scroll).await
    }
    .await;
    // A successful drive returns only after the local input worker has retired.
    // Reuse the authenticated v5 close fence used by the Windows/macOS sender.
    let reason = if result.is_ok() {
        super::session::NATIVE_INPUT_RECOVERED
    } else {
        b"X11 input session ended"
    };
    endpoint.close(0u8.into(), reason);
    endpoint.wait_idle().await;
    result
}

async fn drive<F>(
    connection: &quinn::Connection,
    stop: F,
    allow_discrete: bool,
    allow_scroll: bool,
) -> Result<()>
where
    F: Future<Output = std::io::Result<()>>,
{
    tokio::pin!(stop);
    let (mut send, mut receive, peer, local) = tokio::select! {
        biased;
        result = &mut stop => return result.map_err(Into::into),
        result = async {
            let (mut send, mut receive) =
                tokio::time::timeout(Duration::from_secs(5), connection.accept_bi()).await??;
            let local = capabilities(allow_discrete, allow_scroll).await?;
            let peer = super::exchange_hello(&mut send, &mut receive, local.clone()).await?;
            Ok::<_, anyhow::Error>((send, receive, peer, local))
        } => result?,
    };
    let (send_messages, mut messages) = mpsc::channel(16);
    let reader = tokio::spawn(async move {
        loop {
            let result = transport::receive(&mut receive).await;
            let terminal = result.is_err();
            if send_messages.send(result).await.is_err() || terminal {
                break;
            }
        }
    });
    let mut receiver = Receiver::default();
    let mut worker: Option<Worker> = None;
    let result = async {
        loop {
            tokio::select! {
                biased;
                result = &mut stop => return result.map_err(Into::into),
                _ = connection.closed() => return Ok(()),
                finished = worker_end(&mut worker) => return finished,
                message = messages.recv() => {
                    let Some(message) = message else { return Ok(()); };
                    let now = now_ms();
                    match message? {
                        Message::Acquire { lease, edge, along } => {
                            if worker.is_some() || !(0.0..=1.0).contains(&along) || !receiver.acquire(lease, now, peer.motion()) {
                                reply(&mut send, &Message::Denied { lease }).await?;
                                continue;
                            }
                            let (x, y) = super::session::entry_position(edge.opposite(), along);
                            let (owner, admitted) = Worker::start(ReceivePoint { x, y }, now)?;
                            worker = Some(owner);
                            if !matches!(tokio::time::timeout(ADMISSION, admitted).await, Ok(Ok(()))) {
                                if let Some(owner) = &worker { owner.stop(); }
                                reply(&mut send, &Message::Denied { lease }).await?;
                                anyhow::bail!("X11 pointer admission could not be confirmed");
                            }
                            reply(&mut send, &Message::Granted { lease }).await?;
                        }
                        Message::Ping { lease: 0 } => reply(&mut send, &Message::Pong { lease: 0 }).await?,
                        Message::Ping { lease } if receiver.heartbeat(lease, now) => {
                            if let Some(owner) = &worker { owner.renew(now); }
                            reply(&mut send, &Message::Pong { lease }).await?;
                        }
                        Message::Release { lease, .. } if receiver.active() == Some(lease) => return Ok(()),
                        Message::Input { lease, event, cursor } if receiver.active() == Some(lease) => {
                            ensure!(local.supports(event), "X11 receive capability is not enabled");
                            if cursor.lease != lease || !receiver.accepts_input(lease, event, now)
                                || !receiver.anchor(cursor, now) {
                                continue;
                            }
                            let owner = worker.as_ref().context("X11 input owner unavailable")?;
                            let submitted = owner.submit_input(ReceivePoint { x: cursor.x, y: cursor.y }, event)?;
                            tokio::select! {
                                biased;
                                result = &mut stop => return result.map_err(Into::into),
                                result = tokio::time::timeout(ADMISSION, submitted) => {
                                    result.context("X11 input submission timed out")?
                                        .context("X11 input submission ended")?;
                                }
                            }
                            ensure!(receiver.input(lease, event, now_ms()), "X11 input lease ended during submission");
                        },
                        Message::Hello { .. } => anyhow::bail!("unexpected repeated input handshake"),
                        _ => (),
                    }
                }
                datagram = connection.read_datagram() => {
                    let bytes = datagram?;
                    if let (Some(owner), Ok(point)) = (&worker, decode::<Motion>(&bytes)) {
                        if receiver.motion(point, now_ms()) {
                            owner.replace_motion(ReceivePoint { x: point.x, y: point.y })?;
                        }
                    }
                }
            }
        }
    }.await;
    reader.abort();
    if let Some(owner) = &mut worker {
        owner.stop();
        let cleanup = tokio::time::timeout(CLEANUP, owner.finish())
            .await
            .context("X11 input cleanup could not be confirmed")?;
        if let Some(lease) = receiver.active() {
            let _ = reply(
                &mut send,
                &Message::Release {
                    lease,
                    cursor: None,
                },
            )
            .await;
        }
        cleanup?;
    }
    result
}

async fn worker_end(worker: &mut Option<Worker>) -> Result<()> {
    match worker {
        Some(owner) => owner.finish().await,
        None => pending().await,
    }
}

async fn reply(send: &mut quinn::SendStream, message: &Message) -> Result<()> {
    tokio::time::timeout(ADMISSION, transport::send(send, message)).await??;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use peerglide_protocol::Edge;

    #[test]
    fn receiver_requires_local_opt_in_and_limits_native_admission_to_the_lease() {
        assert!(super::super::Cli::try_parse_from([
            "peerglide",
            "listen-x11",
            "--peer-cert",
            "peer.der"
        ])
        .is_err());
        assert!(super::super::Cli::try_parse_from([
            "peerglide",
            "listen-x11",
            "--peer-cert",
            "peer.der",
            "--allow-discrete-input",
        ])
        .is_err());
        let mailbox = Mailbox {
            stop: AtomicBool::new(false),
            deadline: AtomicU64::new(20),
            latest: Mutex::new(None),
            input: Mutex::new(None),
        };
        assert!(mailbox.live(19));
        assert!(!mailbox.live(20));
        mailbox.stop.store(true, Ordering::Release);
        assert!(!mailbox.live(19));
    }

    #[tokio::test]
    #[ignore = "requires the dedicated GitHub Actions Xvfb server"]
    async fn authenticated_release_confirms_x11_recovery_only_after_cleanup() -> Result<()> {
        ensure!(
            std::env::var("GITHUB_ACTIONS").ok().as_deref() == Some("true")
                && std::env::var("LINKO_TEST_ISOLATED_X11").ok().as_deref() == Some("1"),
            "this test requires the dedicated CI display"
        );
        let source = transport::Identity::generate()?;
        let destination = transport::Identity::generate()?;
        let address = "127.0.0.1:0".parse()?;
        let sender = transport::endpoint(address, &source, &destination.certificate)?;
        for fail in [false, true] {
            let receiver = transport::endpoint(address, &destination, &source.certificate)?;
            let server = async {
                let connection = receiver.accept().await.context("listener closed")?.await?;
                let result = serve(
                    &receiver,
                    &connection,
                    &source.certificate,
                    pending(),
                    true,
                    false,
                )
                .await;
                assert_eq!(result.is_err(), fail);
                Ok::<_, anyhow::Error>(())
            };
            let client = async {
                let connection = sender
                    .connect(receiver.local_addr()?, "peerglide.local")?
                    .await?;
                transport::verify_peer(&connection, &destination.certificate)?;
                let (mut send, mut recv) = connection.open_bi().await?;
                let capabilities =
                    InputCapabilities::new(&[], true, 1, false).map_err(anyhow::Error::msg)?;
                let peer =
                    super::super::exchange_hello(&mut send, &mut recv, capabilities).await?;
                let event = Input::Button {
                    button: 1,
                    down: true,
                };
                assert!(peer.motion() && peer.supports(event));
                let lease = 17;
                reply(
                    &mut send,
                    &Message::Acquire {
                        lease,
                        edge: Edge::Right,
                        along: 0.5,
                    },
                )
                .await?;
                assert!(matches!(
                    transport::receive(&mut recv).await?,
                    Message::Granted { lease: 17 }
                ));
                reply(
                    &mut send,
                    &Message::Input {
                        lease,
                        event,
                        cursor: Motion {
                            lease,
                            sequence: 1,
                            x: 0.3,
                            y: 0.5,
                        },
                    },
                )
                .await?;
                reply(&mut send, &Message::Ping { lease }).await?;
                assert!(matches!(
                    transport::receive(&mut recv).await?,
                    Message::Pong { lease: 17 }
                ));
                let terminal = if fail {
                    Message::Hello {
                        version: peerglide_protocol::VERSION,
                        capabilities: InputCapabilities::default(),
                    }
                } else {
                    Message::Release {
                        lease,
                        cursor: None,
                    }
                };
                reply(&mut send, &terminal).await?;
                let quinn::ConnectionError::ApplicationClosed(close) = connection.closed().await
                else {
                    anyhow::bail!("expected the authenticated close fence");
                };
                assert_eq!(close.error_code, 0u8.into());
                let expected = if fail {
                    b"X11 input session ended".as_slice()
                } else {
                    super::super::session::NATIVE_INPUT_RECOVERED
                };
                assert_eq!(close.reason.as_ref(), expected);
                Ok::<_, anyhow::Error>(())
            };
            tokio::time::timeout(Duration::from_secs(8), async {
                tokio::try_join!(server, client)
            })
            .await??;
        }
        Ok(())
    }
}
