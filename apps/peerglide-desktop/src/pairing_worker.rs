//! Private non-native pairing child. No filesystem writes, enrollment or input
//! capabilities. Only its supervisor's pipes carry the code and public proposal.
use anyhow::Result;
use peerglide_protocol::pairing_worker::{
    self as ipc, Failure, PairingCommand, PairingEvent, State, StopReason,
};
use peerglide_transport::{
    pairing::{self, CodeEntry, Offer, PairingSession},
    Identity,
};
use std::{
    io::{self, IsTerminal, Write},
    net::SocketAddr,
    path::PathBuf,
    sync::{mpsc, Arc},
    time::Duration,
};
use tokio::{
    net::TcpListener,
    sync::{oneshot, watch},
    time::Instant,
};

pub enum Mode {
    Offer(SocketAddr),
    Connect(SocketAddr),
}

struct PendingOutput {
    event: PairingEvent,
    receipt: oneshot::Sender<bool>,
}
struct Output(mpsc::SyncSender<PendingOutput>);
impl Output {
    fn spawn() -> Result<Self> {
        Self::spawn_with(io::stdout())
    }
    fn spawn_with(mut writer: impl Write + Send + 'static) -> Result<Self> {
        let (tx, rx) = mpsc::sync_channel::<PendingOutput>(1);
        std::thread::Builder::new()
            .name("pairing-output".into())
            .spawn(move || {
                while let Ok(pending) = rx.recv() {
                    let success = pending.event.line().is_some_and(|mut line| {
                        line.push('\n');
                        writer
                            .write_all(line.as_bytes())
                            .and_then(|_| writer.flush())
                            .is_ok()
                    });
                    let _ = pending.receipt.send(success);
                    if !success {
                        break;
                    }
                }
            })?;
        Ok(Self(tx))
    }
    async fn emit(&self, event: State) -> std::result::Result<(), Failure> {
        let (receipt, ack) = oneshot::channel();
        self.0
            .try_send(PendingOutput {
                event: PairingEvent::new(event),
                receipt,
            })
            .map_err(|_| Failure::Internal)?;
        // A GUI that stops draining stdout cannot stall the async lifecycle.
        let written = tokio::time::timeout(Duration::from_secs(2), ack)
            .await
            .map_err(|_| Failure::Internal)?
            .map_err(|_| Failure::Internal)?;
        if written {
            Ok(())
        } else {
            Err(Failure::Internal)
        }
    }
}

#[derive(Clone)]
enum Input {
    Waiting,
    Code(Arc<PairingCommand>),
    Stop,
    Invalid,
    InternalFault,
}

