//! Explicit one-shot text transfer. OS clipboard effects stay on the local owner.
use crate::{data, Identity};
use anyhow::{Context, Result};
use peerglide_transfer::{
    clipboard::{Backend, Origin, MAX_TEXT_BYTES},
    data_policy::Permissions,
};
pub use peerglide_transfer::{clipboard::Classification, codec::TextOutcome};
use quinn::Connection;
use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::{mpsc, Arc},
    thread::{self, JoinHandle},
    time::Duration,
};
use tokio::sync::{oneshot, watch};

const TEXT: data::Capabilities = data::Capabilities {
    text: true,
    files: false,
};
const WIRE_BYTES: u64 = MAX_TEXT_BYTES as u64 + 1024;
const CONTROL_BYTES: u64 = 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
const LISTEN_TIMEOUT: Duration = Duration::from_secs(30);
const DECISION_TIMEOUT: Duration = Duration::from_secs(35);
const GRANT_MS: u64 = 60_000;

type Wake = Arc<dyn Fn() + Send + Sync>;

pub enum Event {
    Listening(SocketAddr),
    PeerReceived,
    Pending(PendingText),
    Finished(TextOutcome),
    Stopped,
    Failed(String),
}

pub struct PendingText {
    consent: data::Consent,
    frame: data::AdmittedFrame,
}

impl PendingText {
    pub fn import(mut self, backend: &mut impl Backend) -> Result<Completion> {
        let result = self.consent.apply_text(self.frame, backend)?;
        Ok(Completion {
            consent: self.consent,
            result,
        })
    }

    pub fn decline(mut self) -> Result<Completion> {
        let result = self.consent.decline_text(self.frame)?;
        Ok(Completion {
            consent: self.consent,
            result,
        })
    }
}

pub struct Completion {
    consent: data::Consent,
    result: data::ClipboardCompletion,
}

impl Completion {
    pub fn outcome(&self) -> TextOutcome {
        self.result.outcome()
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

enum Operation {
    Send {
        target: SocketAddr,
        text: LocalText,
    },
    Receive {
        bind: SocketAddr,
        revision: u64,
        decision: oneshot::Receiver<Completion>,
    },
}

pub fn send(
    target: SocketAddr,
    identity: Identity,
    pinned_peer: Vec<u8>,
    text: LocalText,
    wake: Wake,
) -> Result<Job> {
    spawn(
        Operation::Send { target, text },
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
    wake: Wake,
) -> Result<Job> {
    let (decide, decision) = oneshot::channel();
    spawn(
        Operation::Receive {
            bind,
            revision,
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
        Operation::Send { target, text } => {
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
            let stream = data::open(&connection.0, &pinned_peer, TEXT).await?;
            let (mut consent, mut send, mut receive) = stream.into_session(WIRE_BYTES, CONTROL_BYTES)?;
            consent.grant(data_permissions(true, false), GRANT_MS)?;
            let sequence = send
                .write_text(&text.text, text.classification, text.origin)
                .await?;
            receive.wait_text_receipt(sequence).await?;
            events.post(Event::PeerReceived)?;
            receive.wait_text_result(sequence).await?
        }
        Operation::Receive {
            bind,
            revision,
            decision,
        } => {
            let endpoint = data::endpoint(bind, &identity, &pinned_peer)?;
            events.post(Event::Listening(endpoint.local_addr()?))?;
            let connection = OwnedConnection(
                tokio::time::timeout(LISTEN_TIMEOUT, async {
                    let incoming = endpoint.accept().await.context("clipboard listener closed")?;
                    Ok::<_, anyhow::Error>(incoming.await?)
                })
                .await
                .context("clipboard listener timed out")??,
            );
            let stream = data::accept(&connection.0, &pinned_peer, TEXT).await?;
            let (mut consent, mut send, mut receive) = stream.into_session(CONTROL_BYTES, WIRE_BYTES)?;
            consent.grant(data_permissions(false, true), GRANT_MS)?;
            let mut frame = receive.read(Some(revision)).await?;
            send.acknowledge_text(&mut frame).await?;
            events.post(Event::Pending(PendingText { consent, frame }))?;
            let completion = tokio::time::timeout(DECISION_TIMEOUT, decision)
                .await
                .context("clipboard import decision timed out")??;
            let outcome = completion.outcome();
            send.report_text_result(completion.result).await?;
            // Keep the endpoint and consent alive until the source reads its
            // terminal result and closes. This does not retry any OS effect.
            let _ = tokio::time::timeout(Duration::from_secs(5), connection.0.closed()).await;
            drop(completion.consent);
            outcome
        }
    };
    events.post(Event::Finished(outcome))
}

fn data_permissions(send: bool, receive: bool) -> Permissions {
    Permissions {
        text_send: send,
        text_receive: receive,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peerglide_transfer::clipboard::WriteResult;

    struct Memory {
        revision: u64,
        text: String,
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

    #[test]
    fn two_workers_keep_consent_until_local_import_and_report_the_result() -> Result<()> {
        loopback(false)
    }

    #[test]
    fn stopping_the_receiver_invalidates_an_already_admitted_frame() -> Result<()> {
        loopback(true)
    }

    fn loopback(cancel: bool) -> Result<()> {
        let source = Identity::generate()?;
        let destination = Identity::generate()?;
        let source_cert = source.certificate.clone();
        let destination_cert = destination.certificate.clone();
        let wake: Wake = Arc::new(|| {});
        let mut backend = Memory {
            revision: 5,
            text: String::new(),
        };
        let mut receiving = receive(
            "127.0.0.1:0".parse()?,
            destination,
            source_cert,
            backend.revision,
            wake.clone(),
        )?;
        let Event::Listening(address) = receiving.events.recv_timeout(Duration::from_secs(5))? else {
            anyhow::bail!("expected clipboard listener readiness");
        };
        let sending = send(
            address,
            source,
            destination_cert,
            LocalText {
                text: "屏跃".into(),
                classification: Classification::ShareableText,
                origin: Origin::Local,
            },
            wake,
        )?;
        let Event::Pending(pending) = receiving.events.recv_timeout(Duration::from_secs(8))? else {
            anyhow::bail!("expected an admitted clipboard frame");
        };
        assert!(matches!(
            sending.events.recv_timeout(Duration::from_secs(5))?,
            Event::PeerReceived
        ));
        assert!(backend.text.is_empty());
        if cancel {
            receiving.stop();
            assert!(matches!(
                receiving.events.recv_timeout(Duration::from_secs(5))?,
                Event::Stopped
            ));
            assert!(pending.import(&mut backend).is_err());
            assert!(backend.text.is_empty());
            assert!(matches!(
                sending.events.recv_timeout(Duration::from_secs(5))?,
                Event::Failed(_)
            ));
            return Ok(());
        }
        receiving.complete(pending.import(&mut backend)?)?;
        assert_eq!(backend.text, "屏跃");
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
