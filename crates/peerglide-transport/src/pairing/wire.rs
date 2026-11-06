use super::{Binding, MAX_FRAME};
use crate::{validate_certificate, MAX_CERTIFICATE_BYTES};
use anyhow::{Context, Result};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub(super) const CLIENT_HELLO: u8 = 1;
pub(super) const SERVER_HELLO: u8 = 2;
pub(super) const KE1: u8 = 3;
pub(super) const KE2: u8 = 4;
pub(super) const KE3: u8 = 5;
pub(super) const CLIENT_READY: u8 = 6;
pub(super) const SERVER_READY: u8 = 7;
const MAGIC: &[u8; 8] = b"PGPAIR02";

// No buffered reader: bytes following KE3 belong to the TLS engine, even when
// a TLS ClientHello is coalesced into the same underlying TCP receive.
pub(super) async fn read(
    stream: &mut (impl AsyncRead + Unpin),
    kind: u8,
    limit: usize,
) -> Result<Vec<u8>> {
    let size = stream.read_u32().await? as usize;
    anyhow::ensure!(
        size > 1 && size <= limit.min(MAX_FRAME) + 1,
        "invalid pairing frame size"
    );
    anyhow::ensure!(stream.read_u8().await? == kind, "unexpected pairing frame");
    let mut bytes = vec![0; size - 1];
    stream.read_exact(&mut bytes).await?;
    Ok(bytes)
}
pub(super) async fn write(
    stream: &mut (impl AsyncWrite + Unpin),
    kind: u8,
    bytes: &[u8],
) -> Result<()> {
    anyhow::ensure!(
        !bytes.is_empty() && bytes.len() <= MAX_FRAME,
        "invalid pairing frame size"
    );
    stream.write_u32((bytes.len() + 1) as u32).await?;
    stream.write_u8(kind).await?;
    stream.write_all(bytes).await?;
    stream.flush().await?;
    Ok(())
}
pub(super) fn hello(id: &[u8; 32], cert: &[u8]) -> Result<Vec<u8>> {
    validate_certificate(cert)?;
    let mut bytes = Vec::with_capacity(44 + cert.len());
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(id);
    bytes.extend_from_slice(&(cert.len() as u32).to_be_bytes());
    bytes.extend_from_slice(cert);
    Ok(bytes)
}
pub(super) fn parse_hello(bytes: &[u8]) -> Result<([u8; 32], Vec<u8>)> {
    anyhow::ensure!(
        bytes.len() >= 44 && &bytes[..8] == MAGIC,
        "unsupported pairing version"
    );
    let size = u32::from_be_bytes(bytes[40..44].try_into()?) as usize;
    anyhow::ensure!(
        size > 0 && size <= MAX_CERTIFICATE_BYTES && bytes.len() == 44 + size,
        "invalid pairing certificate size"
    );
    let id = bytes[8..40]
        .try_into()
        .context("invalid pairing identifier")?;
    let cert = bytes[44..].to_vec();
    validate_certificate(&cert)?;
    Ok((id, cert))
}
pub(super) fn verify_ready(bytes: &[u8], binding: &Binding) -> Result<()> {
    anyhow::ensure!(
        bytes == binding.context,
        "pairing completion differs from authenticated context"
    );
    Ok(())
}
