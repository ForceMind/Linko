use anyhow::{bail, Result};
use peerglide_core::{EdgeGate, PendingReleases, Receiver};
use peerglide_platform::{self as platform, now_ms, Captured};
use peerglide_protocol::{
    decode, encode, worker::WorkerInputState, Edge, Input, InputCapabilities, Message, Motion,
};
use peerglide_transport as transport;
use std::time::Duration;
use tokio::sync::mpsc;

struct Recovery {
    receiver: Receiver,
    native: bool,
    pending_releases: PendingReleases,
}
impl Recovery {
    fn release(&mut self) -> Result<()> {
        self.pending_releases.extend(self.receiver.release());
        self.flush_releases()
    }
    fn finish(&mut self, lease: u64, cursor: Option<Motion>, now: u64) -> Result<()> {
        let native = self.native;
        self.finish_with(
            lease,
            cursor,
            now,
            |m| {
                if native {
                    platform::pointer(m.x, m.y)
                } else {
                    Ok(())
                }
            },
            |event| {
                if native {
                    platform::inject(event)
                } else {
                    Ok(())
                }
            },
        )
    }
    fn finish_with(
        &mut self,
        lease: u64,
        cursor: Option<Motion>,
        now: u64,
        mut pointer: impl FnMut(Motion) -> Result<()>,
        inject: impl FnMut(Input) -> Result<()>,
    ) -> Result<()> {
        let Some(end) = self.receiver.finish(lease, cursor, now) else {
            return Ok(());
        };
        self.pending_releases.extend(end.releases);
        let pointer_result = end.cursor.map(&mut pointer).unwrap_or(Ok(()));
        // Pointer failure must not bypass balanced key/button cleanup.
        self.pending_releases.flush(inject).map_err(|remaining| {
            anyhow::anyhow!(
                "native input cleanup failed: {remaining} releases remain pending; session stopped"
            )
        })?;
        pointer_result
    }
    fn flush_releases(&mut self) -> Result<()> {
        let native = self.native;
        self.pending_releases.flush(|event| {
            if native { platform::inject(event) } else { Ok(()) }
        }).map_err(|remaining| anyhow::anyhow!("native input cleanup failed: {remaining} releases remain pending; session stopped"))
    }
    fn input(&mut self, event: Input) -> Result<()> {
        if self.native {
            platform::inject(event)?;
        }
        Ok(())
    }
    fn pointer(&self, x: f64, y: f64) -> Result<()> {
        if self.native {
            platform::pointer(x, y)?;
        }
        Ok(())
    }
}
impl Drop for Recovery {
    fn drop(&mut self) {
        if self.release().is_err() {
            eprintln!("Native input cleanup remains incomplete; check local input state.");
        }
    }
}
async fn send(tx: &mut quinn::SendStream, m: &Message) -> Result<()> {
    tokio::time::timeout(Duration::from_millis(250), transport::send(tx, m)).await??;
    Ok(())
}
fn normal_close(c: &quinn::Connection) -> bool {
    matches!(c.close_reason(),Some(quinn::ConnectionError::ApplicationClosed(closed)) if closed.error_code==0u8.into())
}
pub struct Policy {
    pub allow: bool,
    pub native: bool,
    pub edge: Edge,
    pub local: InputCapabilities,
    pub peer: InputCapabilities,
}
/// State is emitted by this async session only. Routing is not OS/application
/// completion, and a failed cleanup remains unconfirmed even if Drop retries it.
struct InputStatus<'a> {
    native: bool,
    reporter: &'a mut super::InputReporter,
    unconfirmed: bool,
}
impl<'a> InputStatus<'a> {
    fn new(native: bool, reporter: &'a mut super::InputReporter) -> Self {
        Self {
            native,
            reporter,
            unconfirmed: false,
        }
    }
    fn routing(&mut self, outgoing: bool, incoming: bool) {
        let state = if !self.native {
            WorkerInputState::Disabled
        } else if self.unconfirmed {
            WorkerInputState::RecoveryUnconfirmed
        } else if outgoing {
            WorkerInputState::ControllingRemote
        } else if incoming {
            WorkerInputState::ReceivingRemote
        } else {
            WorkerInputState::Local
        };
        self.reporter.set(state);
    }
    fn recovering(&mut self) {
        self.reporter.set(if !self.native {
            WorkerInputState::Disabled
        } else if self.unconfirmed {
            WorkerInputState::RecoveryUnconfirmed
        } else {
            WorkerInputState::Recovering
        });
    }
    fn checked<T>(&mut self, result: Result<T>) -> Result<T> {
        if result.is_err() && self.native {
            self.unconfirmed = true;
            self.reporter.set(WorkerInputState::RecoveryUnconfirmed);
        }
        result
    }
    fn restore(&mut self, capture: Option<&platform::CaptureSession>) -> Result<()> {
        self.recovering();
        if let Some(capture) = capture {
            self.checked(capture.restore_checked())?;
        }
        Ok(())
    }
    fn release(&mut self, recovery: &mut Recovery) -> Result<()> {
        self.recovering();
        self.checked(recovery.release())
    }
    fn finish(&mut self, clean: bool) {
        self.reporter.set(if !self.native {
            WorkerInputState::Disabled
        } else if self.unconfirmed {
            WorkerInputState::RecoveryUnconfirmed
        } else if clean {
            WorkerInputState::Local
        } else {
            WorkerInputState::Unavailable
        });
    }
}

