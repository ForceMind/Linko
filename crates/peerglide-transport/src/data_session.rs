//! Direction consent owned locally and bound to one authenticated data connection.
use super::{
    data::Capabilities,
    data_frames::{FrameReceiver, FrameSender},
};
use anyhow::{ensure, Result};
use peerglide_transfer::{
    clipboard::{Backend, Classification, Clipboard, Error as ClipboardError, Origin, WriteResult},
    clipboard_image::{Image, ImageBackend},
    codec::{ClipboardOutcome, Frame, Kind, TextOutcome},
    data_policy::{Capability, DataSession, Permissions, Permit},
};
use quinn::Connection;
use rand::RngCore;
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

struct Policy {
    connection: Connection,
    peer_digest: [u8; 32],
    clipboard: Mutex<Clipboard>,
    started: Instant,
}

impl Policy {
    fn check<T, E: std::fmt::Debug>(
        &self,
        operation: impl FnOnce(&mut Clipboard, u64) -> Result<T, E>,
    ) -> Result<T> {
        let mut clipboard = self
            .clipboard
            .lock()
            .map_err(|_| anyhow::anyhow!("data consent unavailable"))?;
        ensure!(
            self.connection.close_reason().is_none(),
            "data connection closed"
        );
        let now = u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        operation(&mut clipboard, now)
            .map_err(|error| anyhow::anyhow!("data policy rejected operation ({error:?})"))
    }

    fn close(&self) {
        if let Ok(mut clipboard) = self.clipboard.lock() {
            clipboard.close();
        }
        self.connection
            .close(0u8.into(), b"local data session stopped");
    }
}

/// Only the local controller owns this handle. No network message grants consent.
pub struct Consent {
    policy: Arc<Policy>,
    capabilities: Capabilities,
    granted: bool,
}

pub struct Sender {
    policy: Arc<Policy>,
    frames: FrameSender,
    image_sequence: u64,
}

pub struct Receiver {
    policy: Arc<Policy>,
    frames: FrameReceiver,
    image_watermark: u64,
}

/// A frame admitted at the network header. Its ticket must still be valid at use.
pub struct AdmittedFrame {
    frame: Frame,
    permit: Permit,
    clipboard_revision: Option<u64>,
    acknowledged: bool,
}

/// A local terminal decision for one admitted sequence, with its original ticket.
pub struct ClipboardCompletion {
    sequence: u64,
    outcome: TextOutcome,
    permit: Permit,
}

impl ClipboardCompletion {
    pub fn outcome(&self) -> TextOutcome {
        self.outcome
    }
}

/// Validated pixels, bound to the original receive grant and local revision.
pub struct AdmittedImage {
    sequence: u64,
    permit: Permit,
    revision: u64,
    image: Image,
}

impl AdmittedImage {
    pub fn dimensions(&self) -> (u32, u32) {
        self.image.dimensions()
    }
}

pub struct ImageCompletion {
    sequence: u64,
    outcome: ClipboardOutcome,
    permit: Permit,
}

impl ImageCompletion {
    pub fn outcome(&self) -> ClipboardOutcome {
        self.outcome
    }
}

pub(super) fn new(
    connection: Connection,
    peer_digest: [u8; 32],
    capabilities: Capabilities,
    sender: FrameSender,
    receiver: FrameReceiver,
) -> Result<(Consent, Sender, Receiver)> {
    let mut nonce = [0; 32];
    if let Err(error) = rand::rngs::OsRng.try_fill_bytes(&mut nonce) {
        connection.close(1u8.into(), b"data session setup failed");
        return Err(anyhow::anyhow!(
            "data session randomness unavailable: {error}"
        ));
    }
    let policy = Arc::new(Policy {
        connection,
        peer_digest,
        clipboard: Mutex::new(Clipboard::new(DataSession::new(peer_digest, nonce, 0))),
        started: Instant::now(),
    });
    Ok((
        Consent {
            policy: policy.clone(),
            capabilities,
            granted: false,
        },
        Sender {
            policy: policy.clone(),
            frames: sender,
            image_sequence: 0,
        },
        Receiver {
            policy,
            frames: receiver,
            image_watermark: 0,
        },
    ))
}

impl Consent {
    /// Set the initial local grant once. Changing it requires revocation and
    /// a fresh connection, so in-flight work cannot outlive its scope.
    pub fn grant(&mut self, permissions: Permissions, lifetime_ms: u64) -> Result<()> {
        ensure!(
            !self.granted,
            "open a fresh connection to change data consent"
        );
        ensure!(
            (self.capabilities.text || !permissions.text_send && !permissions.text_receive)
                && (self.capabilities.files || !permissions.file_send && !permissions.file_receive)
                && (self.capabilities.images || !permissions.image_send && !permissions.image_receive),
            "data consent requests an unsupported capability"
        );
        self.policy
            .check(|session, now| session.consent(permissions, lifetime_ms, now))?;
        self.granted = true;
        Ok(())
    }

