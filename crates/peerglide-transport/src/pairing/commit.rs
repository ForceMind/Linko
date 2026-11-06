//! Pairing-v2 commit facts on the SAME authenticated TLS connection. No disk,
//! input grants, or claims of distributed atomicity. Parent approval and local
//! durable-save claims belong to the separate owned IPC protocol.
use super::{wire, Cancellation, Lifetime, PairingSession};
use anyhow::{Context, Result};
use tokio::{io::{AsyncRead, AsyncReadExt, WriteHalf}, net::TcpStream, sync::mpsc, task::JoinHandle};
use tokio_rustls::TlsStream;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Message { Prepared, Saved, Closed }
const CLIENT_PREPARED: u8 = 8;
const SERVER_PREPARED: u8 = 9;
const CLIENT_SAVED: u8 = 10;
const SERVER_SAVED: u8 = 11;

pub struct Channel {
    writer: WriteHalf<TlsStream<TcpStream>>,
    incoming: mpsc::Receiver<Result<Message>>,
    reader: JoinHandle<()>,
    lifetime: Lifetime,
    cancellation: Cancellation,
    context: Vec<u8>,
    id: [u8; 32],
    client: bool,
    sent_prepared: bool,
    sent_saved: bool,
    peer_prepared: bool,
}
impl Channel {
    pub(super) fn new(mut session: PairingSession) -> Result<Self> {
        session.peer_certificate()?;
        let runtime = tokio::runtime::Handle::try_current().context("pairing runtime unavailable")?;
        let client = matches!(session.stream.as_ref(), Some(TlsStream::Client(_)));
        let context = session.binding.context.clone();
        let id = session.context_id()?;
        let lifetime = session.lifetime.clone();
        let cancellation = session.cancellation.clone();
        let stream = session.stream.take().context("pairing stream unavailable")?;
        let (reader, writer) = tokio::io::split(stream);
        let (out, incoming) = mpsc::channel(2);
        let binding = context.clone();
        let deadline = lifetime.deadline;
        // One persistent frame reader: cancelling recv() cannot discard half a
        // consumed length/payload and corrupt the next frame boundary.
        let reader = runtime.spawn(async move {
            let _ = tokio::time::timeout_at(deadline, read_loop(reader, out, binding, client)).await;
        });
        Ok(Self { writer, incoming, reader, lifetime, cancellation, context, id, client,
            sent_prepared: false, sent_saved: false, peer_prepared: false })
    }
    pub fn context(&self) -> [u8; 32] { self.id }
    pub fn check(&self) -> Result<()> { self.lifetime.check() }
    /// Drain only already authenticated/parsed facts after an output error.
    /// This is not renewed liveness or permission to start a filesystem write.
    pub fn buffered_fact(&mut self) -> Option<Result<Message>> {
        self.incoming.try_recv().ok()
    }
    pub async fn receive(&mut self) -> Result<Message> {
        let result = self.lifetime.run(async { self.incoming.recv().await.context("pairing reader stopped")? }).await;
        match result {
            Ok(Message::Prepared) => { self.peer_prepared = true; Ok(Message::Prepared) }
            Ok(Message::Saved) if !self.sent_prepared => {
                self.cancellation.cancel(); anyhow::bail!("peer saved before local prepare")
            }
            Err(error) => { self.cancellation.cancel(); Err(error) }
            other => other,
        }
    }
    pub async fn prepare(&mut self) -> Result<()> {
        self.check()?;
        anyhow::ensure!(!self.sent_prepared, "duplicate local pairing prepare");
        self.sent_prepared = true;
        self.send(if self.client { CLIENT_PREPARED } else { SERVER_PREPARED }).await
    }
    pub async fn saved(&mut self) -> Result<()> {
        self.check()?;
        anyhow::ensure!(self.sent_prepared && self.peer_prepared && !self.sent_saved, "invalid local pairing saved order");
        self.sent_saved = true;
        self.send(if self.client { CLIENT_SAVED } else { SERVER_SAVED }).await
    }
    async fn send(&mut self, tag: u8) -> Result<()> {
        let mut guard = WriteGuard { cancellation: self.cancellation.clone(), completed: false };
        let result = self.lifetime.run(wire::write(&mut self.writer, tag, &self.context)).await;
        guard.completed = result.is_ok();
        result
    }
}
impl Drop for Channel {
    fn drop(&mut self) { self.cancellation.cancel(); self.reader.abort(); }
}
struct WriteGuard { cancellation: Cancellation, completed: bool }
impl Drop for WriteGuard {
    fn drop(&mut self) { if !self.completed { self.cancellation.cancel(); } }
}
async fn read_loop(
    mut stream: impl AsyncRead + Unpin, out: mpsc::Sender<Result<Message>>,
    context: Vec<u8>, client: bool,
) {
    let mut prepared = false;
    let mut saved = false;
    loop {
        let result = read_one(&mut stream, &context, client).await.and_then(|message| {
            match message {
                Message::Prepared => { anyhow::ensure!(!prepared, "duplicate peer prepare"); prepared = true; }
                Message::Saved => { anyhow::ensure!(prepared && !saved, "invalid peer saved order"); saved = true; }
                Message::Closed => (),
            }
            Ok(message)
        });
        let terminal = matches!(&result, Err(_) | Ok(Message::Closed));
        if out.send(result).await.is_err() || terminal { break; }
    }
}
async fn read_one(stream: &mut (impl AsyncRead + Unpin), context: &[u8], client: bool) -> Result<Message> {
    let mut first = [0; 1];
    match stream.read(&mut first).await {
        Ok(0) => return Ok(Message::Closed),
        Ok(_) => (),
        Err(error) if matches!(error.kind(), std::io::ErrorKind::UnexpectedEof
            | std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted
            | std::io::ErrorKind::BrokenPipe) => return Ok(Message::Closed),
        Err(error) => return Err(error.into()),
    }
    let mut rest = [0; 3];
    stream.read_exact(&mut rest).await?;
    let size = u32::from_be_bytes([first[0], rest[0], rest[1], rest[2]]) as usize;
    anyhow::ensure!(size == context.len() + 1, "invalid pairing commit frame size");
    let tag = stream.read_u8().await?;
    let message = match (client, tag) {
        (true, SERVER_PREPARED) | (false, CLIENT_PREPARED) => Message::Prepared,
        (true, SERVER_SAVED) | (false, CLIENT_SAVED) => Message::Saved,
        _ => anyhow::bail!("invalid pairing commit role or phase"),
    };
    let mut bytes = vec![0; context.len()];
    stream.read_exact(&mut bytes).await?;
    anyhow::ensure!(bytes == context, "pairing commit context mismatch");
    Ok(message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;
    #[tokio::test]
    async fn cancelling_a_receiver_keeps_the_partially_read_frame() -> Result<()> {
        let (mut writer, reader) = tokio::io::duplex(1024);
        let context = b"parser fixture only, not authentication".to_vec();
        let (tx, mut rx) = mpsc::channel(2);
        let task = tokio::spawn(read_loop(reader,tx,context.clone(),true));
        let mut bytes = ((context.len()+1) as u32).to_be_bytes().to_vec();
        bytes.push(SERVER_PREPARED); bytes.extend_from_slice(&context);
        writer.write_all(&bytes[..2]).await?;
        assert!(tokio::time::timeout(std::time::Duration::from_millis(5),rx.recv()).await.is_err());
        writer.write_all(&bytes[2..]).await?;
        assert_eq!(rx.recv().await.context("missing frame")??,Message::Prepared);
        wire::write(&mut writer,SERVER_PREPARED,&context).await?;
        assert!(rx.recv().await.context("missing duplicate failure")?.is_err());
        task.await?;
        Ok(())
    }
    #[tokio::test]
    async fn truncated_header_wrong_role_and_changed_context_are_protocol_failures() -> Result<()> {
        for case in 0..3 {
            let (mut writer,mut reader) = tokio::io::duplex(1024);
            let context=b"synthetic parser context";
            if case==0 { writer.write_all(&[0]).await?; }
            else {
                let payload=if case==2 { b"different parser context".as_slice() } else { context.as_slice() };
                wire::write(&mut writer,if case==1 { CLIENT_PREPARED } else { SERVER_PREPARED },payload).await?;
            }
            writer.shutdown().await?;
            assert!(read_one(&mut reader,context,true).await.is_err());
        }
        Ok(())
    }
}
