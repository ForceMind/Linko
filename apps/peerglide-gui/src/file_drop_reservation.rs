//! Receive-side file approval coordinated through one live managed worker.
use peerglide_protocol::{file_drop_ipc::Event, worker::{WorkerInputState, WorkerState}};
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
            self.events.push_back(Reply { event, lease_fence: self.lease_fence });
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
    use super::Reply;
    use super::super::{data_access::Pin, App, SessionMode, WorkerSnapshot};
    use anyhow::{ensure, Context, Result};
    use peerglide_protocol::file_drop_ipc::{Command, State};
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
                && self.worker.7.lock().is_ok_and(|inbox| {
                    inbox.authenticated && inbox.epoch == self.epoch
                })
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
            if stdin.write_all(line.as_bytes()).and_then(|()| stdin.flush()).is_err() {
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
            && app.worker_state.as_ref().is_some_and(|current| Arc::ptr_eq(current, worker))
            && !worker.2.load(Ordering::Acquire)
            && !worker.3.load(Ordering::Acquire)
    }

    enum Phase {
        AwaitingWorker(Option<Arc<WorkerSnapshot>>),
        Inspecting(Link, Instant),
        Registering(Link, u32, Instant),
        Armed(Link, u32),
        Matched(Link, u32, u64, u64),
    }

    pub(crate) enum Status {
        WaitingForWorker,
        Confirming,
        Listening(u16),
        Matched,
    }

    pub(crate) struct Reservation {
        proof: Option<String>,
        request: u32,
        bind: SocketAddr,
        expires: Instant,
        phase: Phase,
    }

    impl Reservation {
        pub(crate) fn new(proof: String, request: u32, bind: SocketAddr, expires: Instant) -> Self {
            Self { proof: Some(proof), request, bind, expires, phase: Phase::AwaitingWorker(None) }
        }

        pub(crate) fn status(&self) -> Status {
            match self.phase {
                Phase::AwaitingWorker(_) => Status::WaitingForWorker,
                Phase::Inspecting(..) | Phase::Registering(..) => Status::Confirming,
                Phase::Armed(..) => Status::Listening(self.bind.port()),
                Phase::Matched(..) => Status::Matched,
            }
        }

        pub(crate) fn matched(&self) -> Option<u64> {
            match self.phase {
                Phase::Matched(_, _, lease, _) => Some(lease),
                _ => None,
            }
        }

        pub(crate) fn tick(&mut self, app: &mut App, pin: &Pin, now: Instant) -> Result<()> {
            ensure!(now < self.expires, "文件接棒已过期");
            if let Phase::AwaitingWorker(bound) = &mut self.phase {
                if app.child.is_none() {
                    ensure!(bound.is_none(), "已准备的工作进程已退出");
                    return Ok(());
                }
                let worker = app.worker_state.as_ref().context("工作进程输出通道不可用")?;
                ensure!(same_worker(app, worker), "工作进程不是本次键鼠会话");
                let worker = bound.get_or_insert_with(|| worker.clone());
                ensure!(same_worker(app, worker), "文件接棒工作进程已变化");
                if !app.connected {
                    return Ok(());
                }
                let epoch = worker.7.lock().ok().and_then(|mut inbox| inbox.begin())
                    .context("键鼠工作进程尚未确认连接")?;
                let link = Link { worker: worker.clone(), epoch };
                link.send(app, Command::Inspect)?;
                self.phase = Phase::Inspecting(link, now + REPLY_TIMEOUT);
                return Ok(());
            }

            let link = match &self.phase {
                Phase::AwaitingWorker(_) => unreachable!("worker wait handled above"),
                Phase::Inspecting(link, _) | Phase::Registering(link, ..) | Phase::Armed(link, _) | Phase::Matched(link, ..) => link.clone(),
            };
            ensure!(link.current(app), "文件接棒会话已变化");
            let (lease_fence, events) = link.worker.7.lock().ok().and_then(|mut inbox| inbox.take(link.epoch))
                .context("文件接棒回执已失效")?;
            if let Phase::Matched(_, _, _, original_fence) = self.phase {
                ensure!(lease_fence == original_fence, "已匹配的输入租约已结束");
            }
            for Reply { event, lease_fence: event_fence } in events {
                match &self.phase {
                    Phase::Inspecting(_, _) if event.state == State::Session => {
                        pin.recheck(app)?;
                        let lifetime_ms = u32::try_from(self.expires.saturating_duration_since(Instant::now()).as_millis())?;
                        ensure!(lifetime_ms != 0, "文件接棒已过期");
                        let proof = self.proof.take().context("文件接棒已经登记")?;
                        link.send(app, Command::Arm {
                            session: event.session,
                            request: self.request,
                            bind: self.bind,
                            lifetime_ms,
                            proof,
                        })?;
                        self.phase = Phase::Registering(link.clone(), event.session, now + REPLY_TIMEOUT);
                    }
                    Phase::Registering(_, session, _) if event.session == *session && event.request == self.request => {
                        ensure!(event.state == State::Armed && event.port == Some(self.bind.port()), "工作进程未确认文件接棒端口");
                        self.phase = Phase::Armed(link.clone(), *session);
                    }
                    Phase::Armed(_, session) if event.session == *session && event.request == self.request => {
                        ensure!(event.state == State::Claimed, "文件接棒已撤销或被拒绝");
                        ensure!(event_fence == lease_fence, "接棒回执的输入租约已结束");
                        let lease = event.lease.context("接棒回执缺少输入租约")?;
                        self.phase = Phase::Matched(link.clone(), *session, lease, lease_fence);
                    }
                    _ => (),
                }
            }
            if let Phase::Inspecting(_, deadline) | Phase::Registering(_, _, deadline) = self.phase {
                ensure!(now < deadline, "未收到工作进程的接棒确认");
            }
            Ok(())
        }

        pub(crate) fn cancel(self, app: &mut App) {
            if let Phase::Registering(link, session, _) | Phase::Armed(link, session) | Phase::Matched(link, session, _, _) = self.phase {
                let _ = link.send(app, Command::Cancel { session, request: self.request });
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use super::super::{Event, WorkerState};
        use crate::Identity;
        use std::{io::BufRead, process::{Command as ProcessCommand, Stdio}};

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
            app.child = Some(ProcessCommand::new(std::env::current_exe()?)
                .args(["--exact", "file_drop_reservation::native::tests::echo_private_commands", "--nocapture"])
                .env("LINKO_GUI_FILE_DROP_PIPE_TEST", "1")
                .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn()?);
            app.active_mode = Some(SessionMode::Native);
            app.connected = true;
            let worker = Arc::new(WorkerSnapshot::default());
            worker.7.lock().unwrap().connection(WorkerState::Authenticated);
            app.worker_state = Some(worker.clone());

            let now = Instant::now();
            let mut reservation = Reservation::new("ab".repeat(80), 5, "127.0.0.1:24872".parse()?, now + Duration::from_secs(60));
            reservation.tick(&mut app, &pin, now)?;
            assert!(matches!(reservation.status(), Status::Confirming));
            worker.7.lock().unwrap().publish(Event::new(3, 0, State::Session, None));
            reservation.tick(&mut app, &pin, now)?;
            worker.7.lock().unwrap().publish(Event::armed(2, 5, 24872));
            reservation.tick(&mut app, &pin, now)?;
            assert!(matches!(reservation.status(), Status::Confirming));
            worker.7.lock().unwrap().publish(Event::armed(3, 5, 24872));
            worker.7.lock().unwrap().publish(Event::new(3, 5, State::Claimed, Some(9)));
            reservation.tick(&mut app, &pin, now)?;
            assert_eq!(reservation.matched(), Some(9));
            reservation.cancel(&mut app);

            let child = app.child.take().unwrap();
            let output = child.wait_with_output()?;
            assert!(output.status.success());
            let commands = String::from_utf8(output.stdout)?;
            assert_eq!(commands.matches("fixture fd inspect").count(), 1);
            assert_eq!(commands.matches("fixture fd arm 3 5 ").count(), 1);
            assert_eq!(commands.matches("fixture fd cancel 3 5").count(), 1);
            drop(app);
            fixture.close()
        }
    }
}

#[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
pub(super) use native::{Reservation, Status};

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
