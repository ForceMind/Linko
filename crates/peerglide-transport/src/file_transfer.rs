//! File offer and disk orchestration on the authenticated data connection.
use crate::{
    data,
    file_drop_link::{Receipt, SentReceipt},
};
use anyhow::{ensure, Context, Result};
pub use peerglide_transfer::drop_admission;
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

#[cfg(any(target_os = "macos", test))]
mod promised_copy;
#[cfg(any(all(windows, target_env = "msvc"), test))]
mod verified_drop_reader;
#[cfg(any(all(windows, target_env = "msvc"), test))]
pub use verified_drop_reader::VerifiedDropReader;

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
    origin: DropOrigin,
    receipt: Option<Arc<Receipt>>,
}

/// Locally retained provenance of one committed transfer. Never put the
/// connection nonce in a network message, file, diagnostic, or user report.
#[derive(Clone, Copy)]
pub struct DropOrigin {
    transfer: [u8; 16],
    peer: [u8; 32],
    connection: [u8; 32],
}

impl DropOrigin {
    pub(crate) fn new(transfer: [u8; 16], peer: [u8; 32], connection: [u8; 32]) -> Self {
        Self {
            transfer,
            peer,
            connection,
        }
    }

    pub fn transfer(self) -> [u8; 16] {
        self.transfer
    }

    pub fn connection(self) -> [u8; 32] {
        self.connection
    }

    pub fn matches_peer(self, fingerprint: &str) -> bool {
        blake3::Hash::from_bytes(self.peer).to_hex().as_str() == fingerprint
    }
}

/// A file rechecked before the native adapter can offer its local path.
/// The cooperating owner keeps the same open file during the drag. A hostile
/// local filesystem or writers ignoring OS share rules are outside the transfer.
pub struct PreparedDropPath {
    path: PathBuf,
    _source: File,
    pub(crate) origin: DropOrigin,
    pub(crate) receipt: Option<Arc<Receipt>>,
    #[cfg(any(target_os = "macos", all(windows, target_env = "msvc"), test))]
    manifest: Manifest,
}

