//! Receive-side file approval coordinated through one live managed worker.
use peerglide_protocol::{
    file_drop_ipc::Event,
    worker::{WorkerInputState, WorkerState},
};
use std::collections::VecDeque;

const EVENT_LIMIT: usize = 8;

#[derive(Default)]
pub(super) struct Inbox {
    epoch: u64,
    lease_fence: u64,
    authenticated: bool,
    overflow: bool,
    events: VecDeque<Reply>,
}

struct Reply {
    event: Event,
    lease_fence: u64,
}

impl Inbox {
    pub(super) fn connection(&mut self, state: WorkerState) {
        match state {
            WorkerState::Authenticated
            | WorkerState::Disconnected
            | WorkerState::Reconnecting
            | WorkerState::Stopped
            | WorkerState::Failed => {
                self.epoch = self.epoch.saturating_add(1);
                self.authenticated = state == WorkerState::Authenticated && self.epoch < u64::MAX;
                self.overflow = false;
                self.events.clear();
            }
            _ => (),
        }
    }

    pub(super) fn input(&mut self, state: WorkerInputState) {
        if state != WorkerInputState::ReceivingRemote {
            self.lease_fence = self.lease_fence.saturating_add(1);
        }
    }

    pub(super) fn publish(&mut self, event: Event) {
        if !self.authenticated || self.overflow {
            return;
        }
        if self.events.len() == EVENT_LIMIT {
            self.events.clear();
            self.overflow = true;
        } else {
            self.events.push_back(Reply {
                event,
                lease_fence: self.lease_fence,
            });
        }
    }

    #[cfg(any(all(windows, target_env = "msvc"), target_os = "macos", test))]
    fn begin(&mut self) -> Option<u64> {
        self.events.clear();
        self.overflow = false;
        self.authenticated.then_some(self.epoch)
    }

    #[cfg(any(all(windows, target_env = "msvc"), target_os = "macos", test))]
    fn take(&mut self, epoch: u64) -> Option<(u64, VecDeque<Reply>)> {
        (self.authenticated && self.epoch == epoch && !self.overflow && self.lease_fence < u64::MAX)
            .then(|| (self.lease_fence, std::mem::take(&mut self.events)))
    }
}

#[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
mod native {
    use super::super::{data_access::Pin, App, SessionMode, WorkerSnapshot};
    use super::Reply;
    use anyhow::{ensure, Context, Result};
    use peerglide_protocol::file_drop_ipc::{Command, EntryPoint, State};
    use std::{
        io::Write,
        net::SocketAddr,
        sync::{atomic::Ordering, Arc},
        time::{Duration, Instant},
    };

    const REPLY_TIMEOUT: Duration = Duration::from_secs(2);

    #[derive(Clone)]
    struct Link {
        worker: Arc<WorkerSnapshot>,
        epoch: u64,
    }

    impl Link {
        fn current_pipe(&self, app: &App) -> bool {
            same_pipe(app, &self.worker)
                && self
                    .worker
                    .7
                    .lock()
                    .is_ok_and(|inbox| inbox.authenticated && inbox.epoch == self.epoch)
        }

        fn current(&self, app: &App) -> bool {
            self.current_pipe(app) && app.connected && app.stopping.is_none() && !app.input_terminal
        }

        fn send(&self, app: &mut App, command: Command) -> Result<()> {
            let current = if matches!(command, Command::Cancel { .. }) {
                self.current_pipe(app)
            } else {
                self.current(app)
            };
            ensure!(current, "文件接棒会话已变化");
            let may_arm = !matches!(command, Command::Inspect);
            let line = command.line() + "\n";
            ensure!(line.len() <= 256, "文件接棒命令超出限制");
            let child = app.child.as_mut().context("工作进程已退出")?;
            let stdin = child.stdin.as_mut().context("工作进程控制通道不可用")?;
            if stdin
                .write_all(line.as_bytes())
                .and_then(|()| stdin.flush())
                .is_err()
            {
                // A partial write may have registered the arm. Closing this
                // child's inherited pipe invokes its existing stop/cleanup.
                if may_arm {
                    child.stdin.take();
                }
                anyhow::bail!("无法发送文件接棒命令");
            }
            Ok(())
        }
    }

    fn same_worker(app: &App, worker: &Arc<WorkerSnapshot>) -> bool {
        same_pipe(app, worker) && !app.input_terminal && app.stopping.is_none()
    }

