//! Finite, separately admitted PGD1 frame I/O on an already negotiated connection.
use super::data::Capabilities;
use anyhow::{ensure, Context, Result};
use peerglide_transfer::codec::{Frame, Header, Kind, HEADER_BYTES};
use quinn::{Connection, RecvStream, SendStream};
use std::time::Duration;

const FRAME_TIMEOUT: Duration = Duration::from_secs(5);

pub struct FrameSender {
    stream: SendStream,
    connection: Connection,
    capabilities: Capabilities,
    remaining: u64,
}

pub struct FrameReceiver {
    stream: RecvStream,
    connection: Connection,
    capabilities: Capabilities,
    remaining: u64,
}

fn supported(capabilities: Capabilities, kind: Kind) -> bool {
    match kind {
        Kind::Text | Kind::TextReceived | Kind::TextResult => capabilities.text,
        Kind::Image | Kind::ImageResult => capabilities.images,
        Kind::Offer | Kind::Chunk | Kind::Cancel | Kind::FileAccepted | Kind::FileResult => {
            capabilities.files
        }
    }
}

/// Partial I/O and cancellation cannot leave a reusable misaligned stream.
struct FrameAttempt<'a>(Option<&'a Connection>);

impl FrameAttempt<'_> {
    fn finish(mut self) {
        self.0 = None;
    }
}

impl Drop for FrameAttempt<'_> {
    fn drop(&mut self) {
        if let Some(connection) = self.0 {
            connection.close(1u8.into(), b"data frame interrupted");
        }
    }
}

impl FrameSender {
    pub(super) fn new(
        stream: SendStream,
        connection: Connection,
        capabilities: Capabilities,
        remaining: u64,
    ) -> Self {
        Self {
            stream,
            connection,
            capabilities,
            remaining,
        }
    }

    #[cfg(test)]
    pub(super) fn remaining_bytes(&self) -> u64 {
        self.remaining
    }

    /// The caller checks and consumes fresh outbound consent immediately
    /// before the first network write. An I/O result is never peer acceptance.
    pub async fn write_checked(
        &mut self,
        frame: &Frame,
        admit: impl FnOnce(Kind) -> Result<()>,
    ) -> Result<()> {
        let kind = frame.kind();
        ensure!(
            supported(self.capabilities, kind),
            "unsupported data capability"
        );
        let mut wire = Vec::new();
        frame.write(&mut wire)?;
        let bytes = wire.len() as u64;
        ensure!(bytes <= self.remaining, "data send budget exceeded");
        admit(kind)?;
        let attempt = FrameAttempt(Some(&self.connection));
        tokio::time::timeout(FRAME_TIMEOUT, self.stream.write_all(&wire))
            .await
            .context("data write timed out")??;
        self.remaining -= bytes;
        attempt.finish();
        Ok(())
    }
}

impl FrameReceiver {
    pub(super) fn new(
        stream: RecvStream,
        connection: Connection,
        capabilities: Capabilities,
        remaining: u64,
    ) -> Self {
        Self {
            stream,
            connection,
            capabilities,
            remaining,
        }
    }

    #[cfg(test)]
    pub(super) fn remaining_bytes(&self) -> u64 {
        self.remaining
    }

    /// Admission runs after reading the fixed header and before allocating its
    /// body. Return the caller's ticket so consent can be rechecked at the
    /// eventual OS effect, after a possibly slow read.
    pub async fn read_checked<T>(
        &mut self,
        admit: impl FnOnce(Header) -> Result<T>,
    ) -> Result<(Frame, T)> {
        self.read_checked_before(FRAME_TIMEOUT, admit).await
    }

    pub(super) async fn read_checked_before<T>(
        &mut self,
        timeout: Duration,
        admit: impl FnOnce(Header) -> Result<T>,
    ) -> Result<(Frame, T)> {
        let attempt = FrameAttempt(Some(&self.connection));
        let stream = &mut self.stream;
        let remaining = &mut self.remaining;
        let capabilities = self.capabilities;
        let result = tokio::time::timeout(timeout, async {
            let mut bytes = [0; HEADER_BYTES];
            stream.read_exact(&mut bytes).await?;
            let header = Header::parse(bytes)?;
            ensure!(
                supported(capabilities, header.kind()),
                "unsupported data capability"
            );
            let bytes = (HEADER_BYTES + header.body_bytes()) as u64;
            ensure!(bytes <= *remaining, "data receive budget exceeded");
            let ticket = admit(header)?;
            let mut body = vec![0; header.body_bytes()];
            stream.read_exact(&mut body).await?;
            let frame = Frame::decode_body(header, &body)?;
            *remaining -= bytes;
            Ok::<_, anyhow::Error>((frame, ticket))
        })
        .await
        .context("data read timed out")??;
        attempt.finish();
        Ok(result)
    }
}
