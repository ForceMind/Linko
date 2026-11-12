//! One native capture owner with a fresh pinned connection per outgoing lease.
use super::*;
use crate::{
    exchange_hello,
    layout_probe::unspecified_for,
    layout_routes::{Peer, Routes},
};
use anyhow::{ensure, Context};
use platform::{
    display_layout::EdgeAnchor, display_routes::Route, CaptureEvents, CaptureSession, RouteReturn,
};
use std::path::Path;

const LEASE: u64 = 1;
type Ending<'a> = Pin<&'a mut dyn Future<Output = Result<()>>>;

pub(crate) async fn run(
    identity: &Path,
    plan: &Path,
    expected: Option<(&str, &str)>,
    mut stop: tokio::sync::watch::Receiver<bool>,
    reporter: &mut crate::InputReporter,
) -> Result<()> {
    if *stop.borrow() || stop.has_changed().is_err() {
        return Ok(());
    }
    let layout = monitor_layout(true)
        .context("configured native routing needs a Windows or macOS display snapshot")?;
    let identity = transport::Identity::load(identity)?;
    if let Some((_, local)) = expected {
        ensure!(identity.fingerprint() == local, "local identity changed after route confirmation");
    }
    let routes = Routes::load_for_launch(plan, &identity.certificate, &layout, expected.map(|(layout, _)| layout))?;
    if *stop.borrow() || stop.has_changed().is_err() {
        return Ok(());
    }
    let (tx, mut input) = platform::capture_event_channel();
    let mut capture = platform::capture_routed_source(tx)?;
    let driver = capture.take_receive_driver()?;
    let _registration = driver.as_ref().map(|driver| crate::NativeRecoveryRegistration::new(
        driver.control(), *stop.borrow() || stop.has_changed().is_err(),
    ));
    reporter.set(WorkerInputState::Local);
    crate::worker_event(reporter.managed, crate::WorkerState::Listening);
    let ending = async {
        tokio::select! {
            biased;
            _ = crate::stop_requested(&mut stop) => Ok(()),
            changed = watch_display_layout(Some(&layout)) => changed,
        }
    };
    tokio::pin!(ending);
    let result = Source {
        layout: &layout,
        routes: &routes,
        capture: &capture,
        input: &mut input,
        ending: ending.as_mut(),
        reporter,
    }
    .run(&identity)
    .await;

    let mut status = InputStatus::new(true, reporter);
    let started = std::time::Instant::now();
    let first = (capture.shutdown_poll(), capture.shutdown_pending());
    let shutdown = drain_capture(
        first,
        || (capture.shutdown_poll(), capture.shutdown_pending()),
        &mut status,
        started,
        Duration::from_millis(500),
    )
    .await;
    status.finish(shutdown.is_ok());
    shutdown?;
    result
}

#[derive(Clone, Copy)]
struct Choice<'a> {
    anchor: EdgeAnchor,
    route: Route,
    peer: &'a Peer,
    epoch: CaptureEpoch,
}

impl Choice<'_> {
    fn same_segment(self, next: Self) -> bool {
        self.epoch == next.epoch && self.route.segment == next.route.segment
    }

    fn edges(self) -> OutgoingEdges {
        OutgoingEdges::new(self.anchor.edge, Some(self.route.target.edge))
    }
}

enum Exit {
    Local(Option<Motion>),
    Stop(Result<()>),
}

struct Source<'a> {
    layout: &'a Layout,
    routes: &'a Routes,
    capture: &'a CaptureSession,
    input: &'a mut CaptureEvents,
    ending: Ending<'a>,
    reporter: &'a mut crate::InputReporter,
}

impl<'a> Source<'a> {
    fn local_position(&self, event: Captured) -> Option<((f64, f64), CaptureEpoch)> {
        let Captured::Position {
            x,
            y,
            route: CaptureRoute::Local(epoch),
            ..
        } = event
        else {
            return None;
        };
        (x.is_finite() && y.is_finite() && self.capture.gate.is_current(CaptureRoute::Local(epoch)))
            .then_some(((x, y), epoch))
    }

