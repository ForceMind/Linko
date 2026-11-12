//! One explicit v5 sending lease on a matching Wayland portal activation.
mod barrier;

use super::{
    routed_source::{next_motion, Link, LEASE},
    OutgoingEdges,
};
use crate::{layout_routes::Peer, transport};
use anyhow::{ensure, Context, Result};
use ashpd::desktop::{
    input_capture::{
        ActivatedBarrier, Barrier, BarrierID, BarrierPosition, Capabilities, CreateSessionOptions,
        InputCapture, ReleaseOptions, StartOptions,
    },
    Session,
};
use barrier::{Segment, Zone};
use futures_util::{future, stream, Stream, StreamExt};
use peerglide_platform::{
    now_ms,
    wayland_input::capture::{Batch, Capture, CaptureOptions, Sample},
};
use peerglide_protocol::{encode, Edge, Input, InputCapabilities, Message, Motion};
use std::{
    collections::BTreeSet,
    net::SocketAddr,
    path::Path,
    pin::Pin,
    time::Duration,
};

const POLL: Duration = Duration::from_millis(5);
const PORTAL_WAIT: Duration = Duration::from_secs(60);
const CAPTURE_WAIT: Duration = Duration::from_secs(3);
const CONNECT_WAIT: Duration = Duration::from_secs(5);
const GRANT_WAIT: Duration = Duration::from_millis(600);
const CLEANUP: Duration = Duration::from_secs(3);
const RELEASE_WAIT: Duration = Duration::from_millis(500);
const HEARTBEAT: Duration = Duration::from_millis(200);
const WATCHDOG_MS: u64 = 1_000;

pub(crate) fn parse_zone(text: &str) -> Result<(i32, i32)> {
    let (x, y) = text.split_once(',').context("expected zone X,Y")?;
    Ok((x.parse()?, y.parse()?))
}

#[derive(Clone, Copy)]
struct Activation {
    id: u32,
    segment: Segment,
    along: f64,
}

impl Activation {
    fn new(
        id: Option<u32>,
        barrier: Option<u32>,
        position: Option<(f64, f64)>,
        segments: &[Segment],
    ) -> Result<Self> {
        let id = id.context("the portal did not identify the capture activation")?;
        let segment = segments.iter().copied()
            .find(|segment| Some(segment.id) == barrier)
            .context("the portal activated an unknown or ambiguous barrier")?;
        let along = segment.fraction(position.context("the portal omitted the activation cursor")?)?;
        Ok(Self { id, segment, along })
    }
}

enum Signal {
    Activated {
        id: Option<u32>,
        barrier: Option<u32>,
        position: Option<(f64, f64)>,
    },
    Deactivated(Option<u32>),
    Disabled,
    ZonesChanged(Option<u32>),
    Closed,
    StreamEnded,
}

type Signals<'a> = stream::SelectAll<Pin<Box<dyn Stream<Item = Signal> + 'a>>>;

fn terminate<'a>(stream: impl Stream<Item = Signal> + 'a) -> Pin<Box<dyn Stream<Item = Signal> + 'a>> {
    stream.chain(stream::once(future::ready(Signal::StreamEnded))).boxed_local()
}

async fn subscribe<'a>(portal: &'a InputCapture, session: &'a Session<InputCapture>) -> Result<Signals<'a>> {
    let handle = serde_json::to_value(session)?;
    let handle = handle.as_str().context("the portal did not identify its session")?.to_owned();
    let matches = move |session: &str| session == handle;
    let activated = portal.receive_activated().await?.filter_map({
        let matches = matches.clone();
        move |event| {
            future::ready(matches(event.session_handle().as_str()).then(|| Signal::Activated {
                id: event.activation_id(),
                barrier: event.barrier_id().and_then(|barrier| match barrier {
                    ActivatedBarrier::Barrier(id) => Some(id.get()),
                    ActivatedBarrier::UnknownBarrier => None,
                }),
                position: event.cursor_position().map(|(x, y)| (f64::from(x), f64::from(y))),
            }))
        }
    });
    let deactivated = portal.receive_deactivated().await?.filter_map({
        let matches = matches.clone();
        move |event| future::ready(matches(event.session_handle().as_str()).then(|| {
            Signal::Deactivated(event.activation_id())
        }))
    });
    let disabled = portal.receive_disabled().await?.filter_map({
        let matches = matches.clone();
        move |event| future::ready(matches(event.session_handle().as_str()).then_some(Signal::Disabled))
    });
    let changed = portal.receive_zones_changed().await?.filter_map(move |event| {
        future::ready(matches(event.session_handle().as_str()).then(|| Signal::ZonesChanged(event.zone_set())))
    });
    let closed = session.receive_closed().await?.map(|_| Signal::Closed);
    Ok(stream::select_all([
        terminate(activated),
        terminate(deactivated),
        terminate(disabled),
        terminate(changed),
        terminate(closed),
    ]))
}