    pub fn revoke(&mut self) {
        self.policy.close();
    }

    pub(crate) fn file_receive_peer(&self) -> Result<[u8; 32]> {
        self.policy
            .check(|session, now| session.authorize(Capability::FileReceive, now))?;
        Ok(self.policy.peer_digest)
    }

    /// File dispatch still requires its own offer and transfer-ID checks.
    pub fn apply_received<T>(
        &mut self,
        admitted: AdmittedFrame,
        apply: impl FnOnce(Frame) -> Result<T>,
    ) -> Result<T> {
        let kind = admitted.frame.kind();
        ensure!(is_file(kind), "use the clipboard receive path");
        let capability = inbound(kind);
        self.policy
            .check(|session, now| session.validate(&admitted.permit, capability, now))?;
        apply(admitted.frame)
    }

    /// The revision was sampled before the frame was queued. This owner checks
    /// consent and sequence without holding the policy lock during an OS write.
    pub fn apply_text(
        &mut self,
        admitted: AdmittedFrame,
        backend: &mut impl Backend,
    ) -> Result<ClipboardCompletion> {
        let Frame::Text { sequence, text } = admitted.frame else {
            anyhow::bail!("expected a clipboard text frame");
        };
        let revision = admitted
            .clipboard_revision
            .ok_or_else(|| anyhow::anyhow!("clipboard revision missing"))?;
        self.policy.check(|clipboard, now| {
            clipboard
                .validate(&admitted.permit, Capability::TextReceive, now)
                .map_err(ClipboardError::Permission)?;
            clipboard.accept_received_text(sequence, text.as_bytes(), now)?;
            Ok::<_, ClipboardError>(())
        })?;
        let outcome = clipboard_outcome(backend.compare_and_write(revision, &text));
        Ok(ClipboardCompletion {
            sequence,
            outcome,
            permit: admitted.permit,
        })
    }

    /// Decline the admitted text without consulting or changing the system clipboard.
    pub fn decline_text(&mut self, admitted: AdmittedFrame) -> Result<ClipboardCompletion> {
        let Frame::Text { sequence, .. } = admitted.frame else {
            anyhow::bail!("expected a clipboard text frame");
        };
        self.policy.check(|session, now| {
            session.validate(&admitted.permit, Capability::TextReceive, now)
        })?;
        Ok(ClipboardCompletion {
            sequence,
            outcome: TextOutcome::Declined,
            permit: admitted.permit,
        })
    }

    /// Import only the locally chosen image, after rechecking the original grant.
    pub fn apply_image(
        &mut self,
        admitted: AdmittedImage,
        backend: &mut impl ImageBackend,
    ) -> Result<ImageCompletion> {
        self.complete_image(admitted, |revision, image| {
            clipboard_outcome(backend.compare_and_write_image(revision, image))
        })
    }

    pub fn decline_image(&mut self, admitted: AdmittedImage) -> Result<ImageCompletion> {
        self.complete_image(admitted, |_, _| ClipboardOutcome::Declined)
    }

    fn complete_image(
        &mut self,
        admitted: AdmittedImage,
        complete: impl FnOnce(u64, &Image) -> ClipboardOutcome,
    ) -> Result<ImageCompletion> {
        self.policy.check(|session, now| {
            session.validate(&admitted.permit, Capability::ImageReceive, now)
        })?;
        Ok(ImageCompletion {
            sequence: admitted.sequence,
            outcome: complete(admitted.revision, &admitted.image),
            permit: admitted.permit,
        })
    }
}

impl Drop for Consent {
    fn drop(&mut self) {
        self.revoke();
    }
}

impl Sender {
    pub async fn write(&mut self, frame: &Frame) -> Result<()> {
        ensure!(is_file(frame.kind()), "use the clipboard send path");
        let policy = &self.policy;
        self.frames
            .write_checked(frame, |kind| {
                policy.check(|session, now| session.authorize(outbound(kind), now))?;
                Ok(())
            })
            .await
    }

    /// Classify the local snapshot, prevent echo and recheck its original
    /// direction ticket immediately before the network write.
    pub async fn write_text(
        &mut self,
        text: &str,
        classification: Classification,
        origin: Origin,
    ) -> Result<u64> {
        let policy = &self.policy;
        let candidate =
            policy.check(|clipboard, now| clipboard.export(text, classification, origin, now))?;
        let (sequence, text, permit) = candidate.into_parts();
        self.frames
            .write_checked(&Frame::Text { sequence, text }, |_| {
                policy
                    .check(|clipboard, now| clipboard.validate(&permit, Capability::TextSend, now))
            })
            .await?;
        Ok(sequence)
    }

