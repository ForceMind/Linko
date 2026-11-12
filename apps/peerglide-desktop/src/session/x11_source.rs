//! One explicit Xorg source lease over the existing authenticated v5 stream.
use super::{
    routed_source::{next_motion, Link, LEASE},
    OutgoingEdges,
};
use crate::{layout_routes::Peer, transport};
use anyhow::{ensure, Context, Result};
use peerglide_core::EdgeGate;
use peerglide_platform::{
    now_ms,
    x11_activity::{PointerSample, PointerSource},
};
use peerglide_protocol::{encode, Edge, Input, InputCapabilities, Message, Motion};
use std::{
    net::SocketAddr,
    path::Path,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    thread::{self, Thread},
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, oneshot};

const NATIVE_POLL: Duration = Duration::from_millis(5);
const PENDING: Duration = Duration::from_secs(2);
const CLEANUP: Duration = Duration::from_secs(1);
const HEARTBEAT: Duration = Duration::from_millis(200);
const WATCHDOG_MS: u64 = 1_000;

#[derive(Default)]
struct Flags {
    stop: AtomicBool,
    deadline: AtomicU64,
    drain_progress: AtomicU64,
}

impl Flags {
    fn running(&self) -> bool {
        !self.stop.load(Ordering::Acquire)
    }

    fn lease_live(&self) -> bool {
        self.running() && now_ms() < self.deadline.load(Ordering::Acquire)
    }

    fn refresh(&self) {
        self.deadline
            .store(now_ms().saturating_add(WATCHDOG_MS), Ordering::Release);
    }
}

struct PointerPermissions {
    local: InputCapabilities,
    peer: InputCapabilities,
}

impl PointerPermissions {
    fn supports(&self, input: Input) -> bool {
        self.local.supports(input) && self.peer.supports(input)
    }

    fn check_mapping(&self, source: &PointerSource) -> Result<()> {
        let buttons = (1..=3).any(|button| self.supports(Input::Button { button, down: true }));
        let wheel = self.supports(Input::Scroll { x: 0, y: 1 });
        source.check_button_mapping(buttons, wheel)
    }
}

struct PointerEvent {
    dx: f64,
    dy: f64,
    input: Option<Input>,
}

struct Native {
    flags: Arc<Flags>,
    thread: Thread,
    edge: oneshot::Receiver<f64>,
    claim: Option<oneshot::Sender<PointerPermissions>>,
    ready: oneshot::Receiver<()>,
    events: mpsc::Receiver<PointerEvent>,
    done: oneshot::Receiver<Result<()>>,
}

impl Native {
    fn start(edge: Edge) -> Result<Self> {
        let flags = Arc::new(Flags::default());
        let owned = flags.clone();
        let (arrived, position) = oneshot::channel();
        let (claim, permit) = oneshot::channel();
        let (admitted, ready) = oneshot::channel();
        let (publish, events) = mpsc::channel(16);
        let (closed, done) = oneshot::channel();
        let owner = thread::Builder::new()
            .name("linko-xorg-pointer-source".into())
            .spawn(move || {
                let result = native_source(edge, &owned, arrived, permit, admitted, publish);
                owned.stop.store(true, Ordering::Release);
                let _ = closed.send(result);
            })?;
        Ok(Self {
            flags,
            thread: owner.thread().clone(),
            edge: position,
            claim: Some(claim),
            ready,
            events,
            done,
        })
    }

    fn stop(&self) {
        self.flags.stop.store(true, Ordering::Release);
        self.thread.unpark();
    }

    async fn finish(&mut self) -> Result<()> {
        self.stop();
        let mut reported = false;
        loop {
            if let Ok(done) = tokio::time::timeout(CLEANUP, &mut self.done).await {
                return done.context("Xorg source owner ended unexpectedly")?;
            }
            let progress = self.flags.drain_progress.load(Ordering::Acquire);
            ensure!(
                progress != 0 && now_ms().saturating_sub(progress - 1) < WATCHDOG_MS,
                "Xorg local pointer recovery is unconfirmed"
            );
            if !reported {
                eprintln!("Release the held mouse buttons to restore the local pointer.");
                reported = true;
            }
        }
    }
}