fn guard_reader(tx: &watch::Sender<Input>, read: impl FnOnce()) {
    // The reader is discarded after unwind; no poisoned IO state is reused.
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(read)).is_err() {
        tx.send_replace(Input::InternalFault);
    }
}
struct Inputs {
    rx: watch::Receiver<Input>,
    tx: watch::Sender<Input>,
}
impl Inputs {
    fn spawn() -> Result<Self> {
        let (tx, rx) = watch::channel(Input::Waiting);
        let reader_tx = tx.clone();
        // std thread, not spawn_blocking: a still-open stdin must not keep the
        // Tokio runtime alive after an expired/failed handshake. No input hooks.
        std::thread::Builder::new()
            .name("pairing-input".into())
            .spawn(move || {
                guard_reader(&reader_tx, || {
                    let stdin = io::stdin();
                    let mut reader = stdin.lock();
                    let mut code_seen = false;
                    loop {
                        match ipc::read_command(&mut reader) {
                            Ok(Some(command)) if command.entered_code().is_some() && !code_seen => {
                                code_seen = true;
                                reader_tx.send_replace(Input::Code(Arc::new(command)));
                            }
                            Ok(Some(command)) if command.entered_code().is_none() => {
                                reader_tx.send_replace(Input::Stop);
                                break;
                            }
                            Ok(None) => {
                                reader_tx.send_replace(Input::Stop);
                                break;
                            }
                            Ok(Some(_)) => {
                                reader_tx.send_replace(Input::Invalid);
                                break;
                            }
                            Err(error) if error.kind() == io::ErrorKind::InvalidData => {
                                reader_tx.send_replace(Input::Invalid);
                                break;
                            }
                            Err(_) => {
                                reader_tx.send_replace(Input::InternalFault);
                                break;
                            }
                        }
                    }
                });
            })?;
        Ok(Self { rx, tx })
    }
    async fn code(&mut self) -> std::result::Result<Option<Arc<PairingCommand>>, Failure> {
        loop {
            let state = self.rx.borrow_and_update().clone();
            match state {
                Input::Code(code) => {
                    // Clear the stored code without overwriting a racing Stop.
                    self.tx.send_if_modified(|state| {
                        if matches!(state, Input::Code(current) if Arc::ptr_eq(current, &code)) {
                            *state = Input::Waiting;
                            true
                        } else {
                            false
                        }
                    });
                    return Ok(Some(code));
                }
                Input::Stop => return Ok(None),
                Input::Invalid => return Err(Failure::InvalidCommand),
                Input::InternalFault => return Err(Failure::Internal),
                Input::Waiting => (),
            }
            self.rx
                .changed()
                .await
                .map_err(|_| Failure::InvalidCommand)?;
        }
    }
}
async fn stop(
    mut rx: watch::Receiver<Input>,
    offering: bool,
) -> std::result::Result<StopReason, Failure> {
    loop {
        match &*rx.borrow_and_update() {
            Input::Stop => return Ok(StopReason::Cancelled),
            Input::Invalid => return Err(Failure::InvalidCommand),
            Input::InternalFault => return Err(Failure::Internal),
            Input::Code(_) if offering => return Err(Failure::InvalidCommand),
            _ => (),
        }
        if rx.changed().await.is_err() {
            return Ok(StopReason::Cancelled);
        }
    }
}

pub async fn run(identity: PathBuf, mode: Mode, managed: bool) -> Result<()> {
    anyhow::ensure!(
        managed && !io::stdin().is_terminal() && !io::stdout().is_terminal(),
        "配对工作进程只接受本地管理管道"
    );
    // This dedicated non-native child reports panics as Failed/Internal + exit1.
    // Raw panic hooks must not print secret arguments or block on a stalled
    // stderr pipe before the supervisor can observe the terminal failure.
    std::panic::set_hook(Box::new(|_| {}));
    let deadline = Instant::now() + pairing::OFFER_LIFETIME;
    let output = Arc::new(Output::spawn()?);
    let outcome = match Identity::load(&identity) {
        Err(_) => Err(Failure::Identity),
        Ok(identity) => match Inputs::spawn() {
            Err(_) => Err(Failure::Internal),
            Ok(input) => {
                let writer = output.clone();
                supervise(async move {
                    execute(Arc::new(identity), mode, input, writer, deadline).await
                })
                .await
            }
        },
    };
    match outcome {
        Ok(reason) => {
            output
                .emit(State::Stopped { reason })
                .await
                .map_err(|_| anyhow::anyhow!("配对状态通道已关闭"))?;
            Ok(())
        }
        Err(reason) => {
            let _ = output.emit(State::Failed { reason }).await;
            anyhow::bail!("配对未完成")
        }
    }
}

async fn supervise(
    future: impl std::future::Future<Output = std::result::Result<StopReason, Failure>> + Send + 'static,
) -> std::result::Result<StopReason, Failure> {
    tokio::spawn(future).await.unwrap_or(Err(Failure::Internal))
}

