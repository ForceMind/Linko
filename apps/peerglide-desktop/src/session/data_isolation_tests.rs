//! Exercise the live session and both explicit data workers for the same pair.
use super::{run, send, Policy};
use anyhow::{Context, Result};
use peerglide_protocol::{Edge, InputCapabilities, Message};
use peerglide_transport::{self as transport, clipboard_worker as clip, file_worker as file};
use std::{
    fs,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use transport::Identity;

const FILE_BYTES: &[u8] = b"isolated file worker fixture";
const TEXT: &str = "isolated clipboard worker fixture";

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Result<Self> {
        let suffix = Identity::generate()?.fingerprint();
        let directory = std::env::temp_dir().join(format!("linko-shared-pair-{suffix}"));
        fs::create_dir(&directory)?;
        Ok(Self(directory))
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

async fn next<T>(mut poll: impl FnMut() -> Option<T>) -> Result<T> {
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if let Some(value) = poll() {
                return value;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .context("data worker did not report its next state")
}

async fn live_input(tx: &mut quinn::SendStream, rx: &mut quinn::RecvStream) -> Result<()> {
    send(tx, &Message::Ping { lease: 0 }).await?;
    assert!(matches!(
        transport::receive(rx).await?,
        Message::Pong { lease: 0 }
    ));
    Ok(())
}

struct Pair {
    left: PathBuf,
    right: PathBuf,
}

impl Pair {
    async fn open_file(&self, source: &Path) -> Result<(file::Job, file::Job)> {
        let a = Identity::load(&self.left)?;
        let b = Identity::load(&self.right)?;
        let (a_pin, b_pin) = (a.certificate.clone(), b.certificate.clone());
        let mut incoming = file::start(
            file::Request::Receive {
                bind: loopback()?,
                budget: 1024,
            },
            b,
            a_pin,
            Arc::new(|| {}),
        )?;
        let file::Event::Listening(target) = next(|| incoming.poll()).await? else {
            anyhow::bail!("file listener did not start");
        };
        let mut outgoing = file::start(
            file::Request::Send {
                target,
                source: source.to_owned(),
                resume_id: None,
                prepare_handoff: false,
            },
            a,
            b_pin,
            Arc::new(|| {}),
        )?;
        let file::Event::Prepared(id) = next(|| outgoing.poll()).await? else {
            anyhow::bail!("file data was not prepared");
        };
        let file::Event::Offer(offer) = next(|| incoming.poll()).await? else {
            anyhow::bail!("file decision did not reach its local owner");
        };
        assert_eq!(offer.id, id);
        assert_eq!(offer.size, FILE_BYTES.len() as u64);
        Ok((outgoing, incoming))
    }

    async fn open_clipboard(
        &self,
        for_drop: bool,
    ) -> Result<(clip::Job, clip::Job, Box<clip::PendingClipboard>)> {
        let a = Identity::load(&self.left)?;
        let b = Identity::load(&self.right)?;
        let (a_pin, b_pin) = (a.certificate.clone(), b.certificate.clone());
        let mut incoming = clip::receive(
            loopback()?,
            b,
            a_pin,
            (!for_drop).then_some(11),
            clip::Mode::Text,
            Arc::new(|| {}),
        )?;
        let clip::Event::Listening(target) = next(|| incoming.poll()).await? else {
            anyhow::bail!("clipboard listener did not start");
        };
        let mut outgoing = clip::send(
            target,
            a,
            b_pin,
            clip::LocalPayload::Text(clip::LocalText {
                text: TEXT.to_owned(),
                classification: clip::Classification::ShareableText,
                origin: clip::Origin::Local,
            }),
            for_drop,
            Arc::new(|| {}),
        )?;
        let clip::Event::Pending(pending) = next(|| incoming.poll()).await? else {
            anyhow::bail!("clipboard decision did not reach its local owner");
        };
        assert!(matches!(
            next(|| outgoing.poll()).await?,
            clip::Event::PeerReceived
        ));
        Ok((outgoing, incoming, pending))
    }
}

fn loopback() -> Result<SocketAddr> {
    Ok("127.0.0.1:0".parse()?)
}

#[tokio::test]
async fn authenticated_input_and_data_workers_keep_independent_consent_and_stop() -> Result<()> {
    let directory = Fixture::new()?;
    let pair = Pair {
        left: directory.0.join("left"),
        right: directory.0.join("right"),
    };
    let a = Identity::create(&pair.left)?;
    let b = Identity::create(&pair.right)?;
    let source = directory.0.join("shared.txt");
    let destination = directory.0.join("received");
    fs::write(&source, FILE_BYTES)?;
    fs::create_dir(&destination)?;
    let left = transport::endpoint(loopback()?, &a, &b.certificate)?;
    let right = transport::endpoint(loopback()?, &b, &a.certificate)?;

    let server = async {
        let connection = right.accept().await.context("missing input peer")?.await?;
        let (tx, rx) = connection.accept_bi().await?;
        let result = Box::pin(run(
            connection.clone(),
            tx,
            rx,
            Policy {
                allow: false,
                native: false,
                edge: Edge::Right,
                local: InputCapabilities::default(),
                peer: InputCapabilities::default(),
            },
        ))
        .await;
        connection.close(0u8.into(), b"data isolation checked");
        result
    };
    let client = async {
        let connection = left.connect(right.local_addr()?, "peerglide.local")?.await?;
        let (mut tx, mut rx) = connection.open_bi().await?;
        live_input(&mut tx, &mut rx).await?;

        let (mut file_tx, mut file_rx) = pair.open_file(&source).await?;
        let (mut clip_tx, mut clip_rx, pending) = pair.open_clipboard(true).await?;
        live_input(&mut tx, &mut rx).await?;
        file_rx.stop();
        assert!(matches!(
            next(|| file_rx.poll()).await?,
            file::Event::Stopped
        ));
        assert!(matches!(
            next(|| file_tx.poll()).await?,
            file::Event::Failed
        ));
        assert!(fs::read_dir(&destination)?.next().is_none());
        let (completion, payload) = pending.prepare_drop()?;
        assert!(matches!(payload, clip::DragPayload::Text(text) if text == TEXT));
        clip_rx.complete(completion)?;
        for job in [&mut clip_tx, &mut clip_rx] {
            assert!(matches!(
                next(|| job.poll()).await?,
                clip::Event::Finished {
                    outcome: clip::TextOutcome::Submitted,
                    copy_receipt: Some(_),
                }
            ));
        }
        send(
            &mut tx,
            &Message::Acquire {
                lease: 1,
                edge: Edge::Right,
                along: 0.5,
            },
        )
        .await?;
        assert!(matches!(
            transport::receive(&mut rx).await?,
            Message::Denied { lease: 1 }
        ));

        let (mut file_tx, mut file_rx) = pair.open_file(&source).await?;
        let (mut clip_tx, mut clip_rx, pending) = pair.open_clipboard(false).await?;
        clip_rx.complete(pending.decline()?)?;
        file_rx.decide(file::Decision::Accept {
            directory: destination.clone(),
            resume: false,
        })?;
        live_input(&mut tx, &mut rx).await?;
        for job in [&mut clip_tx, &mut clip_rx] {
            assert!(matches!(
                next(|| job.poll()).await?,
                clip::Event::Finished {
                    outcome: clip::TextOutcome::Declined,
                    copy_receipt: None,
                }
            ));
        }
        assert!(matches!(
            next(|| file_tx.poll()).await?,
            file::Event::Sent {
                outcome: file::FileOutcome::Committed,
                handoff: None,
            }
        ));
        assert!(matches!(
            next(|| file_rx.poll()).await?,
            file::Event::Received(file::ReceiveResult {
                outcome: file::FileOutcome::Committed,
                report_queued: true,
                ..
            })
        ));
        assert_eq!(fs::read(destination.join("shared.txt"))?, FILE_BYTES);
        live_input(&mut tx, &mut rx).await?;
        tx.finish()?;
        let _ = connection.closed().await;
        Ok::<_, anyhow::Error>(())
    };
    tokio::time::timeout(Duration::from_secs(30), async {
        tokio::try_join!(server, client)
    })
    .await??;
    Ok(())
}