    fn choose(&self, point: (f64, f64), epoch: CaptureEpoch) -> Option<Choice<'a>> {
        let routes = self.routes;
        let (anchor, route, peer) = routes.select_at(self.layout, point)?;
        if route.target.display.is_some() {
            return None;
        }
        Some(Choice {
            anchor,
            route,
            peer,
            epoch,
        })
    }

    fn update_choice(&self, event: Captured, chosen: &mut Choice<'a>) -> bool {
        let Some((point, epoch)) = self.local_position(event) else {
            return true;
        };
        let Some(next) = self
            .choose(point, epoch)
            .filter(|next| chosen.same_segment(*next))
        else {
            return false;
        };
        *chosen = next;
        true
    }

    async fn run(&mut self, identity: &transport::Identity) -> Result<()> {
        let mut rearm = session_edge_gate(true);
        loop {
            let event = tokio::select! {
                biased;
                stop = &mut self.ending => return stop,
                event = self.input.recv() => event.context("native capture channel closed")?,
            };
            if matches!(event, Captured::Emergency) {
                return Ok(());
            }
            let Some((point, epoch)) = self.local_position(event) else {
                continue;
            };
            let Some(chosen) = self.choose(point, epoch) else {
                if is_interior(self.layout, point) {
                    rearm.update(false, now_ms());
                }
                continue;
            };
            if !rearm.update(true, now_ms()) {
                continue;
            }
            let Some(token) = self.capture.gate.prepare_commit(epoch) else {
                continue;
            };
            let request = PendingAcquire::new(LEASE, token, now_ms());
            self.reporter.switch(crate::WorkerSwitchState::Pending);
            let dial = tokio::time::timeout(
                Duration::from_millis(request.deadline.saturating_sub(now_ms())),
                Link::connect(identity, chosen.peer),
            );
            let dialed = tokio::select! {
                biased;
                stop = &mut self.ending => return stop,
                result = dial => result,
            };
            let mut link = match dialed {
                Ok(Ok(link)) if link.capabilities.motion() => link,
                _ => {
                    eprintln!("Selected route is unavailable or has no native input capability.");
                    self.reporter.switch(crate::WorkerSwitchState::PeerUnavailable);
                    continue;
                }
            };
            let mut attempted = false;
            let result = self
                .acquire(&mut link, chosen, request, &mut attempted)
                .await;
            // Stop suppression before waiting on any peer. Every possibly sent
            // Acquire is retired on its own connection before the next route.
            let local = if attempted {
                self.reporter.set(WorkerInputState::Recovering);
                self.capture.restore_checked()
            } else {
                Ok(())
            };
            let peer = if attempted {
                let cursor = match &result {
                    Ok(Exit::Local(cursor)) => *cursor,
                    _ => None,
                };
                link.retire(cursor).await
            } else {
                Ok(())
            };
            if local.is_err() || peer.is_err() {
                self.reporter.set(WorkerInputState::RecoveryUnconfirmed);
            }
            local?;
            peer?;
            self.reporter.set(WorkerInputState::Local);
            self.reporter.switch(crate::WorkerSwitchState::Idle);
            if let Exit::Stop(stop) = result? {
                return stop;
            }
        }
    }

    async fn acquire(
        &mut self,
        link: &mut Link,
        mut chosen: Choice<'a>,
        request: PendingAcquire,
        attempted: &mut bool,
    ) -> Result<Exit> {
        let current_layout = tokio::select! {
            biased;
            stop = &mut self.ending => return Ok(Exit::Stop(stop)),
            _ = pending_wake(Some(request.deadline)) => return Ok(Exit::Local(None)),
            layout = tokio::task::spawn_blocking(|| monitor_layout(true)) => layout?,
        };
        if let Some(change) = layout_change(Some(self.layout), current_layout.as_ref()) {
            return Err(change.into());
        }
        if let Some(exit) = self.check_choice(&mut chosen, request).await? {
            return Ok(exit);
        }
        let edges = chosen.edges();
        let along = chosen.route.fraction;
        let acquire = tokio::select! {
            biased;
            stop = &mut self.ending => return Ok(Exit::Stop(stop)),
            _ = pending_wake(Some(request.deadline)) => return Ok(Exit::Local(None)),
            acquire = link.acquire_proposal(edges.wire_edge(), along) => acquire,
        };
        let Some(acquire) = acquire else {
            eprintln!("Selected route did not confirm the configured receive display.");
            return Ok(Exit::Local(None));
        };
        if let Some(exit) = self.check_choice(&mut chosen, request).await? {
            return Ok(exit);
        }
        *attempted = true;
        if !link.send(&acquire).await? {
            return Ok(Exit::Local(None));
        }
        let mut pending = Some(request);
        loop {
            if now_ms() >= request.deadline || !self.capture.gate.commit_pending(request.token) {
                return Ok(Exit::Local(None));
            }
            tokio::select! {
                biased;
                stop = &mut self.ending => return Ok(Exit::Stop(stop)),
                message = link.next() => {
                    let Some(message) = message? else { return Ok(Exit::Local(None)); };
                    match message {
                        Message::Granted { lease: LEASE } => {
                            if !pending.as_mut().is_some_and(|request| request.grant(LEASE, now_ms())) {
                                return Ok(Exit::Local(None));
                            }
                        }
                        Message::Denied { lease: LEASE } | Message::Release { lease: LEASE, .. } => return Ok(Exit::Local(None)),
                        other => link.reply(other).await?,
                    }
                }
                _ = pending_wake(pending.map(PendingAcquire::wake_at)), if self.input.position_current() => {
                    let step = advance_pending(&mut pending, now_ms(), self.capture.gate.commit_pending(request.token), |token, deadline| {
                        let position = RouteReturn::new(self.layout, chosen.anchor.display, entry_position(chosen.anchor.edge, chosen.anchor.fraction))
                            .context("configured route source display is unavailable")?;
                        self.capture.begin_configured_route(token, deadline, position, &link.capabilities, || self.input.position_current())
                    })?;
                    match step {
                        PendingStep::Committed { epoch, .. } => {
                            let bounds = display_bounds(self.layout, chosen.anchor.display).context("configured source display disappeared")?;
                            let size = (bounds.right - bounds.left, bounds.bottom - bounds.top);
                            self.reporter.switch(crate::WorkerSwitchState::Idle);
                            self.reporter.set(WorkerInputState::ControllingRemote);
                            return self.forward(link, edges, entry_position(edges.remote, along), size, epoch).await;
                        }
                        PendingStep::Cancelled { .. } => return Ok(Exit::Local(None)),
                        PendingStep::Waiting => (),
                    }
                }
                event = self.input.recv() => {
                    let event = event.context("native capture channel closed")?;
                    if matches!(event, Captured::Emergency) { return Ok(Exit::Stop(Ok(()))); }
                    if !self.update_choice(event, &mut chosen) { return Ok(Exit::Local(None)); }
                }
            }
        }
    }

    async fn check_choice(
        &mut self,
        chosen: &mut Choice<'a>,
        request: PendingAcquire,
    ) -> Result<Option<Exit>> {
        loop {
            if now_ms() >= request.deadline || !self.capture.gate.commit_pending(request.token) {
                return Ok(Some(Exit::Local(None)));
            }
            if self.input.position_current() {
                return Ok(None);
            }
            let event = tokio::select! {
                biased;
                stop = &mut self.ending => return Ok(Some(Exit::Stop(stop))),
                _ = pending_wake(Some(request.deadline)) => return Ok(Some(Exit::Local(None))),
                event = self.input.recv() => event.context("native capture channel closed")?,
            };
            if matches!(event, Captured::Emergency) {
                return Ok(Some(Exit::Stop(Ok(()))));
            }
            if !self.update_choice(event, chosen) {
                return Ok(Some(Exit::Local(None)));
            }
        }
    }

    async fn forward(
        &mut self,
        link: &mut Link,
        edges: OutgoingEdges,
        mut position: (f64, f64),
        size: (f64, f64),
        epoch: CaptureEpoch,
    ) -> Result<Exit> {
        let remote = CaptureRoute::Remote(epoch);
        let mut last_pong = now_ms();
        self.capture.gate.refresh();
        let mut outbox = MotionOutbox::new(tokio::time::Instant::now());
        let mut sequence = 0u64;
        let mut tick = tokio::time::interval(Duration::from_millis(200));
        loop {
            if !self.capture.gate.is_current(remote) {
                return Ok(Exit::Local(None));
            }
            tokio::select! {
                stop = &mut self.ending => return Ok(Exit::Stop(stop)),
                _ = tick.tick() => {
                    if now_ms().saturating_sub(last_pong) >= 1000 { return Ok(Exit::Local(None)); }
                    if self.input.is_empty() && edges.crossed_back(position) && self.capture.restore_at_boundary(epoch)? {
                        return Ok(Exit::Local(Some(next_motion(&mut sequence, position)?)));
                    }
                    if !link.send(&Message::Ping { lease: LEASE }).await? { return Ok(Exit::Local(None)); }
                }
                message = link.next() => {
                    let Some(message) = message? else { return Ok(Exit::Local(None)); };
                    match message {
                        Message::Release { lease: LEASE, .. } => return Ok(Exit::Local(None)),
                        Message::Pong { lease: LEASE } => {
                            last_pong = now_ms();
                            self.capture.gate.refresh();
                        }
                        other => link.reply(other).await?,
                    }
                }
                event = self.input.recv() => {
                    let event = event.context("native capture channel closed")?;
                    if matches!(event, Captured::Emergency) { return Ok(Exit::Stop(Ok(()))); }
                    if matches!(event, Captured::UnsupportedInput) { return Ok(Exit::Local(None)); }
                    if captured_route(event) != Some(remote) || !self.capture.gate.is_current(remote) { continue; }
                    match event {
                        Captured::Motion { dx, dy, .. } => {
                            let (dx, dy) = edges.motion(dx / size.0, dy / size.1);
                            position.0 = (position.0 + dx).clamp(0.0, 1.0);
                            position.1 = (position.1 + dy).clamp(0.0, 1.0);
                            let cursor = next_motion(&mut sequence, position)?;
                            if edges.crossed_back(position) && self.capture.restore_at_boundary(epoch)? {
                                return Ok(Exit::Local(Some(cursor)));
                            }
                            outbox.latest = Some(cursor);
                        }
                        Captured::Input { event, .. } if !is_zero_scroll(event) => {
                            if !link.capabilities.supports(event) { return Ok(Exit::Local(None)); }
                            let cursor = next_motion(&mut sequence, position)?;
                            outbox.latest = None;
                            if !link.send(&Message::Input { lease: LEASE, event, cursor }).await? { return Ok(Exit::Local(None)); }
                        }
                        Captured::Toggle { .. } => return Ok(Exit::Local(Some(next_motion(&mut sequence, position)?))),
                        _ => (),
                    }
                }
                _ = tokio::time::sleep_until(outbox.next_send), if outbox.latest.is_some() => {
                    if let Some(cursor) = outbox.take_due(tokio::time::Instant::now()).filter(|_| self.capture.gate.is_current(remote)) {
                        let _ = link.connection.send_datagram(encode(&cursor)?.into());
                    }
                }
            }
        }
    }
}

