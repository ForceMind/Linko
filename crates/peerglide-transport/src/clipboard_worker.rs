//! Explicit one-shot clipboard transfer. OS clipboard effects stay on the local owner.
use crate::{data, Identity};
use anyhow::{ensure, Context, Result};
pub use peerglide_transfer::{clipboard::Classification, codec::TextOutcome};
use peerglide_transfer::{
    clipboard::{Backend, Origin, MAX_TEXT_BYTES},
    clipboard_image::{Image, ImageBackend, MAX_PNG_BYTES},
    data_policy::Permissions,
};
use quinn::Connection;
use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::{mpsc, Arc},
    thread::{self, JoinHandle},
    time::Duration,
};
use tokio::sync::{oneshot, watch};

const CONTROL_BYTES: u64 = 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
const LISTEN_TIMEOUT: Duration = Duration::from_secs(30);
const DECISION_TIMEOUT: Duration = Duration::from_secs(35);
const GRANT_MS: u64 = 60_000;

type Wake = Arc<dyn Fn() + Send + Sync>;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Text,
    Image,
}

impl Mode {
    fn capabilities(self) -> data::Capabilities {
        data::Capabilities {
            text: self == Self::Text,
            files: false,
            images: self == Self::Image,
        }
    }

    fn budget(self) -> u64 {
        let bytes = match self {
            Self::Text => MAX_TEXT_BYTES,
            Self::Image => MAX_PNG_BYTES,
        };
        bytes as u64 + 1024
    }

    fn permissions(self, sending: bool) -> Permissions {
        Permissions {
            text_send: self == Self::Text && sending,
            text_receive: self == Self::Text && !sending,
            image_send: self == Self::Image && sending,
            image_receive: self == Self::Image && !sending,
            ..Permissions::default()
        }
    }
}

pub enum Event {
    Listening(SocketAddr),
    PeerReceived,
    WaitingForImageResult,
    Pending(PendingClipboard),
    Finished(TextOutcome),
    Stopped,
    Failed(String),
}

enum PendingFrame {
    Text(data::AdmittedFrame),
    Image(data::AdmittedImage),
}

pub struct PendingClipboard {
    consent: data::Consent,
    frame: PendingFrame,
}

impl PendingClipboard {
    pub fn dimensions(&self) -> Option<(u32, u32)> {
        match &self.frame {
            PendingFrame::Text(_) => None,
            PendingFrame::Image(image) => Some(image.dimensions()),
        }
    }

    pub fn import(mut self, backend: &mut (impl Backend + ImageBackend)) -> Result<Completion> {
        let result = match self.frame {
            PendingFrame::Text(frame) => {
                CompletedFrame::Text(self.consent.apply_text(frame, backend)?)
            }
            PendingFrame::Image(frame) => {
                CompletedFrame::Image(self.consent.apply_image(frame, backend)?)
            }
        };
        Ok(Completion {
            consent: self.consent,
            result,
        })
    }

    pub fn decline(mut self) -> Result<Completion> {
        let result = match self.frame {
            PendingFrame::Text(frame) => CompletedFrame::Text(self.consent.decline_text(frame)?),
            PendingFrame::Image(frame) => CompletedFrame::Image(self.consent.decline_image(frame)?),
        };
        Ok(Completion {
            consent: self.consent,
            result,
        })
    }
}

enum CompletedFrame {
    Text(data::ClipboardCompletion),
    Image(data::ImageCompletion),
}

pub struct Completion {
    consent: data::Consent,
    result: CompletedFrame,
}

impl Completion {
    pub fn outcome(&self) -> TextOutcome {
        match &self.result {
            CompletedFrame::Text(result) => result.outcome(),
            CompletedFrame::Image(result) => result.outcome(),
        }
    }
}

pub struct Job {
    stop: watch::Sender<bool>,
    events: mpsc::Receiver<Event>,
    decision: Option<oneshot::Sender<Completion>>,
    owner: JoinHandle<()>,
}

impl Job {
    pub fn poll(&mut self) -> Option<Event> {
        self.events.try_recv().ok()
    }

    pub fn complete(&mut self, completion: Completion) -> Result<()> {
        self.decision
            .take()
            .context("clipboard import is no longer pending")?
            .send(completion)
            .map_err(|_| anyhow::anyhow!("clipboard result could not be reported"))
    }