#[derive(Default)]
struct Route {
    link: Option<Link>,
    armed: bool,
    closed: bool,
    activation_id: Option<u32>,
    activation: Option<Activation>,
    attempted: bool,
    cursor: Option<Motion>,
}

struct Source {
    identity: transport::Identity,
    peer: Peer,
    edge: Edge,
    zone: Option<(i32, i32)>,
    options: CaptureOptions,
    offered: InputCapabilities,
}

pub(crate) async fn run(
    identity: &Path,
    certificate: &Path,
    address: SocketAddr,
    edge: Edge,
    zone: Option<(i32, i32)>,
    allowed: bool,
    options: CaptureOptions,
) -> Result<()> {
    ensure!(allowed, "Wayland pointer capture requires explicit local consent");
    let identity = transport::Identity::load(identity)?;
    let certificate = transport::read_certificate(certificate)?;
    ensure!(certificate != identity.certificate, "the Wayland source cannot control itself");
    let source = Source {
        identity,
        peer: Peer { address, certificate, secondary_layout: None },
        edge,
        zone,
        options,
        offered: options.capabilities()?,
    };
    let mut capture = Capture::reserve(options)?;
    let stop = tokio::signal::ctrl_c();
    tokio::pin!(stop);
    println!("Approve the selected devices in the Wayland InputCapture dialog. Ctrl+C cancels.");
    let portal = tokio::select! {
        biased;
        result = &mut stop => return result.map_err(Into::into),
        portal = tokio::time::timeout(PORTAL_WAIT, InputCapture::new()) => portal??,
    };
    let requested = Capabilities::POINTER
        | if options.keyboard { Capabilities::KEYBOARD } else { Capabilities::empty() };
    let (session, started) = tokio::select! {
        biased;
        result = &mut stop => return result.map_err(Into::into),
        session = tokio::time::timeout(PORTAL_WAIT, async {
            if portal.version() >= 2 {
                let session = portal.create_session2(Default::default()).await?;
                Ok::<_, anyhow::Error>((session, None))
            } else {
                let (session, granted) = portal.create_session(
                    None, CreateSessionOptions::default().set_capabilities(requested),
                ).await?;
                Ok((session, Some(granted)))
            }
        }) => session.context("Wayland capture session setup timed out")??,
    };
    let mut route = Route::default();
    let result = tokio::select! {
        biased;
        result = &mut stop => result.map_err(Into::into),
        result = run_session(&portal, &session, started, &source, &mut capture, &mut route) => result,
    };
    let local = tokio::time::timeout(CLEANUP, restore(&portal, &session, &route))
        .await
        .context("Wayland local capture recovery is unconfirmed");
    let remote = if let Some(link) = route.link.as_mut().filter(|_| route.attempted) {
        link.retire(route.cursor).await
    } else {
        Ok(())
    };
    local??;
    remote?;
    result
}

async fn restore(portal: &InputCapture, session: &Session<InputCapture>, route: &Route) -> Result<()> {
    if route.closed {
        return Ok(());
    }
    if route.armed {
        if let Some(id) = route.activation_id {
            let mut options = ReleaseOptions::default().set_activation_id(id);
            if let Some(activation) = route.activation {
                let along = route.cursor.map_or(activation.along, |cursor| {
                    match activation.segment.edge {
                        Edge::Left | Edge::Right => cursor.y,
                        Edge::Top | Edge::Bottom => cursor.x,
                    }
                });
                options = options.set_cursor_position(activation.segment.restore(along));
            }
            let _ = tokio::time::timeout(RELEASE_WAIT, portal.release(session, options)).await;
        }
        let _ = tokio::time::timeout(
            RELEASE_WAIT,
            portal.disable(session, Default::default()),
        )
        .await;
    }
    session.close().await.context("Wayland capture session did not close")
}

