//! Direction consent owned locally and bound to one authenticated data connection.
use super::{
    data::Capabilities,
    data_frames::{FrameReceiver, FrameSender},
};
use anyhow::{ensure, Result};
use peerglide_transfer::{
    codec::{Frame, Kind},
    data_policy::{Capability, DataSession, Denied, Permissions, Permit},
};
use quinn::Connection;
use rand::RngCore;
use std::{
    sync::{Arc, Mutex},
    time::Instant,
};

struct Policy {
    connection: Connection,
    session: Mutex<DataSession>,
    started: Instant,
}

impl Policy {
    fn check<T>(
        &self,
        operation: impl FnOnce(&mut DataSession, u64) -> Result<T, Denied>,
    ) -> Result<T> {
        let mut session = self
            .session
            .lock()
            .map_err(|_| anyhow::anyhow!("data consent unavailable"))?;
        ensure!(
            self.connection.close_reason().is_none(),
            "data connection closed"
        );
        let now = u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        operation(&mut session, now)
            .map_err(|error| anyhow::anyhow!("data consent denied ({error:?})"))
    }

    fn close(&self) {
        if let Ok(mut session) = self.session.lock() {
            session.close();
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
}

pub struct Receiver {
    policy: Arc<Policy>,
    frames: FrameReceiver,
}

/// A frame admitted at the network header. Its ticket must still be valid at use.
pub struct AdmittedFrame {
    frame: Frame,
    permit: Permit,
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
        session: Mutex::new(DataSession::new(peer_digest, nonce, 0)),
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
        },
        Receiver {
            policy,
            frames: receiver,
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
                && (self.capabilities.files || !permissions.file_send && !permissions.file_receive),
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

    /// Recheck on the local owner immediately before an OS or disk side effect.
    pub fn apply_received<T>(
        &mut self,
        admitted: AdmittedFrame,
        apply: impl FnOnce(Frame) -> Result<T>,
    ) -> Result<T> {
        let capability = inbound(admitted.frame.kind());
        self.policy
            .check(|session, now| session.validate(&admitted.permit, capability, now))?;
        apply(admitted.frame)
    }
}

impl Drop for Consent {
    fn drop(&mut self) {
        self.revoke();
    }
}

impl Sender {
    pub async fn write(&mut self, frame: &Frame) -> Result<()> {
        let policy = &self.policy;
        self.frames
            .write_checked(frame, |kind| {
                policy.check(|session, now| session.authorize(outbound(kind), now))?;
                Ok(())
            })
            .await
    }
}

impl Receiver {
    pub async fn read(&mut self) -> Result<AdmittedFrame> {
        let policy = &self.policy;
        let (frame, permit) = self
            .frames
            .read_checked(|header| {
                policy.check(|session, now| session.authorize(inbound(header.kind()), now))
            })
            .await?;
        Ok(AdmittedFrame { frame, permit })
    }
}

fn outbound(kind: Kind) -> Capability {
    match kind {
        Kind::Text => Capability::TextSend,
        Kind::Offer | Kind::Chunk | Kind::Cancel => Capability::FileSend,
    }
}

fn inbound(kind: Kind) -> Capability {
    match kind {
        Kind::Text => Capability::TextReceive,
        Kind::Offer | Kind::Chunk | Kind::Cancel => Capability::FileReceive,
    }
}
