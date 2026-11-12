//! One v5 input lease on a locally approved RemoteDesktop session.
use super::session::{entry_position, send, Abort, NATIVE_INPUT_RECOVERED};
use anyhow::{ensure, Context, Result};
use peerglide_core::Receiver;
use peerglide_platform::{
    now_ms,
    wayland_input::{Options, WaylandInput},
    ReceivePoint,
};
use peerglide_protocol::{decode, Input, InputCapabilities, Message, Motion};
use peerglide_transport as transport;
use std::{future::Future, net::SocketAddr, path::Path, time::Duration};
use tokio::sync::mpsc;

const POLL: Duration = Duration::from_millis(5);
const ACQUIRE: Duration = Duration::from_millis(600);

pub(super) fn parse_region(text: &str) -> Result<(u32, u32)> {
    let (x, y) = text.split_once(',').context("expected region X,Y")?;
    Ok((x.parse()?, y.parse()?))
}

trait InputOwner {
    fn capabilities(&self) -> Result<InputCapabilities>;
    fn check(&mut self) -> Result<()>;
    async fn begin(&mut self, point: ReceivePoint) -> Result<()>;
    fn motion(&mut self, point: ReceivePoint) -> Result<()>;
    fn input(&mut self, point: ReceivePoint, event: Input) -> Result<()>;
}

impl InputOwner for WaylandInput {
    fn capabilities(&self) -> Result<InputCapabilities> {
        Ok(WaylandInput::capabilities(self))
    }

    fn check(&mut self) -> Result<()> {
        WaylandInput::check(self)
    }

    async fn begin(&mut self, point: ReceivePoint) -> Result<()> {
        WaylandInput::begin(self, point).await
    }

    fn motion(&mut self, point: ReceivePoint) -> Result<()> {
        WaylandInput::motion(self, point)
    }

    fn input(&mut self, point: ReceivePoint, event: Input) -> Result<()> {
        WaylandInput::input(self, point, event)
    }
}

pub(super) async fn run(
    identity: &Path,
    certificate: &Path,
    bind: SocketAddr,
    allowed: bool,
    options: Options,
) -> Result<()> {
    ensure!(
        allowed,
        "Wayland pointer control requires explicit local consent"
    );
    let identity = transport::Identity::load(identity)?;
    let peer = transport::read_certificate(certificate)?;
    ensure!(
        peer != identity.certificate,
        "the Wayland pointer source must be another computer"
    );
    let stop = tokio::signal::ctrl_c();
    tokio::pin!(stop);
    println!("Approve the requested devices in the Wayland RemoteDesktop dialog. Ctrl+C cancels.");
    let mut pointer = tokio::select! {
        biased;
        result = &mut stop => return result.map_err(Into::into),
        pointer = WaylandInput::open(options) => pointer?,
    };
    let endpoint = transport::endpoint(bind, &identity, &peer)?;
    println!(
        "Waiting for one verified computer on {}. Ctrl+C stops input control.",
        endpoint.local_addr()?
    );
    let connection = tokio::select! {
        biased;
        result = &mut stop => return result.map_err(Into::into),
        result = watch(&mut pointer) => return result,
        result = async {
            let incoming = endpoint.accept().await.context("Wayland listener closed")?;
            Ok::<_, anyhow::Error>(tokio::time::timeout(Duration::from_secs(5), incoming).await??)
        } => result?,
    };
    transport::verify_peer(&connection, &peer)?;
    let result = drive(&connection, &mut stop, &mut pointer).await;
    let cleanup = pointer.finish().await;
    drop(pointer);
    let reason = if result.is_ok() && cleanup.is_ok() {
        NATIVE_INPUT_RECOVERED
    } else {
        b"Wayland input recovery unconfirmed"
    };
    endpoint.close(0u8.into(), reason);
    endpoint.wait_idle().await;
    cleanup?;
    result
}

async fn watch(pointer: &mut impl InputOwner) -> Result<()> {
    loop {
        pointer.check()?;
        tokio::time::sleep(POLL).await;
    }
}