    fn same_pipe(app: &App, worker: &Arc<WorkerSnapshot>) -> bool {
        app.child.is_some()
            && app.active_mode == Some(SessionMode::Native)
            && !app.routed_active
            && app
                .worker_state
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, worker))
            && !worker.2.load(Ordering::Acquire)
            && !worker.3.load(Ordering::Acquire)
    }

    pub(crate) fn next_request(app: &mut App) -> Result<u32> {
        app.file_drop_sequence = app
            .file_drop_sequence
            .checked_add(1)
            .context("文件接棒请求已达上限")?;
        Ok(app.file_drop_sequence)
    }

    #[derive(Clone, Copy)]
    enum Direction {
        Receive(SocketAddr),
        Send(SocketAddr),
    }

    enum Phase {
        AwaitingWorker(Option<Arc<WorkerSnapshot>>),
        Inspecting(Link, Instant),
        Press(Link, u32),
        SourceCommitted(Link, u32, u64),
        SourceReleased,
        Registering(Link, u32, Instant),
        Armed(Link, u32),
        Matched(Link, u32, u64, u64),
        AwaitingClaim(Link, u32, u64, u64, Instant),
        Claimed(Link, u32, u64, u64),
        TargetDown(Link, u32, u64, u64),
        TargetDragging(Link, u32, u64, u64),
        TargetDrop(Link, u32, u64, u64),
        TargetReleasing(Link, u32, u64, u64),
        TargetReleased,
    }

    pub(crate) enum Status {
        WaitingForWorker,
        Confirming,
        Listening(u16),
        Matched,
        Press,
        SourceArmed,
        SourceCommitted,
        SourceReleased,
    }

    pub(crate) struct Reservation {
        proof: Option<String>,
        request: u32,
        direction: Direction,
        expires: Instant,
        phase: Phase,
    }

    impl Reservation {
        pub(crate) fn new(proof: String, request: u32, bind: SocketAddr, expires: Instant) -> Self {
            Self::with_direction(proof, request, Direction::Receive(bind), expires)
        }

        pub(crate) fn source(
            receipt: crate::transport::file_drop_link::SentReceipt,
            request: u32,
            target: SocketAddr,
            expires: Instant,
        ) -> Self {
            Self::with_direction(
                receipt.into_local_worker(),
                request,
                Direction::Send(target),
                expires,
            )
        }

        fn with_direction(
            proof: String,
            request: u32,
            direction: Direction,
            expires: Instant,
        ) -> Self {
            Self {
                proof: Some(proof),
                request,
                direction,
                expires,
                phase: Phase::AwaitingWorker(None),
            }
        }

        pub(crate) fn status(&self) -> Status {
            match self.phase {
                Phase::AwaitingWorker(_) => Status::WaitingForWorker,
                Phase::Inspecting(..)
                | Phase::Registering(..)
                | Phase::Matched(..)
                | Phase::AwaitingClaim(..) => Status::Confirming,
                Phase::Armed(..) => match self.direction {
                    Direction::Receive(bind) => Status::Listening(bind.port()),
                    Direction::Send(_) => Status::SourceArmed,
                },
                Phase::Claimed(..)
                | Phase::TargetDown(..)
                | Phase::TargetDragging(..)
                | Phase::TargetDrop(..)
                | Phase::TargetReleasing(..)
                | Phase::TargetReleased => Status::Matched,
                Phase::Press(..) => Status::Press,
                Phase::SourceCommitted(..) => Status::SourceCommitted,
                Phase::SourceReleased => Status::SourceReleased,
            }
        }

        pub(crate) fn press(&mut self, app: &mut App, pin: &Pin, now: Instant) -> Result<()> {
            let Phase::Press(link, session) = &self.phase else {
                anyhow::bail!("文件接棒尚未就绪");
            };
            let link = link.clone();
            let session = *session;
            self.register(app, pin, link, session, now)
        }

        fn register(
            &mut self,
            app: &mut App,
            pin: &Pin,
            link: Link,
            session: u32,
            now: Instant,
        ) -> Result<()> {
            pin.recheck(app)?;
            let lifetime_ms = u32::try_from(
                self.expires
                    .saturating_duration_since(Instant::now())
                    .as_millis(),
            )?;
            ensure!(lifetime_ms != 0, "文件接棒已过期");
            let proof = self.proof.take().context("文件接棒已经登记")?;
            let request = self.request;
            let command = match self.direction {
                Direction::Receive(bind) => Command::Arm {
                    session,
                    request,
                    bind,
                    lifetime_ms,
                    proof,
                },
                Direction::Send(target) => Command::Send {
                    session,
                    request,
                    target,
                    lifetime_ms,
                    proof,
                },
            };
            link.send(app, command)?;
            self.phase = Phase::Registering(link, session, now + REPLY_TIMEOUT);
            Ok(())
        }

        pub(crate) fn pending_lease(&self) -> Option<u64> {
            match self.phase {
                Phase::Matched(_, _, lease, _) => Some(lease),
                _ => None,
            }
        }

        pub(crate) fn confirm_ready(
            &mut self,
            app: &mut App,
            pin: &Pin,
            lease: u64,
            entry: EntryPoint,
            now: Instant,
        ) -> Result<()> {
            let Phase::Matched(link, session, matched, fence) = &self.phase else {
                anyhow::bail!("文件预约尚未匹配");
            };
            ensure!(lease == *matched && now < self.expires, "文件预约已变化");
            ensure!(
                link.worker
                    .7
                    .lock()
                    .is_ok_and(|inbox| inbox.lease_fence == *fence),
                "输入租约已结束"
            );
            pin.recheck(app)?;
            link.send(
                app,
                Command::PrepareTarget {
                    session: *session,
                    request: self.request,
                    lease,
                    entry,
                },
            )?;
            self.phase =
                Phase::AwaitingClaim(link.clone(), *session, lease, *fence, now + REPLY_TIMEOUT);
            Ok(())
        }

        pub(crate) fn target_position_frozen(&self) -> bool {
            matches!(
                self.phase,
                Phase::AwaitingClaim(..) | Phase::Claimed(..) | Phase::TargetDown(..)
            )
        }

        pub(crate) fn target_down(&self) -> bool {
            matches!(self.phase, Phase::TargetDown(..))
        }

        #[cfg(target_os = "macos")]
        pub(crate) fn target_drop(&self) -> bool {
            matches!(self.phase, Phase::TargetDrop(..))
        }

        pub(crate) fn target_released(&self) -> bool {
            matches!(self.phase, Phase::TargetReleased)
        }

        #[cfg(target_os = "macos")]
        pub(crate) fn target_started(&mut self, app: &mut App) -> Result<()> {
            let Phase::TargetDown(link, session, lease, fence) = &self.phase else {
                anyhow::bail!("没有当前文件接棒的鼠标按下");
            };
            link.send(
                app,
                Command::TargetStarted {
                    session: *session,
                    request: self.request,
                    lease: *lease,
                },
            )?;
            self.phase = Phase::TargetDragging(link.clone(), *session, *lease, *fence);
            Ok(())
        }

        #[cfg(target_os = "macos")]
        pub(crate) fn target_drop_ready(&mut self, app: &mut App) -> Result<()> {
            let Phase::TargetDrop(link, session, lease, fence) = &self.phase else {
                anyhow::bail!("没有当前文件接棒的正常释放");
            };
            link.send(
                app,
                Command::TargetDropReady {
                    session: *session,
                    request: self.request,
                    lease: *lease,
                },
            )?;
            self.phase = Phase::TargetReleasing(link.clone(), *session, *lease, *fence);
            Ok(())
        }

        pub(crate) fn tick(&mut self, app: &mut App, pin: &Pin, now: Instant) -> Result<()> {
            ensure!(now < self.expires, "文件接棒已过期");
            if let Phase::AwaitingWorker(bound) = &mut self.phase {
                if app.child.is_none() {
                    ensure!(bound.is_none(), "已准备的工作进程已退出");
                    return Ok(());
                }
                let worker = app
                    .worker_state
                    .as_ref()
                    .context("工作进程输出通道不可用")?;
                ensure!(same_worker(app, worker), "工作进程不是本次键鼠会话");
                let worker = bound.get_or_insert_with(|| worker.clone());
                ensure!(same_worker(app, worker), "文件接棒工作进程已变化");
                if !app.connected {
                    return Ok(());
                }
                let epoch = worker
                    .7
                    .lock()
                    .ok()
                    .and_then(|mut inbox| inbox.begin())
                    .context("键鼠工作进程尚未确认连接")?;
                let link = Link {
                    worker: worker.clone(),
                    epoch,
                };
                link.send(app, Command::Inspect)?;
                self.phase = Phase::Inspecting(link, now + REPLY_TIMEOUT);
                return Ok(());
            }

            let link = match &self.phase {
                Phase::SourceReleased | Phase::TargetReleased => return Ok(()),
                Phase::AwaitingWorker(_) => unreachable!("worker wait handled above"),
                Phase::Inspecting(link, _)
                | Phase::Press(link, _)
                | Phase::SourceCommitted(link, ..)
                | Phase::Registering(link, ..)
                | Phase::Armed(link, _)
                | Phase::Matched(link, ..)
                | Phase::AwaitingClaim(link, ..)
                | Phase::Claimed(link, ..)
                | Phase::TargetDown(link, ..)
                | Phase::TargetDragging(link, ..)
                | Phase::TargetDrop(link, ..)
                | Phase::TargetReleasing(link, ..) => link.clone(),
            };
            ensure!(link.current(app), "文件接棒会话已变化");
            let (lease_fence, events) = link
                .worker
                .7
                .lock()
                .ok()
                .and_then(|mut inbox| inbox.take(link.epoch))
                .context("文件接棒回执已失效")?;
            if let Phase::Matched(_, _, _, original_fence)
            | Phase::AwaitingClaim(_, _, _, original_fence, _)
            | Phase::Claimed(_, _, _, original_fence)
            | Phase::TargetDown(_, _, _, original_fence)
            | Phase::TargetDragging(_, _, _, original_fence)
            | Phase::TargetDrop(_, _, _, original_fence) = self.phase
            {
                ensure!(lease_fence == original_fence, "已匹配的输入租约已结束");
            }
            if let Phase::TargetReleasing(_, session, lease, fence) = self.phase {
                self.accept_target_release(session, lease, fence, lease_fence, &events)?;
                return Ok(());
            }
            for Reply {
                event,
                lease_fence: event_fence,
            } in events
            {
                match &self.phase {
                    Phase::Inspecting(_, _) if event.state == State::Session => {
                        if matches!(self.direction, Direction::Send(_)) {
                            self.phase = Phase::Press(link.clone(), event.session);
                        } else {
                            self.register(app, pin, link.clone(), event.session, now)?;
                        }
                    }
                    Phase::Registering(_, session, _)
                        if event.session == *session && event.request == self.request =>
                    {
                        let expected = match self.direction {
                            Direction::Receive(bind) => {
                                event.state == State::Armed && event.port == Some(bind.port())
                            }
                            Direction::Send(_) => event.state == State::SourceArmed,
                        };
                        ensure!(expected, "工作进程未确认文件接棒");
                        self.phase = Phase::Armed(link.clone(), *session);
                    }
                    Phase::Armed(_, session)
                        if event.session == *session && event.request == self.request =>
                    {
                        let lease = event.lease.context("接棒回执缺少输入租约")?;
                        self.phase = match self.direction {
                            Direction::Receive(_) => {
                                ensure!(
                                    event.state == State::Matched && event_fence == lease_fence,
                                    "接棒回执的输入租约已结束"
                                );
                                Phase::Matched(link.clone(), *session, lease, lease_fence)
                            }
                            Direction::Send(_) => {
                                ensure!(
                                    event.state == State::SourceCommitted,
                                    "文件接棒已撤销或被拒绝"
                                );
                                Phase::SourceCommitted(link.clone(), *session, lease)
                            }
                        };
                    }
                    Phase::SourceCommitted(_, session, lease)
                        if event.session == *session && event.request == self.request =>
                    {
                        ensure!(
                            event.state == State::SourceReleased && event.lease == Some(*lease),
                            "文件接棒已结束"
                        );
                        self.phase = Phase::SourceReleased;
                    }
                    Phase::AwaitingClaim(_, session, lease, _, _)
                        if event.session == *session && event.request == self.request =>
                    {
                        ensure!(
                            event.state == State::Claimed
                                && event.lease == Some(*lease)
                                && event_fence == lease_fence,
                            "文件接棒就绪确认已失效"
                        );
                        self.phase = Phase::Claimed(link.clone(), *session, *lease, lease_fence);
                    }
                    Phase::Claimed(_, session, lease, _)
                    | Phase::TargetDragging(_, session, lease, _)
                        if event.session == *session && event.request == self.request =>
                    {
                        ensure!(
                            event.lease == Some(*lease) && event_fence == lease_fence,
                            "文件拖放租约已失效"
                        );
                        self.phase = match (&self.phase, event.state) {
                            (Phase::Claimed(..), State::TargetDown) => {
                                Phase::TargetDown(link.clone(), *session, *lease, lease_fence)
                            }
                            (Phase::TargetDragging(..), State::TargetDrop) => {
                                Phase::TargetDrop(link.clone(), *session, *lease, lease_fence)
                            }
                            _ => anyhow::bail!("文件拖放回执顺序无效"),
                        };
                    }
                    Phase::TargetDown(_, session, lease, _)
                    | Phase::TargetDrop(_, session, lease, _)
                        if event.session == *session && event.request == self.request =>
                    {
                        ensure!(event.lease == Some(*lease), "文件拖放租约已失效");
                        anyhow::bail!("文件拖放已结束");
                    }
                    _ => (),
                }
            }
            if let Phase::Inspecting(_, deadline)
            | Phase::Registering(_, _, deadline)
            | Phase::AwaitingClaim(_, _, _, _, deadline) = self.phase
            {
                ensure!(now < deadline, "未收到工作进程的接棒确认");
            }
            Ok(())
        }

        fn accept_target_release(
            &mut self,
            session: u32,
            lease: u64,
            fence: u64,
            current_fence: u64,
            events: &std::collections::VecDeque<Reply>,
        ) -> Result<()> {
            if let Some(reply) = events
                .iter()
                .find(|reply| reply.event.session == session && reply.event.request == self.request)
            {
                ensure!(
                    reply.event.state == State::TargetReleased
                        && reply.event.lease == Some(lease)
                        && reply.lease_fence == fence,
                    "文件释放未确认"
                );
                self.phase = Phase::TargetReleased;
            } else {
                ensure!(current_fence == fence, "输入租约已结束");
            }
            Ok(())
        }

        pub(crate) fn cancel(self, app: &mut App) {
            if let Phase::Registering(link, session, _)
            | Phase::Armed(link, session)
            | Phase::SourceCommitted(link, session, _)
            | Phase::Matched(link, session, _, _)
            | Phase::AwaitingClaim(link, session, _, _, _)
            | Phase::Claimed(link, session, _, _)
            | Phase::TargetDown(link, session, _, _)
            | Phase::TargetDragging(link, session, _, _)
            | Phase::TargetDrop(link, session, _, _)
            | Phase::TargetReleasing(link, session, _, _) = self.phase
            {
                let _ = link.send(
                    app,
                    Command::Cancel {
                        session,
                        request: self.request,
                    },
                );
            }
        }
    }

    #[cfg(all(windows, target_env = "msvc"))]
    pub(crate) struct WindowsDelivery {
        signal: Arc<std::sync::atomic::AtomicU8>,
        started: std::sync::mpsc::SyncSender<()>,
        owner: Option<std::thread::JoinHandle<Option<std::process::ChildStdin>>>,
    }

    #[cfg(all(windows, target_env = "msvc"))]
    impl WindowsDelivery {
        pub(crate) fn open(reservation: &Reservation, app: &mut App) -> Result<Self> {
            let Phase::TargetDown(link, session, lease, fence) = &reservation.phase else {
                anyhow::bail!("没有当前文件接棒的鼠标按下");
            };
            ensure!(link.current(app), "文件接棒会话已变化");
            let (link, session, lease, fence) = (link.clone(), *session, *lease, *fence);
            let mut reservation = Reservation {
                proof: None,
                request: reservation.request,
                direction: reservation.direction,
                expires: reservation.expires,
                phase: Phase::TargetDown(link.clone(), session, lease, fence),
            };
            let pipe = app
                .child
                .as_mut()
                .and_then(|child| child.stdin.take())
                .context("工作进程控制通道不可用")?;
            let signal = Arc::new(std::sync::atomic::AtomicU8::new(0));
            let shared = signal.clone();
            let (started, start) = std::sync::mpsc::sync_channel(1);
            let owner = std::thread::Builder::new()
                .name("linko-ole-drop-control".into())
                .spawn(move || {
                    let mut pipe = pipe;
                    let mut send = |command: Command| -> Result<()> {
                        writeln!(pipe, "{}", command.line())?;
                        pipe.flush()?;
                        Ok(())
                    };
                    let result = (|| -> Result<()> {
                        start.recv_timeout(Duration::from_millis(600))?;
                        ensure!(shared.load(Ordering::Acquire) == 0, "拖放已取消");
                        send(Command::TargetStarted {
                            session,
                            request: reservation.request,
                            lease,
                        })?;
                        reservation.phase =
                            Phase::TargetDragging(link.clone(), session, lease, fence);
                        while shared.load(Ordering::Acquire) == 0
                            && Instant::now() < reservation.expires
                        {
                            let (current, events) = link
                                .worker
                                .7
                                .lock()
                                .ok()
                                .and_then(|mut inbox| inbox.take(link.epoch))
                                .context("工作进程已退出")?;
                            ensure!(
                                !link.worker.2.load(Ordering::Acquire)
                                    && !link.worker.3.load(Ordering::Acquire),
                                "工作进程已退出"
                            );
                            if matches!(reservation.phase, Phase::TargetReleasing(..)) {
                                reservation.accept_target_release(
                                    session, lease, fence, current, &events,
                                )?;
                                if reservation.target_released() {
                                    shared
                                        .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
                                        .ok();
                                    return Ok(());
                                }
                            } else {
                                ensure!(current == fence, "输入租约已结束");
                                for reply in events {
                                    if reply.event.session != session
                                        || reply.event.request != reservation.request
                                    {
                                        continue;
                                    }
                                    ensure!(
                                        reply.event.state == State::TargetDrop
                                            && reply.event.lease == Some(lease)
                                            && reply.lease_fence == fence,
                                        "文件拖放已结束"
                                    );
                                    reservation.phase =
                                        Phase::TargetDrop(link.clone(), session, lease, fence);
                                    send(Command::TargetDropReady {
                                        session,
                                        request: reservation.request,
                                        lease,
                                    })?;
                                    reservation.phase =
                                        Phase::TargetReleasing(link.clone(), session, lease, fence);
                                }
                            }
                            std::thread::sleep(Duration::from_millis(5));
                        }
                        anyhow::bail!("文件拖放已结束");
                    })();
                    if result.is_err() {
                        shared.store(2, Ordering::Release);
                        if send(Command::Cancel {
                            session,
                            request: reservation.request,
                        })
                        .is_err()
                        {
                            return None;
                        }
                    }
                    Some(pipe)
                })?;
            Ok(Self {
                signal,
                started,
                owner: Some(owner),
            })
        }

        pub(crate) fn native_check(&self, phase: u8) -> bool {
            match phase {
                0 => self.started.try_send(()).is_ok(),
                1 => self.signal.load(Ordering::Acquire) != 2,
                2 => {
                    // The native loop may receive Up before stdout is parsed.
                    let deadline = Instant::now() + Duration::from_millis(250);
                    while self.signal.load(Ordering::Acquire) == 0 && Instant::now() < deadline {
                        std::thread::sleep(Duration::from_millis(2));
                    }
                    self.signal.load(Ordering::Acquire) == 1
                }
                _ => false,
            }
        }

        pub(crate) fn finish(mut self, app: &mut App) {
            if self.signal.load(Ordering::Acquire) != 1 {
                self.signal.store(2, Ordering::Release);
            }
            // Wake a control owner that is still waiting for OLE initialization.
            let _ = self.started.try_send(());
            if let Some(child) = app.child.as_mut() {
                child.stdin = self
                    .owner
                    .take()
                    .and_then(|owner| owner.join().ok())
                    .flatten();
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::super::{Event, WorkerInputState, WorkerState};
        use super::*;
        use crate::Identity;
        use std::{
            io::BufRead,
            process::{Command as ProcessCommand, Stdio},
        };

        #[test]
        fn echo_private_commands() -> Result<()> {
            if std::env::var_os("LINKO_GUI_FILE_DROP_PIPE_TEST").is_some() {
                for line in std::io::stdin().lock().lines() {
                    println!("fixture {}", line?);
                }
            }
            Ok(())
        }

        #[test]
        fn approval_writes_one_current_arm_and_cancels_only_that_request() -> Result<()> {
            let fixture = crate::tests::Fixture::new()?;
            let mut app = App::load(fixture.path.clone());
            Identity::create(&app.identity_dir())?;
            let peer = Identity::generate()?;
            std::fs::write(app.dir.join("trusted-peer.crt.der"), &peer.certificate)?;
            app.config.fingerprint = Some(peer.fingerprint());
            let (pin, _, _) = Pin::load(&app)?;
            app.child = Some(
                ProcessCommand::new(std::env::current_exe()?)
                    .args([
                        "--exact",
                        "file_drop_reservation::native::tests::echo_private_commands",
                        "--nocapture",
                    ])
                    .env("LINKO_GUI_FILE_DROP_PIPE_TEST", "1")
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .spawn()?,
            );
            app.active_mode = Some(SessionMode::Native);
            app.connected = true;
            let worker = Arc::new(WorkerSnapshot::default());
            worker
                .7
                .lock()
                .unwrap()
                .connection(WorkerState::Authenticated);
            app.worker_state = Some(worker.clone());

            let now = Instant::now();
            let mut reservation = Reservation::new(
                "ab".repeat(80),
                5,
                "127.0.0.1:24872".parse()?,
                now + Duration::from_secs(60),
            );
            reservation.tick(&mut app, &pin, now)?;
            assert!(matches!(reservation.status(), Status::Confirming));
            worker
                .7
                .lock()
                .unwrap()
                .publish(Event::new(3, 0, State::Session, None));
            reservation.tick(&mut app, &pin, now)?;
            worker.7.lock().unwrap().publish(Event::armed(2, 5, 24872));
            reservation.tick(&mut app, &pin, now)?;
            assert!(matches!(reservation.status(), Status::Confirming));
            worker.7.lock().unwrap().publish(Event::armed(3, 5, 24872));
            worker
                .7
                .lock()
                .unwrap()
                .publish(Event::new(3, 5, State::Matched, Some(9)));
            reservation.tick(&mut app, &pin, now)?;
            assert_eq!(reservation.pending_lease(), Some(9));
            let entry = EntryPoint::new(0.5, 0.5).unwrap();
            assert!(reservation
                .confirm_ready(&mut app, &pin, 8, entry, now)
                .is_err());
            reservation.confirm_ready(&mut app, &pin, 9, entry, now)?;
            assert!(reservation
                .confirm_ready(&mut app, &pin, 9, entry, now)
                .is_err());
            worker
                .7
                .lock()
                .unwrap()
                .publish(Event::new(3, 5, State::Claimed, Some(9)));
            reservation.tick(&mut app, &pin, now)?;
            assert!(matches!(reservation.status(), Status::Matched));
            worker
                .7
                .lock()
                .unwrap()
                .publish(Event::new(3, 5, State::TargetDown, Some(9)));
            reservation.tick(&mut app, &pin, now)?;
            assert!(reservation.target_down());

            #[cfg(target_os = "macos")]
            {
                reservation.target_started(&mut app)?;
                worker
                    .7
                    .lock()
                    .unwrap()
                    .publish(Event::new(3, 5, State::TargetDrop, Some(9)));
                reservation.tick(&mut app, &pin, now)?;
                assert!(reservation.target_drop());
                reservation.target_drop_ready(&mut app)?;
                assert!(!reservation.target_released());
                let mut inbox = worker.7.lock().unwrap();
                inbox.publish(Event::new(3, 5, State::TargetReleased, Some(9)));
                inbox.input(WorkerInputState::Local);
                drop(inbox);
                reservation.tick(&mut app, &pin, now)?;
                assert!(reservation.target_released());
            }
            #[cfg(all(windows, target_env = "msvc"))]
            let consumed = {
                let delivery = WindowsDelivery::open(&reservation, &mut app)?;
                assert!(delivery.native_check(0));
                worker
                    .7
                    .lock()
                    .unwrap()
                    .publish(Event::new(3, 5, State::TargetDrop, Some(9)));
                // The fake peer cannot issue the matching release until it
                // observes the local drop-ready command. Keep the fixture
                // deterministic by waiting for the actual inherited pipe.
                let child = app.child.as_mut().unwrap();
                let mut stdout = std::io::BufReader::new(child.stdout.take().unwrap());
                let (send, received) = std::sync::mpsc::sync_channel(1);
                let reader = std::thread::spawn(move || {
                    let mut consumed = String::new();
                    let mut line = String::new();
                    while stdout.read_line(&mut line).is_ok_and(|bytes| bytes != 0) {
                        consumed.push_str(&line);
                        if line.contains("fixture fd target-drop-ready 3 5 9") {
                            let _ = send.send(consumed);
                            break;
                        }
                        line.clear();
                    }
                    stdout
                });
                let commands = received.recv_timeout(Duration::from_secs(5))?;
                assert_eq!(delivery.signal.load(Ordering::Acquire), 0);
                let mut inbox = worker.7.lock().unwrap();
                inbox.publish(Event::new(3, 5, State::TargetReleased, Some(9)));
                inbox.input(WorkerInputState::Local);
                drop(inbox);
                assert!(delivery.native_check(2));
                delivery.finish(&mut app);
                // Keep stdout open until the echo child has finished writing.
                (commands, reader.join().unwrap())
            };
            #[cfg(target_os = "macos")]
            reservation.cancel(&mut app);

            let child = app.child.take().unwrap();
            let output = child.wait_with_output()?;
            assert!(output.status.success());
            #[cfg(target_os = "macos")]
            let commands = String::from_utf8(output.stdout)?;
            #[cfg(all(windows, target_env = "msvc"))]
            let (commands, _stdout) = consumed;
            assert_eq!(commands.matches("fixture fd inspect").count(), 1);
            assert_eq!(commands.matches("fixture fd arm 3 5 ").count(), 1);
            assert_eq!(
                commands
                    .matches("fixture fd prepare-target 3 5 9 500000 500000")
                    .count(),
                1
            );
            assert_eq!(
                commands.matches("fixture fd prepare-target 3 5 8").count(),
                0
            );
            assert_eq!(
                commands.matches("fixture fd target-started 3 5 9").count(),
                1
            );
            assert_eq!(
                commands
                    .matches("fixture fd target-drop-ready 3 5 9")
                    .count(),
                1
            );
            assert_eq!(commands.matches("fixture fd cancel 3 5").count(), 0);
            drop(app);
            fixture.close()
        }

        #[test]
        fn source_pipe_waits_for_an_explicit_press_and_accepts_only_the_matching_release(
        ) -> Result<()> {
            let fixture = crate::tests::Fixture::new()?;
            let mut app = App::load(fixture.path.clone());
            Identity::create(&app.identity_dir())?;
            let peer = Identity::generate()?;
            std::fs::write(app.dir.join("trusted-peer.crt.der"), &peer.certificate)?;
            app.config.fingerprint = Some(peer.fingerprint());
            let (pin, _, _) = Pin::load(&app)?;
            app.child = Some(
                ProcessCommand::new(std::env::current_exe()?)
                    .args([
                        "--exact",
                        "file_drop_reservation::native::tests::echo_private_commands",
                        "--nocapture",
                    ])
                    .env("LINKO_GUI_FILE_DROP_PIPE_TEST", "1")
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .spawn()?,
            );
            app.active_mode = Some(SessionMode::Native);
            app.connected = true;
            let worker = Arc::new(WorkerSnapshot::default());
            worker
                .7
                .lock()
                .unwrap()
                .connection(WorkerState::Authenticated);
            app.worker_state = Some(worker.clone());
            let now = Instant::now();
            assert_eq!(next_request(&mut app)?, 1);
            let request = next_request(&mut app)?;
            let mut reservation = Reservation::with_direction(
                "ab".repeat(80),
                request,
                Direction::Send("127.0.0.1:24873".parse()?),
                now + Duration::from_secs(60),
            );
            assert!(reservation.press(&mut app, &pin, now).is_err());
            reservation.tick(&mut app, &pin, now)?;
            worker
                .7
                .lock()
                .unwrap()
                .publish(Event::new(3, 0, State::Session, None));
            reservation.tick(&mut app, &pin, now)?;
            assert!(matches!(reservation.status(), Status::Press));
            reservation.press(&mut app, &pin, now)?;
            assert!(reservation.press(&mut app, &pin, now).is_err());
            for event in [
                Event::new(4, request, State::SourceArmed, None),
                Event::new(3, request, State::SourceArmed, None),
                Event::new(3, request, State::SourceCommitted, Some(9)),
                Event::new(4, request, State::SourceReleased, Some(9)),
            ] {
                worker.7.lock().unwrap().publish(event);
            }
            reservation.tick(&mut app, &pin, now)?;
            assert!(matches!(reservation.status(), Status::SourceCommitted));
            worker.7.lock().unwrap().publish(Event::new(
                3,
                request,
                State::SourceReleased,
                Some(9),
            ));
            reservation.tick(&mut app, &pin, now)?;
            assert!(matches!(reservation.status(), Status::SourceReleased));
            reservation.cancel(&mut app);

            let child = app.child.take().unwrap();
            let output = child.wait_with_output()?;
            assert!(output.status.success());
            let commands = String::from_utf8(output.stdout)?;
            assert_eq!(commands.matches("fixture fd inspect").count(), 1);
            assert_eq!(commands.matches("fixture fd send 3 2 ").count(), 1);
            assert!(!commands.contains("fixture fd cancel"));
            drop(app);
            fixture.close()
        }
    }
}

#[cfg(all(windows, target_env = "msvc"))]
pub(super) use native::WindowsDelivery;
#[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
pub(super) use native::{next_request, Reservation, Status};

#[cfg(test)]
mod tests {
    use super::*;
    use peerglide_protocol::file_drop_ipc::State;

    #[test]
    fn replies_are_bounded_and_the_old_worker_epoch_cannot_be_reused() {
        let mut inbox = Inbox::default();
        assert!(inbox.begin().is_none());
        inbox.connection(WorkerState::Authenticated);
        let first = inbox.begin().unwrap();
        for request in 1..=EVENT_LIMIT as u32 + 1 {
            inbox.publish(Event::new(1, request, State::Rejected, None));
        }
        assert!(inbox.take(first).is_none());
        inbox.connection(WorkerState::Disconnected);
        inbox.publish(Event::armed(1, 9, 24872));
        assert!(inbox.begin().is_none());
        inbox.connection(WorkerState::Authenticated);
        assert!(inbox.take(first).is_none());
        let current = inbox.begin().unwrap();
        assert!(inbox.take(current).unwrap().1.is_empty());
    }

    #[test]
    fn a_release_observed_after_a_claim_invalidates_its_queued_lease() {
        let mut inbox = Inbox::default();
        inbox.connection(WorkerState::Authenticated);
        let epoch = inbox.begin().unwrap();
        inbox.input(WorkerInputState::ReceivingRemote);
        inbox.publish(Event::new(1, 2, State::Claimed, Some(7)));
        inbox.input(WorkerInputState::Local);
        let (current, events) = inbox.take(epoch).unwrap();
        let reply = events.front().unwrap();
        assert_eq!(reply.event.state, State::Claimed);
        assert_ne!(reply.lease_fence, current);
    }
}