#[cfg(test)]
pub async fn run(
    c: quinn::Connection,
    tx: quinn::SendStream,
    rx: quinn::RecvStream,
    policy: Policy,
) -> Result<()> {
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let mut reporter = super::InputReporter::new(false);
    run_with_status(c, tx, rx, policy, stopped, &mut reporter).await
}

pub(super) async fn run_with_status(
    c: quinn::Connection,
    mut tx: quinn::SendStream,
    mut rx: quinn::RecvStream,
    policy: Policy,
    mut stop: tokio::sync::watch::Receiver<bool>,
    reporter: &mut super::InputReporter,
) -> Result<()> {
    let Policy {
        allow,
        native,
        edge,
        local,
        peer,
    } = policy;
    anyhow::ensure!(
        local.valid() && peer.valid(),
        "invalid session capabilities"
    );
    let mut input_status = InputStatus::new(native, reporter);
    if *stop.borrow() {
        input_status.reporter.set(WorkerInputState::Disabled);
        return Ok(());
    }
    let (itx, mut input) = mpsc::channel(128);
    let mut capture = if native {
        match platform::capture(itx, peer.clone()) {
            Ok(capture) => Some(capture),
            Err(error) => {
                input_status.reporter.set(WorkerInputState::Disabled);
                return Err(error);
            }
        }
    } else {
        None
    };
    let (mtx, mut messages) = mpsc::channel(64);
    let reader = tokio::spawn(async move {
        loop {
            let m = transport::receive(&mut rx).await;
            let failed = m.is_err();
            if mtx.send(m).await.is_err() || failed {
                break;
            }
        }
    });
    struct Abort(tokio::task::JoinHandle<()>);
    impl Drop for Abort {
        fn drop(&mut self) {
            self.0.abort();
        }
    }
    let _reader = Abort(reader);
    let mut recovery = Recovery {
        receiver: Receiver::default(),
        native,
        pending_releases: PendingReleases::default(),
    };
    let mut pending: Option<(u64, u64)> = None;
    let mut outgoing: Option<u64> = None;
    let mut next_lease = 0u64;
    let mut sequence = 0u64;
    let mut position = (0.5, 0.5);
    let mut local_return = (0.5, 0.5);
    let mut size = (1920.0, 1080.0);
    let mut last_pong = 0u64;
    let mut edge_gate = EdgeGate::default();
    let mut tick = tokio::time::interval(Duration::from_millis(200));
    let result: Result<()> = async {
    loop {
        input_status.routing(outgoing.is_some(), recovery.receiver.active().is_some());
        tokio::select! {
            _=super::stop_requested(&mut stop)=>return Ok(()),
            _=c.closed()=>return Ok(()),
            _=tick.tick()=>{
                let now=now_ms();
                if recovery.receiver.expired(now){input_status.release(&mut recovery)?;}
                if pending.is_some_and(|(_,start)|now.saturating_sub(start)>1000){pending=None;}
                if let Some(lease)=outgoing {
                    if now.saturating_sub(last_pong)>=1000 {
                        input_status.restore(capture.as_ref())?;
                        outgoing=None; send(&mut tx,&Message::Release { lease, cursor: None }).await?;
                    }else{send(&mut tx,&Message::Ping{lease}).await?;}
                }
                // A full native event queue disables capture in the callback. Reconcile protocol immediately.
                if let (Some(cap),Some(lease))=(&capture,outgoing){if !cap.gate.active(){input_status.restore(Some(cap))?;outgoing=None;send(&mut tx,&Message::Release { lease, cursor: None }).await?;}}
            },
            m=messages.recv()=>{
                let Some(m)=m else{return Ok(());};
                let m=match m {Ok(m)=>m,Err(_) if normal_close(&c)=>return Ok(()),Err(e)=>return Err(e)};
                let now=now_ms();
                match m {
                    Message::Acquire{lease,edge:remote_edge,along}=>{
                        let granted=along.is_finite() && (0.0..=1.0).contains(&along) && pending.is_none() && outgoing.is_none() && capture.as_ref().is_none_or(|cap|cap.gate.idle()) && recovery.receiver.acquire(lease,now,allow && local.motion());
                        if granted {
                            position=entry_position(remote_edge.opposite(),along);
                            input_status.checked(recovery.pointer(position.0,position.1))?;
                            send(&mut tx,&Message::Granted{lease}).await?;
                        }else{send(&mut tx,&Message::Denied{lease}).await?;}
                    },
                    Message::Granted{lease}=>{
                        let activated=if pending.is_some_and(|(id,_)|id==lease) {if let Some(cap)=&capture{input_status.checked(cap.begin(local_return.0,local_return.1))?}else{false}}else{false};
                        if activated{pending=None;outgoing=Some(lease);sequence=0;last_pong=now;if let Some(cap)=&capture{cap.gate.refresh();}}
                        else{send(&mut tx,&Message::Release { lease, cursor: None }).await?;}
                    },
                    Message::Denied{lease}=>{if pending.is_some_and(|(id,_)|id==lease){pending=None;}},
                    Message::Input{lease,event,cursor}=>{
                        if recovery.receiver.accepts(lease,now) && !local.supports(event) { bail!("peer sent input outside advertised capabilities"); }
                        if event.valid() && cursor.lease==lease && recovery.receiver.anchor(cursor,now) && recovery.receiver.input(lease,event,now) {
                            input_status.checked(recovery.pointer(cursor.x,cursor.y))?;input_status.checked(recovery.input(event))?;
                        }
                    },
                    Message::Release { lease, cursor }=>{
                        if recovery.receiver.active()==Some(lease){input_status.recovering();}
                        input_status.checked(recovery.finish(lease,cursor,now))?;
                        if outgoing==Some(lease){outgoing=None;input_status.restore(capture.as_ref())?;}
                    },
                    Message::Ping{lease}=>{if lease==0 || recovery.receiver.heartbeat(lease,now){send(&mut tx,&Message::Pong{lease}).await?;}},
                    Message::Pong{lease}=>{if outgoing==Some(lease){last_pong=now;if let Some(cap)=&capture{cap.gate.refresh();}}},
                    Message::Hello{..}=>bail!("unexpected repeated handshake"),
                }
            },
            d=c.read_datagram()=>{
                let d=match d {Ok(d)=>d,Err(_) if normal_close(&c)=>return Ok(()),Err(e)=>return Err(e.into())};
                if let Ok(m)=decode::<Motion>(&d) {if recovery.receiver.motion(m,now_ms()) {
                    input_status.checked(recovery.pointer(m.x,m.y))?;
                }}
            },
            e=input.recv(),if native=>{
                let Some(e)=e else{bail!("native capture channel closed");};
                if matches!(e,Captured::Position{..}|Captured::Input(_)|Captured::Toggle) {
                    if let Some(lease)=recovery.receiver.active(){input_status.release(&mut recovery)?;send(&mut tx,&Message::Release { lease, cursor: None }).await?;}
                }
                match e {
                    Captured::Emergency|Captured::UnsupportedInput=>{
                        if matches!(e,Captured::UnsupportedInput) {eprintln!("Unsupported input capability: stopping control; input was not forwarded.");}
                        input_status.restore(capture.as_ref())?;
                        pending=None; if let Some(lease)=outgoing.take(){send(&mut tx,&Message::Release { lease, cursor: None }).await?;}
                        if let Some(lease)=recovery.receiver.active(){input_status.release(&mut recovery)?;send(&mut tx,&Message::Release { lease, cursor: None }).await?;}
                    },
                    Captured::Position{x,y,width,height}=>{
                        size=(width.max(1.0),height.max(1.0));
                        let nx=(x/(size.0-1.0).max(1.0)).clamp(0.0,1.0);let ny=(y/(size.1-1.0).max(1.0)).clamp(0.0,1.0);
                        let cross=edge_gate.update(distance(edge,nx,ny)<=0.001,now_ms());
                        if cross && peer.motion() && capture.as_ref().is_some_and(|cap|cap.gate.idle()) && outgoing.is_none() && pending.is_none() && recovery.receiver.active().is_none(){
                            next_lease=next_lease.checked_add(1).ok_or_else(||anyhow::anyhow!("lease counter exhausted"))?;
                            let along=match edge{Edge::Left|Edge::Right=>ny,_=>nx};position=entry_position(edge.opposite(),along);local_return=entry_position(edge,along);
                            pending=Some((next_lease,now_ms()));send(&mut tx,&Message::Acquire{lease:next_lease,edge,along}).await?;
                        }
                    },
                    Captured::Toggle=>{
                        if let Some(lease)=outgoing.take(){input_status.restore(capture.as_ref())?;send(&mut tx,&Message::Release { lease, cursor: Some(Motion {lease,sequence,x:position.0,y:position.1}) }).await?;}
                        else if peer.motion() && pending.is_none() && recovery.receiver.active().is_none(){next_lease=next_lease.checked_add(1).ok_or_else(||anyhow::anyhow!("lease counter exhausted"))?;position=entry_position(edge.opposite(),0.5);local_return=entry_position(edge,0.5);pending=Some((next_lease,now_ms()));send(&mut tx,&Message::Acquire{lease:next_lease,edge,along:0.5}).await?;}
                    },
                    Captured::Motion{dx,dy}=>{if let Some(lease)=outgoing {
                        position.0=(position.0+dx/size.0).clamp(0.0,1.0);position.1=(position.1+dy/size.1).clamp(0.0,1.0);
                        sequence=sequence.checked_add(1).ok_or_else(||anyhow::anyhow!("input sequence exhausted"))?;let m=Motion{lease,sequence,x:position.0,y:position.1};
                        if returned_to_source(edge, position.0, position.1) {
                            // Same reliable stream as earlier discrete input: motion cannot
                            // overtake those events and revoke their lease at the receiver.
                            outgoing=None;
                            input_status.restore(capture.as_ref())?;
                            send(&mut tx,&Message::Release { lease, cursor: Some(m) }).await?;
                        } else {
                            // Datagram queue pressure may discard motion; the next absolute position repairs it.
                            let _=c.send_datagram(encode(&m)?.into());
                        }
                    }},
                    Captured::Input(event)=>{if let Some(lease)=outgoing{
                        if !peer.supports(event) { input_status.restore(capture.as_ref())?; outgoing=None; send(&mut tx,&Message::Release{lease,cursor:None}).await?; continue; }
                        sequence=sequence.checked_add(1).ok_or_else(||anyhow::anyhow!("input sequence exhausted"))?;let cursor=Motion{lease,sequence,x:position.0,y:position.1};
                        send(&mut tx,&Message::Input{lease,event,cursor}).await?;
                    }},
                }
            }
        }
    }    }.await;
    // Always finish local release and capture shutdown before returning to the
    // supervisor, including managed stop and network errors. Both are attempted
    // even when one fails; no canceled future bypasses this tail.
    input_status.recovering();
    let receiving = recovery.receiver.active();
    let sending = outgoing.take();
    let cleanup = input_status.checked(recovery.release());
    let shutdown = input_status.checked(
        capture
            .as_mut()
            .map(|capture| capture.shutdown())
            .transpose(),
    );
    // Notify a still-connected source/receiver after stopping local routing. A
    // queued Release is not an acknowledgement of the peer's native effects.
    let mut notification = Ok(());
    for lease in [receiving, sending].into_iter().flatten() {
        if let Err(error) = send(
            &mut tx,
            &Message::Release {
                lease,
                cursor: None,
            },
        )
        .await
        {
            if native && notification.is_ok() {
                notification = Err(error);
            }
        }
    }
    let clean = cleanup.is_ok() && shutdown.is_ok();
    input_status.finish(clean);
    cleanup?;
    shutdown?;
    notification?;
    result
}