fn display_bounds(
    layout: &Layout,
    id: platform::display_layout::DisplayId,
) -> Option<platform::display_layout::Bounds> {
    layout
        .displays()
        .iter()
        .find(|display| display.id == id)
        .map(|display| display.bounds)
}

fn is_interior(layout: &Layout, (x, y): (f64, f64)) -> bool {
    layout
        .display_at(x, y)
        .and_then(|id| display_bounds(layout, id))
        .is_some_and(|bounds| {
            x > bounds.left + 1.0
                && x < bounds.right - 1.0
                && y > bounds.top + 1.0
                && y < bounds.bottom - 1.0
        })
}

fn next_motion(sequence: &mut u64, (x, y): (f64, f64)) -> Result<Motion> {
    *sequence = sequence
        .checked_add(1)
        .context("input sequence exhausted")?;
    let cursor = Motion {
        lease: LEASE,
        sequence: *sequence,
        x,
        y,
    };
    ensure!(cursor.valid(), "invalid native motion");
    Ok(cursor)
}

struct Link {
    connection: quinn::Connection,
    tx: quinn::SendStream,
    messages: mpsc::Receiver<Result<Message>>,
    capabilities: InputCapabilities,
    secondary: Option<crate::secondary_layout::Routes>,
    _reader: Abort,
    _endpoint: quinn::Endpoint,
}