impl Drop for Native {
    fn drop(&mut self) {
        self.stop();
    }
}

fn native_source(
    edge: Edge,
    flags: &Flags,
    arrived: oneshot::Sender<f64>,
    mut permit: oneshot::Receiver<PointerPermissions>,
    admitted: oneshot::Sender<()>,
    publish: mpsc::Sender<PointerEvent>,
) -> Result<()> {
    let mut source = PointerSource::open()?;
    let mut gate = EdgeGate::default();
    gate.require_interior();
    let along = loop {
        if !flags.running() {
            return Ok(());
        }
        if let PointerSample::Motion { dx, dy } = source.poll()? {
            let position = source.edge_fraction(edge)?;
            match position {
                None => {
                    gate.update(false, now_ms());
                }
                Some(along) if outward(edge, dx, dy) && gate.update(true, now_ms()) => {
                    break along;
                }
                _ => (),
            }
        }
        thread::park_timeout(NATIVE_POLL);
    };
    if arrived.send(along).is_err() {
        return Ok(());
    }
    let limit = Instant::now() + PENDING;
    while flags.running() && Instant::now() < limit {
        match source.poll()? {
            PointerSample::LocalInput => return Ok(()),
            PointerSample::Motion { .. } if source.edge_fraction(edge)? != Some(along) => {
                return Ok(());
            }
            _ => (),
        }
        match permit.try_recv() {
            Ok(permissions) => {
                if !flags.lease_live() {
                    return Ok(());
                }
                permissions.check_mapping(&source)?;
                if !flags.lease_live() || !source.claim(edge, along)? {
                    return Ok(());
                }
                let result = if flags.lease_live() && admitted.send(()).is_ok() {
                    forward_native(&mut source, flags, &permissions, &publish)
                } else {
                    Ok(())
                };
                // Stop the network side before releasing the local owner.
                flags.stop.store(true, Ordering::Release);
                source.release(|| {
                    flags
                        .drain_progress
                        .store(now_ms().saturating_add(1), Ordering::Release);
                })?;
                return result;
            }
            Err(oneshot::error::TryRecvError::Closed) => return Ok(()),
            Err(oneshot::error::TryRecvError::Empty) => thread::park_timeout(NATIVE_POLL),
        }
    }
    Ok(())
}

fn forward_native(
    source: &mut PointerSource,
    flags: &Flags,
    permissions: &PointerPermissions,
    publish: &mpsc::Sender<PointerEvent>,
) -> Result<()> {
    while flags.lease_live() {
        let event = match source.poll()? {
            PointerSample::Idle => None,
            PointerSample::LocalInput => break,
            PointerSample::Motion { dx, dy } => Some(PointerEvent { dx, dy, input: None }),
            PointerSample::Button { dx, dy, number, down } => {
                let Some(input) = pointer_input(number, down) else {
                    break;
                };
                if !flags.lease_live() || !permissions.supports(input) {
                    break;
                }
                source.accept_button(number, down)?;
                let input = (number < 4 || down).then_some(input);
                (input.is_some() || dx != 0.0 || dy != 0.0)
                    .then_some(PointerEvent { dx, dy, input })
            }
        };
        if event.is_some_and(|event| !flags.lease_live() || publish.try_send(event).is_err()) {
            break;
        }
        thread::park_timeout(NATIVE_POLL);
    }
    Ok(())
}

fn pointer_input(number: u8, down: bool) -> Option<Input> {
    Some(match number {
        1..=3 => Input::Button {
            button: [1, 3, 2][usize::from(number - 1)],
            down,
        },
        4 => Input::Scroll { x: 0, y: 1 },
        5 => Input::Scroll { x: 0, y: -1 },
        6 => Input::Scroll { x: -1, y: 0 },
        7 => Input::Scroll { x: 1, y: 0 },
        _ => return None,
    })
}

