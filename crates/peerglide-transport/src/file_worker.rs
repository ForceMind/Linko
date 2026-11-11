//! One local file job with a bounded event queue and a dedicated runtime.
pub use crate::file_transfer::{Decision, Offer, PreparedDropPath, ReceiveResult, ReceivedFile};
use crate::{
    data,
    file_transfer::{self, Cancellation, ReceiveOptions, Source},
    Identity,
};
use anyhow::{ensure, Context, Result};
pub use peerglide_transfer::{codec::FileOutcome, History, HistoryState, MAX_FILE_BYTES};
use std::{
    io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    path::PathBuf,
    sync::{mpsc, Arc, Mutex},
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
    Sent(FileOutcome),
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
                    tokio::select! {
                        biased;
                        _ = stopped.wait_for(|stopped| *stopped) => Ok(Event::Stopped),
                        result = run(request, identity, pinned_peer, decision, job_stop, &status) => result,
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
) -> Result<Event> {
    match request {
        Request::Send {
            target,
            source,
            resume_id,
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
            let outcome = file_transfer::send(&connection, &pinned_peer, source, stop, |p| {
                status.progress(Progress {
                    bytes: p.queued_bytes,
                    total: p.total_bytes,
                    verified_locally: false,
                });
            })
            .await?;
            Ok(Event::Sent(outcome))
        }
        Request::Receive { bind, budget } => {
            let endpoint = data::endpoint(bind, &identity, &pinned_peer)?;
            status.post(Event::Listening(endpoint.local_addr()?))?;
            let connection = tokio::time::timeout(LISTEN_TIMEOUT, async {
                let incoming = endpoint.accept().await.context("file listener closed")?;
                Ok::<_, anyhow::Error>(incoming.await?)
            })
            .await??;
            let result = file_transfer::receive(
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
            )
            .await?;
            Ok(Event::Received(result))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
        loopback(false)
    }

    #[test]
    fn stopping_a_pending_offer_leaves_the_selected_directory_untouched() -> Result<()> {
        loopback(true)
    }

    fn loopback(stop: bool) -> Result<()> {
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
            Request::Receive {
                bind: "127.0.0.1:0".parse()?,
                budget: 1024,
            },
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
            Event::Sent(FileOutcome::Committed)
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
