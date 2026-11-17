//! One local file job with a bounded event queue and a dedicated runtime.
pub use crate::file_transfer::{Decision, Offer, PreparedDropPath, ReceiveResult, ReceivedFile};
use crate::{
    data,
    file_transfer::{self, Cancellation, ReceiveFinalization, ReceiveOptions, Source},
    Identity,
};
use anyhow::{ensure, Context, Result};
pub use peerglide_transfer::{codec::FileOutcome, History, HistoryState, MAX_FILE_BYTES};
use std::{
    io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::Duration,
};
use tokio::sync::{oneshot, watch};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
const LISTEN_TIMEOUT: Duration = Duration::from_secs(60);

pub enum Request {
    Send {
        target: SocketAddr,
        source: PathBuf,
        resume_id: Option<[u8; 16]>,
        prepare_handoff: bool,
    },
    Receive {
        bind: SocketAddr,
        budget: u64,
    },
}

pub enum Event {
    Prepared([u8; 16]),
    Listening(SocketAddr),
    Offer(Offer),
    Sent {
        outcome: FileOutcome,
        handoff: Option<(SocketAddr, crate::file_drop_link::SentReceipt)>,
    },
    Received(ReceiveResult),
    Stopped,
    Failed,
}

#[derive(Clone, Copy)]
pub struct Progress {
    pub bytes: u64,
    pub total: u64,
    pub verified_locally: bool,
}

pub struct Job {
    stop: watch::Sender<bool>,
    cancellation: Cancellation,
    decision: Option<oneshot::Sender<Decision>>,
    offer_pending: bool,
    events: mpsc::Receiver<Event>,
    progress: Arc<Mutex<Option<Progress>>>,
    owner: JoinHandle<()>,
}

impl Job {
    pub fn poll(&mut self) -> Option<Event> {
        let event = self.events.try_recv().ok()?;
        self.offer_pending = matches!(event, Event::Offer(_));
        Some(event)
    }

    pub fn progress(&self) -> Option<Progress> {
        self.progress.lock().ok().and_then(|progress| *progress)
    }

    pub fn decide(&mut self, decision: Decision) -> Result<()> {
        ensure!(
            self.offer_pending && !*self.stop.borrow(),
            "no file approval pending"
        );
        self.offer_pending = false;
        self.decision
            .take()
            .context("no file approval pending")?
            .send(decision)
            .map_err(|_| anyhow::anyhow!("file approval ended"))
    }

    pub fn stop(&self) {
        self.cancellation.stop();
        self.stop.send_replace(true);
    }

    pub fn is_finished(&self) -> bool {
        self.owner.is_finished()
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        self.stop();
    }
}