    pub fn stop(&self) {
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

pub struct LocalText {
    pub text: String,
    pub classification: Classification,
    pub origin: Origin,
}

pub enum LocalPayload {
    Text(LocalText),
    Image { png: Vec<u8>, origin: Origin },
}

enum PreparedPayload {
    Text(LocalText),
    Image(Image),
}

impl LocalPayload {
    async fn prepare(self) -> Result<(Mode, PreparedPayload)> {
        match self {
            Self::Text(text) => Ok((Mode::Text, PreparedPayload::Text(text))),
            Self::Image { png, origin } => {
                ensure!(origin == Origin::Local, "imported images stay local");
                let image = tokio::task::spawn_blocking(move || Image::normalize(&png)).await??;
                Ok((Mode::Image, PreparedPayload::Image(image)))
            }
        }
    }
}

enum Operation {
    Send {
        target: SocketAddr,
        payload: LocalPayload,
    },
    Receive {
        bind: SocketAddr,
        revision: u64,
        mode: Mode,
        decision: oneshot::Receiver<Completion>,
    },
}

pub fn send(
    target: SocketAddr,
    identity: Identity,
    pinned_peer: Vec<u8>,
    payload: LocalPayload,
    wake: Wake,
) -> Result<Job> {
    spawn(
        Operation::Send { target, payload },
        identity,
        pinned_peer,
        None,
        wake,
    )
}

pub fn receive(
    bind: SocketAddr,
    identity: Identity,
    pinned_peer: Vec<u8>,
    revision: u64,
    mode: Mode,
    wake: Wake,
) -> Result<Job> {
    let (decide, decision) = oneshot::channel();
    spawn(
        Operation::Receive {
            bind,
            revision,
            mode,
            decision,
        },
        identity,
        pinned_peer,
        Some(decide),
        wake,
    )
}

struct Events {
    tx: mpsc::SyncSender<Event>,
    wake: Wake,
}

impl Events {
    fn post(&self, event: Event) -> Result<()> {
        self.tx
            .try_send(event)
            .map_err(|_| anyhow::anyhow!("clipboard controller unavailable"))?;
        (self.wake)();
        Ok(())
    }
}

fn spawn(
    operation: Operation,
    identity: Identity,
    pinned_peer: Vec<u8>,
    decision: Option<oneshot::Sender<Completion>>,
    wake: Wake,
) -> Result<Job> {
    let (tx, events) = mpsc::sync_channel(4);
    let (stop, mut stopped) = watch::channel(false);
    let owner = thread::Builder::new()
        .name("linko-clipboard-data".into())
        .spawn(move || {
            let events = Events { tx, wake };
            let result = (|| -> Result<()> {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .max_blocking_threads(1)
                    .enable_all()
                    .build()?;
                runtime.block_on(async {
                    let result = tokio::select! {
                        biased;
                        _ = stopped.wait_for(|stopped| *stopped) => None,
                        result = run(operation, identity, pinned_peer, &events) => Some(result),
                    };
                    match result {
                        Some(result) => result,
                        None => events.post(Event::Stopped),
                    }
                })
            })();
            if let Err(error) = result {
                let _ = events.post(Event::Failed(error.to_string()));
            }
        })?;
    Ok(Job {
        stop,
        events,
        decision,
        owner,
    })
}

/// Cancellation also closes the connection while the GUI owns an admitted frame.
struct OwnedConnection(Connection);

impl Drop for OwnedConnection {
    fn drop(&mut self) {
        self.0.close(0u8.into(), b"clipboard operation ended");
    }
}

async fn run(
    operation: Operation,
    identity: Identity,
    pinned_peer: Vec<u8>,
    events: &Events,
) -> Result<()> {
    let outcome = match operation {
        Operation::Send { target, payload } => {
            let (mode, payload) = payload.prepare().await?;
            let bind = match target.ip() {
                IpAddr::V4(_) => SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)),
                IpAddr::V6(_) => SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0)),
            };
            let endpoint = data::endpoint(bind, &identity, &pinned_peer)?;
            let connecting = endpoint.connect(target, "peerglide.local")?;
            let connection = OwnedConnection(
                tokio::time::timeout(CONNECT_TIMEOUT, connecting)
                    .await
                    .context("clipboard connection timed out")??,
            );
            let stream = data::open(&connection.0, &pinned_peer, mode.capabilities()).await?;
            let (mut consent, mut send, mut receive) =
                stream.into_session(mode.budget(), CONTROL_BYTES)?;
            consent.grant(mode.permissions(true), GRANT_MS)?;
            match payload {
                PreparedPayload::Text(text) => {
                    let sequence = send
                        .write_text(&text.text, text.classification, text.origin)
                        .await?;
                    receive.wait_text_receipt(sequence).await?;
                    events.post(Event::PeerReceived)?;
                    receive.wait_text_result(sequence).await?
                }
                PreparedPayload::Image(image) => {
                    let sequence = send.write_image(image, Origin::Local).await?;
                    events.post(Event::WaitingForImageResult)?;
                    receive.wait_image_result(sequence).await?
                }
            }
        }
        Operation::Receive {
            bind,
            revision,
            mode,
            decision,
        } => {
            let endpoint = data::endpoint(bind, &identity, &pinned_peer)?;
            events.post(Event::Listening(endpoint.local_addr()?))?;
            let connection = OwnedConnection(
                tokio::time::timeout(LISTEN_TIMEOUT, async {
                    let incoming = endpoint
                        .accept()
                        .await
                        .context("clipboard listener closed")?;
                    Ok::<_, anyhow::Error>(incoming.await?)
                })
                .await
                .context("clipboard listener timed out")??,
            );
            let stream = data::accept(&connection.0, &pinned_peer, mode.capabilities()).await?;
            let (mut consent, mut send, mut receive) =
                stream.into_session(CONTROL_BYTES, mode.budget())?;
            consent.grant(mode.permissions(false), GRANT_MS)?;
            let frame = match mode {
                Mode::Text => {
                    let mut frame = receive.read(Some(revision)).await?;
                    send.acknowledge_text(&mut frame).await?;
                    PendingFrame::Text(frame)
                }
                Mode::Image => PendingFrame::Image(receive.read_image(revision).await?),
            };
            events.post(Event::Pending(PendingClipboard { consent, frame }))?;
            let completion = tokio::time::timeout(DECISION_TIMEOUT, decision)
                .await
                .context("clipboard import decision timed out")??;
            let outcome = completion.outcome();
            match completion.result {
                CompletedFrame::Text(result) => send.report_text_result(result).await?,
                CompletedFrame::Image(result) => send.report_image_result(result).await?,
            }
            // Keep the endpoint and consent alive until the source reads its
            // terminal result and closes. This does not retry any OS effect.
            let _ = tokio::time::timeout(Duration::from_secs(5), connection.0.closed()).await;
            drop(completion.consent);
            outcome
        }
    };
    events.post(Event::Finished(outcome))
}

