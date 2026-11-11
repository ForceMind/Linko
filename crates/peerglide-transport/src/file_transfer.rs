//! File offer and disk orchestration on the authenticated data connection.
use crate::data;
use anyhow::{ensure, Context, Result};
use peerglide_transfer::{
    codec::{FileOutcome, Frame, Kind, HEADER_BYTES},
    data_policy::{DataSession, Permissions},
    manager::Manager,
    Manifest, PeerBinding, Progress, CHUNK_BYTES, MAX_FILE_BYTES, MAX_MANIFEST_BYTES,
};
use quinn::Connection;
use rand::RngCore;
use std::{
    fs::File,
    future::Future,
    io,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

const FILES: data::Capabilities = data::Capabilities {
    text: false,
    files: true,
    images: false,
};
const DATA_BYTES: u64 = MAX_FILE_BYTES
    + MAX_MANIFEST_BYTES as u64
    + (MAX_FILE_BYTES.div_ceil(CHUNK_BYTES as u64) + 2) * (HEADER_BYTES as u64 + 20);
const REPLY_BYTES: u64 = 256;
const FRAME_TIMEOUT: Duration = Duration::from_secs(5);
const DECISION_TIMEOUT: Duration = Duration::from_secs(60);
const ACCEPT_TIMEOUT: Duration = Duration::from_secs(180);
const GRANT: Duration = Duration::from_secs(20 * 60);
// 256 KiB every 31.25 ms bounds one file direction to 8 MiB/s. No
// unused credit accumulates while waiting on disk, consent or the network.
const CHUNK_SPACING: Duration = Duration::from_micros(31_250);

struct Pacer {
    next: tokio::time::Instant,
}

impl Pacer {
    fn new() -> Self {
        Self {
            next: tokio::time::Instant::now(),
        }
    }

    async fn wait(&mut self) {
        tokio::time::sleep_until(self.next).await;
        self.next = tokio::time::Instant::now() + CHUNK_SPACING;
    }
}

#[derive(Clone, Default)]
pub struct Cancellation(Arc<AtomicBool>);

impl Cancellation {
    pub fn stop(&self) {
        self.0.store(true, Ordering::Release);
    }

    fn check(&self) -> io::Result<()> {
        if self.0.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "file task stopped",
            ));
        }
        Ok(())
    }
}

#[derive(Clone)]
struct Guard {
    connection: Connection,
    stop: Cancellation,
    started: Instant,
}

impl Guard {
    fn check(&self) -> io::Result<()> {
        self.stop.check()?;
        if self.connection.close_reason().is_some() || self.started.elapsed() >= GRANT {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "file session ended",
            ));
        }
        Ok(())
    }

    fn elapsed_ms(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

struct CloseOnExit<'a>(&'a Connection);

impl Drop for CloseOnExit<'_> {
    fn drop(&mut self) {
        self.0.close(0u8.into(), b"file transfer ended");
    }
}

/// Metadata presented only to the local user; file names stay out of logs.
pub struct Offer {
    pub id: [u8; 16],
    pub name: String,
    pub size: u64,
}

pub enum Decision {
    Decline,
    Accept { directory: PathBuf, resume: bool },
}

/// The local disk outcome is independent of the peer receiving a receipt.
pub struct ReceiveResult {
    pub outcome: FileOutcome,
    pub report_queued: bool,
    pub file: Option<ReceivedFile>,
}

/// The published local file and the exact verified transfer manifest.
pub struct ReceivedFile {
    path: PathBuf,
    manifest: Manifest,
}

/// A file rechecked before the native adapter can offer its local path.
/// The cooperating owner keeps the same open file during the drag. A hostile
/// local filesystem or writers ignoring OS share rules are outside the transfer.
pub struct PreparedDropPath {
    path: PathBuf,
    _source: File,
}

impl ReceivedFile {
    pub fn name(&self) -> &str {
        self.manifest.name()
    }