impl Link {
    async fn connect(identity: &transport::Identity, peer: &Peer) -> Result<Self> {
        let endpoint =
            transport::endpoint(unspecified_for(peer.address), identity, &peer.certificate)?;
        let connection = endpoint.connect(peer.address, "peerglide.local")?.await?;
        transport::verify_peer(&connection, &peer.certificate)?;
        let (mut tx, mut rx) = connection.open_bi().await?;
        let capabilities = exchange_hello(&mut tx, &mut rx, platform::input_capabilities()).await?;
        let secondary = crate::secondary_layout::Routes::start(
            identity,
            &peer.certificate,
            &connection,
            peer.secondary_layout,
            None,
            None,
        )?;
        let (sender, messages) = mpsc::channel(64);
        let reader = tokio::spawn(async move {
            loop {
                let message = transport::receive(&mut rx).await;
                let failed = message.is_err();
                if sender.send(message).await.is_err() || failed {
                    break;
                }
            }
        });
        Ok(Self {
            connection,
            tx,
            messages,
            capabilities,
            secondary,
            _reader: Abort(reader),
            _endpoint: endpoint,
        })
    }

    async fn acquire_proposal(&self, edge: Edge, along: f64) -> Option<Message> {
        let confirmed = match &self.secondary {
            Some(layout) => {
                layout
                    .reserve_outbound(&self.connection, LEASE, edge, along)
                    .await
            }
            None => true,
        };
        confirmed.then_some(Message::Acquire {
            lease: LEASE,
            edge,
            along,
        })
    }

