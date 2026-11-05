use anyhow::{bail, Result};
use peerglide_core::{EdgeGate, PendingReleases, Receiver};
use peerglide_platform::{self as platform, now_ms, CaptureEpoch, CaptureRoute, Captured};
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
    fn input(&mut self, lease: u64, event: Input, cursor: Motion, now: u64) -> Result<bool> {
        let native = self.native;
        self.input_with(
            lease,
            event,
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
    fn input_with(
        &mut self,
        lease: u64,
        event: Input,
        cursor: Motion,
        now: u64,
        pointer: impl FnOnce(Motion) -> Result<()>,
        inject: impl FnOnce(Input) -> Result<()>,
    ) -> Result<bool> {
        if !self.receiver.accepts_input(lease, event, now)
            || cursor.lease != lease
            || !self.receiver.anchor(cursor, now)
        {
            return Ok(false);
        }
        // Positioning can fail before injection. Never retire an owned release
        // before either operation succeeds; session cleanup must still retry it.
        pointer(cursor)?;
        self.receiver.input_with(lease, event, now, inject)
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
#[derive(Clone, Copy)]
struct Outgoing {
    lease: u64,
    epoch: CaptureEpoch,
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

async fn drain_capture(
    initial: (Result<bool>, bool),
    mut poll: impl FnMut() -> (Result<bool>, bool),
    status: &mut InputStatus<'_>,
    started: std::time::Instant,
    budget: Duration,
) -> Result<()> {
    let mut state = initial;
    let mut failure = None;
    loop {
        let (result, pending) = state;
        match result {
            Ok(true) if !pending => return failure.map_or(Ok(()), Err),
            Err(error) => {
                if failure.is_none() {
                    failure = status.checked::<()>(Err(error)).err();
                }
            }
            _ => (),
        }
        if !pending {
            return Err(failure.unwrap_or_else(|| anyhow::anyhow!("capture ended without confirmed cleanup")));
        }
        if started.elapsed() >= budget && failure.is_none() {
            failure = status.checked::<()>(Err(anyhow::anyhow!(
                "native capture cleanup is still pending; waiting for the original thread and reaper"
            ))).err();
        }
        // The UI can still send stop or explicitly force the process. Ordinary
        // stop must not kill a hook that still owes a suppressed physical Up.
        tokio::time::sleep(Duration::from_millis(20)).await;
        state = poll();
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
    let mut outgoing: Option<Outgoing> = None;
    let mut incoming_epoch: Option<CaptureEpoch> = None;
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
                if recovery.receiver.expired(now){input_status.release(&mut recovery)?;incoming_epoch=None;}
                if pending.is_some_and(|(_,start)|now.saturating_sub(start)>1000){pending=None;}
                if let Some(Outgoing {lease,..})=outgoing {
                    if now.saturating_sub(last_pong)>=1000 {
                        input_status.restore(capture.as_ref())?;
                        outgoing=None; send(&mut tx,&Message::Release { lease, cursor: None }).await?;
                    }else{send(&mut tx,&Message::Ping{lease}).await?;}
                }
                // A full native event queue disables capture in the callback. Reconcile protocol immediately.
                if let (Some(cap),Some(current))=(&capture,outgoing){if !cap.gate.is_current(CaptureRoute::Remote(current.epoch)){input_status.restore(Some(cap))?;outgoing=None;send(&mut tx,&Message::Release { lease: current.lease, cursor: None }).await?;}}
            },
            m=messages.recv()=>{
                let Some(m)=m else{return Ok(());};
                let m=match m {Ok(m)=>m,Err(_) if normal_close(&c)=>return Ok(()),Err(e)=>return Err(e)};
                let now=now_ms();
                match m {
                    Message::Acquire{lease,edge:remote_edge,along}=>{
                        let mut granted=along.is_finite() && (0.0..=1.0).contains(&along) && pending.is_none() && outgoing.is_none() && capture.as_ref().is_none_or(|cap|cap.gate.idle()) && recovery.receiver.acquire(lease,now,allow && local.motion());
                        if granted {
                            incoming_epoch=capture.as_ref().and_then(|cap|cap.gate.try_local_epoch());
                            if capture.is_some() && incoming_epoch.is_none(){input_status.release(&mut recovery)?;granted=false;}
                        }
                        if granted {
                            edge_gate.require_interior();
                            position=entry_position(remote_edge.opposite(),along);
                            input_status.checked(recovery.pointer(position.0,position.1))?;
                            send(&mut tx,&Message::Granted{lease}).await?;
                        }else{send(&mut tx,&Message::Denied{lease}).await?;}
                    },
                    Message::Granted{lease}=>{
                        let activated=if pending.is_some_and(|(id,_)|id==lease) {if let Some(cap)=&capture{input_status.checked(cap.begin_routed(local_return.0,local_return.1))?}else{None}}else{None};
                        if let Some(epoch)=activated{edge_gate.require_interior();pending=None;outgoing=Some(Outgoing{lease,epoch});sequence=0;last_pong=now;if let Some(cap)=&capture{cap.gate.refresh();}}
                        else{send(&mut tx,&Message::Release { lease, cursor: None }).await?;}
                    },
                    Message::Denied{lease}=>{if pending.is_some_and(|(id,_)|id==lease){pending=None;}},
                    Message::Input{lease,event,cursor}=>{
                        if recovery.receiver.accepts(lease,now) && !local.supports(event) { bail!("peer sent input outside advertised capabilities"); }
                        input_status.checked(recovery.input(lease,event,cursor,now))?;
                    },
                    Message::Release { lease, cursor }=>{
                        if recovery.receiver.active()==Some(lease){input_status.recovering();incoming_epoch=None;}
                        input_status.checked(recovery.finish(lease,cursor,now))?;
                        if outgoing.is_some_and(|current|current.lease==lease){outgoing=None;input_status.restore(capture.as_ref())?;}
                    },
                    Message::Ping{lease}=>{if lease==0 || recovery.receiver.heartbeat(lease,now){send(&mut tx,&Message::Pong{lease}).await?;}},
                    Message::Pong{lease}=>{if outgoing.is_some_and(|current|current.lease==lease){last_pong=now;if let Some(cap)=&capture{cap.gate.refresh();}}},
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
                if let Some(route)=captured_route(e){if !capture.as_ref().is_some_and(|cap|cap.gate.is_current(route)){continue;}}
                if let Some(lease)=local_takeover(e,recovery.receiver.active().zip(incoming_epoch),&mut edge_gate) {
                    input_status.release(&mut recovery)?;
                    incoming_epoch=None;
                    send(&mut tx,&Message::Release { lease, cursor: None }).await?;
                    // This event only reclaims this computer. It cannot also
                    // request control of the peer in the same select iteration.
                    continue;
                }
                match e {
                    Captured::Emergency|Captured::UnsupportedInput=>{
                        edge_gate.require_interior();
                        if matches!(e,Captured::UnsupportedInput) {eprintln!("Unsupported input capability: stopping control; input was not forwarded.");}
                        input_status.restore(capture.as_ref())?;
                        pending=None; if let Some(Outgoing{lease,..})=outgoing.take(){send(&mut tx,&Message::Release { lease, cursor: None }).await?;}
                        if let Some(lease)=recovery.receiver.active(){input_status.release(&mut recovery)?;incoming_epoch=None;send(&mut tx,&Message::Release { lease, cursor: None }).await?;}
                    },
                    Captured::Position{x,y,width,height,route}=>{
                        if !matches!(route,CaptureRoute::Local(_)){continue;}
                        size=(width.max(1.0),height.max(1.0));
                        let nx=(x/(size.0-1.0).max(1.0)).clamp(0.0,1.0);let ny=(y/(size.1-1.0).max(1.0)).clamp(0.0,1.0);
                        let cross=edge_gate.update(distance(edge,nx,ny)<=0.001,now_ms());
                        if cross && peer.motion() && capture.as_ref().is_some_and(|cap|cap.gate.idle()) && outgoing.is_none() && pending.is_none() && recovery.receiver.active().is_none(){
                            next_lease=next_lease.checked_add(1).ok_or_else(||anyhow::anyhow!("lease counter exhausted"))?;
                            let along=match edge{Edge::Left|Edge::Right=>ny,_=>nx};position=entry_position(edge.opposite(),along);local_return=entry_position(edge,along);
                            pending=Some((next_lease,now_ms()));send(&mut tx,&Message::Acquire{lease:next_lease,edge,along}).await?;
                        }
                    },
                    Captured::Toggle{route}=>{
                        if let Some(Outgoing{lease,..})=outgoing.take(){input_status.restore(capture.as_ref())?;send(&mut tx,&Message::Release { lease, cursor: Some(Motion {lease,sequence,x:position.0,y:position.1}) }).await?;}
                        else if matches!(route,CaptureRoute::Local(_)) && peer.motion() && pending.is_none() && recovery.receiver.active().is_none(){next_lease=next_lease.checked_add(1).ok_or_else(||anyhow::anyhow!("lease counter exhausted"))?;position=entry_position(edge.opposite(),0.5);local_return=entry_position(edge,0.5);pending=Some((next_lease,now_ms()));send(&mut tx,&Message::Acquire{lease:next_lease,edge,along:0.5}).await?;}
                    },
                    Captured::Motion{dx,dy,route}=>{if let Some(lease)=capture.as_ref().and_then(|cap|forwarding_lease(route,outgoing,&cap.gate)) {
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
                    Captured::Input{event,route}=>{if let Some(lease)=capture.as_ref().and_then(|cap|forwarding_lease(route,outgoing,&cap.gate)){
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
    let sending = outgoing.take().map(|current| current.lease);
    let cleanup = input_status.checked(recovery.release());
    let stop_started = std::time::Instant::now();
    let first_shutdown = capture.as_mut().map(|capture| {
        let result = capture.shutdown_poll();
        (result, capture.shutdown_pending())
    });
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
    let shutdown = if let (Some(capture), Some(first)) = (capture.as_mut(), first_shutdown) {
        drain_capture(first, || {
            let result = capture.shutdown_poll();
            (result, capture.shutdown_pending())
        }, &mut input_status, stop_started, Duration::from_millis(500)).await
    } else {
        Ok(())
    };
    let shutdown = input_status.checked(shutdown);
    let clean = cleanup.is_ok() && shutdown.is_ok();
    input_status.finish(clean);
    cleanup?;
    shutdown?;
    notification?;
    result
}

fn captured_route(event: Captured) -> Option<CaptureRoute> {
    match event {
        Captured::Position { route, .. } | Captured::Motion { route, .. }
            | Captured::Input { route, .. } | Captured::Toggle { route } => Some(route),
        Captured::Emergency | Captured::UnsupportedInput => None,
    }
}
fn forwarding_lease(route: CaptureRoute, outgoing: Option<Outgoing>, gate: &platform::CaptureGate) -> Option<u64> {
    let current = outgoing?;
    (route == CaptureRoute::Remote(current.epoch) && gate.is_current(route)).then_some(current.lease)
}
fn local_takeover(event: Captured, receiving: Option<(u64, CaptureEpoch)>, edge_gate: &mut EdgeGate) -> Option<u64> {
    if !matches!(event, Captured::Position { .. } | Captured::Input { .. } | Captured::Toggle { .. }) {
        return None;
    }
    let (lease, epoch) = receiving?;
    if captured_route(event) != Some(CaptureRoute::Local(epoch)) {
        return None;
    }
    edge_gate.require_interior();
    Some(lease)
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

    #[tokio::test]
    async fn shutdown_timeout_keeps_the_worker_future_alive_until_the_original_capture_drains() {
        use std::sync::atomic::{AtomicBool, Ordering};
        for initial_failure in [false, true] {
            let drained = AtomicBool::new(false);
            let mut reporter = super::super::InputReporter::new(false);
            {
                let mut status = InputStatus::new(true, &mut reporter);
                let first = if initial_failure { Err(anyhow::anyhow!("fake pending restore failure")) } else { Ok(false) };
                let future = drain_capture((first, true), || {
                    let done = drained.load(Ordering::Acquire);
                    (Ok(done), !done)
                }, &mut status, std::time::Instant::now(), Duration::ZERO);
                tokio::pin!(future);
                tokio::select! {
                    _ = &mut future => panic!("pending capture must not let the worker exit"),
                    _ = tokio::time::sleep(Duration::from_millis(10)) => (),
                }
                drained.store(true, Ordering::Release);
                assert!(future.await.is_err());
            }
            assert_eq!(reporter.last, Some(WorkerInputState::RecoveryUnconfirmed));
        }
    }
    #[tokio::test]
    async fn already_reaped_source_failure_does_not_wait_forever() {
        let mut reporter = super::super::InputReporter::new(false);
        let mut status = InputStatus::new(true, &mut reporter);
        let result = tokio::time::timeout(Duration::from_secs(1), drain_capture(
            (Err(anyhow::anyhow!("fake source lost after reap")), false),
            || panic!("already reaped capture must not be polled again"),
            &mut status, std::time::Instant::now(), Duration::ZERO,
        )).await.unwrap();
        assert!(result.is_err());
        assert_eq!(reporter.last, Some(WorkerInputState::RecoveryUnconfirmed));
    }

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
    #[test]
    fn physical_takeover_at_each_edge_cannot_immediately_reverse_control() {
        for edge in [Edge::Left, Edge::Right, Edge::Top, Edge::Bottom] {
            let mut gate = EdgeGate::default();
            let capture = platform::CaptureGate::default();
            let epoch = capture.try_local_epoch().unwrap();
            let (nx, ny) = match edge {
                Edge::Left => (0.0, 0.5),
                Edge::Right => (1.0, 0.5),
                Edge::Top => (0.5, 0.0),
                Edge::Bottom => (0.5, 1.0),
            };
            let event = Captured::Position {
                x: nx * 1919.0,
                y: ny * 1079.0,
                width: 1920.0,
                height: 1080.0,
                route: CaptureRoute::Local(epoch),
            };
            assert_eq!(local_takeover(event, Some((7, epoch)), &mut gate), Some(7));
            assert!(!gate.update(distance(edge, nx, ny) <= 0.001, 1000));
            assert!(!gate.update(false, 1001));
            assert!(gate.update(true, 1002));
            assert_eq!(local_takeover(event, None, &mut gate), None);
        }
    }
    #[test]
    fn queued_local_and_old_remote_routes_cannot_enter_a_new_outgoing_lease() {
        let gate = platform::CaptureGate::default();
        let local = gate.route().unwrap();
        assert!(gate.try_activate());
        let first = gate.route().unwrap();
        let CaptureRoute::Remote(epoch) = first else { panic!("expected remote route") };
        let outgoing = Some(Outgoing { lease: 1, epoch });
        assert_eq!(forwarding_lease(local, outgoing, &gate), None);
        assert_eq!(forwarding_lease(first, outgoing, &gate), Some(1));
        gate.disable();
        assert_eq!(forwarding_lease(first, outgoing, &gate), None);
        assert!(gate.try_activate());
        let second = gate.route().unwrap();
        let CaptureRoute::Remote(epoch) = second else { panic!("expected remote route") };
        let outgoing = Some(Outgoing { lease: 2, epoch });
        assert_eq!(forwarding_lease(first, outgoing, &gate), None);
        assert_eq!(forwarding_lease(local, outgoing, &gate), None);
        assert_eq!(forwarding_lease(second, outgoing, &gate), Some(2));
    }
    #[test]
    fn local_events_queued_before_an_incoming_fence_do_not_reclaim_it() {
        let capture = platform::CaptureGate::default();
        let old_route = capture.route().unwrap();
        let epoch = capture.try_local_epoch().unwrap();
        let mut gate = EdgeGate::default();
        let event = Captured::Input { event: Input::Key { usage: 4, down: false }, route: old_route };
        assert_eq!(local_takeover(event, Some((7, epoch)), &mut gate), None);
        let event = Captured::Input { event: Input::Key { usage: 4, down: true }, route: CaptureRoute::Local(epoch) };
        assert_eq!(local_takeover(event, Some((7, epoch)), &mut gate), Some(7));
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
    fn normal_up_failures_keep_key_and_button_ownership_until_cleanup() -> Result<()> {
        for (down, up) in [
            (
                Input::Key {
                    usage: 0xe0,
                    down: true,
                },
                Input::Key {
                    usage: 0xe0,
                    down: false,
                },
            ),
            (
                Input::Button {
                    button: 1,
                    down: true,
                },
                Input::Button {
                    button: 1,
                    down: false,
                },
            ),
        ] {
            for fail_pointer in [true, false] {
                let mut recovery = Recovery {
                    receiver: Receiver::default(),
                    native: false,
                    pending_releases: PendingReleases::default(),
                };
                assert!(recovery.receiver.acquire(1, 0, true));
                let cursor = Motion {
                    lease: 1,
                    sequence: 1,
                    x: 0.5,
                    y: 0.5,
                };
                assert!(recovery.input_with(1, down, cursor, 1, |_| Ok(()), |_| Ok(()))?);
                let mut up_attempts = 0;
                assert!(recovery
                    .input_with(
                        1,
                        up,
                        Motion {
                            sequence: 2,
                            ..cursor
                        },
                        2,
                        |_| {
                            if fail_pointer {
                                anyhow::bail!("fake pointer failure before Up");
                            }
                            Ok(())
                        },
                        |_| {
                            up_attempts += 1;
                            anyhow::bail!("fake Up injection failure")
                        },
                    )
                    .is_err());
                assert_eq!(up_attempts, usize::from(!fail_pointer));
                let mut released = Vec::new();
                recovery.finish_with(
                    1,
                    None,
                    3,
                    |_| Ok(()),
                    |event| {
                        released.push(event);
                        Ok(())
                    },
                )?;
                assert!(released == [up]);
                recovery.finish_with(
                    1,
                    None,
                    4,
                    |_| Ok(()),
                    |_| panic!("acknowledged cleanup must not replay an Up"),
                )?;
            }
        }
        Ok(())
    }
    #[test]
    fn successful_up_and_unowned_up_never_move_or_release_again() -> Result<()> {
        let mut recovery = held_recovery();
        let up = Input::Key {
            usage: 0xe0,
            down: false,
        };
        let cursor = Motion {
            lease: 1,
            sequence: 1,
            x: 0.5,
            y: 0.5,
        };
        assert!(recovery.input_with(1, up, cursor, 2, |_| Ok(()), |_| Ok(()))?);
        assert!(!recovery.input_with(
            1,
            up,
            Motion {
                sequence: 2,
                ..cursor
            },
            3,
            |_| panic!("unowned Up must not position the pointer"),
            |_| panic!("unowned Up must not reach the native injector"),
        )?);
        recovery.finish_with(
            1,
            None,
            4,
            |_| Ok(()),
            |_| panic!("successful Up was already acknowledged"),
        )?;
        Ok(())
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