#[cfg(test)]
mod tests {
    use super::*;
    use peerglide_transfer::clipboard::WriteResult;

    struct Memory {
        revision: u64,
        text: String,
        images: usize,
    }

    impl Backend for Memory {
        fn compare_and_write(&mut self, revision: u64, text: &str) -> WriteResult {
            if revision != self.revision {
                return WriteResult::Changed;
            }
            self.text = text.into();
            self.revision += 1;
            WriteResult::Submitted
        }
    }

    impl ImageBackend for Memory {
        fn compare_and_write_image(&mut self, revision: u64, _: &Image) -> WriteResult {
            if revision != self.revision {
                return WriteResult::Changed;
            }
            self.revision += 1;
            self.images += 1;
            WriteResult::Submitted
        }
    }

    #[test]
    fn two_workers_keep_consent_until_local_import_and_report_the_result() -> Result<()> {
        loopback(Mode::Text, false)
    }

    #[test]
    fn stopping_the_receiver_invalidates_an_already_admitted_frame() -> Result<()> {
        loopback(Mode::Text, true)
    }

    #[test]
    fn image_worker_waits_for_the_local_import_and_reports_the_result() -> Result<()> {
        loopback(Mode::Image, false)
    }

    #[test]
    fn stopping_the_receiver_invalidates_a_validated_image() -> Result<()> {
        loopback(Mode::Image, true)
    }

    fn loopback(mode: Mode, cancel: bool) -> Result<()> {
        let source = Identity::generate()?;
        let destination = Identity::generate()?;
        let source_cert = source.certificate.clone();
        let destination_cert = destination.certificate.clone();
        let wake: Wake = Arc::new(|| {});
        let mut backend = Memory {
            revision: 5,
            text: String::new(),
            images: 0,
        };
        let mut receiving = receive(
            "127.0.0.1:0".parse()?,
            destination,
            source_cert,
            backend.revision,
            mode,
            wake.clone(),
        )?;
        let Event::Listening(address) = receiving.events.recv_timeout(Duration::from_secs(5))?
        else {
            anyhow::bail!("expected clipboard listener readiness");
        };
        let payload = match mode {
            Mode::Text => LocalPayload::Text(LocalText {
                text: "屏跃".into(),
                classification: Classification::ShareableText,
                origin: Origin::Local,
            }),
            Mode::Image => LocalPayload::Image {
                png: data::TEST_PNG.to_vec(),
                origin: Origin::Local,
            },
        };
        let sending = send(address, source, destination_cert, payload, wake)?;
        let Event::Pending(pending) = receiving.events.recv_timeout(Duration::from_secs(8))? else {
            anyhow::bail!("expected an admitted clipboard frame");
        };
        let notice = sending.events.recv_timeout(Duration::from_secs(5))?;
        assert!(matches!(
            (mode, notice),
            (Mode::Text, Event::PeerReceived) | (Mode::Image, Event::WaitingForImageResult)
        ));
        assert_eq!(
            pending.dimensions(),
            (mode == Mode::Image).then_some((1, 1))
        );
        assert!(backend.text.is_empty());
        assert_eq!(backend.images, 0);
        if cancel {
            receiving.stop();
            assert!(matches!(
                receiving.events.recv_timeout(Duration::from_secs(5))?,
                Event::Stopped
            ));
            assert!(pending.import(&mut backend).is_err());
            assert!(backend.text.is_empty());
            assert_eq!(backend.images, 0);
            assert!(matches!(
                sending.events.recv_timeout(Duration::from_secs(5))?,
                Event::Failed(_)
            ));
            return Ok(());
        }
        receiving.complete(pending.import(&mut backend)?)?;
        match mode {
            Mode::Text => assert_eq!(backend.text, "屏跃"),
            Mode::Image => assert_eq!(backend.images, 1),
        }
        assert!(matches!(
            sending.events.recv_timeout(Duration::from_secs(5))?,
            Event::Finished(TextOutcome::Submitted)
        ));
        assert!(matches!(
            receiving.events.recv_timeout(Duration::from_secs(8))?,
            Event::Finished(TextOutcome::Submitted)
        ));
        Ok(())
    }
}