async fn execute(
    identity: Arc<Identity>,
    mode: Mode,
    mut input: Inputs,
    output: Arc<Output>,
    deadline: Instant,
) -> std::result::Result<StopReason, Failure> {
    let control = stop(input.rx.clone(), matches!(mode, Mode::Offer(_)));
    let final_control = input.rx.clone();
    let work = async {
        let mut session = match mode {
            Mode::Offer(address) => {
                let listener = TcpListener::bind(address)
                    .await
                    .map_err(|_| Failure::Listen)?;
                let mut offer = Offer::new(&identity).map_err(|_| Failure::Internal)?;
                output
                    .emit(State::Listening {
                        code: offer.code().map_err(|_| Failure::Internal)?.to_owned(),
                        address: listener.local_addr().map_err(|_| Failure::Listen)?,
                    })
                    .await?;
                loop {
                    let (socket, _) = listener.accept().await.map_err(|_| Failure::Listen)?;
                    output.emit(State::Authenticating {}).await?;
                    match offer.accept_connected(socket, identity.clone()).await {
                        Ok(session) => break session,
                        Err(error)
                            if error.downcast_ref::<pairing::ComputationFailed>().is_some() =>
                        {
                            return Err(Failure::Internal);
                        }
                        Err(_) => {
                            let remaining = offer.attempts_remaining();
                            output.emit(State::AttemptRejected { remaining }).await?;
                            if remaining == 0 {
                                return Ok(StopReason::AttemptsExhausted);
                            }
                        }
                    }
                }
            }
            Mode::Connect(address) => {
                let Some(command) = input.code().await? else {
                    return Ok(StopReason::Cancelled);
                };
                let code = command
                    .entered_code()
                    .ok_or(Failure::InvalidCommand)?
                    .to_owned();
                let entry = CodeEntry::new(code).map_err(|_| Failure::InvalidCommand)?;
                drop(command);
                output.emit(State::Authenticating {}).await?;
                entry
                    .connect(address, identity.clone())
                    .await
                    .map_err(|error| {
                        if error.downcast_ref::<pairing::ComputationFailed>().is_some() {
                            Failure::Internal
                        } else {
                            Failure::Authentication
                        }
                    })?
            }
        };
        authenticated(&mut session, &identity, &output).await
    };
    let result = tokio::select! {
        biased;
        result = control => result,
        _ = tokio::time::sleep_until(deadline) => Ok(StopReason::Expired),
        result = work => result,
    };
    // select's bias orders polls, not updates made by the stdin thread between
    // polls. Reconcile the final control snapshot before publishing a result.
    // Protocol/internal failures remain failures even if Stop races them.
    if matches!(result, Err(Failure::Protocol | Failure::Internal)) {
        return result;
    }
    let state = final_control.borrow().clone();
    match state {
        Input::Stop => Ok(StopReason::Cancelled),
        Input::Invalid => Err(Failure::InvalidCommand),
        Input::InternalFault => Err(Failure::Internal),
        _ => result,
    }
}
async fn authenticated(
    session: &mut PairingSession,
    identity: &Identity,
    output: &Output,
) -> std::result::Result<StopReason, Failure> {
    output
        .emit(State::Authenticated {
            certificate: session
                .peer_certificate()
                .map_err(|_| Failure::Protocol)?
                .to_vec(),
            context: session.context_id().map_err(|_| Failure::Protocol)?,
            local_fingerprint: identity.fingerprint(),
        })
        .await?;
    // No commit command exists in this batch. Do not imply persistence or grant.
    peer_end(session.wait_peer_end().await)
}