    /// Send only a normalized local snapshot. Imported images stay local.
    pub async fn write_image(&mut self, image: Image, origin: Origin) -> Result<u64> {
        ensure!(origin == Origin::Local, "imported images stay local");
        let policy = &self.policy;
        let permit = policy.check(|session, now| session.authorize(Capability::ImageSend, now))?;
        let sequence = self
            .image_sequence
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("image sequence exhausted"))?;
        self.image_sequence = sequence;
        self.frames
            .write_checked(
                &Frame::Image {
                    sequence,
                    png: image.into_png(),
                },
                |_| {
                    policy.check(|session, now| session.validate(&permit, Capability::ImageSend, now))
                },
            )
            .await?;
        Ok(sequence)
    }

    pub async fn report_image_result(&mut self, completed: ImageCompletion) -> Result<()> {
        let policy = &self.policy;
        self.frames
            .write_checked(
                &Frame::ImageResult {
                    sequence: completed.sequence,
                    outcome: completed.outcome,
                },
                |_| {
                    policy.check(|session, now| {
                        session.validate(&completed.permit, Capability::ImageReceive, now)
                    })
                },
            )
            .await
    }

    /// Acknowledge an admitted text that is waiting for a local write decision.
    /// This neither applies the text nor grants a clipboard send direction.
    pub async fn acknowledge_text(&mut self, admitted: &mut AdmittedFrame) -> Result<()> {
        let Frame::Text { sequence, .. } = &admitted.frame else {
            anyhow::bail!("expected a clipboard text frame");
        };
        ensure!(
            !admitted.acknowledged,
            "clipboard frame already acknowledged"
        );
        let policy = &self.policy;
        self.frames
            .write_checked(
                &Frame::TextReceived {
                    sequence: *sequence,
                },
                |_| {
                    policy.check(|clipboard, now| {
                        clipboard.validate(&admitted.permit, Capability::TextReceive, now)
                    })
                },
            )
            .await?;
        admitted.acknowledged = true;
        Ok(())
    }

    /// Send the local write/decline result once, consuming the completion.
    pub async fn report_text_result(&mut self, completed: ClipboardCompletion) -> Result<()> {
        let policy = &self.policy;
        self.frames
            .write_checked(
                &Frame::TextResult {
                    sequence: completed.sequence,
                    outcome: completed.outcome,
                },
                |_| {
                    policy.check(|clipboard, now| {
                        clipboard.validate(&completed.permit, Capability::TextReceive, now)
                    })
                },
            )
            .await
    }
}

impl Receiver {
    /// Supply the local clipboard revision before reading an incoming text frame.
    pub async fn read(&mut self, clipboard_revision: Option<u64>) -> Result<AdmittedFrame> {
        let policy = &self.policy;
        let (frame, permit) = self
            .frames
            .read_checked(|header| {
                ensure!(
                    header.kind() == Kind::Text,
                    "use the file or clipboard response reader"
                );
                let capability = inbound(header.kind());
                ensure!(
                    capability != Capability::TextReceive || clipboard_revision.is_some(),
                    "clipboard revision missing"
                );
                policy.check(|session, now| session.authorize(capability, now))
            })
            .await?;
        Ok(AdmittedFrame {
            frame,
            permit,
            clipboard_revision,
            acknowledged: false,
        })
    }

    /// Check size and direction before allocating. Normalize off the network
    /// runtime, then recheck the original permission before exposing an image.
    pub async fn read_image(&mut self, revision: u64) -> Result<AdmittedImage> {
        let policy = &self.policy;
        let (frame, permit) = self
            .frames
            .read_checked(|header| {
                ensure!(header.kind() == Kind::Image, "expected a clipboard image");
                policy.check(|session, now| session.authorize(Capability::ImageReceive, now))
            })
            .await?;
        let Frame::Image { sequence, png } = frame else {
            anyhow::bail!("expected a clipboard image");
        };
        if sequence <= self.image_watermark {
            policy.close();
            anyhow::bail!("clipboard image sequence replayed");
        }
        self.image_watermark = sequence;
        let image = tokio::task::spawn_blocking(move || Image::normalize(&png)).await?;
        let Ok(image) = image else {
            policy.close();
            anyhow::bail!("clipboard image validation failed");
        };
        policy.check(|session, now| session.validate(&permit, Capability::ImageReceive, now))?;
        Ok(AdmittedImage {
            sequence,
            permit,
            revision,
            image,
        })
    }

