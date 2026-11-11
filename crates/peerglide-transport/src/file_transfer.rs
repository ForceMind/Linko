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
    future::Future,
    io,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

const FILES: data::Capabilities = data::Capabilities {
    text: false,
    files: true,
};
const DATA_BYTES: u64 = MAX_FILE_BYTES
    + MAX_MANIFEST_BYTES as u64
    + (MAX_FILE_BYTES.div_ceil(CHUNK_BYTES as u64) + 2) * (HEADER_BYTES as u64 + 20);
const REPLY_BYTES: u64 = 256;
const FRAME_TIMEOUT: Duration = Duration::from_secs(5);
const DECISION_TIMEOUT: Duration = Duration::from_secs(60);
const GRANT: Duration = Duration::from_secs(20 * 60);

#[derive(Clone, Default)]
pub struct Cancellation(Arc<AtomicBool>);

impl Cancellation {
    pub fn stop(&self) {
        self.0.store(true, Ordering::Release);
    }

    fn check(&self) -> io::Result<()> {
        if self.0.load(Ordering::Acquire) {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "file task stopped"));
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
    Accept {
        directory: PathBuf,
        resume: bool,
    },
}

/// The local disk outcome is independent of the peer receiving a receipt.
pub struct ReceiveResult {
    pub outcome: FileOutcome,
    pub report_queued: bool,
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

    for expected in next..total_chunks {
        let admitted = receive
            .read_file(&[Kind::Chunk, Kind::Cancel], FRAME_TIMEOUT)
            .await?;
        let bytes = consent.apply_received(admitted, |frame| match frame {
            Frame::Cancel { id: received } if received == id => Ok(None),
            Frame::Chunk {
                id: received,
                index,
                bytes,
            } if received == id && index as usize == expected =>
            {
                Ok(Some(bytes))
            }
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
    let outcome = tokio::task::spawn_blocking({
        let guard = guard.clone();
        move || {
            match manager.commit_checked(id, || guard.elapsed_ms(), || guard.check()) {
                Ok(_) => FileOutcome::Committed,
                Err(_) => FileOutcome::Unknown,
            }
        }
    })
    .await?;
    Ok(report(connection, &mut send, id, outcome).await)
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
    async fn rejected_offer_leaves_receive_directory_untouched() -> Result<()> {
        loopback(false).await
    }

    #[tokio::test]
    async fn approved_offer_is_published_and_reported_after_verified_chunks() -> Result<()> {
        loopback(true).await
    }

    async fn loopback(approve: bool) -> Result<()> {
        let dir = TestDirectory::new()?;
        let source_path = dir.0.join("example.bin");
        let destination = dir.0.join("received");
        let contents = vec![0x2a; CHUNK_BYTES + 19];
        fs::write(&source_path, &contents)?;
        fs::create_dir(&destination)?;
        let (manifest, mut source_file) = Manifest::open_checked(&source_path, || Ok(()))?;

        let source = Identity::generate()?;
        let peer = Identity::generate()?;
        let address: SocketAddr = "127.0.0.1:0".parse()?;
        let left = data::endpoint(address, &source, &peer.certificate)?;
        let right = data::endpoint(address, &peer, &source.certificate)?;
        let (client, server) = tokio::try_join!(
            async {
                Ok::<_, anyhow::Error>(
                    left.connect(right.local_addr()?, "peerglide.local")?.await?,
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
                    assert!(fs::read_dir(&directory)?.next().is_none());
                    if approve {
                        Ok(Decision::Accept {
                            directory,
                            resume: false,
                        })
                    } else {
                        Ok(Decision::Decline)
                    }
                }
            },
            |progress| last_progress = Some(progress),
        );
        let sender = async {
            let _close = CloseOnExit(&client);
            let (mut consent, mut send, mut receive) =
                data::open(&client, &peer.certificate, FILES)
                    .await?
                    .into_session(DATA_BYTES, REPLY_BYTES)?;
            consent.grant(
                Permissions {
                    file_send: true,
                    ..Permissions::default()
                },
                60_000,
            )?;
            let id = [1; 16];
            send.write(&Frame::Offer {
                id,
                manifest: manifest.clone(),
            })
            .await?;
            if approve {
                let accepted = receive.read_file(&[Kind::FileAccepted], FRAME_TIMEOUT).await?;
                consent.apply_received(accepted, |frame| {
                    ensure!(matches!(
                        frame,
                        Frame::FileAccepted { id: received, next_chunk: 0 } if received == id
                    ));
                    Ok(())
                })?;
                for index in 0..manifest.chunks() {
                    send.write(&Frame::Chunk {
                        id,
                        index: u32::try_from(index)?,
                        bytes: manifest.read_chunk(&mut source_file, index)?,
                    })
                    .await?;
                }
            }
            let result = receive.read_file(&[Kind::FileResult], FRAME_TIMEOUT).await?;
            consent.apply_received(result, |frame| match frame {
                Frame::FileResult { id: received, outcome } if received == id => Ok(outcome),
                _ => anyhow::bail!("wrong file outcome"),
            })
        };
        let (received, sent) = tokio::try_join!(receiver, sender)?;
        assert_eq!(received.outcome, sent);
        assert!(received.report_queued);
        if approve {
            assert_eq!(sent, FileOutcome::Committed);
            assert!(last_progress.context("missing disk progress")?.chunks_complete);
            assert_eq!(fs::read(destination.join("example.bin"))?, contents);
        } else {
            assert_eq!(sent, FileOutcome::Declined);
            assert!(last_progress.is_none());
            assert!(fs::read_dir(destination)?.next().is_none());
        }
        Ok(())
    }
}