fn returned_to_source(source_edge: Edge, x: f64, y: f64) -> bool {
    distance(source_edge.opposite(), x, y) <= 0.001
}
fn distance(edge: Edge, x: f64, y: f64) -> f64 {
    match edge {
        Edge::Left => x,
        Edge::Right => 1.0 - x,
        Edge::Top => y,
        Edge::Bottom => 1.0 - y,
    }
}
fn entry_position(edge: Edge, along: f64) -> (f64, f64) {
    match edge {
        Edge::Left => (0.002, along),
        Edge::Right => (0.998, along),
        Edge::Top => (along, 0.002),
        Edge::Bottom => (along, 0.998),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use transport::Identity;

    #[test]
    fn diagnostic_mode_never_claims_native_ownership_even_for_simulated_leases() {
        let mut reporter = super::super::InputReporter::new(false);
        {
            let mut status = InputStatus::new(false, &mut reporter);
            status.routing(false, false);
            status.routing(true, false);
            status.routing(false, true);
            status.recovering();
            assert!(status
                .checked::<()>(Err(anyhow::anyhow!("simulated failure")))
                .is_err());
            status.finish(false);
        }
        assert_eq!(reporter.observed, [WorkerInputState::Disabled]);
    }

    #[test]
    fn real_receiver_ledger_transitions_are_separate_from_connection_and_cleanup_failure_is_sticky()
    {
        // Reporting is exercised against a real Receiver ledger and fake native
        // effects. No hook, OS injection, permissions or current input are used.
        let mut recovery = held_recovery();
        let mut reporter = super::super::InputReporter::new(false);
        {
            let mut status = InputStatus::new(true, &mut reporter);
            status.routing(false, recovery.receiver.active().is_some());
            status.release(&mut recovery).unwrap();
            status.routing(false, recovery.receiver.active().is_some());
            status.routing(true, false);
            status.recovering();
            assert!(status
                .checked::<()>(Err(anyhow::anyhow!("restore rejected")))
                .is_err());
            status.routing(false, false);
            status.finish(true);
        }
        assert_eq!(
            reporter.observed,
            [
                WorkerInputState::ReceivingRemote,
                WorkerInputState::Recovering,
                WorkerInputState::Local,
                WorkerInputState::ControllingRemote,
                WorkerInputState::Recovering,
                WorkerInputState::RecoveryUnconfirmed,
            ]
        );
    }

    #[test]
    fn source_return_boundary_handles_all_edges_without_minimum_travel() {
        for edge in [Edge::Left, Edge::Right, Edge::Top, Edge::Bottom] {
            let entry = entry_position(edge.opposite(), 0.5);
            assert!(!returned_to_source(edge, entry.0, entry.1));
            assert!(!returned_to_source(edge, 0.5, 0.5));
            let boundary = match edge {
                Edge::Left => (1.0, 0.5),
                Edge::Right => (0.0, 0.5),
                Edge::Top => (0.5, 1.0),
                Edge::Bottom => (0.5, 0.0),
            };
            assert!(returned_to_source(edge, boundary.0, boundary.1));
        }
    }
    fn held_recovery() -> Recovery {
        let mut recovery = Recovery {
            receiver: Receiver::default(),
            native: false,
            pending_releases: PendingReleases::default(),
        };
        assert!(recovery.receiver.acquire(1, 0, true));
        assert!(recovery.receiver.input(
            1,
            Input::Key {
                usage: 0xe0,
                down: true
            },
            1
        ));
        recovery
    }
    #[test]
    fn terminal_pointer_failure_still_releases_pressed_input() {
        let mut recovery = held_recovery();
        let mut releases = Vec::new();
        let result = recovery.finish_with(
            1,
            Some(Motion {
                lease: 1,
                sequence: 4,
                x: 0.0,
                y: 0.7,
            }),
            2,
            |_| anyhow::bail!("pointer rejected"),
            |event| {
                releases.push(event);
                Ok(())
            },
        );
        assert!(result.is_err());
        assert!(recovery.receiver.active().is_none());
        assert!(releases.contains(&Input::Key {
            usage: 0xe0,
            down: false
        }));
    }
    #[test]
    fn terminal_release_failure_remains_pending_for_cleanup() {
        let mut recovery = held_recovery();
        let mut attempts = 0;
        assert!(recovery
            .finish_with(
                1,
                None,
                2,
                |_| Ok(()),
                |_| {
                    attempts += 1;
                    anyhow::bail!("inject rejected")
                }
            )
            .is_err());
        assert_eq!(attempts, 3);
        let mut remaining = Vec::new();
        assert!(recovery
            .pending_releases
            .flush(|event| {
                remaining.push(event);
                Ok::<_, ()>(())
            })
            .is_ok());
        assert_eq!(remaining.len(), 1);
        assert!(recovery.receiver.active().is_none());
    }

    #[test]
    fn failed_terminal_cleanup_is_not_reported_local_after_a_later_software_retry() {
        let mut recovery = held_recovery();
        let mut reporter = super::super::InputReporter::new(false);
        let mut attempts = 0;
        {
            let mut status = InputStatus::new(true, &mut reporter);
            status.routing(false, recovery.receiver.active().is_some());
            status.recovering();
            assert!(status
                .checked(recovery.finish_with(
                    1,
                    None,
                    2,
                    |_| Ok(()),
                    |_| {
                        attempts += 1;
                        anyhow::bail!("fake native release rejected")
                    },
                ))
                .is_err());
            assert_eq!(attempts, 3);
            let mut remaining = Vec::new();
            recovery
                .pending_releases
                .flush(|event| {
                    remaining.push(event);
                    Ok::<_, ()>(())
                })
                .unwrap();
            assert!(matches!(
                remaining.as_slice(),
                [Input::Key {
                    usage: 0xe0,
                    down: false
                }]
            ));
            status.routing(false, recovery.receiver.active().is_some());
            status.finish(true);
        }
        assert_eq!(
            reporter.observed,
            [
                WorkerInputState::ReceivingRemote,
                WorkerInputState::Recovering,
                WorkerInputState::RecoveryUnconfirmed,
            ]
        );
    }

    #[tokio::test]
    async fn cooperative_stop_releases_a_real_session_before_return_without_native_input(
    ) -> Result<()> {
        let a = Identity::generate()?;
        let b = Identity::generate()?;
        let ea = transport::endpoint("127.0.0.1:0".parse()?, &a, &b.certificate)?;
        let eb = transport::endpoint("127.0.0.1:0".parse()?, &b, &a.certificate)?;
        let (stop, stopped) = tokio::sync::watch::channel(false);
        let (release_seen, release_confirmed) = tokio::sync::oneshot::channel();
        let server = async {
            let c = eb.accept().await.unwrap().await?;
            let (tx, rx) = c.accept_bi().await?;
            let mut reporter = super::super::InputReporter::new(false);
            run_with_status(
                c.clone(),
                tx,
                rx,
                Policy {
                    allow: true,
                    native: false,
                    edge: Edge::Right,
                    local: InputCapabilities::new(&[0xe0], true, 1, true).unwrap(),
                    peer: InputCapabilities::default(),
                },
                stopped,
                &mut reporter,
            )
            .await?;
            assert_eq!(reporter.observed, [WorkerInputState::Disabled]);
            release_confirmed.await?;
            c.close(0u8.into(), b"stopped without native input");
            Ok::<_, anyhow::Error>(())
        };
        let client = async {
            let c = ea.connect(eb.local_addr()?, "peerglide.local")?.await?;
            let (mut tx, mut rx) = c.open_bi().await?;
            send(
                &mut tx,
                &Message::Acquire {
                    lease: 1,
                    edge: Edge::Right,
                    along: 0.5,
                },
            )
            .await?;
            assert!(matches!(
                transport::receive(&mut rx).await?,
                Message::Granted { lease: 1 }
            ));
            send(
                &mut tx,
                &Message::Input {
                    lease: 1,
                    event: Input::Key {
                        usage: 0xe0,
                        down: true,
                    },
                    cursor: Motion {
                        lease: 1,
                        sequence: 1,
                        x: 0.5,
                        y: 0.5,
                    },
                },
            )
            .await?;
            send(&mut tx, &Message::Ping { lease: 1 }).await?;
            assert!(matches!(
                transport::receive(&mut rx).await?,
                Message::Pong { lease: 1 }
            ));
            stop.send(true)?;
            assert!(matches!(
                transport::receive(&mut rx).await?,
                Message::Release {
                    lease: 1,
                    cursor: None
                }
            ));
            let _ = release_seen.send(());
            c.close(0u8.into(), b"release observed");
            Ok::<_, anyhow::Error>(())
        };
        tokio::time::timeout(Duration::from_secs(8), async {
            tokio::try_join!(server, client)
        })
        .await??;
        Ok(())
    }

    async fn exercise(allow: bool) -> Result<()> {
        let a = Identity::generate()?;
        let b = Identity::generate()?;
        let ea = transport::endpoint("127.0.0.1:0".parse()?, &a, &b.certificate)?;
        let eb = transport::endpoint("127.0.0.1:0".parse()?, &b, &a.certificate)?;
        let server = async {
            let c = eb.accept().await.unwrap().await?;
            let (tx, rx) = c.accept_bi().await?;
            run(
                c,
                tx,
                rx,
                Policy {
                    allow,
                    native: false,
                    edge: Edge::Right,
                    local: InputCapabilities::new(&[0xe0], true, 1, true).unwrap(),
                    peer: InputCapabilities::default(),
                },
            )
            .await
        };
        let client = async {
            let c = ea.connect(eb.local_addr()?, "peerglide.local")?.await?;
            let (mut tx, mut rx) = c.open_bi().await?;
            send(&mut tx, &Message::Ping { lease: 0 }).await?;
            assert!(matches!(
                transport::receive(&mut rx).await?,
                Message::Pong { lease: 0 }
            ));
            send(
                &mut tx,
                &Message::Acquire {
                    lease: 1,
                    edge: Edge::Right,
                    along: 0.5,
                },
            )
            .await?;
            let response = transport::receive(&mut rx).await?;
            if allow {
                assert!(matches!(response, Message::Granted { lease: 1 }));
                send(
                    &mut tx,
                    &Message::Input {
                        lease: 1,
                        cursor: Motion {
                            lease: 1,
                            sequence: 1,
                            x: 0.5,
                            y: 0.5,
                        },
                        event: Input::Key {
                            usage: 0xe0,
                            down: true,
                        },
                    },
                )
                .await?;
                send(&mut tx, &Message::Ping { lease: 1 }).await?;
                assert!(matches!(
                    transport::receive(&mut rx).await?,
                    Message::Pong { lease: 1 }
                ));
                send(
                    &mut tx,
                    &Message::Acquire {
                        lease: 2,
                        edge: Edge::Right,
                        along: 0.5,
                    },
                )
                .await?;
                assert!(matches!(
                    transport::receive(&mut rx).await?,
                    Message::Denied { lease: 2 }
                ));
                tokio::time::sleep(Duration::from_millis(1800)).await;
                send(
                    &mut tx,
                    &Message::Acquire {
                        lease: 3,
                        edge: Edge::Right,
                        along: 0.5,
                    },
                )
                .await?;
                assert!(matches!(
                    transport::receive(&mut rx).await?,
                    Message::Granted { lease: 3 }
                ));
                c.send_datagram(
                    encode(&Motion {
                        lease: 3,
                        sequence: 1,
                        x: 0.0,
                        y: 0.5,
                    })?
                    .into(),
                )?;
                // Let the edge datagram overtake a deliberately delayed reliable input.
                // It must not terminate the lease or discard that input.
                assert!(tokio::time::timeout(
                    Duration::from_millis(100),
                    transport::receive(&mut rx)
                )
                .await
                .is_err());
                send(
                    &mut tx,
                    &Message::Input {
                        lease: 3,
                        event: Input::Button {
                            button: 1,
                            down: true,
                        },
                        cursor: Motion {
                            lease: 3,
                            sequence: 2,
                            x: 0.5,
                            y: 0.5,
                        },
                    },
                )
                .await?;
                send(&mut tx, &Message::Ping { lease: 3 }).await?;
                assert!(matches!(
                    transport::receive(&mut rx).await?,
                    Message::Pong { lease: 3 }
                ));
                // Source-originated release shares the discrete input stream.
                send(
                    &mut tx,
                    &Message::Release {
                        lease: 3,
                        cursor: Some(Motion {
                            lease: 3,
                            sequence: 3,
                            x: 0.0,
                            y: 0.7,
                        }),
                    },
                )
                .await?;
                send(
                    &mut tx,
                    &Message::Acquire {
                        lease: 4,
                        edge: Edge::Right,
                        along: 0.5,
                    },
                )
                .await?;
                assert!(matches!(
                    transport::receive(&mut rx).await?,
                    Message::Granted { lease: 4 }
                ));
                send(
                    &mut tx,
                    &Message::Acquire {
                        lease: 1,
                        edge: Edge::Left,
                        along: 0.5,
                    },
                )
                .await?;
                assert!(matches!(
                    transport::receive(&mut rx).await?,
                    Message::Denied { lease: 1 }
                ));
            } else {
                assert!(matches!(response, Message::Denied { lease: 1 }));
            }
            c.close(0u8.into(), b"done");
            tokio::time::sleep(Duration::from_millis(50)).await;
            Ok::<_, anyhow::Error>(())
        };
        tokio::time::timeout(Duration::from_secs(8), async {
            tokio::try_join!(server, client)
        })
        .await??;
        Ok(())
    }
    async fn capability_case(motion: bool) -> Result<()> {
        let a = Identity::generate()?;
        let b = Identity::generate()?;
        let ea = transport::endpoint("127.0.0.1:0".parse()?, &a, &b.certificate)?;
        let eb = transport::endpoint("127.0.0.1:0".parse()?, &b, &a.certificate)?;
        let server = async {
            let c = eb.accept().await.unwrap().await?;
            let (tx, rx) = c.accept_bi().await?;
            let result = run(
                c.clone(),
                tx,
                rx,
                Policy {
                    allow: true,
                    native: false,
                    edge: Edge::Right,
                    local: InputCapabilities::new(&[4], motion, 1, true).unwrap(),
                    peer: InputCapabilities::default(),
                },
            )
            .await;
            if motion {
                assert!(result
                    .unwrap_err()
                    .to_string()
                    .contains("outside advertised"));
            } else {
                result?;
            }
            c.close(0u8.into(), b"test complete");
            Ok::<_, anyhow::Error>(())
        };
        let client = async {
            let c = ea.connect(eb.local_addr()?, "peerglide.local")?.await?;
            let (mut tx, mut rx) = c.open_bi().await?;
            send(
                &mut tx,
                &Message::Acquire {
                    lease: 1,
                    edge: Edge::Right,
                    along: 0.5,
                },
            )
            .await?;
            let response = transport::receive(&mut rx).await?;
            if motion {
                assert!(matches!(response, Message::Granted { lease: 1 }));
                send(
                    &mut tx,
                    &Message::Input {
                        lease: 1,
                        event: Input::Key {
                            usage: 5,
                            down: true,
                        },
                        cursor: Motion {
                            lease: 1,
                            sequence: 1,
                            x: 0.5,
                            y: 0.5,
                        },
                    },
                )
                .await?;
                c.closed().await;
            } else {
                assert!(matches!(response, Message::Denied { lease: 1 }));
                c.close(0u8.into(), b"done");
            }
            Ok::<_, anyhow::Error>(())
        };
        tokio::time::timeout(Duration::from_secs(5), async {
            tokio::try_join!(server, client)
        })
        .await??;
        Ok(())
    }
    #[tokio::test]
    async fn unsupported_input_is_rejected_before_injection() -> Result<()> {
        capability_case(true).await
    }
    #[tokio::test]
    async fn allow_flag_does_not_make_diagnostic_endpoint_controllable() -> Result<()> {
        capability_case(false).await
    }
    #[tokio::test]
    async fn default_denies_input() -> Result<()> {
        exercise(false).await
    }
    #[tokio::test]
    async fn live_session_grants_conflicts_expires_and_rejects_replay() -> Result<()> {
        exercise(true).await
    }
}