impl ReceivedFile {
    pub fn drop_origin(&self) -> DropOrigin {
        self.origin
    }

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
            origin: self.origin,
            receipt: self.receipt.clone(),
            #[cfg(any(target_os = "macos", all(windows, target_env = "msvc"), test))]
            manifest: actual,
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
/// Call this for a file locally selected for a later drag reservation.
pub async fn send_for_drop(
    connection: &Connection,
    pinned_peer: &[u8],
    source: Source,
    stop: Cancellation,
    progress: impl FnMut(SentProgress),
) -> Result<(FileOutcome, Option<SentReceipt>)> {
    let receipt = Receipt::from_data(connection, pinned_peer, source.id())?;
    let outcome = send(connection, pinned_peer, source, stop, progress).await?;
    Ok((
        outcome,
        (outcome == FileOutcome::Committed).then_some(SentReceipt(receipt)),
    ))
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
    let origin = consent.file_receive_origin(id)?;
    let peer = origin.peer;
    let receipt = Receipt::from_data(connection, pinned_peer, id).ok();
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
        origin,
        receipt: receipt.map(Arc::new),
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
    use crate::{file_drop_link, layout_reservation, Identity};
    use quinn::Endpoint;
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

    fn local_fixture_file(path: &Path) -> Result<ReceivedFile> {
        Ok(ReceivedFile {
            manifest: Manifest::from_file(path)?,
            path: path.to_path_buf(),
            origin: DropOrigin::new([1; 16], [2; 32], [3; 32]),
            receipt: None,
        })
    }

    #[test]
    fn promised_copy_checks_each_chunk_and_publishes_without_overwrite() -> Result<()> {
        let directory = TestDirectory::new()?;
        let original = directory.0.join("received.bin");
        let target = directory.0.join("target.bin");
        let content = vec![17; CHUNK_BYTES + 1];
        fs::write(&original, &content)?;
        let file = local_fixture_file(&original)?;

        let mut checks = 0;
        let collision = file
            .prepare_drop(|| Ok(()))?
            .copy_to_drop_destination(&target, || {
                checks += 1;
                if checks == 4 {
                    fs::write(&target, b"new local file")?;
                }
                Ok(())
            });
        assert!(collision.is_err());
        assert_eq!(fs::read(&target)?, b"new local file");
        assert_eq!(fs::read_dir(&directory.0)?.count(), 2);

        fs::remove_file(&target)?;
        file.prepare_drop(|| Ok(()))?
            .copy_to_drop_destination(&target, || Ok(()))?;
        assert_eq!(fs::read(&target)?, content);
        assert_eq!(fs::read(&original)?, content);
        assert_eq!(fs::read_dir(&directory.0)?.count(), 2);
        Ok(())
    }

    #[test]
    fn promised_copy_discards_cancelled_or_modified_staging() -> Result<()> {
        let directory = TestDirectory::new()?;
        let original = directory.0.join("received.bin");
        let target = directory.0.join("target.bin");
        let content = vec![17; CHUNK_BYTES + 1];
        fs::write(&original, &content)?;
        let file = local_fixture_file(&original)?;
        let mut checks = 0;
        let cancelled = file
            .prepare_drop(|| Ok(()))?
            .copy_to_drop_destination(&target, || {
                checks += 1;
                if checks == 3 {
                    return Err(io::ErrorKind::Interrupted.into());
                }
                Ok(())
            });
        assert!(cancelled.is_err());
        assert_eq!(fs::read_dir(&directory.0)?.count(), 1);
        assert_eq!(fs::read(&original)?, content);

        let prepared = file.prepare_drop(|| Ok(()))?;
        fs::write(&original, vec![21; CHUNK_BYTES + 1])?;
        assert!(prepared
            .copy_to_drop_destination(&target, || Ok(()))
            .is_err());
        assert_eq!(fs::read_dir(&directory.0)?.count(), 1);
        Ok(())
    }

    #[test]
    fn verified_drop_reader_checks_chunk_boundaries_and_rejects_changed_content() -> Result<()> {
        use std::io::Write;

        let directory = TestDirectory::new()?;
        let path = directory.0.join("received.bin");
        let mut content = vec![17; CHUNK_BYTES];
        content.extend_from_slice(&[19, 21, 23]);
        fs::write(&path, &content)?;
        let file = local_fixture_file(&path)?;
        let mut reader = file.prepare_drop(|| Ok(()))?.into_verified_reader();
        assert_eq!(reader.size(), content.len() as u64);
        let mut piece = [0; 4];
        reader.read_at(CHUNK_BYTES as u64 - 1, &mut piece)?;
        assert_eq!(piece, [17, 19, 21, 23]);
        assert!(reader.read_at(reader.size() - 1, &mut piece).is_err());
        assert!(reader.read_at(u64::MAX, &mut piece).is_err());
        fs::OpenOptions::new()
            .write(true)
            .open(path)?
            .write_all(&[25])?;
        assert!(reader.read_at(0, &mut piece).is_err());
        Ok(())
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
        let (client, server) = connect_pair(&left, &right).await?;
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
        let sender = send_for_drop(
            &client,
            &peer.certificate,
            prepared,
            Cancellation::default(),
            |progress| {
                first_progress.get_or_insert(progress.queued_bytes);
            },
        );
        let (received, (sent, receipt)) = tokio::try_join!(receiver, sender)?;
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
            let origin = file.drop_origin();
            assert_eq!(origin.transfer(), id);
            assert!(origin.matches_peer(&source.fingerprint()));
            assert!(!origin.matches_peer(&peer.fingerprint()));
            assert_ne!(origin.connection(), [0; 32]);
            exercise_drop_reservation(
                &source,
                &peer,
                &file,
                receipt.context("missing committed send receipt")?,
            )
            .await?;
            let prepared = file.prepare_drop(|| Ok(()))?;
            assert_eq!(fs::canonicalize(prepared.path())?, fs::canonicalize(&path)?);
            drop(prepared);
            fs::write(&path, vec![0x7f; contents.len()])?;
            assert!(file.prepare_drop(|| Ok(())).is_err());
        } else {
            assert_eq!(sent, FileOutcome::Declined);
            assert!(receipt.is_none());
            assert!(received.file.is_none());
            assert!(last_progress.is_none());
            assert!(fs::read_dir(destination)?.next().is_none());
        }
        Ok(())
    }

    async fn connect_pair(
        client: &Endpoint,
        server: &Endpoint,
    ) -> Result<(Connection, Connection)> {
        tokio::time::timeout(Duration::from_secs(5), async {
            tokio::try_join!(
                async {
                    let connecting = client.connect(server.local_addr()?, "peerglide.local")?;
                    Ok::<_, anyhow::Error>(connecting.await?)
                },
                async {
                    let incoming = server.accept().await.context("missing connection")?;
                    Ok::<_, anyhow::Error>(incoming.await?)
                }
            )
        })
        .await?
    }