fn outward(edge: Edge, dx: f64, dy: f64) -> bool {
    match edge {
        Edge::Left => dx < 0.0,
        Edge::Right => dx > 0.0,
        Edge::Top => dy < 0.0,
        Edge::Bottom => dy > 0.0,
    }
}

pub(crate) async fn run(
    identity: &Path,
    certificate: &Path,
    address: SocketAddr,
    edge: Edge,
    allow: bool,
    buttons: bool,
    scroll: bool,
) -> Result<()> {
    ensure!(
        allow,
        "Xorg pointer sending requires explicit local consent"
    );
    let identity = transport::Identity::load(identity)?;
    let certificate = transport::read_certificate(certificate)?;
    ensure!(
        certificate != identity.certificate,
        "Xorg source cannot control itself"
    );
    let peer = Peer {
        address,
        certificate,
        secondary_layout: None,
    };
    let mut native = Native::start(edge)?;
    let offered =
        InputCapabilities::new(&[], true, if buttons { 7 } else { 0 }, scroll).map_err(anyhow::Error::msg)?;
    let stop = tokio::signal::ctrl_c();
    tokio::pin!(stop);
    println!("Move the local pointer to the selected edge. Ctrl+C stops pointer sending.");
    let setup = tokio::select! {
        biased;
        stopped = &mut stop => stopped.map(|()| None).map_err(Into::into),
        result = connect_at_edge(&mut native, &identity, &peer, offered.clone()) => result.map(Some),
    };
    let (mut link, along) = match setup {
        Ok(Some(setup)) => setup,
        other => {
            native.finish().await?;
            return other.map(|_| ());
        }
    };
    let mut attempted = false;
    let result = tokio::select! {
        biased;
        stopped = &mut stop => stopped.map(|()| None).map_err(Into::into),
        result = send_pointer(&mut link, &mut native, edge, along, offered, &mut attempted) => result,
    };
    native.stop();
    let cursor = result.as_ref().ok().copied().flatten();
    let (local, remote) = tokio::join!(native.finish(), async {
        if attempted {
            link.retire(cursor).await
        } else {
            Ok(())
        }
    });
    local?;
    remote?;
    result.map(|_| ())
}

async fn connect_at_edge(
    native: &mut Native,
    identity: &transport::Identity,
    peer: &Peer,
    capabilities: InputCapabilities,
) -> Result<(Link, f64)> {
    let along = (&mut native.edge)
        .await
        .context("Xorg edge owner stopped")?;
    let link = tokio::time::timeout(PENDING, Link::connect(identity, peer, capabilities))
        .await
        .context("Xorg peer connection timed out")??;
    ensure!(
        link.capabilities.motion(),
        "selected peer does not receive pointer motion"
    );
    Ok((link, along))
}

async fn send_pointer(
    link: &mut Link,
    native: &mut Native,
    edge: Edge,
    along: f64,
    offered: InputCapabilities,
    attempted: &mut bool,
) -> Result<Option<Motion>> {
    if !native.flags.running() {
        return Ok(None);
    }
    let edges = OutgoingEdges::new(edge, None);
    *attempted = true;
    if !link
        .send(&Message::Acquire {
            lease: LEASE,
            edge: edges.wire_edge(),
            along,
        })
        .await?
    {
        return Ok(None);
    }
    let granted = tokio::time::timeout(PENDING, async {
        while let Some(message) = link.next().await? {
            match message {
                Message::Granted { lease: LEASE } => return Ok::<_, anyhow::Error>(true),
                Message::Denied { lease: LEASE } => {
                    *attempted = false;
                    return Ok(false);
                }
                Message::Release { lease: LEASE, .. } => return Ok(false),
                other => link.reply(other).await?,
            }
        }
        Ok(false)
    })
    .await
    .context("Xorg peer grant timed out")??;
    if !granted || !native.flags.running() {
        return Ok(None);
    }
    native.flags.refresh();
    let claim = native.claim.take().context("Xorg claim already used")?;
    if claim.send(PointerPermissions { local: offered, peer: link.capabilities.clone() }).is_err() {
        return Ok(None);
    }
    native.thread.unpark();
    let ready = tokio::select! {
        biased;
        _ = link.connection.closed() => false,
        result = tokio::time::timeout(CLEANUP, &mut native.ready) => matches!(result, Ok(Ok(()))),
    };
    if !ready {
        return Ok(None);
    }
    println!("Pointer lease active. Move back across the entry edge or press a local key to stop.");
    forward(link, native, edges, along).await
}