async fn run_session(
    portal: &InputCapture,
    session: &Session<InputCapture>,
    started: Option<Capabilities>,
    source: &Source,
    capture: &mut Capture,
    route: &mut Route,
) -> Result<()> {
    let requested = Capabilities::POINTER
        | if source.options.keyboard { Capabilities::KEYBOARD } else { Capabilities::empty() };
    let granted = if let Some(granted) = started {
        granted
    } else {
        tokio::time::timeout(PORTAL_WAIT, portal.start(
            session, None, StartOptions::default().set_capabilities(requested),
        )).await.context("Wayland capture approval timed out")??
            .response()?.capabilities()
    };
    ensure!(granted.contains(requested), "the compositor did not grant the requested capture devices");

    let mut signals = tokio::time::timeout(CAPTURE_WAIT, subscribe(portal, session)).await??;
    let (zone_set, segments, fd) = tokio::time::timeout(CAPTURE_WAIT, async {
        let zones = portal.zones(session, Default::default()).await?.response()?;
        let bounds = zones.regions().iter().map(|region| {
            Zone::new(region.x_offset(), region.y_offset(), region.width(), region.height())
        }).collect::<Result<Vec<_>>>()?;
        let segments = barrier::exposed(&bounds, source.edge, source.zone)?;
        let barriers = segments.iter().map(|segment| {
            let (x1, y1, x2, y2) = segment.position();
            let id = BarrierID::new(segment.id).context("invalid Wayland barrier ID")?;
            Ok(Barrier::new(id, BarrierPosition::new(x1, y1, x2, y2)))
        }).collect::<Result<Vec<_>>>()?;
        let selected = portal.set_pointer_barriers(session, &barriers, zones.zone_set(), Default::default())
            .await?.response()?;
        ensure!(selected.failed_barriers().is_empty(), "the compositor rejected a Wayland barrier");
        let fd = portal.connect_to_eis(session, Default::default()).await?;
        Ok::<_, anyhow::Error>((zones.zone_set(), segments, fd))
    }).await.context("Wayland capture barriers or EIS setup timed out")??;
    capture.connect(fd)?;
    tokio::time::timeout(CAPTURE_WAIT, async {
        loop {
            let batch = capture.poll(None)?;
            ensure!(!batch.pointer_started && !batch.discrete, "capture began before the portal was armed");
            if batch.ready {
                return Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(POLL).await;
        }
    }).await.context("Wayland capture devices did not initialize")??;

    let link = tokio::time::timeout(CONNECT_WAIT, Link::connect(&source.identity, &source.peer, source.offered.clone()))
        .await.context("Wayland peer connection timed out")??;
    ensure!(link.capabilities.motion(), "selected peer does not receive pointer motion");
    route.link = Some(link);
    route.armed = true;
    tokio::time::timeout(CAPTURE_WAIT, portal.enable(session, Default::default())).await??;
    println!("Move the Wayland pointer across the selected exposed edge. Ctrl+C cancels.");
    let Some(activation) = wait_activation(capture, &mut signals, route, &segments, zone_set).await? else {
        return Ok(());
    };
    route.activation = Some(activation);
    acquire_and_forward(capture, &mut signals, route, source, zone_set).await
}

async fn wait_activation(
    capture: &mut Capture,
    signals: &mut Signals<'_>,
    route: &mut Route,
    segments: &[Segment],
    zone_set: u32,
) -> Result<Option<Activation>> {
    let mut tick = tokio::time::interval(POLL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let link = route.link.as_mut().context("the Wayland peer is unavailable")?;
    loop {
        tokio::select! {
            biased;
            signal = signals.next() => {
                match signal.context("Wayland capture signal stream ended")? {
                    Signal::Activated { id, barrier, position } => {
                        route.activation_id = id;
                        return Ok(Some(Activation::new(id, barrier, position, segments)?));
                    }
                    Signal::Closed => {
                        route.closed = true;
                        return Ok(None);
                    }
                    Signal::Disabled => return Ok(None),
                    Signal::ZonesChanged(invalidated) if zone_invalidated(zone_set, invalidated) => {
                        anyhow::bail!("Wayland screen layout changed");
                    }
                    Signal::StreamEnded => anyhow::bail!("Wayland capture signal stream ended"),
                    Signal::ZonesChanged(_) | Signal::Deactivated(_) => (),
                }
            }
            _ = tick.tick() => {
                ensure!(!capture.poll(None)?.discrete, "input arrived before capture admission");
            }
            message = link.next() => {
                let Some(message) = message? else { return Ok(None); };
                link.reply(message).await?;
            }
        }
    }
}

fn zone_invalidated(current: u32, invalidated: Option<u32>) -> bool {
    invalidated.is_none_or(|id| current.wrapping_sub(id) >= (1 << 31) || id == current)
}

fn cancelled(signal: Signal, route: &mut Route, zone_set: u32, expected: u32) -> Result<bool> {
    match signal {
        Signal::Closed => {
            route.closed = true;
            Ok(true)
        }
        Signal::Disabled => Ok(true),
        Signal::Deactivated(id) => Ok(id.is_none_or(|id| id == expected)),
        Signal::ZonesChanged(id) if zone_invalidated(zone_set, id) => anyhow::bail!("Wayland screen layout changed"),
        Signal::ZonesChanged(_) => Ok(false),
        Signal::Activated { .. } => anyhow::bail!("Wayland capture activation changed"),
        Signal::StreamEnded => anyhow::bail!("Wayland capture signal stream ended"),
    }
}

async fn interruption(signals: &mut Signals<'_>, zone_set: u32, expected: u32) -> Result<Signal> {
    loop {
        let signal = signals.next().await.context("Wayland capture signal stream ended")?;
        match signal {
            Signal::Deactivated(Some(id)) if id != expected => continue,
            Signal::ZonesChanged(id) if !zone_invalidated(zone_set, id) => continue,
            _ => return Ok(signal),
        }
    }
}

fn pending_batch(capture: &mut Capture, activation: Activation) -> Result<bool> {
    let batch = capture.poll(Some(activation.id))?;
    ensure!(!batch.discrete, "release local keys and buttons before entering the remote screen");
    Ok(batch.pointer_started)
}

async fn acquire_and_forward(
    capture: &mut Capture,
    signals: &mut Signals<'_>,
    route: &mut Route,
    source: &Source,
    zone_set: u32,
) -> Result<()> {
    let activation = route.activation.context("missing Wayland activation")?;
    let edges = OutgoingEdges::new(source.edge, None);
    let mut tick = tokio::time::interval(POLL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let deadline = tokio::time::sleep(GRANT_WAIT);
    tokio::pin!(deadline);
    loop {
        if pending_batch(capture, activation)? {
            break;
        }
        tokio::select! {
            biased;
            signal = signals.next() => {
                if cancelled(signal.context("Wayland capture signal stream ended")?, route, zone_set, activation.id)? {
                    return Ok(());
                }
            }
            _ = &mut deadline => anyhow::bail!("EIS did not start the matching pointer activation"),
            _ = tick.tick() => (),
        }
    }

    route.attempted = true;
    let link = route.link.as_mut().context("the Wayland peer is unavailable")?;
    let request = Message::Acquire {
        lease: LEASE,
        edge: edges.wire_edge(),
        along: activation.along,
    };
    let sent = tokio::select! {
        biased;
        signal = interruption(signals, zone_set, activation.id) => {
            cancelled(signal?, route, zone_set, activation.id)?;
            return Ok(());
        }
        result = link.send(&request) => result?,
    };
    if !sent {
        return Ok(());
    }
    loop {
        let link = route.link.as_mut().context("the Wayland peer is unavailable")?;
        tokio::select! {
            biased;
            signal = signals.next() => {
                if cancelled(signal.context("Wayland capture signal stream ended")?, route, zone_set, activation.id)? {
                    return Ok(());
                }
            }
            _ = &mut deadline => anyhow::bail!("Wayland input lease was not granted in time"),
            _ = tick.tick() => {
                ensure!(pending_batch(capture, activation)?, "the matching Wayland activation ended");
            }
            message = link.next() => {
                let Some(message) = message? else { return Ok(()); };
                match message {
                    Message::Granted { lease: LEASE } => break,
                    Message::Denied { lease: LEASE } => {
                        route.attempted = false;
                        return Ok(());
                    }
                    Message::Release { lease: LEASE, .. } => return Ok(()),
                    other => link.reply(other).await?,
                }
            }
        }
    }
    ensure!(pending_batch(capture, activation)?, "the matching Wayland activation ended");
    println!("Wayland input lease active. Move back across the entry edge to return.");
    forward(capture, signals, route, source, zone_set, edges).await
}

struct Forward {
    position: (f64, f64),
    sequence: u64,
    keys: BTreeSet<u16>,
    buttons: BTreeSet<u8>,
    wheel: (i32, i32),
}

impl Forward {
    fn new(position: (f64, f64)) -> Self {
        Self {
            position,
            sequence: 0,
            keys: BTreeSet::new(),
            buttons: BTreeSet::new(),
            wheel: (0, 0),
        }
    }

    fn transition(&mut self, event: Input) -> bool {
        match event {
            Input::Key { usage, down: true } => self.keys.insert(usage),
            Input::Key { usage, down: false } => self.keys.remove(&usage),
            Input::Button { button, down: true } => self.buttons.insert(button),
            Input::Button { button, down: false } => self.buttons.remove(&button),
            Input::Scroll { x, y } => x != 0 || y != 0,
        }
    }

    fn emergency(&self, event: Input) -> bool {
        matches!(event, Input::Key { usage: 0x29, down: true })
            && [0xe0, 0xe4].iter().any(|usage| self.keys.contains(usage))
            && [0xe2, 0xe6].iter().any(|usage| self.keys.contains(usage))
    }

    fn scroll(&mut self, x: i32, y: i32) -> Input {
        self.wheel.0 = self.wheel.0.saturating_add(x);
        self.wheel.1 = self.wheel.1.saturating_sub(y);
        let steps = ((self.wheel.0 / 120).clamp(-12, 12), (self.wheel.1 / 120).clamp(-12, 12));
        self.wheel = (self.wheel.0 % 120, self.wheel.1 % 120);
        Input::Scroll { x: steps.0 as i16, y: steps.1 as i16 }
    }

    fn returned(&self, edges: OutgoingEdges) -> bool {
        edges.crossed_back(self.position) && self.keys.is_empty() && self.buttons.is_empty()
    }
}

async fn forward(
    capture: &mut Capture,
    signals: &mut Signals<'_>,
    route: &mut Route,
    source: &Source,
    zone_set: u32,
    edges: OutgoingEdges,
) -> Result<()> {
    let activation = route.activation.context("missing Wayland activation")?;
    let mut state = Forward::new(edges.positions(activation.along).0);
    let mut last_pong = now_ms();
    let mut tick = tokio::time::interval(POLL);
    let mut heartbeat = tokio::time::interval(HEARTBEAT);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        let link = route.link.as_mut().context("the Wayland peer is unavailable")?;
        tokio::select! {
            biased;
            signal = signals.next() => {
                if cancelled(signal.context("Wayland capture signal stream ended")?, route, zone_set, activation.id)? {
                    return Ok(());
                }
            }
            _ = heartbeat.tick() => {
                if now_ms().saturating_sub(last_pong) >= WATCHDOG_MS
                    || !link.send(&Message::Ping { lease: LEASE }).await?
                {
                    return Ok(());
                }
            }
            message = link.next() => {
                let Some(message) = message? else { return Ok(()); };
                match message {
                    Message::Pong { lease: LEASE } => last_pong = now_ms(),
                    Message::Release { lease: LEASE, .. } => return Ok(()),
                    other => link.reply(other).await?,
                }
            }
            _ = tick.tick() => {
                let batch = capture.poll(Some(activation.id))?;
                ensure!(batch.pointer_started, "the matching Wayland activation ended");
                let step = tokio::select! {
                    biased;
                    signal = interruption(signals, zone_set, activation.id) => {
                        cancelled(signal?, route, zone_set, activation.id)?;
                        return Ok(());
                    }
                    step = tokio::time::timeout(
                        HEARTBEAT,
                        submit_batch(link, &source.offered, &mut state, batch, edges, activation.segment.zone),
                    ) => step.context("Wayland input batch stalled")??,
                };
                match step {
                    ForwardStep::Continue => (),
                    ForwardStep::Stop => return Ok(()),
                    ForwardStep::Returned(cursor) => {
                        route.cursor = Some(cursor);
                        return Ok(());
                    }
                }
            }
        }
    }
}

enum ForwardStep {
    Continue,
    Stop,
    Returned(Motion),
}

async fn submit_batch(
    link: &mut Link,
    offered: &InputCapabilities,
    state: &mut Forward,
    batch: Batch,
    edges: OutgoingEdges,
    zone: Zone,
) -> Result<ForwardStep> {
    let (width, height) = zone.size();
    let mut motion = false;
    for sample in batch.samples {
        let input = match sample {
            Sample::Motion(dx, dy) => {
                let (dx, dy) = edges.motion(dx / width, dy / height);
                state.position.0 = (state.position.0 + dx).clamp(0.0, 1.0);
                state.position.1 = (state.position.1 + dy).clamp(0.0, 1.0);
                if state.returned(edges) {
                    return Ok(ForwardStep::Returned(next_motion(&mut state.sequence, state.position)?));
                }
                motion = true;
                continue;
            }
            Sample::Input(input) => input,
            Sample::Wheel(x, y) => state.scroll(x, y),
            Sample::Unsupported => return Ok(ForwardStep::Stop),
        };
        if state.emergency(input) || !offered.supports(input) || !link.capabilities.supports(input) {
            return Ok(ForwardStep::Stop);
        }
        if state.transition(input) {
            let cursor = next_motion(&mut state.sequence, state.position)?;
            if !link.send(&Message::Input { lease: LEASE, event: input, cursor }).await? {
                return Ok(ForwardStep::Stop);
            }
            motion = false;
            if state.returned(edges) {
                return Ok(ForwardStep::Returned(next_motion(&mut state.sequence, state.position)?));
            }
        }
    }
    if motion {
        let cursor = next_motion(&mut state.sequence, state.position)?;
        let _ = link.connection.send_datagram(encode(&cursor)?.into());
    }
    Ok(ForwardStep::Continue)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn capture_requires_a_known_barrier_and_explicit_local_permission() -> Result<()> {
        let args = ["peerglide", "send-wayland", "127.0.0.1:24870", "--peer-cert", "peer.der"];
        assert!(crate::Cli::try_parse_from(args).is_err());
        assert!(crate::Cli::try_parse_from(args.into_iter().chain(["--allow-pointer-capture"])).is_ok());
        assert_eq!(parse_zone("-1920,20")?, (-1920, 20));
        let zones = [Zone::new(-100, 0, 100, 100)?];
        let segments = barrier::exposed(&zones, Edge::Left, None)?;
        assert!(Activation::new(Some(20), Some(2), Some((-101.0, 25.0)), &segments).is_err());
        assert!(Activation::new(None, Some(1), Some((-101.0, 25.0)), &segments).is_err());
        let active = Activation::new(Some(20), Some(1), Some((-101.0, 25.0)), &segments)?;
        assert_eq!(active.id, 20);
        assert!(zone_invalidated(5, Some(5)));
        assert!(!zone_invalidated(5, Some(4)));
        assert!(!zone_invalidated(0, Some(u32::MAX)));
        let mut route = Route::default();
        assert!(!cancelled(Signal::Deactivated(Some(19)), &mut route, 5, 20)?);
        assert!(cancelled(Signal::Deactivated(Some(20)), &mut route, 5, 20)?);
        Ok(())
    }

    #[test]
    fn capture_keeps_key_pairs_and_wheel_remainders_with_local_escape() {
        let mut state = Forward::new((0.0, 0.5));
        let edges = OutgoingEdges::new(Edge::Right, None);
        assert!(state.returned(edges));
        assert!(state.transition(Input::Key { usage: 0xe0, down: true }));
        assert!(!state.transition(Input::Key { usage: 0xe0, down: true }));
        assert!(state.transition(Input::Key { usage: 0xe2, down: true }));
        assert!(!state.returned(edges));
        assert!(state.emergency(Input::Key { usage: 0x29, down: true }));
        assert!(state.transition(Input::Key { usage: 0xe0, down: false }));
        assert!(state.transition(Input::Key { usage: 0xe2, down: false }));
        assert!(state.returned(edges));
        assert!(state.scroll(70, -60) == Input::Scroll { x: 0, y: 0 });
        assert!(state.scroll(70, -60) == Input::Scroll { x: 1, y: 1 });
    }

    #[tokio::test]
    async fn granted_wayland_batch_preserves_held_input_through_the_return_edge() -> Result<()> {
        let sender = transport::Identity::generate()?;
        let receiver = transport::Identity::generate()?;
        let endpoint = transport::endpoint("127.0.0.1:0".parse()?, &receiver, &sender.certificate)?;
        let peer = Peer {
            address: endpoint.local_addr()?,
            certificate: receiver.certificate,
            secondary_layout: None,
        };
        let capabilities = InputCapabilities::new(&[0xe0], true, 0, false).map_err(anyhow::Error::msg)?;
        let server = async {
            let connection = endpoint.accept().await.context("listener closed")?.await?;
            transport::verify_peer(&connection, &sender.certificate)?;
            let (mut tx, mut rx) = connection.accept_bi().await?;
            crate::exchange_hello(&mut tx, &mut rx, capabilities.clone()).await?;
            assert!(matches!(
                transport::receive(&mut rx).await?,
                Message::Acquire { lease: LEASE, edge: Edge::Right, along } if along == 0.5
            ));
            transport::send(&mut tx, &Message::Granted { lease: LEASE }).await?;
            let mut sequence = 0;
            for down in [true, false] {
                let Message::Input { lease, event, cursor } = transport::receive(&mut rx).await?
                else {
                    anyhow::bail!("expected a reliable Wayland key event");
                };
                assert_eq!(lease, LEASE);
                assert!(event == (Input::Key { usage: 0xe0, down }));
                assert!(cursor.sequence > sequence && cursor.y == 0.5);
                if down {
                    assert!(cursor.x > 0.19 && cursor.x < 0.21);
                } else {
                    assert_eq!(cursor.x, 0.0);
                }
                sequence = cursor.sequence;
            }
            assert!(matches!(
                transport::receive(&mut rx).await?,
                Message::Release { lease: LEASE, cursor: Some(cursor) }
                    if cursor.sequence > sequence && cursor.x == 0.0
            ));
            connection.close(0u8.into(), super::super::NATIVE_INPUT_RECOVERED);
            endpoint.wait_idle().await;
            Ok::<_, anyhow::Error>(())
        };
        let client = async {
            let mut link = Link::connect(&sender, &peer, capabilities.clone()).await?;
            assert!(link.send(&Message::Acquire {
                lease: LEASE, edge: Edge::Right, along: 0.5,
            }).await?);
            assert!(matches!(link.next().await?, Some(Message::Granted { lease: LEASE })));
            let edges = OutgoingEdges::new(Edge::Right, None);
            let mut state = Forward::new(edges.positions(0.5).0);
            let batch = Batch {
                samples: vec![
                    Sample::Motion(200.0, 0.0),
                    Sample::Input(Input::Key { usage: 0xe0, down: true }),
                    Sample::Motion(-300.0, 0.0),
                    Sample::Input(Input::Key { usage: 0xe0, down: false }),
                ],
                ..Batch::default()
            };
            let step = submit_batch(&mut link, &capabilities, &mut state, batch, edges, Zone::new(0, 0, 1000, 800)?).await?;
            let ForwardStep::Returned(cursor) = step else {
                anyhow::bail!("the matched release did not return to the source");
            };
            link.retire(Some(cursor)).await
        };
        tokio::time::timeout(Duration::from_secs(5), async {
            tokio::try_join!(server, client)
        }).await??;
        Ok(())
    }
}