    async fn exercise_drop_reservation(
        source: &Identity,
        target: &Identity,
        file: &ReceivedFile,
        receipt: SentReceipt,
    ) -> Result<()> {
        use drop_admission::{Admission, Content, Context as DropContext, Effect};
        let bind: SocketAddr = "127.0.0.1:0".parse()?;
        let a = crate::endpoint(bind, source, &target.certificate)?;
        let b = crate::endpoint(bind, target, &source.certificate)?;
        let input = connect_pair(&a, &b).await?;
        let replacement = connect_pair(&a, &b).await?;
        let sender = layout_reservation::endpoint(bind, source, &target.certificate)?;
        let receiver = layout_reservation::endpoint(bind, target, &source.certificate)?;

        let origin = file.drop_origin();
        let context = DropContext {
            connection: origin.connection(),
            layout_epoch: (1, 1),
            gesture_generation: 1,
            focused: true,
        };
        let mut admission = Admission::default();
        let ticket = admission
            .begin(context, origin.transfer(), Content::File, 0, true)
            .unwrap();
        admission.verified(ticket, context, 1, true).unwrap();
        let slot = file_drop_link::Slot::arm(
            file.prepare_drop(|| Ok(()))?,
            &mut admission,
            ticket,
            context,
            2,
            true,
        )?;

        let other_data_source = data::endpoint(bind, source, &target.certificate)?;
        let other_data_target = data::endpoint(bind, target, &source.certificate)?;
        let other_data = connect_pair(&other_data_source, &other_data_target).await?;
        let wrong = Receipt::from_data(&other_data.0, &target.certificate, origin.transfer())?;
        let sidecar = connect_pair(&sender, &receiver).await?;
        let lease = 7;
        let rejected = tokio::try_join!(
            file_drop_link::reserve(
                &sidecar.0,
                &input.0,
                &target.certificate,
                SentReceipt(wrong),
                lease
            ),
            file_drop_link::accept(&sidecar.1, &input.1, &source.certificate, &slot, || Some(
                context
            )),
        )?;
        assert_eq!(
            rejected,
            (false, false),
            "a matching transfer id alone is insufficient"
        );

        let sidecar = connect_pair(&sender, &receiver).await?;
        let rejected = tokio::try_join!(
            file_drop_link::reserve(
                &sidecar.0,
                &replacement.0,
                &target.certificate,
                SentReceipt(receipt.0.clone()),
                lease
            ),
            file_drop_link::accept(&sidecar.1, &input.1, &source.certificate, &slot, || Some(
                context
            )),
        )?;
        assert_eq!(
            rejected,
            (false, false),
            "file proof cannot move to another input connection"
        );

        let sidecar = connect_pair(&sender, &receiver).await?;
        let accepted = tokio::try_join!(
            file_drop_link::reserve(&sidecar.0, &input.0, &target.certificate, receipt, lease),
            file_drop_link::accept(&sidecar.1, &input.1, &source.certificate, &slot, || Some(
                context
            )),
        )?;
        assert_eq!(accepted, (true, true));
        let granted = confirm_input_lease(&input, lease).await?;
        assert!(slot
            .claim(
                &input.1,
                &source.certificate,
                lease + 1,
                Some(context),
                &mut admission,
                3
            )?
            .is_none());
        assert!(slot
            .claim(
                &replacement.1,
                &source.certificate,
                lease,
                Some(context),
                &mut admission,
                3
            )?
            .is_none());
        let claim = slot
            .claim(
                &input.1,
                &source.certificate,
                granted,
                Some(context),
                &mut admission,
                4,
            )?
            .context("current lease did not claim its file")?;
        assert!(claim.ticket == ticket && claim.file.path() == file.path.as_path());
        admission
            .submit(claim.ticket, context, 5, true, Effect::Copy)
            .unwrap();
        assert!(slot
            .claim(
                &input.1,
                &source.certificate,
                lease,
                Some(context),
                &mut admission,
                6
            )?
            .is_none());
        Ok(())
    }

    async fn confirm_input_lease(input: &(Connection, Connection), lease: u64) -> Result<u64> {
        use peerglide_protocol::{Edge, Message};
        let (granted, _) = tokio::try_join!(
            async {
                let (mut send, mut receive) = input.0.open_bi().await?;
                crate::send(
                    &mut send,
                    &Message::Acquire {
                        lease,
                        edge: Edge::Right,
                        along: 0.5,
                    },
                )
                .await?;
                match crate::receive(&mut receive).await? {
                    Message::Granted { lease: granted } if granted == lease => Ok(granted),
                    _ => anyhow::bail!("test v5 lease not granted"),
                }
            },
            async {
                let (mut send, mut receive) = input.1.accept_bi().await?;
                match crate::receive(&mut receive).await? {
                    Message::Acquire {
                        lease: requested, ..
                    } if requested == lease => {
                        crate::send(&mut send, &Message::Granted { lease }).await
                    }
                    _ => anyhow::bail!("test v5 acquire differed"),
                }
            },
        )?;
        Ok(granted)
    }
}