    /// Run on the local storage worker, outside native input or drag callbacks.
    pub fn prepare_drop(&self, check: impl FnMut() -> io::Result<()>) -> Result<PreparedDropPath> {
        let (actual, source) = Manifest::open_checked(&self.path, check)?;
        ensure!(
            actual == self.manifest,
            "received file changed before native drop"
        );
        Ok(PreparedDropPath {
            path: self.path.clone(),
            _source: source,
        })
    }
}

impl PreparedDropPath {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// A locally selected file and its frozen manifest. The data job owns the
/// handle, so later path replacements cannot redirect reads.
pub struct Source {
    id: [u8; 16],
    manifest: Manifest,
    file: File,
}

impl Source {
    /// A resume ID is only a matching key. The peer pin and exact manifest
    /// are verified independently by the receiving storage owner.
    pub async fn open(
        path: PathBuf,
        resume_id: Option<[u8; 16]>,
        stop: Cancellation,
    ) -> Result<Self> {
        tokio::task::spawn_blocking(move || {
            let mut id = resume_id.unwrap_or_default();
            if resume_id.is_none() {
                rand::rngs::OsRng
                    .try_fill_bytes(&mut id)
                    .map_err(|_| anyhow::anyhow!("file transfer randomness unavailable"))?;
            }
            ensure!(id != [0; 16], "invalid file transfer ID");
            let (manifest, file) = Manifest::open_checked(&path, || stop.check())?;
            Ok(Self { id, manifest, file })
        })
        .await?
    }