async fn drive(
    connection: &quinn::Connection,
    stop: impl Future<Output = std::io::Result<()>>,
    pointer: &mut impl InputOwner,
) -> Result<()> {
    tokio::pin!(stop);
    let local = pointer.capabilities()?;
    let (mut send_stream, mut receive_stream, peer) = tokio::select! {
        biased;
        result = &mut stop => return result.map_err(Into::into),
        result = watch(pointer) => return result,
        result = async {
            let (mut tx, mut rx) =
                tokio::time::timeout(Duration::from_secs(5), connection.accept_bi()).await??;
            let peer = super::exchange_hello(&mut tx, &mut rx, local.clone()).await?;
            Ok::<_, anyhow::Error>((tx, rx, peer))
        } => result?,
    };
    let (messages, mut received) = mpsc::channel(16);
    let _reader = Abort(tokio::spawn(async move {
        loop {
            let message = transport::receive(&mut receive_stream).await;
            let ended = message.is_err();
            if messages.send(message).await.is_err() || ended {
                return;
            }
        }
    }));
    let mut receiver = Receiver::default();
    let mut latest = None;
    let mut tick = tokio::time::interval(POLL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            biased;
            result = &mut stop => return result.map_err(Into::into),
            _ = connection.closed() => return Ok(()),
            _ = tick.tick() => {
                pointer.check()?;
                if receiver.expired(now_ms()) {
                    return Ok(());
                }
                if let Some(point) = latest.take() {
                    pointer.motion(point)?;
                }
            }
            message = received.recv() => {
                let Some(message) = message else { return Ok(()); };
                match message? {
                    Message::Acquire { lease, edge, along } => {
                        if !(0.0..=1.0).contains(&along)
                            || !receiver.acquire(lease, now_ms(), peer.motion() && local.motion())
                        {
                            send(&mut send_stream, &Message::Denied { lease }).await?;
                            continue;
                        }
                        let (x, y) = entry_position(edge.opposite(), along);
                        let admitted = tokio::select! {
                            biased;
                            result = &mut stop => return result.map_err(Into::into),
                            _ = connection.closed() => return Ok(()),
                            result = tokio::time::timeout(ACQUIRE, pointer.begin(ReceivePoint { x, y })) => result,
                        };
                        if !matches!(admitted, Ok(Ok(()))) || !receiver.accepts(lease, now_ms()) {
                            send(&mut send_stream, &Message::Denied { lease }).await?;
                            admitted.context("Wayland pointer admission timed out")??;
                            anyhow::bail!("the Wayland pointer lease expired before admission");
                        }
                        send(&mut send_stream, &Message::Granted { lease }).await?;
                    }
                    Message::Ping { lease: 0 } => send(&mut send_stream, &Message::Pong { lease: 0 }).await?,
                    Message::Ping { lease } if receiver.heartbeat(lease, now_ms()) => {
                        pointer.check()?;
                        send(&mut send_stream, &Message::Pong { lease }).await?;
                    }
                    Message::Release { lease, .. } if receiver.active() == Some(lease) => return Ok(()),
                    Message::Input { lease, event, cursor } if receiver.active() == Some(lease) => {
                        ensure!(local.supports(event), "Wayland input capability is not enabled");
                        let now = now_ms();
                        if cursor.lease == lease && receiver.accepts_input(lease, event, now)
                            && receiver.anchor(cursor, now)
                        {
                            latest = None;
                            receiver.input_with(lease, event, now, |event| {
                                pointer.input(ReceivePoint { x: cursor.x, y: cursor.y }, event)
                            })?;
                        }
                    }
                    Message::Hello { .. } => anyhow::bail!("unexpected repeated input handshake"),
                    _ => (),
                }
            }
            datagram = connection.read_datagram() => {
                let bytes = datagram?;
                if let Ok(point) = decode::<Motion>(&bytes) {
                    if receiver.motion(point, now_ms()) {
                        latest = Some(ReceivePoint { x: point.x, y: point.y });
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use peerglide_protocol::Edge;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };

    #[test]
    fn wayland_receiver_requires_local_consent_and_exact_region() -> Result<()> {
        let args = ["peerglide", "listen-wayland", "--peer-cert", "peer.der"];
        assert!(crate::Cli::try_parse_from(args).is_err());
        assert!(crate::Cli::try_parse_from(args.into_iter().chain([
            "--allow-pointer-control",
            "--allow-buttons",
            "--allow-keyboard",
            "--region",
            "1920,24",
        ]))
        .is_ok());
        assert_eq!(parse_region("1920,24")?, (1920, 24));
        assert!(parse_region("-1,24").is_err());
        assert!(parse_region("0,0,1").is_err());
        Ok(())
    }

    #[derive(Default)]
    struct Model {
        starts: usize,
        effects: Vec<(ReceivePoint, Input)>,
        revoked: Arc<AtomicBool>,
        keyboard: bool,
    }

    impl InputOwner for Model {
        fn capabilities(&self) -> Result<InputCapabilities> {
            let keys: &[u16] = if self.keyboard { &[0xe4] } else { &[] };
            InputCapabilities::new(keys, true, 1, false).map_err(anyhow::Error::msg)
        }

        fn check(&mut self) -> Result<()> {
            ensure!(!self.revoked.load(Ordering::Acquire), "test portal revoked");
            Ok(())
        }

        async fn begin(&mut self, point: ReceivePoint) -> Result<()> {
            assert_eq!((point.x, point.y), (0.002, 0.5));
            self.starts += 1;
            Ok(())
        }

        fn motion(&mut self, point: ReceivePoint) -> Result<()> {
            assert!([point.x, point.y]
                .into_iter()
                .all(|coordinate| (0.0..=1.0).contains(&coordinate)));
            Ok(())
        }

        fn input(&mut self, point: ReceivePoint, event: Input) -> Result<()> {
            self.check()?;
            assert_eq!(self.starts, 1);
            self.effects.push((point, event));
            Ok(())
        }
    }

    #[tokio::test]
    async fn authenticated_wayland_loopback_filters_ungranted_and_unowned_events() -> Result<()> {
        loopback(Case::Pointer).await
    }

    #[tokio::test]
    async fn wayland_loopback_stops_forwarding_after_portal_revocation() -> Result<()> {
        loopback(Case::Revoked).await
    }

    #[tokio::test]
    async fn wayland_keyboard_requires_its_own_grant_and_preserves_key_pairs() -> Result<()> {
        loopback(Case::Keyboard).await?;
        loopback(Case::DeniedKeyboard).await
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Case {
        Pointer,
        Revoked,
        Keyboard,
        DeniedKeyboard,
    }

    impl Case {
        fn input(self, down: bool) -> Input {
            if matches!(self, Self::Keyboard | Self::DeniedKeyboard) {
                Input::Key { usage: 0xe4, down }
            } else {
                Input::Button { button: 1, down }
            }
        }
    }

    async fn loopback(case: Case) -> Result<()> {
        let source = transport::Identity::generate()?;
        let destination = transport::Identity::generate()?;
        let address = "127.0.0.1:0".parse()?;
        let sender = transport::endpoint(address, &source, &destination.certificate)?;
        let receiver = transport::endpoint(address, &destination, &source.certificate)?;
        let mut model = Model {
            keyboard: case == Case::Keyboard,
            ..Model::default()
        };
        let revoked = model.revoked.clone();
        let server = async {
            let connection = receiver.accept().await.context("listener closed")?.await?;
            transport::verify_peer(&connection, &source.certificate)?;
            let result = drive(&connection, std::future::pending(), &mut model).await;
            let expected_error = match case {
                Case::Revoked => Some("test portal revoked"),
                Case::DeniedKeyboard => Some("Wayland input capability is not enabled"),
                _ => None,
            };
            if let Some(expected) = expected_error {
                assert_eq!(
                    result.expect_err("input should have ended").to_string(),
                    expected
                );
            } else {
                result?;
            }
            connection.close(0u8.into(), b"loopback done");
            receiver.wait_idle().await;
            Ok::<_, anyhow::Error>(())
        };
        let client = async {
            let connection = sender
                .connect(receiver.local_addr()?, "peerglide.local")?
                .await?;
            transport::verify_peer(&connection, &destination.certificate)?;
            let (mut tx, mut rx) = connection.open_bi().await?;
            let caps =
                InputCapabilities::new(&[0xe4], true, 1, false).map_err(anyhow::Error::msg)?;
            let offered = super::super::exchange_hello(&mut tx, &mut rx, caps).await?;
            assert!(offered.motion());
            assert_eq!(
                offered.supports(Input::Key {
                    usage: 0xe4,
                    down: true
                }),
                case == Case::Keyboard
            );
            let input = |sequence, cursor_lease, down| Message::Input {
                lease: 7,
                event: case.input(down),
                cursor: Motion {
                    lease: cursor_lease,
                    sequence,
                    x: 0.25,
                    y: 0.5,
                },
            };
            send(&mut tx, &input(1, 7, true)).await?;
            send(
                &mut tx,
                &Message::Acquire {
                    lease: 7,
                    edge: Edge::Right,
                    along: 0.5,
                },
            )
            .await?;
            assert!(matches!(
                transport::receive(&mut rx).await?,
                Message::Granted { lease: 7 }
            ));
            if case == Case::DeniedKeyboard {
                send(&mut tx, &input(2, 7, true)).await?;
                connection.closed().await;
                return Ok::<_, anyhow::Error>(());
            }
            send(&mut tx, &input(2, 7, false)).await?;
            send(&mut tx, &input(3, 8, true)).await?;
            send(&mut tx, &input(4, 7, true)).await?;
            if case == Case::Revoked {
                send(&mut tx, &Message::Ping { lease: 7 }).await?;
                assert!(matches!(
                    transport::receive(&mut rx).await?,
                    Message::Pong { lease: 7 }
                ));
                revoked.store(true, Ordering::Release);
                let _ = send(&mut tx, &input(5, 7, false)).await;
            } else {
                send(&mut tx, &input(5, 7, false)).await?;
                send(
                    &mut tx,
                    &Message::Release {
                        lease: 7,
                        cursor: None,
                    },
                )
                .await?;
            }
            connection.closed().await;
            Ok::<_, anyhow::Error>(())
        };
        tokio::time::timeout(Duration::from_secs(5), async {
            tokio::try_join!(server, client)
        })
        .await??;
        assert_eq!(model.starts, 1);
        let expected: &[bool] = match case {
            Case::Revoked => &[true],
            Case::DeniedKeyboard => &[],
            _ => &[true, false],
        };
        assert_eq!(model.effects.len(), expected.len());
        for ((point, event), &down) in model.effects.iter().zip(expected) {
            assert!(*event == case.input(down));
            assert_eq!((point.x, point.y), (0.25, 0.5));
        }
        Ok(())
    }
}
