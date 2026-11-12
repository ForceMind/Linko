//! Explicit single-peer, pointer-only v5 receive session on local Xorg.
use anyhow::{ensure, Context, Result};
use peerglide_core::Receiver;
use peerglide_platform::{now_ms, x11_activity::Activity, x11_inject::Injector, ReceivePoint};
use peerglide_protocol::{decode, InputCapabilities, Message, Motion, LEASE_MS};
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

struct Mailbox {
    stop: AtomicBool,
    deadline: AtomicU64,
    latest: Mutex<Option<ReceivePoint>>,
}

impl Mailbox {
    fn live(&self, now: u64) -> bool {
        !self.stop.load(Ordering::Acquire) && now < self.deadline.load(Ordering::Acquire)
    }
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
        });
        let request = mailbox.clone();
        let admit_before = now.saturating_add(ADMISSION.as_millis() as u64);
        let (ready, admitted) = oneshot::channel();
        let (done, finished) = oneshot::channel();
        let owner = thread::Builder::new()
            .name("linko-x11-pointer".into())
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
        result.context("X11 pointer owner ended unexpectedly")?
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.stop();
    }
}

fn serve_native(mailbox: &Mailbox, ready: oneshot::Sender<()>, admit_before: u64) -> Result<()> {
    let activity = Activity::open_for_local_takeover()?;
    let injector = Injector::open_after_explicit_consent()?;
    let mut ready = Some(ready);
    while mailbox.live(now_ms()) {
        if activity.local_activity()? {
            break;
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
                Ok(!local && mailbox.live(now) && (ready.is_none() || now < admit_before))
            })?;
            if !placed {
                break;
            }
            if let Some(ready) = ready.take() {
                if ready.send(()).is_err() {
                    break;
                }
            }
        }
        thread::park_timeout(NATIVE_POLL);
    }
    Ok(())
}

pub(super) async fn run(
    identity: &Path,
    certificate: &Path,
    bind: SocketAddr,
    allow: bool,
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
        "Waiting for one verified computer on {}. Ctrl+C stops pointer control.",
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
    transport::verify_peer(&connection, &peer)?;
    let result = drive(&connection, &mut stop).await;
    endpoint.close(0u8.into(), b"X11 pointer session ended");
    result
}

async fn drive<F>(connection: &quinn::Connection, stop: F) -> Result<()>
where
    F: Future<Output = std::io::Result<()>>,
{
    tokio::pin!(stop);
    let (mut send, mut receive, peer) = tokio::select! {
        biased;
        result = &mut stop => return result.map_err(Into::into),
        result = async {
            let (mut send, mut receive) =
                tokio::time::timeout(Duration::from_secs(5), connection.accept_bi()).await??;
            let local = InputCapabilities::new(&[], true, 0, false)
                .map_err(anyhow::Error::msg)?;
            let peer = super::exchange_hello(&mut send, &mut receive, local).await?;
            Ok::<_, anyhow::Error>((send, receive, peer))
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
                        Message::Input { lease, .. } if receiver.active() == Some(lease) => anyhow::bail!("X11 pointer receiver does not admit discrete input"),
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
        let cleanup = tokio::time::timeout(ADMISSION, owner.finish())
            .await
            .context("X11 pointer stop could not be confirmed")?;
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

    #[test]
    fn receiver_requires_local_opt_in_and_limits_native_admission_to_the_lease() {
        assert!(super::super::Cli::try_parse_from([
            "peerglide",
            "listen-x11",
            "--peer-cert",
            "peer.der"
        ])
        .is_err());
        let mailbox = Mailbox {
            stop: AtomicBool::new(false),
            deadline: AtomicU64::new(20),
            latest: Mutex::new(None),
        };
        assert!(mailbox.live(19));
        assert!(!mailbox.live(20));
        mailbox.stop.store(true, Ordering::Release);
        assert!(!mailbox.live(19));
    }
}