    /// Read only the file kinds valid in the current transfer phase. Admission
    /// happens before a body allocation. The owner must still check the ID and
    /// its per-offer acceptance before disk I/O.
    pub async fn read_file(
        &mut self,
        expected: &[Kind],
        timeout: Duration,
    ) -> Result<AdmittedFrame> {
        let policy = &self.policy;
        let (frame, permit) = self
            .frames
            .read_checked_before(timeout, |header| {
                let kind = header.kind();
                ensure!(
                    is_file(kind) && expected.contains(&kind),
                    "unexpected frame for the current file transfer phase"
                );
                policy.check(|session, now| session.authorize(inbound(kind), now))
            })
            .await?;
        Ok(AdmittedFrame {
            frame,
            permit,
            clipboard_revision: None,
            acknowledged: false,
        })
    }

    /// Wait for the exact sent sequence. A receipt only means the peer
    /// admitted the text; it does not prove an OS clipboard write.
    pub async fn wait_text_receipt(&mut self, sequence: u64) -> Result<()> {
        ensure!(sequence != 0, "invalid clipboard receipt sequence");
        let frame = self.read_clipboard_response(Kind::TextReceived).await?;
        if !matches!(frame, Frame::TextReceived { sequence: received } if received == sequence) {
            self.policy.close();
            anyhow::bail!("clipboard receipt sequence differs");
        }
        Ok(())
    }

    pub async fn wait_text_result(&mut self, sequence: u64) -> Result<TextOutcome> {
        ensure!(sequence != 0, "invalid clipboard result sequence");
        let frame = self.read_clipboard_response(Kind::TextResult).await?;
        match frame {
            Frame::TextResult {
                sequence: received,
                outcome,
            } if received == sequence => Ok(outcome),
            _ => {
                self.policy.close();
                anyhow::bail!("clipboard result sequence differs");
            }
        }
    }

    pub async fn wait_image_result(&mut self, sequence: u64) -> Result<ClipboardOutcome> {
        ensure!(sequence != 0, "invalid image result sequence");
        let frame = self.read_clipboard_response(Kind::ImageResult).await?;
        match frame {
            Frame::ImageResult {
                sequence: received,
                outcome,
            } if received == sequence => Ok(outcome),
            _ => {
                self.policy.close();
                anyhow::bail!("clipboard image result sequence differs");
            }
        }
    }

    async fn read_clipboard_response(&mut self, expected: Kind) -> Result<Frame> {
        let policy = &self.policy;
        // A fixed-size import result may wait for a person's local decision.
        // All other frames, including the initial receipt, keep the short limit.
        let timeout = if matches!(expected, Kind::TextResult | Kind::ImageResult) {
            Duration::from_secs(45)
        } else {
            Duration::from_secs(5)
        };
        let capability = inbound(expected);
        let (frame, permit) = self
            .frames
            .read_checked_before(timeout, |header| {
                ensure!(header.kind() == expected, "unexpected clipboard response");
                policy.check(|clipboard, now| clipboard.authorize(capability, now))
            })
            .await?;
        policy.check(|clipboard, now| clipboard.validate(&permit, capability, now))?;
        Ok(frame)
    }
}

fn clipboard_outcome(result: WriteResult) -> ClipboardOutcome {
    match result {
        WriteResult::Submitted => ClipboardOutcome::Submitted,
        WriteResult::Changed => ClipboardOutcome::Changed,
        WriteResult::Failed => ClipboardOutcome::Failed,
        WriteResult::UnknownOutcome => ClipboardOutcome::Unknown,
    }
}

fn is_file(kind: Kind) -> bool {
    matches!(
        kind,
        Kind::Offer | Kind::Chunk | Kind::Cancel | Kind::FileAccepted | Kind::FileResult
    )
}

fn outbound(kind: Kind) -> Capability {
    match kind {
        Kind::Text => Capability::TextSend,
        Kind::Image => Capability::ImageSend,
        Kind::ImageResult => Capability::ImageReceive,
        Kind::TextReceived | Kind::TextResult => Capability::TextReceive,
        Kind::FileAccepted | Kind::FileResult => Capability::FileReceive,
        Kind::Offer | Kind::Chunk | Kind::Cancel => Capability::FileSend,
    }
}

fn inbound(kind: Kind) -> Capability {
    match kind {
        Kind::Text => Capability::TextReceive,
        Kind::Image => Capability::ImageReceive,
        Kind::ImageResult => Capability::ImageSend,
        Kind::TextReceived | Kind::TextResult => Capability::TextSend,
        Kind::FileAccepted | Kind::FileResult => Capability::FileSend,
        Kind::Offer | Kind::Chunk | Kind::Cancel => Capability::FileReceive,
    }
}