struct Status {
    events: mpsc::SyncSender<Event>,
    progress: Arc<Mutex<Option<Progress>>>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl Status {
    fn post(&self, event: Event) -> Result<()> {
        self.events
            .try_send(event)
            .map_err(|_| anyhow::anyhow!("file controller unavailable"))?;
        (self.wake)();
        Ok(())
    }

    fn progress(&self, progress: Progress) {
        if let Ok(mut snapshot) = self.progress.lock() {
            *snapshot = Some(progress);
        }
    }
}

pub fn start(
    request: Request,
    identity: Identity,
    pinned_peer: Vec<u8>,
    wake: Arc<dyn Fn() + Send + Sync>,
) -> Result<Job> {
    let (tx, events) = mpsc::sync_channel(4);
    let (approve, decision) = oneshot::channel();
    let receive = matches!(request, Request::Receive { .. });
    let (stop, mut stopped) = watch::channel(false);
    let cancellation = Cancellation::default();
    let progress = Arc::new(Mutex::new(None));
    let status = Status {
        events: tx,
        progress: progress.clone(),
        wake,
    };
    let job_stop = cancellation.clone();
    let owner = thread::Builder::new()
        .name("linko-file-data".into())
        .spawn(move || {
            let result = (|| -> Result<Event> {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .max_blocking_threads(1)
                    .enable_all()
                    .build()?;
                runtime.block_on(async {
                    let publishing = AtomicBool::new(false);
                    let finalization = ReceiveFinalization {
                        started: &publishing,
                        stopped: stopped.clone(),
                    };
                    let job = run(
                        request,
                        identity,
                        pinned_peer,
                        decision,
                        job_stop,
                        &status,
                        finalization,
                    );
                    tokio::pin!(job);
                    tokio::select! {
                        biased;
                        _ = stopped.wait_for(|stopped| *stopped) => {
                            if publishing.load(Ordering::Acquire) {
                                job.await
                            } else {
                                Ok(Event::Stopped)
                            }
                        }
                        result = &mut job => result,
                    }
                })
                // Drop the dedicated runtime before posting the terminal event.
                // At most one blocking disk operation can still be unwinding.
            })();
            let _ = status.post(result.unwrap_or(Event::Failed));
        })?;
    Ok(Job {
        stop,
        cancellation,
        decision: receive.then_some(approve),
        offer_pending: false,
        events,
        progress,
        owner,
    })
}

/// Scan one explicitly selected local receive directory off the desktop owner.
pub fn inspect_history(
    directory: PathBuf,
    wake: Arc<dyn Fn() + Send + Sync>,
) -> Result<mpsc::Receiver<io::Result<History>>> {
    let (send, receive) = mpsc::sync_channel(1);
    thread::Builder::new()
        .name("linko-file-history".into())
        .spawn(move || {
            let history = peerglide_transfer::receive_history(&directory);
            if send.try_send(history).is_ok() {
                wake();
            }
        })?;
    Ok(receive)
}

async fn run(
    request: Request,
    identity: Identity,
    pinned_peer: Vec<u8>,
    decision: oneshot::Receiver<Decision>,
    stop: Cancellation,
    status: &Status,
    finalization: ReceiveFinalization<'_>,
) -> Result<Event> {
    match request {
        Request::Send {
            target,
            source,
            resume_id,
            prepare_handoff,
        } => {
            let source = Source::open(source, resume_id, stop.clone()).await?;
            status.post(Event::Prepared(source.id()))?;
            let bind = match target.ip() {
                IpAddr::V4(_) => SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)),
                IpAddr::V6(_) => SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0)),
            };
            let endpoint = data::endpoint(bind, &identity, &pinned_peer)?;
            let connecting = endpoint.connect(target, "peerglide.local")?;
            let connection = tokio::time::timeout(CONNECT_TIMEOUT, connecting).await??;
            let progress = |p: file_transfer::SentProgress| {
                status.progress(Progress {
                    bytes: p.queued_bytes,
                    total: p.total_bytes,
                    verified_locally: false,
                });
            };
            let (outcome, receipt) = if prepare_handoff {
                file_transfer::send_for_drop(&connection, &pinned_peer, source, stop, progress)
                    .await?
            } else {
                (
                    file_transfer::send(&connection, &pinned_peer, source, stop, progress).await?,
                    None,
                )
            };
            Ok(Event::Sent {
                outcome,
                handoff: receipt.map(|receipt| (target, receipt)),
            })
        }
        Request::Receive { bind, budget } => {
            let endpoint = data::endpoint(bind, &identity, &pinned_peer)?;
            status.post(Event::Listening(endpoint.local_addr()?))?;
            let connection = tokio::time::timeout(LISTEN_TIMEOUT, async {
                let incoming = endpoint.accept().await.context("file listener closed")?;
                Ok::<_, anyhow::Error>(incoming.await?)
            })
            .await??;
            let result = file_transfer::receive_with_finalization(
                &connection,
                &pinned_peer,
                ReceiveOptions { budget, stop },
                |offer| async {
                    status.post(Event::Offer(offer))?;
                    decision.await.context("file approval ended")
                },
                |p| {
                    status.progress(Progress {
                        bytes: p.verified_bytes,
                        total: p.total_bytes,
                        verified_locally: true,
                    });
                },
                Some(finalization),
            )
            .await?;
            Ok(Event::Received(result))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peerglide_transfer::{
        codec::{Frame, Kind},
        data_policy::Permissions,
        Manifest, CHUNK_BYTES,
    };
    use std::{fs, time::Instant};

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Result<Self> {
            let root = std::env::temp_dir().join(format!(
                "linko-file-job-{}-{}",
                std::process::id(),
                rand::random::<u64>()
            ));
            fs::create_dir(&root)?;
            Ok(Self(root))
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn next(job: &mut Job) -> Result<Event> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(event) = job.poll() {
                return Ok(event);
            }
            ensure!(Instant::now() < deadline, "file worker event timed out");
            thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn history_query_wakes_without_network_or_directory_changes() -> Result<()> {
        let fixture = Fixture::new()?;
        let (send, wake) = mpsc::sync_channel(1);
        let query = inspect_history(
            fixture.0.clone(),
            Arc::new(move || {
                let _ = send.try_send(());
            }),
        )?;
        wake.recv_timeout(Duration::from_secs(5))?;
        let history = query.recv_timeout(Duration::from_secs(5))??;
        assert!(history.entries.is_empty());
        assert!(fs::read_dir(&fixture.0)?.next().is_none());
        Ok(())
    }

    #[test]
    fn independent_jobs_publish_local_progress_and_receipts() -> Result<()> {
        loopback(false, "127.0.0.1:0".parse()?)
    }

    #[test]
    fn file_workers_publish_verified_contents_over_ipv6() -> Result<()> {
        loopback(false, "[::1]:0".parse()?)
    }

    #[test]
    fn stopping_a_pending_offer_leaves_the_selected_directory_untouched() -> Result<()> {
        loopback(true, "127.0.0.1:0".parse()?)
    }

    struct FilePeer {
        _endpoint: quinn::Endpoint,
        connection: quinn::Connection,
        consent: data::Consent,
        send: data::Sender,
        receive: data::Receiver,
    }

    impl FilePeer {
        async fn offer(
            job: &mut Job,
            identity: &Identity,
            peer_pin: &[u8],
            id: [u8; 16],
            manifest: Manifest,
        ) -> Result<Self> {
            let Event::Listening(address) = next_async(job).await? else {
                anyhow::bail!("file listener did not start");
            };
            let endpoint = data::endpoint("127.0.0.1:0".parse()?, identity, peer_pin)?;
            let connection = endpoint.connect(address, "peerglide.local")?.await?;
            let stream = data::open(
                &connection,
                peer_pin,
                data::Capabilities {
                    files: true,
                    ..data::Capabilities::default()
                },
            )
            .await?;
            let (mut consent, mut send, receive) =
                stream.into_session((2 * CHUNK_BYTES + 4096) as u64, 4096)?;
            consent.grant(
                Permissions {
                    file_send: true,
                    ..Permissions::default()
                },
                10_000,
            )?;
            send.write(&Frame::Offer { id, manifest }).await?;
            let Event::Offer(offer) = next_async(job).await? else {
                anyhow::bail!("file approval did not arrive");
            };
            assert_eq!(offer.id, id);
            Ok(Self {
                _endpoint: endpoint,
                connection,
                consent,
                send,
                receive,
            })
        }

        async fn reply(&mut self, kind: Kind) -> Result<Frame> {
            let admitted = self
                .receive
                .read_file(&[kind], Duration::from_secs(5))
                .await?;
            self.consent.apply_received(admitted, Ok)
        }

        async fn chunk(&mut self, id: [u8; 16], index: u32, bytes: &[u8]) -> Result<()> {
            self.send
                .write(&Frame::Chunk {
                    id,
                    index,
                    bytes: bytes.to_vec(),
                })
                .await
        }
    }

    #[tokio::test]
    async fn stopping_after_publication_preserves_the_verified_local_result() -> Result<()> {
        let fixture = Fixture::new()?;
        let source = fixture.0.join("shared.bin");
        let destination = fixture.0.join("received");
        let bytes = b"stopped after file publication";
        fs::write(&source, bytes)?;
        fs::create_dir(&destination)?;
        let manifest = Manifest::from_file(&source)?;
        let sender = Identity::generate()?;
        let receiver = Identity::generate()?;
        let peer_pin = receiver.certificate.clone();
        let bind = "127.0.0.1:0".parse()?;
        let mut job = start(
            Request::Receive { bind, budget: 1024 },
            receiver,
            sender.certificate.clone(),
            Arc::new(|| {}),
        )?;
        let id = [7; 16];
        let mut peer = FilePeer::offer(&mut job, &sender, &peer_pin, id, manifest).await?;
        job.decide(Decision::Accept {
            directory: destination.clone(),
            resume: false,
        })?;
        assert!(
            matches!(peer.reply(Kind::FileAccepted).await?, Frame::FileAccepted { id: accepted_id, next_chunk: 0 } if accepted_id == id)
        );
        peer.chunk(id, 0, bytes).await?;
        assert!(
            matches!(peer.reply(Kind::FileResult).await?, Frame::FileResult { id: finished, outcome: FileOutcome::Committed } if finished == id)
        );

        // Keep the peer alive so only local stop can end the receipt wait.
        job.stop();
        let Event::Received(result) =
            tokio::time::timeout(Duration::from_secs(2), next_async(&mut job)).await??
        else {
            anyhow::bail!("worker lost the file publication result");
        };
        assert_eq!(result.outcome, FileOutcome::Committed);
        assert!(result.report_queued && result.file.is_some());
        assert_eq!(fs::read(destination.join("shared.bin"))?, bytes);
        peer.connection
            .close(0u8.into(), b"local publication observed");
        Ok(())
    }

    #[tokio::test]
    async fn a_stopped_or_disconnected_transfer_resumes_its_verified_wire_prefix() -> Result<()> {
        for receive_stopped in [false, true] {
            let fixture = Fixture::new()?;
            let source = fixture.0.join("shared.bin");
            let destination = fixture.0.join("received");
            let receiver_dir = fixture.0.join("identity");
            let mut bytes = vec![0x81; CHUNK_BYTES];
            bytes.extend_from_slice(b"second chunk of the file");
            fs::write(&source, &bytes)?;
            fs::create_dir(&destination)?;
            let manifest = Manifest::from_file(&source)?;
            let sender = Identity::generate()?;
            let peer_pin = Identity::create(&receiver_dir)?.certificate;
            let id = [8; 16];
            let start_receiver = || {
                start(
                    Request::Receive {
                        bind: "127.0.0.1:0".parse()?,
                        budget: bytes.len() as u64,
                    },
                    Identity::load(&receiver_dir)?,
                    sender.certificate.clone(),
                    Arc::new(|| {}),
                )
            };

            let mut first = start_receiver()?;
            let mut peer =
                FilePeer::offer(&mut first, &sender, &peer_pin, id, manifest.clone()).await?;
            first.decide(Decision::Accept {
                directory: destination.clone(),
                resume: false,
            })?;
            assert!(
                matches!(peer.reply(Kind::FileAccepted).await?, Frame::FileAccepted { id: accepted_id, next_chunk: 0 } if accepted_id == id)
            );
            peer.chunk(id, 0, &bytes[..CHUNK_BYTES]).await?;
            tokio::time::timeout(Duration::from_secs(5), async {
                while !first.progress().is_some_and(|progress| {
                    progress.verified_locally && progress.bytes == CHUNK_BYTES as u64
                }) {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await?;
            if receive_stopped {
                first.stop();
            } else {
                peer.connection
                    .close(0u8.into(), b"initial file transfer interrupted");
            }
            assert!(matches!(
                (receive_stopped, next_async(&mut first).await?),
                (true, Event::Stopped) | (false, Event::Failed)
            ));
            drop(peer);
            assert!(!destination.join("shared.bin").try_exists()?);

            let mut resumed = start_receiver()?;
            let mut peer = FilePeer::offer(&mut resumed, &sender, &peer_pin, id, manifest).await?;
            resumed.decide(Decision::Accept {
                directory: destination.clone(),
                resume: true,
            })?;
            assert!(
                matches!(peer.reply(Kind::FileAccepted).await?, Frame::FileAccepted { id: accepted_id, next_chunk: 1 } if accepted_id == id)
            );
            peer.chunk(id, 1, &bytes[CHUNK_BYTES..]).await?;
            assert!(
                matches!(peer.reply(Kind::FileResult).await?, Frame::FileResult { id: finished, outcome: FileOutcome::Committed } if finished == id)
            );
            peer.connection.close(0u8.into(), b"resumed file observed");
            assert!(matches!(
                next_async(&mut resumed).await?,
                Event::Received(ReceiveResult {
                    outcome: FileOutcome::Committed,
                    report_queued: true,
                    file: Some(_),
                })
            ));
            assert_eq!(fs::read(destination.join("shared.bin"))?, bytes);
        }
        Ok(())
    }

    async fn next_async(job: &mut Job) -> Result<Event> {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(event) = job.poll() {
                    return event;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .context("file worker did not report its next state")
    }

    fn loopback(stop: bool, bind: SocketAddr) -> Result<()> {
        let fixture = Fixture::new()?;
        let source = fixture.0.join("shared.bin");
        let destination = fixture.0.join("received");
        let content = b"private file test";
        fs::write(&source, content)?;
        fs::create_dir(&destination)?;
        let left = Identity::generate()?;
        let right = Identity::generate()?;
        let left_pin = left.certificate.clone();
        let right_pin = right.certificate.clone();
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {});
        let mut receiver = start(
            Request::Receive { bind, budget: 1024 },
            right,
            left_pin,
            wake.clone(),
        )?;
        let Event::Listening(target) = next(&mut receiver)? else {
            anyhow::bail!("receiver did not listen");
        };
        let mut sender = start(
            Request::Send {
                target,
                source,
                resume_id: None,
                prepare_handoff: true,
            },
            left,
            right_pin,
            wake,
        )?;
        let Event::Prepared(id) = next(&mut sender)? else {
            anyhow::bail!("sender did not prepare");
        };
        let Event::Offer(offer) = next(&mut receiver)? else {
            anyhow::bail!("receiver did not get an offer");
        };
        assert_eq!(id, offer.id);
        assert_eq!(offer.size, content.len() as u64);
        assert!(fs::read_dir(&destination)?.next().is_none());

        if stop {
            receiver.stop();
            assert!(receiver
                .decide(Decision::Accept {
                    directory: destination.clone(),
                    resume: false,
                })
                .is_err());
            assert!(matches!(next(&mut receiver)?, Event::Stopped));
            assert!(matches!(next(&mut sender)?, Event::Failed));
            assert!(fs::read_dir(destination)?.next().is_none());
            return Ok(());
        }
        receiver.decide(Decision::Accept {
            directory: destination.clone(),
            resume: false,
        })?;
        assert!(matches!(
            next(&mut sender)?,
            Event::Sent {
                outcome: FileOutcome::Committed,
                handoff: Some(_),
            }
        ));
        assert!(matches!(
            next(&mut receiver)?,
            Event::Received(ReceiveResult {
                outcome: FileOutcome::Committed,
                report_queued: true,
                ..
            })
        ));
        let progress = receiver.progress().context("missing verified progress")?;
        assert!(progress.verified_locally);
        assert_eq!(progress.bytes, progress.total);
        assert_eq!(fs::read(destination.join("shared.bin"))?, content);
        Ok(())
    }
}