fn peer_end(result: Result<pairing::PeerEnd>) -> std::result::Result<StopReason, Failure> {
    match result {
        Ok(pairing::PeerEnd::Disconnected) => Ok(StopReason::PeerDisconnected),
        _ => Err(Failure::Protocol),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn reader_panic_is_terminal_even_while_other_senders_stay_alive() -> Result<()> {
        let (tx, rx) = watch::channel(Input::Waiting);
        let retained_sender = tx.clone();
        let reader = std::thread::spawn(move || {
            guard_reader(&tx, || panic!("synthetic input reader failure"));
        });
        let result = tokio::time::timeout(Duration::from_secs(1), stop(rx, false)).await?;
        assert!(reader.join().is_ok());
        assert!(matches!(result, Err(Failure::Internal)));
        assert!(matches!(&*retained_sender.borrow(), Input::InternalFault));
        Ok(())
    }
    struct BlockedWriter {
        release: mpsc::Receiver<()>,
        done: Option<oneshot::Sender<()>>,
        started: Option<oneshot::Sender<()>>,
    }
    impl Write for BlockedWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if let Some(started) = self.started.take() {
                let _ = started.send(());
            }
            self.release
                .recv_timeout(Duration::from_secs(5))
                .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "test writer timeout"))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    impl Drop for BlockedWriter {
        fn drop(&mut self) {
            if let Some(done) = self.done.take() {
                let _ = done.send(());
            }
        }
    }
    #[tokio::test]
    async fn a_non_draining_stdout_cannot_stall_the_async_stop_lifecycle() -> Result<()> {
        let (release, rx) = mpsc::channel();
        let (done, joined) = oneshot::channel();
        let output = Output::spawn_with(BlockedWriter {
            release: rx,
            done: Some(done),
            started: None,
        })?;
        let result = output.emit(State::Authenticating {}).await;
        let _ = release.send(());
        drop(output);
        tokio::time::timeout(Duration::from_secs(1), joined).await??;
        assert!(matches!(result, Err(Failure::Internal)));
        Ok(())
    }
    #[tokio::test]
    async fn stop_preempts_a_really_blocked_output_write() -> Result<()> {
        let (release, rx) = mpsc::channel();
        let (done, joined) = oneshot::channel();
        let (started, writing) = oneshot::channel();
        let output = Arc::new(Output::spawn_with(BlockedWriter {
            release: rx,
            done: Some(done),
            started: Some(started),
        })?);
        let (tx, input) = watch::channel(Input::Waiting);
        let mut task = tokio::spawn(execute(
            Arc::new(Identity::generate()?),
            Mode::Offer("127.0.0.1:0".parse()?),
            Inputs {
                tx: tx.clone(),
                rx: input,
            },
            output.clone(),
            Instant::now() + pairing::OFFER_LIFETIME,
        ));
        writing.await?;
        tx.send_replace(Input::Stop);
        let early = tokio::time::timeout(Duration::from_secs(1), &mut task).await;
        let timely = early.is_ok();
        let _ = release.send(());
        let outcome = match early {
            Ok(result) => result?,
            Err(_) => task.await?,
        };
        drop(output);
        tokio::time::timeout(Duration::from_secs(1), joined).await??;
        assert!(timely && matches!(outcome, Ok(StopReason::Cancelled)));
        Ok(())
    }
    struct FailedWriter(bool);
    impl Write for FailedWriter {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            assert!(!self.0, "synthetic output writer panic");
            Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "test closed output",
            ))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    #[tokio::test]
    async fn writer_failure_and_unexpected_peer_data_cannot_report_success() -> Result<()> {
        for panic in [false, true] {
            let output = Output::spawn_with(FailedWriter(panic))?;
            assert!(matches!(
                output.emit(State::Authenticating {}).await,
                Err(Failure::Internal)
            ));
        }
        assert!(matches!(
            peer_end(Ok(pairing::PeerEnd::UnexpectedData)),
            Err(Failure::Protocol)
        ));
        assert!(matches!(
            peer_end(Err(anyhow::anyhow!("test malformed TLS"))),
            Err(Failure::Protocol)
        ));
        Ok(())
    }
    #[tokio::test]
    async fn absolute_worker_expiry_and_task_panic_are_terminal() -> Result<()> {
        let (tx, rx) = watch::channel(Input::Waiting);
        let output = Arc::new(Output::spawn_with(io::sink())?);
        let result = execute(
            Arc::new(Identity::generate()?),
            Mode::Offer("127.0.0.1:0".parse()?),
            Inputs { tx, rx },
            output,
            Instant::now() + Duration::from_millis(10),
        )
        .await;
        assert!(matches!(result, Ok(StopReason::Expired)));
        let result = supervise(async { panic!("synthetic pairing task failure") }).await;
        assert!(matches!(result, Err(Failure::Internal)));
        Ok(())
    }
}
