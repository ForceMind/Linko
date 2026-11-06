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
enum End {
    Stopped(StopReason),
    Completed([u8; 32]),
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
    Stop,
    Invalid,
    InternalFault,
}

fn guard_reader(tx: &watch::Sender<Input>, read: impl FnOnce()) {
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(read)).is_err() {
        tx.send_replace(Input::InternalFault);
    }
}
struct Inputs {
    rx: watch::Receiver<Input>,
    commands: tokio::sync::mpsc::Receiver<PairingCommand>,
    expected: Arc<std::sync::atomic::AtomicU8>,
}
impl Inputs {
    fn spawn(offering: bool) -> Result<Self> {
        use std::sync::atomic::{AtomicU8, Ordering};
        let (tx, rx) = watch::channel(Input::Waiting);
        let (commands, received) = tokio::sync::mpsc::channel(1);
        let expected = Arc::new(AtomicU8::new(if offering {
            0
        } else {
            ipc::CommandTag::Code as u8
        }));
        let stage = expected.clone();
        std::thread::Builder::new()
            .name("pairing-input".into())
            .spawn(move || {
                guard_reader(&tx, || {
                    let stdin = io::stdin();
                    let mut reader = stdin.lock();
                    loop {
                        match ipc::read_command(&mut reader) {
                            Ok(Some(command)) if command.is_stop() => {
                                tx.send_replace(Input::Stop);
                                break;
                            }
                            Ok(Some(command)) => {
                                let tag = command.tag() as u8;
                                if stage
                                    .compare_exchange(tag, 0, Ordering::AcqRel, Ordering::Acquire)
                                    .is_err()
                                    || commands.try_send(command).is_err()
                                {
                                    tx.send_replace(Input::Invalid);
                                    break;
                                }
                            }
                            Ok(None) => {
                                tx.send_replace(Input::Stop);
                                break;
                            }
                            Err(error) if error.kind() == io::ErrorKind::InvalidData => {
                                tx.send_replace(Input::Invalid);
                                break;
                            }
                            Err(_) => {
                                tx.send_replace(Input::InternalFault);
                                break;
                            }
                        }
                    }
                });
            })?;
        Ok(Self {
            rx,
            commands: received,
            expected,
        })
    }
    fn allow(&self, tag: ipc::CommandTag) -> std::result::Result<(), Failure> {
        use std::sync::atomic::Ordering;
        self.expected
            .compare_exchange(0, tag as u8, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| ())
            .map_err(|_| Failure::Internal)
    }
    async fn next(&mut self) -> std::result::Result<Option<PairingCommand>, Failure> {
        let command = self.commands.recv().await;
        match &*self.rx.borrow() {
            Input::Stop => Ok(None),
            Input::Invalid => Err(Failure::InvalidCommand),
            Input::InternalFault => Err(Failure::Internal),
            Input::Waiting => command.map(Some).ok_or(Failure::Internal),
        }
    }
    #[cfg(test)]
    fn fixture(rx: watch::Receiver<Input>) -> Self {
        let (_, commands) = tokio::sync::mpsc::channel(1);
        Self {
            rx,
            commands,
            expected: Arc::new(std::sync::atomic::AtomicU8::new(0)),
        }
    }
}
async fn stop(mut rx: watch::Receiver<Input>) -> std::result::Result<StopReason, Failure> {
    loop {
        match &*rx.borrow_and_update() {
            Input::Stop => return Ok(StopReason::Cancelled),
            Input::Invalid => return Err(Failure::InvalidCommand),
            Input::InternalFault => return Err(Failure::Internal),
            Input::Waiting => (),
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
        Ok(identity) => match Inputs::spawn(matches!(mode, Mode::Offer(_))) {
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
        Ok(End::Stopped(reason)) => {
            output
                .emit(State::Stopped { reason })
                .await
                .map_err(|_| anyhow::anyhow!("配对状态通道已关闭"))?;
            Ok(())
        }
        Ok(End::Completed(context)) => {
            output
                .emit(State::Completed { context })
                .await
                .map_err(|_| anyhow::anyhow!("配对完成回执通道已关闭"))?;
            Ok(())
        }
        Err(reason) => {
            let _ = output.emit(State::Failed { reason }).await;
            anyhow::bail!("配对未完成")
        }
    }
}

async fn supervise(
    future: impl std::future::Future<Output = std::result::Result<End, Failure>> + Send + 'static,
) -> std::result::Result<End, Failure> {
    tokio::spawn(future).await.unwrap_or(Err(Failure::Internal))
}

async fn execute(
    identity: Arc<Identity>,
    mode: Mode,
    mut input: Inputs,
    output: Arc<Output>,
    deadline: Instant,
) -> std::result::Result<End, Failure> {
    let control = stop(input.rx.clone());
    let final_control = input.rx.clone();
    let work = async {
        let session = match mode {
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
                                return Ok(End::Stopped(StopReason::AttemptsExhausted));
                            }
                        }
                    }
                }
            }
            Mode::Connect(address) => {
                let Some(command) = input.next().await? else {
                    return Ok(End::Stopped(StopReason::Cancelled));
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
        authenticated(session, &identity, &output, &mut input).await
    };
    let result = tokio::select! {
        biased;
        result = control => result.map(End::Stopped),
        _ = tokio::time::sleep_until(deadline) => Ok(End::Stopped(StopReason::Expired)),
        result = work => result,
    };
    // select's bias orders polls, not updates made by the stdin thread between
    // polls. Reconcile the final control snapshot before publishing a result.
    // Protocol/internal failures remain failures even if Stop races them.
    if matches!(
        result,
        Err(Failure::Protocol | Failure::Internal | Failure::CommitUnknown) | Ok(End::Completed(_))
    ) {
        return result;
    }
    let state = final_control.borrow().clone();
    match state {
        Input::Stop => Ok(End::Stopped(StopReason::Cancelled)),
        Input::Invalid => Err(Failure::InvalidCommand),
        Input::InternalFault => Err(Failure::Internal),
        _ => result,
    }
}
async fn authenticated(
    session: PairingSession,
    identity: &Identity,
    output: &Output,
    input: &mut Inputs,
) -> std::result::Result<End, Failure> {
    input.allow(ipc::CommandTag::Approve)?;
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
    let mut channel = session
        .into_commit_channel()
        .map_err(|_| Failure::Protocol)?;
    let context = channel.context();
    let mut approved = false;
    let mut prepared = false;
    let mut ready = false;
    let mut saved = false;
    let mut peer_saved = false;
    let mut receipt = false;
    let mut closed = false;
    loop {
        if approved && prepared && !ready {
            input.allow(ipc::CommandTag::LocalSaved)?;
            output.emit(State::ReadyToSave { context }).await?;
            ready = true;
        }
        if saved && peer_saved && !receipt {
            input.allow(ipc::CommandTag::ReceiptStored)?;
            output.emit(State::PeerSaved { context }).await?;
            receipt = true;
        }
        enum Incoming {
            Peer(Result<pairing::commit::Message>),
            Local(std::result::Result<Option<PairingCommand>, Failure>),
        }
        let event = tokio::select! {
            biased;
            message = channel.receive(), if !closed => Incoming::Peer(message),
            command = input.next() => Incoming::Local(command),
        };
        match event {
            Incoming::Peer(Ok(pairing::commit::Message::Prepared)) => prepared = true,
            Incoming::Peer(Ok(pairing::commit::Message::Saved)) if approved && prepared => {
                peer_saved = true
            }
            Incoming::Peer(Ok(pairing::commit::Message::Closed)) => {
                closed = true;
                if !peer_saved {
                    if ready {
                        return Err(Failure::CommitUnknown);
                    }
                    return peer_end(Ok(pairing::PeerEnd::Disconnected)).map(End::Stopped);
                }
            }
            Incoming::Peer(Err(error)) if ready && expected_disconnect(&error) => {
                return Err(Failure::CommitUnknown)
            }
            Incoming::Peer(_) => return Err(Failure::Protocol),
            Incoming::Local(Err(error)) => return Err(error),
            Incoming::Local(Ok(None)) => return Ok(End::Stopped(StopReason::Cancelled)),
            Incoming::Local(Ok(Some(command))) => {
                if command.context() != Some(context) {
                    return Err(Failure::Protocol);
                }
                match command.tag() {
                    ipc::CommandTag::Approve if !approved => {
                        approved = true;
                        channel.prepare().await.map_err(|_| Failure::Protocol)?;
                    }
                    ipc::CommandTag::LocalSaved if ready && !saved => {
                        saved = true;
                        if let Err(error) = channel.saved().await {
                            // Keep valid peer-saved evidence already read before
                            // closure; it is not a new live authentication ticket.
                            while let Some(fact) = channel.buffered_fact() {
                                match fact {
                                    Ok(pairing::commit::Message::Saved) if approved && prepared => {
                                        peer_saved = true
                                    }
                                    Ok(pairing::commit::Message::Closed) => (),
                                    _ => return Err(Failure::Protocol),
                                }
                            }
                            if !peer_saved {
                                return Err(if expected_disconnect(&error) {
                                    Failure::CommitUnknown
                                } else {
                                    Failure::Protocol
                                });
                            }
                            closed = true;
                        }
                    }
                    ipc::CommandTag::ReceiptStored if receipt && saved && peer_saved => {
                        return Ok(End::Completed(context))
                    }
                    _ => return Err(Failure::InvalidCommand),
                }
            }
        }
    }
}