    pub fn id(&self) -> [u8; 16] {
        self.id
    }
}

/// Locally queued file bytes, including any accepted resume prefix.
#[derive(Clone, Copy)]
pub struct SentProgress {
    pub queued_bytes: u64,
    pub total_bytes: u64,
}

/// Send one explicitly selected file and require the matching terminal receipt.
pub async fn send(
    connection: &Connection,
    pinned_peer: &[u8],
    source: Source,
    stop: Cancellation,
    mut progress: impl FnMut(SentProgress),
) -> Result<FileOutcome> {
    let guard = Guard {
        connection: connection.clone(),
        stop,
        started: Instant::now(),
    };
    let _close = CloseOnExit(connection);
    guard.check()?;
    let stream = data::open(connection, pinned_peer, FILES).await?;
    let (mut consent, mut send, mut receive) = stream.into_session(DATA_BYTES, REPLY_BYTES)?;
    consent.grant(
        Permissions {
            file_send: true,
            ..Permissions::default()
        },
        GRANT.as_millis() as u64,
    )?;
    let id = source.id;
    let total_bytes = source.manifest.size();
    let total_chunks = source.manifest.chunks();
    send.write(&Frame::Offer {
        id,
        manifest: source.manifest.clone(),
    })
    .await?;
    let admitted = receive
        .read_file(&[Kind::FileAccepted, Kind::FileResult], ACCEPT_TIMEOUT)
        .await?;
    let response = consent.apply_received(admitted, Ok)?;
    let next = match response {
        Frame::FileAccepted {
            id: received,
            next_chunk,
        } if received == id && next_chunk as usize <= total_chunks => next_chunk as usize,
        Frame::FileResult {
            id: received,
            outcome: outcome @ (FileOutcome::Declined | FileOutcome::Failed),
        } if received == id => return Ok(outcome),
        _ => anyhow::bail!("file approval differs from the offered transfer"),
    };
    let mut queued_bytes = (next as u64 * CHUNK_BYTES as u64).min(total_bytes);
    progress(SentProgress {
        queued_bytes,
        total_bytes,
    });
    let upload = async {
        let mut source = source;
        let mut pacing = Pacer::new();
        for index in next..total_chunks {
            let (opened, bytes) = tokio::task::spawn_blocking({
                let guard = guard.clone();
                move || -> Result<(Source, Vec<u8>)> {
                    guard.check()?;
                    let bytes = source.manifest.read_chunk(&mut source.file, index)?;
                    Ok((source, bytes))
                }
            })
            .await??;
            source = opened;
            let len = bytes.len() as u64;
            pacing.wait().await;
            send.write(&Frame::Chunk {
                id,
                index: u32::try_from(index)?,
                bytes,
            })
            .await?;
            queued_bytes += len;
            progress(SentProgress {
                queued_bytes,
                total_bytes,
            });
        }
        Ok::<_, anyhow::Error>(())
    };
    let terminal = async {
        let received = receive.read_file(&[Kind::FileResult], GRANT).await?;
        consent.apply_received(received, |frame| match frame {
            Frame::FileResult {
                id: received,
                outcome,
            } if received == id => Ok(outcome),
            _ => anyhow::bail!("file result differs from the offered transfer"),
        })
    };
    tokio::pin!(terminal);
    tokio::select! {
        biased;
        uploaded = upload => {
            uploaded?;
            terminal.await
        }
        outcome = &mut terminal => {
            let outcome = outcome?;
            ensure!(outcome == FileOutcome::Failed, "unexpected early file result");
            Ok(outcome)
        }
    }
}

/// A pre-existing local limit. The peer's offer cannot raise it.
pub struct ReceiveOptions {
    pub budget: u64,
    pub stop: Cancellation,
}

/// Receive one offer on a dedicated file connection. The controller must give
/// each job a fresh cancellation token and cancel/drop this future on local stop.
/// No receive directory is opened until the controller approves this offer.
pub async fn receive<F>(
    connection: &Connection,
    pinned_peer: &[u8],
    options: ReceiveOptions,
    decide: impl FnOnce(Offer) -> F,
    mut progress: impl FnMut(Progress),
) -> Result<ReceiveResult>
where
    F: Future<Output = Result<Decision>>,
{
    let guard = Guard {
        connection: connection.clone(),
        stop: options.stop,
        started: Instant::now(),
    };
    let _close = CloseOnExit(connection);
    guard.check()?;
    let stream = data::accept(connection, pinned_peer, FILES).await?;
    let (mut consent, mut send, mut receive) = stream.into_session(REPLY_BYTES, DATA_BYTES)?;
    let permissions = Permissions {
        file_receive: true,
        ..Permissions::default()
    };
    let grant_ms = GRANT.as_millis() as u64;
    consent.grant(permissions, grant_ms)?;
    let admitted = receive.read_file(&[Kind::Offer], FRAME_TIMEOUT).await?;
    let (id, manifest) = consent.apply_received(admitted, |frame| match frame {
        Frame::Offer { id, manifest } if id != [0; 16] => Ok((id, manifest)),
        _ => anyhow::bail!("invalid file offer"),
    })?;
    let offer = Offer {
        id,
        name: manifest.name().to_owned(),
        size: manifest.size(),
    };
    let decision = tokio::select! {
        biased;
        _ = connection.closed() => anyhow::bail!("file connection closed before approval"),
        decision = tokio::time::timeout(DECISION_TIMEOUT, decide(offer)) => {
            decision.context("file approval timed out")??
        }
    };
    let Decision::Accept { directory, resume } = decision else {
        return Ok(report(connection, &mut send, id, FileOutcome::Declined).await);
    };

    // Use the digest bound by the authenticated connection, independently of
    // the transfer ID and local directory supplied by the controller.
    let peer = consent.file_receive_peer()?;
    let total_chunks = manifest.chunks();
    let verified_manifest = manifest.clone();
    let storage = tokio::task::spawn_blocking({
        let guard = guard.clone();
        move || -> Result<(Manager, usize, Progress)> {
            guard.check()?;
            let mut nonce = [0; 32];
            rand::rngs::OsRng
                .try_fill_bytes(&mut nonce)
                .map_err(|_| anyhow::anyhow!("file session randomness unavailable"))?;
            let session = DataSession::new(peer, nonce, 0);
            let mut manager = Manager::new(&directory, session, options.budget)?;
            manager
                .consent(permissions, grant_ms, 0)
                .map_err(|_| anyhow::anyhow!("file storage grant rejected"))?;
            manager
                .accept_checked(
                    PeerBinding::new(peer, id),
                    manifest,
                    resume,
                    guard.elapsed_ms(),
                    || guard.check(),
                )
                .map_err(|_| anyhow::anyhow!("file storage admission failed"))?;
            let next = manager
                .next_chunk(id)
                .map_err(|_| anyhow::anyhow!("file storage unavailable"))?;
            let summary = manager.summary(id).context("file summary unavailable")?;
            let progress = Progress {
                verified_bytes: summary.verified_bytes,
                total_bytes: summary.total_bytes,
                chunks_complete: next == total_chunks,
            };
            Ok((manager, next, progress))
        }
    })
    .await?;
    let Ok((mut manager, next, initial)) = storage else {
        return Ok(report(connection, &mut send, id, FileOutcome::Failed).await);
    };
    progress(initial);
    send.write(&Frame::FileAccepted {
        id,
        next_chunk: u32::try_from(next)?,
    })
    .await?;

    let mut pacing = Pacer::new();
    for expected in next..total_chunks {
        pacing.wait().await;
        let admitted = receive
            .read_file(&[Kind::Chunk, Kind::Cancel], FRAME_TIMEOUT)
            .await?;
        let bytes = consent.apply_received(admitted, |frame| match frame {
            Frame::Cancel { id: received } if received == id => Ok(None),
            Frame::Chunk {
                id: received,
                index,
                bytes,
            } if received == id && index as usize == expected => Ok(Some(bytes)),
            _ => anyhow::bail!("file chunk differs from the accepted offer"),
        })?;
        let Some(bytes) = bytes else {
            let _ = manager.cancel(id);
            return Ok(report(connection, &mut send, id, FileOutcome::Failed).await);
        };
        let (storage, written) = tokio::task::spawn_blocking({
            let guard = guard.clone();
            move || -> Result<_> {
                guard.check()?;
                let written = manager.write_chunk(id, expected, &bytes, guard.elapsed_ms());
                Ok((manager, written))
            }
        })
        .await??;
        manager = storage;
        match written {
            Ok(verified) => progress(verified),
            Err(_) => return Ok(report(connection, &mut send, id, FileOutcome::Failed).await),
        }
    }
    let published = tokio::task::spawn_blocking({
        let guard = guard.clone();
        move || {
            manager
                .commit_checked(id, || guard.elapsed_ms(), || guard.check())
                .ok()
        }
    })
    .await?;
    let outcome = if published.is_some() {
        FileOutcome::Committed
    } else {
        FileOutcome::Unknown
    };
    let mut result = report(connection, &mut send, id, outcome).await;
    result.file = published.map(|path| ReceivedFile {
        path,
        manifest: verified_manifest,
    });
    Ok(result)
}

async fn report(
    connection: &Connection,
    send: &mut data::Sender,
    id: [u8; 16],
    outcome: FileOutcome,
) -> ReceiveResult {
    let report_queued = send.write(&Frame::FileResult { id, outcome }).await.is_ok();
    if report_queued {
        // Let the sender consume the result before closing the owned connection.
        let _ = tokio::time::timeout(FRAME_TIMEOUT, connection.closed()).await;
    }
    ReceiveResult {
        outcome,
        report_queued,
        file: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Identity;
    use std::{fs, net::SocketAddr};

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Result<Self> {
            let path = std::env::temp_dir().join(format!(
                "linko-file-{}-{}",
                std::process::id(),
                rand::random::<u64>()
            ));
            fs::create_dir(&path)?;
            Ok(Self(path))
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[tokio::test]
    async fn file_pacing_spaces_chunks_without_accumulating_credit() {
        let mut pacing = Pacer::new();
        pacing.wait().await;
        tokio::time::sleep(CHUNK_SPACING * 2).await;
        let after_pause = tokio::time::Instant::now();
        pacing.wait().await;
        let due = pacing.next;
        assert!(due >= after_pause + CHUNK_SPACING);
        pacing.wait().await;
        assert!(tokio::time::Instant::now() >= due);
    }

    #[tokio::test]
    async fn rejected_offer_leaves_receive_directory_untouched() -> Result<()> {
        loopback(false, false).await
    }

    #[tokio::test]
    async fn approved_offer_is_published_and_reported_after_verified_chunks() -> Result<()> {
        loopback(true, false).await
    }

    #[tokio::test]
    async fn a_new_connection_resumes_matching_verified_staging() -> Result<()> {
        loopback(true, true).await
    }

    async fn loopback(approve: bool, resume: bool) -> Result<()> {
        let dir = TestDirectory::new()?;
        let source_path = dir.0.join("example.bin");
        let destination = dir.0.join("received");
        let contents = vec![0x2a; CHUNK_BYTES + 19];
        fs::write(&source_path, &contents)?;
        fs::create_dir(&destination)?;
        let source = Identity::generate()?;
        let peer = Identity::generate()?;
        let id = [1; 16];
        let prepared = Source::open(source_path, Some(id), Cancellation::default()).await?;
        if resume {
            let digest = *blake3::hash(&source.certificate).as_bytes();
            let session = DataSession::new(digest, [2; 32], 0);
            let mut manager = Manager::new(&destination, session, 2 * CHUNK_BYTES as u64)?;
            manager
                .consent(
                    Permissions {
                        file_receive: true,
                        ..Permissions::default()
                    },
                    1000,
                    0,
                )
                .map_err(|error| anyhow::anyhow!("test grant: {error:?}"))?;
            manager
                .accept(
                    PeerBinding::new(digest, id),
                    prepared.manifest.clone(),
                    false,
                    0,
                )
                .map_err(|error| anyhow::anyhow!("test admission: {error:?}"))?;
            manager
                .write_chunk(id, 0, &contents[..CHUNK_BYTES], 1)
                .map_err(|error| anyhow::anyhow!("test write: {error:?}"))?;
        }
        let address: SocketAddr = "127.0.0.1:0".parse()?;
        let left = data::endpoint(address, &source, &peer.certificate)?;
        let right = data::endpoint(address, &peer, &source.certificate)?;
        let (client, server) = tokio::try_join!(
            async {
                Ok::<_, anyhow::Error>(
                    left.connect(right.local_addr()?, "peerglide.local")?
                        .await?,
                )
            },
            async {
                Ok::<_, anyhow::Error>(right.accept().await.context("missing connection")?.await?)
            }
        )?;
        let mut last_progress = None;
        let receiver = receive(
            &server,
            &source.certificate,
            ReceiveOptions {
                budget: 2 * CHUNK_BYTES as u64,
                stop: Cancellation::default(),
            },
            |offer| {
                let directory = destination.clone();
                let size = contents.len() as u64;
                async move {
                    assert_eq!(offer.name, "example.bin");
                    assert_eq!(offer.size, size);
                    if !resume {
                        assert!(fs::read_dir(&directory)?.next().is_none());
                    }
                    if approve {
                        Ok(Decision::Accept { directory, resume })
                    } else {
                        Ok(Decision::Decline)
                    }
                }
            },
            |progress| last_progress = Some(progress),
        );
        let mut first_progress = None;
        let sender = send(
            &client,
            &peer.certificate,
            prepared,
            Cancellation::default(),
            |progress| {
                first_progress.get_or_insert(progress.queued_bytes);
            },
        );
        let (received, sent) = tokio::try_join!(receiver, sender)?;
        assert_eq!(received.outcome, sent);
        assert!(received.report_queued);
        if approve {
            assert_eq!(sent, FileOutcome::Committed);
            assert_eq!(
                first_progress,
                Some(if resume { CHUNK_BYTES as u64 } else { 0 })
            );
            assert!(
                last_progress
                    .context("missing disk progress")?
                    .chunks_complete
            );
            let path = destination.join("example.bin");
            assert_eq!(fs::read(&path)?, contents);
            let file = received.file.context("missing verified local copy")?;
            assert_eq!(file.name(), "example.bin");
            let prepared = file.prepare_drop(|| Ok(()))?;
            assert_eq!(fs::canonicalize(prepared.path())?, fs::canonicalize(&path)?);
            drop(prepared);
            fs::write(&path, vec![0x7f; contents.len()])?;
            assert!(file.prepare_drop(|| Ok(())).is_err());
        } else {
            assert_eq!(sent, FileOutcome::Declined);
            assert!(received.file.is_none());
            assert!(last_progress.is_none());
            assert!(fs::read_dir(destination)?.next().is_none());
        }
        Ok(())
    }
}
