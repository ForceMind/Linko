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
use peerglide_protocol::{encode, Edge, InputCapabilities, Message, Motion};
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

struct Native {
    flags: Arc<Flags>,
    thread: Thread,
    edge: oneshot::Receiver<f64>,
    claim: Option<oneshot::Sender<()>>,
    ready: oneshot::Receiver<()>,
    motions: mpsc::Receiver<(f64, f64)>,
    done: oneshot::Receiver<Result<()>>,
}

impl Native {
    fn start(edge: Edge) -> Result<Self> {
        let flags = Arc::new(Flags::default());
        let owned = flags.clone();
        let (arrived, position) = oneshot::channel();
        let (claim, permit) = oneshot::channel();
        let (admitted, ready) = oneshot::channel();
        let (motion, motions) = mpsc::channel(16);
        let (closed, done) = oneshot::channel();
        let owner = thread::Builder::new()
            .name("linko-xorg-pointer-source".into())
            .spawn(move || {
                let result = native_source(edge, &owned, arrived, permit, admitted, motion);
                owned.stop.store(true, Ordering::Release);
                let _ = closed.send(result);
            })?;
        Ok(Self {
            flags,
            thread: owner.thread().clone(),
            edge: position,
            claim: Some(claim),
            ready,
            motions,
            done,
        })
    }

    fn stop(&self) {
        self.flags.stop.store(true, Ordering::Release);
        self.thread.unpark();
    }

    async fn finish(&mut self) -> Result<()> {
        self.stop();
        tokio::time::timeout(CLEANUP, &mut self.done)
            .await
            .context("Xorg local pointer recovery is unconfirmed")?
            .context("Xorg source owner ended unexpectedly")?
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
    mut permit: oneshot::Receiver<()>,
    admitted: oneshot::Sender<()>,
    motion: mpsc::Sender<(f64, f64)>,
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
            Ok(()) => {
                if !flags.lease_live() || !source.claim(edge, along)? {
                    return Ok(());
                }
                let result = if flags.lease_live() && admitted.send(()).is_ok() {
                    forward_native(&source, flags, &motion)
                } else {
                    Ok(())
                };
                // Stop the network side before releasing the local owner.
                flags.stop.store(true, Ordering::Release);
                source.release()?;
                return result;
            }
            Err(oneshot::error::TryRecvError::Closed) => return Ok(()),
            Err(oneshot::error::TryRecvError::Empty) => thread::park_timeout(NATIVE_POLL),
        }
    }
    Ok(())
}

fn forward_native(
    source: &PointerSource,
    flags: &Flags,
    motion: &mpsc::Sender<(f64, f64)>,
) -> Result<()> {
    while flags.lease_live() {
        match source.poll()? {
            PointerSample::LocalInput => break,
            PointerSample::Motion { dx, dy } if flags.lease_live() => {
                if motion.try_send((dx, dy)).is_err() {
                    break;
                }
            }
            _ => (),
        }
        thread::park_timeout(NATIVE_POLL);
    }
    Ok(())
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
) -> Result<()> {
    ensure!(allow, "Xorg pointer sending requires explicit local consent");
    let identity = transport::Identity::load(identity)?;
    let certificate = transport::read_certificate(certificate)?;
    ensure!(certificate != identity.certificate, "Xorg source cannot control itself");
    let peer = Peer {
        address,
        certificate,
        secondary_layout: None,
    };
    let mut native = Native::start(edge)?;
    let stop = tokio::signal::ctrl_c();
    tokio::pin!(stop);
    println!("Move the local pointer to the selected edge. Ctrl+C stops pointer sending.");
    let setup = tokio::select! {
        biased;
        stopped = &mut stop => stopped.map(|()| None).map_err(Into::into),
        result = connect_at_edge(&mut native, &identity, &peer) => result.map(Some),
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
        result = send_pointer(&mut link, &mut native, edge, along, &mut attempted) => result,
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
) -> Result<(Link, f64)> {
    let along = (&mut native.edge).await.context("Xorg edge owner stopped")?;
    let capabilities = InputCapabilities::new(&[], true, 0, false).map_err(anyhow::Error::msg)?;
    let link = tokio::time::timeout(PENDING, Link::connect(identity, peer, capabilities))
        .await
        .context("Xorg peer connection timed out")??;
    ensure!(link.capabilities.motion(), "selected peer does not receive pointer motion");
    Ok((link, along))
}

async fn send_pointer(
    link: &mut Link,
    native: &mut Native,
    edge: Edge,
    along: f64,
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
    if claim.send(()).is_err() {
        return Ok(None);
    }
    native.thread.unpark();
    if !matches!(tokio::time::timeout(CLEANUP, &mut native.ready).await, Ok(Ok(()))) {
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
    let mut ticks = tokio::time::interval(HEARTBEAT);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    while native.flags.lease_live() {
        tokio::select! {
            _ = ticks.tick() => {
                if !link.send(&Message::Ping { lease: LEASE }).await? {
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
            event = native.motions.recv() => {
                let Some((dx, dy)) = event.filter(|_| native.flags.lease_live()) else {
                    return Ok(None);
                };
                let (dx, dy) = edges.motion(dx, dy);
                position.0 = (position.0 + dx).clamp(0.0, 1.0);
                position.1 = (position.1 + dy).clamp(0.0, 1.0);
                let cursor = next_motion(&mut sequence, position)?;
                if edges.crossed_back(position) {
                    return Ok(Some(cursor));
                }
                let _ = link.connection.send_datagram(encode(&cursor)?.into());
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
            "peerglide", "send-x11", "127.0.0.1:24870", "--peer-cert", "peer.der"
        ])
        .is_err());
        assert!(crate::Cli::try_parse_from([
            "peerglide", "send-x11", "127.0.0.1:24870", "--peer-cert", "peer.der",
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
    #[ignore = "requires the dedicated GitHub Actions Xvfb server"]
    fn xorg_source_watchdog_releases_the_native_pointer_owner() -> Result<()> {
        use peerglide_platform::{
            x11_activity::Activity, x11_inject::Injector, ReceivePoint,
        };

        ensure!(
            std::env::var("GITHUB_ACTIONS").ok().as_deref() == Some("true")
                && std::env::var("LINKO_TEST_ISOLATED_X11").ok().as_deref() == Some("1"),
            "this test requires the dedicated CI display"
        );
        let mut source = PointerSource::open()?;
        let injector = Injector::open_after_explicit_consent()?;
        injector.pointer(ReceivePoint { x: 1.0, y: 0.5 })?;
        assert!(matches!(source.poll()?, PointerSample::Idle));
        let along = source.edge_fraction(Edge::Right)?.context("right edge not reached")?;
        assert!(source.claim(Edge::Right, along)?);
        assert!(Activity::open_for_local_takeover().is_err());
        injector.pointer(ReceivePoint { x: 0.0, y: 0.5 })?;
        assert_eq!(source.edge_fraction(Edge::Right)?, Some(along));
        let flags = Flags::default();
        flags.deadline.store(now_ms() + 50, Ordering::Release);
        let (motion, _events) = mpsc::channel(1);
        forward_native(&source, &flags, &motion)?;
        assert!(!flags.lease_live());
        source.release()?;
        injector.pointer(ReceivePoint { x: 0.0, y: 0.5 })?;
        assert!(source.edge_fraction(Edge::Left)?.is_some());
        drop(source);
        assert!(Activity::open_for_local_takeover().is_ok());
        Ok(())
    }
}