fn expected_disconnect(error: &anyhow::Error) -> bool {
    error.downcast_ref::<io::Error>().is_some_and(|error| {
        matches!(
            error.kind(),
            io::ErrorKind::UnexpectedEof
                | io::ErrorKind::ConnectionReset
                | io::ErrorKind::ConnectionAborted
                | io::ErrorKind::BrokenPipe
                | io::ErrorKind::NotConnected
                | io::ErrorKind::TimedOut
        )
    })
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
        let result = tokio::time::timeout(Duration::from_secs(1), stop(rx)).await?;
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
            Inputs::fixture(input),
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
        assert!(timely && matches!(outcome, Ok(End::Stopped(StopReason::Cancelled))));
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
        let (_tx, rx) = watch::channel(Input::Waiting);
        let output = Arc::new(Output::spawn_with(io::sink())?);
        let result = execute(
            Arc::new(Identity::generate()?),
            Mode::Offer("127.0.0.1:0".parse()?),
            Inputs::fixture(rx),
            output,
            Instant::now() + Duration::from_millis(10),
        )
        .await;
        assert!(matches!(result, Ok(End::Stopped(StopReason::Expired))));
        let result = supervise(async { panic!("synthetic pairing task failure") }).await;
        assert!(matches!(result, Err(Failure::Internal)));
        Ok(())
    }
}