async fn forward(
    link: &mut Link,
    native: &mut Native,
    edges: OutgoingEdges,
    along: f64,
) -> Result<Option<Motion>> {
    let mut position = edges.positions(along).0;
    let mut sequence = 0;
    let mut held = [false; 3];
    let mut ticks = tokio::time::interval(HEARTBEAT);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    while native.flags.lease_live() {
        tokio::select! {
            _ = ticks.tick() => {
                if !native.flags.lease_live() || !link.send(&Message::Ping { lease: LEASE }).await? {
                    return Ok(None);
                }
            }
            message = link.next() => {
                let Some(message) = message? else { return Ok(None); };
                match message {
                    Message::Pong { lease: LEASE } if native.flags.lease_live() => native.flags.refresh(),
                    Message::Release { lease: LEASE, .. } => return Ok(None),
                    other => link.reply(other).await?,
                }
            }
            event = native.events.recv() => {
                let Some(PointerEvent { dx, dy, input }) = event.filter(|_| native.flags.lease_live()) else {
                    return Ok(None);
                };
                let (dx, dy) = edges.motion(dx, dy);
                position.0 = (position.0 + dx).clamp(0.0, 1.0);
                position.1 = (position.1 + dy).clamp(0.0, 1.0);
                let cursor = next_motion(&mut sequence, position)?;
                if let Some(event) = input {
                    ensure!(link.capabilities.supports(event), "Xorg peer capability changed");
                    if !link.send(&Message::Input { lease: LEASE, event, cursor }).await? {
                        return Ok(None);
                    }
                    if let Input::Button { button, down } = event {
                        held[usize::from(button - 1)] = down;
                    }
                }
                if edges.crossed_back(position) && !held.contains(&true) {
                    return if input.is_some() {
                        Ok(Some(next_motion(&mut sequence, position)?))
                    } else {
                        Ok(Some(cursor))
                    };
                }
                if input.is_none() {
                    let _ = link.connection.send_datagram(encode(&cursor)?.into());
                }
            }
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn xorg_send_requires_explicit_local_permission_and_outward_motion() {
        assert!(crate::Cli::try_parse_from([
            "peerglide",
            "send-x11",
            "127.0.0.1:24870",
            "--peer-cert",
            "peer.der"
        ])
        .is_err());
        assert!(crate::Cli::try_parse_from([
            "peerglide",
            "send-x11",
            "127.0.0.1:24870",
            "--peer-cert",
            "peer.der",
            "--allow-pointer-capture"
        ])
        .is_ok());
        assert!(outward(Edge::Right, 0.1, 0.0));
        assert!(!outward(Edge::Right, -0.1, 0.0));
        assert!(!outward(Edge::Right, 0.0, 0.1));
        assert!(outward(Edge::Top, 0.0, -0.1));
        let flags = Flags::default();
        assert!(!flags.lease_live());
        flags.refresh();
        assert!(flags.lease_live());
        flags.stop.store(true, Ordering::Release);
        assert!(!flags.lease_live());
    }

    #[test]
    fn standard_xorg_buttons_and_axes_use_only_the_shared_capabilities() -> Result<()> {
        let local = InputCapabilities::new(&[], true, 7, true).map_err(anyhow::Error::msg)?;
        let peer = InputCapabilities::new(&[], true, 2, false).map_err(anyhow::Error::msg)?;
        let permissions = PointerPermissions { local, peer };
        assert_eq!(pointer_input(3, true), Some(Input::Button { button: 2, down: true }));
        assert!(permissions.supports(pointer_input(3, true).unwrap()));
        assert!(permissions.supports(pointer_input(3, false).unwrap()));
        assert!(!permissions.supports(pointer_input(1, true).unwrap()));
        assert!(!permissions.supports(pointer_input(4, true).unwrap()));
        assert_eq!(pointer_input(4, true), Some(Input::Scroll { x: 0, y: 1 }));
        assert_eq!(pointer_input(5, true), Some(Input::Scroll { x: 0, y: -1 }));
        assert_eq!(pointer_input(6, true), Some(Input::Scroll { x: -1, y: 0 }));
        assert_eq!(pointer_input(7, true), Some(Input::Scroll { x: 1, y: 0 }));
        assert!(pointer_input(8, true).is_none());
        Ok(())
    }

    #[tokio::test]
    async fn xorg_route_waits_for_the_grant_and_retires_with_the_return_cursor() -> Result<()> {
        let source = transport::Identity::generate()?;
        let destination = transport::Identity::generate()?;
        let endpoint =
            transport::endpoint("127.0.0.1:0".parse()?, &destination, &source.certificate)?;
        let peer = Peer {
            address: endpoint.local_addr()?,
            certificate: destination.certificate,
            secondary_layout: None,
        };
        let capabilities =
            InputCapabilities::new(&[], true, 7, true).map_err(anyhow::Error::msg)?;
        let (observed, acquire_received) = oneshot::channel();
        let (checked, can_grant) = oneshot::channel();
        let (arrived, motion_received) = oneshot::channel();
        let (claim, mut permit) = oneshot::channel();
        let (admitted, ready) = oneshot::channel();
        let (publish, events) = mpsc::channel(2);
        let (_no_edge, edge) = oneshot::channel();
        let (_closed, done) = oneshot::channel();
        let mut native = Native {
            flags: Arc::new(Flags::default()),
            thread: thread::current(),
            edge,
            claim: Some(claim),
            ready,
            events,
            done,
        };
        let server = async {
            let connection = endpoint.accept().await.context("listener closed")?.await?;
            transport::verify_peer(&connection, &source.certificate)?;
            let (mut send, mut receive) = connection.accept_bi().await?;
            crate::exchange_hello(&mut send, &mut receive, capabilities.clone()).await?;
            assert!(matches!(
                transport::receive(&mut receive).await?,
                Message::Acquire { lease: LEASE, edge: Edge::Right, along } if along == 0.5
            ));
            observed.send(()).expect("native gate observer");
            can_grant.await?;
            transport::send(&mut send, &Message::Granted { lease: LEASE }).await?;
            let bytes = connection.read_datagram().await?;
            let cursor =
                peerglide_protocol::decode::<Motion>(&bytes).map_err(anyhow::Error::msg)?;
            assert_eq!(cursor.lease, LEASE);
            assert!(cursor.x > 0.19 && cursor.x < 0.21 && cursor.y == 0.5);
            arrived.send(()).expect("native motion observer");
            let mut expected = [
                Input::Button { button: 2, down: true },
                Input::Scroll { x: -1, y: 0 },
                Input::Button { button: 2, down: false },
            ]
            .into_iter();
            let mut sequence = cursor.sequence;
            loop {
                match transport::receive(&mut receive).await? {
                    Message::Ping { lease: LEASE } => {
                        transport::send(&mut send, &Message::Pong { lease: LEASE }).await?;
                    }
                    Message::Input { lease: LEASE, event, cursor } => {
                        assert_eq!(Some(event), expected.next());
                        assert!(cursor.sequence > sequence && cursor.y == 0.5);
                        sequence = cursor.sequence;
                    }
                    Message::Release {
                        lease: LEASE,
                        cursor: Some(cursor),
                    } => {
                        assert!(expected.next().is_none());
                        assert!(cursor.sequence > sequence && cursor.x == 0.0);
                        break;
                    }
                    _ => anyhow::bail!("unexpected Xorg route frame"),
                }
            }
            connection.close(0u8.into(), super::super::NATIVE_INPUT_RECOVERED);
            endpoint.wait_idle().await;
            Ok::<_, anyhow::Error>(())
        };
        let native_input = async {
            acquire_received.await?;
            assert!(matches!(
                permit.try_recv(),
                Err(oneshot::error::TryRecvError::Empty)
            ));
            checked.send(()).expect("grant observer");
            let permissions = permit.await?;
            assert!(permissions.supports(Input::Scroll { x: -1, y: 0 }));
            admitted.send(()).expect("native admission observer");
            publish.send(PointerEvent { dx: 0.2, dy: 0.0, input: None }).await?;
            motion_received.await?;
            for (dx, input) in [
                (0.0, pointer_input(3, true)),
                (-0.3, None),
                (0.0, pointer_input(6, true)),
                (0.0, pointer_input(3, false)),
            ] {
                publish.send(PointerEvent { dx, dy: 0.0, input }).await?;
            }
            // Keep the source available until the return cursor is consumed.
            publish.closed().await;
            Ok::<_, anyhow::Error>(())
        };
        let client = async {
            let mut link = Link::connect(&source, &peer, capabilities.clone()).await?;
            let mut attempted = false;
            let cursor = send_pointer(&mut link, &mut native, Edge::Right, 0.5, capabilities.clone(), &mut attempted)
                .await?
                .context("missing return cursor")?;
            assert!(attempted);
            native.stop();
            native.events.close();
            link.retire(Some(cursor)).await
        };
        tokio::time::timeout(Duration::from_secs(5), async {
            tokio::try_join!(server, native_input, client)
        })
        .await??;
        Ok(())
    }

    #[test]
    #[ignore = "requires the dedicated GitHub Actions Xvfb server"]
    fn xorg_source_watchdog_releases_the_native_pointer_owner() -> Result<()> {
        use peerglide_platform::{x11_activity::Activity, x11_inject::Injector, ReceivePoint};

        ensure!(
            std::env::var("GITHUB_ACTIONS").ok().as_deref() == Some("true")
                && std::env::var("LINKO_TEST_ISOLATED_X11").ok().as_deref() == Some("1"),
            "this test requires the dedicated CI display"
        );
        let mut source = PointerSource::open()?;
        let injector = Injector::open_after_explicit_consent()?;
        injector.pointer(ReceivePoint { x: 1.0, y: 0.5 })?;
        assert!(matches!(source.poll()?, PointerSample::Idle));
        let along = source
            .edge_fraction(Edge::Right)?
            .context("right edge not reached")?;
        assert!(source.claim(Edge::Right, along)?);
        assert!(Activity::open_for_local_takeover().is_err());
        injector.pointer(ReceivePoint { x: 0.0, y: 0.5 })?;
        assert_eq!(source.edge_fraction(Edge::Right)?, Some(along));
        let flags = Flags::default();
        flags.deadline.store(now_ms() + 50, Ordering::Release);
        let (publish, _events) = mpsc::channel(1);
        let permissions = PointerPermissions {
            local: InputCapabilities::default(),
            peer: InputCapabilities::default(),
        };
        forward_native(&mut source, &flags, &permissions, &publish)?;
        assert!(!flags.lease_live());
        source.release(|| {})?;
        injector.pointer(ReceivePoint { x: 0.0, y: 0.5 })?;
        assert!(source.edge_fraction(Edge::Left)?.is_some());
        drop(source);
        assert!(Activity::open_for_local_takeover().is_ok());
        Ok(())
    }
}