    async fn reply(&mut self, message: Message) -> Result<()> {
        match message {
            Message::Acquire { lease, .. } => send(&mut self.tx, &Message::Denied { lease }).await,
            Message::Ping { lease: 0 } => send(&mut self.tx, &Message::Pong { lease: 0 }).await,
            Message::Hello { .. } | Message::Input { .. } => {
                bail!("unexpected input on a send-only route")
            }
            _ => Ok(()),
        }
    }

    async fn next(&mut self) -> Result<Option<Message>> {
        match self.messages.recv().await {
            Some(Ok(message)) => Ok(Some(message)),
            Some(Err(error)) if error.is::<transport::InvalidMessageFrame>() => Err(error),
            Some(Err(error))
                if error.is::<transport::MessageStreamEnded>()
                    || self.connection.close_reason().is_some() =>
            {
                Ok(None)
            }
            Some(Err(error)) => Err(error),
            None => Ok(None),
        }
    }

    async fn send(&mut self, message: &Message) -> Result<bool> {
        match send(&mut self.tx, message).await {
            Ok(()) => Ok(true),
            Err(_) if self.connection.close_reason().is_some() => Ok(false),
            Err(error) => Err(error),
        }
    }

    async fn retire(&mut self, cursor: Option<Motion>) -> Result<()> {
        if self.connection.close_reason().is_none() {
            // Even a write racing with close can succeed through the peer's
            // independently confirmed cleanup. The exact close is the fence.
            let _ = send(
                &mut self.tx,
                &Message::Release {
                    lease: LEASE,
                    cursor,
                },
            )
            .await;
            let _ = self.tx.finish();
        }
        let closed = tokio::time::timeout(Duration::from_secs(3), self.connection.closed())
            .await
            .context("selected route did not confirm input recovery")?;
        ensure!(
            matches!(closed, quinn::ConnectionError::ApplicationClosed(ref close)
                if close.error_code == 0u8.into() && close.reason.as_ref() == NATIVE_INPUT_RECOVERED),
            "selected route did not confirm input recovery"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_stop_before_routed_start_reports_disabled_without_loading_identity() -> Result<()> {
        let (_stop, stopped) = tokio::sync::watch::channel(true);
        let mut reporter = crate::InputReporter::new(false);
        reporter.set(WorkerInputState::Disabled);
        run(Path::new("missing-identity"), Path::new("missing-plan"), None, stopped, &mut reporter).await?;
        assert_eq!(reporter.finish(), WorkerInputState::Disabled);
        Ok(())
    }

    #[tokio::test]
    async fn routed_secondary_proposes_acquire_only_after_the_bound_reservation() -> Result<()> {
        use transport::layout_reservation as wire;

        let source = transport::Identity::generate()?;
        let target = transport::Identity::generate()?;
        let bind = "127.0.0.1:0".parse()?;
        for allowed in [true, false] {
            let input = transport::endpoint(bind, &target, &source.certificate)?;
            let layout = wire::endpoint(bind, &target, &source.certificate)?;
            let peer = Peer {
                address: input.local_addr()?,
                certificate: target.certificate.clone(),
                secondary_layout: Some(layout.local_addr()?),
            };
            let receiver = async {
                let c = input
                    .accept()
                    .await
                    .context("input endpoint closed")?
                    .await?;
                let (mut tx, mut rx) = c.accept_bi().await?;
                exchange_hello(&mut tx, &mut rx, InputCapabilities::default()).await?;
                let sidecar = layout
                    .accept()
                    .await
                    .context("layout endpoint closed")?
                    .await?;
                let slot = wire::ReservationSlot::default();
                let published =
                    wire::accept_for_slot(&sidecar, &c, &source.certificate, &slot, |proposal| {
                        assert_eq!(proposal.lease, LEASE);
                        assert_eq!(proposal.edge, Edge::Right);
                        assert_eq!(proposal.fraction, 0.25);
                        allowed.then_some(1)
                    })
                    .await?;
                assert_eq!(published, allowed);
                if allowed {
                    let msg = transport::receive(&mut rx).await?;
                    let Message::Acquire { lease, edge, along } = msg else {
                        bail!("expected reserved acquire")
                    };
                    let grant = slot
                        .consume(&c, &source.certificate, lease, 1)?
                        .context("reservation missing")?;
                    assert_eq!(
                        (grant.lease, grant.edge, grant.fraction),
                        (lease, edge, along)
                    );
                } else {
                    assert!(
                        matches!(transport::receive(&mut rx).await, Err(error) if error.is::<transport::MessageStreamEnded>())
                    );
                }
                c.close(0u8.into(), b"test ended");
                Ok::<_, anyhow::Error>(())
            };
            let sender = async {
                let mut link = Link::connect(&source, &peer).await?;
                let acquire = link.acquire_proposal(Edge::Right, 0.25).await;
                assert_eq!(acquire.is_some(), allowed);
                if let Some(acquire) = acquire {
                    assert!(link.send(&acquire).await?);
                }
                link.tx.finish()?;
                let _ = link.connection.closed().await;
                Ok::<_, anyhow::Error>(())
            };
            tokio::time::timeout(Duration::from_secs(5), async {
                tokio::try_join!(receiver, sender)
            })
            .await??;
        }
        Ok(())
    }

    #[derive(Clone, Copy)]
    enum ReleaseOrder {
        SourceFirst,
        PeerFirst,
        InvalidFrame,
    }

    #[tokio::test]
    async fn pinned_route_releases_the_same_lease_before_accepting_the_close_receipt() -> Result<()>
    {
        let source = transport::Identity::generate()?;
        let first = transport::Identity::generate()?;
        let second = transport::Identity::generate()?;
        let bind = "127.0.0.1:0".parse()?;
        for (target, confirmed, order) in [
            (&first, true, ReleaseOrder::SourceFirst),
            (&second, true, ReleaseOrder::PeerFirst),
            (&first, false, ReleaseOrder::SourceFirst),
            (&second, false, ReleaseOrder::InvalidFrame),
        ] {
            let endpoint = transport::endpoint(bind, target, &source.certificate)?;
            let peer = Peer {
                address: endpoint.local_addr()?,
                certificate: target.certificate.clone(),
                secondary_layout: None,
            };
            let (saw_fin, fin_observed) = tokio::sync::oneshot::channel();
            let receive = async {
                let c = endpoint
                    .accept()
                    .await
                    .context("receiver endpoint closed")?
                    .await?;
                transport::verify_peer(&c, &source.certificate)?;
                let (mut tx, mut rx) = c.accept_bi().await?;
                exchange_hello(
                    &mut tx,
                    &mut rx,
                    InputCapabilities::new(&[], true, 0, false).unwrap(),
                )
                .await?;
                assert!(matches!(
                    transport::receive(&mut rx).await?,
                    Message::Acquire { lease: LEASE, .. }
                ));
                send(&mut tx, &Message::Granted { lease: LEASE }).await?;
                match order {
                    ReleaseOrder::SourceFirst => {
                        assert!(matches!(
                            transport::receive(&mut rx).await?,
                            Message::Release {
                                lease: LEASE,
                                cursor: None
                            }
                        ));
                        assert!(
                            matches!(transport::receive(&mut rx).await, Err(error) if error.is::<transport::MessageStreamEnded>())
                        );
                    }
                    ReleaseOrder::PeerFirst => {
                        tx.finish()?;
                        fin_observed.await?;
                    }
                    ReleaseOrder::InvalidFrame => {
                        tx.write_all(&[0; 4]).await?;
                        tx.finish()?;
                        fin_observed.await?;
                    }
                }
                // Isolated receiver fixture. No Native driver or OS input is used.
                let reason: &[u8] = if confirmed {
                    NATIVE_INPUT_RECOVERED
                } else {
                    b"session ended"
                };
                c.close(0u8.into(), reason);
                endpoint.wait_idle().await;
                Ok::<_, anyhow::Error>(())
            };
            let handoff = async {
                let mut link = Link::connect(&source, &peer).await?;
                send(
                    &mut link.tx,
                    &Message::Acquire {
                        lease: LEASE,
                        edge: Edge::Right,
                        along: 0.5,
                    },
                )
                .await?;
                assert!(matches!(
                    link.next().await?,
                    Some(Message::Granted { lease: LEASE })
                ));
                match order {
                    ReleaseOrder::SourceFirst => (),
                    ReleaseOrder::PeerFirst => {
                        assert!(link.next().await?.is_none());
                        assert!(saw_fin.send(()).is_ok());
                    }
                    ReleaseOrder::InvalidFrame => {
                        assert!(
                            matches!(link.next().await, Err(error) if error.is::<transport::InvalidMessageFrame>())
                        );
                        assert!(saw_fin.send(()).is_ok());
                    }
                }
                let retired = link.retire(None).await;
                assert_eq!(retired.is_ok(), confirmed);
                Ok::<_, anyhow::Error>(())
            };
            tokio::time::timeout(Duration::from_secs(6), async {
                tokio::try_join!(receive, handoff)
            })
            .await??;
        }
        Ok(())
    }
}
