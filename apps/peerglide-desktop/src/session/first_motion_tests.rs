use super::{monitor_layout, run_with_diagnostics, send, transport, Policy, SessionControls};
use anyhow::{ensure, Context, Result};
use peerglide_platform::input_diagnostics::{InputDiagnostics, Phase};
use peerglide_platform::{now_ms, CaptureGate, CaptureRoute, Captured};
use peerglide_protocol::{decode, encode, Edge, InputCapabilities, Message, Motion};
use std::{io::Write, sync::Arc, time::Duration};
use tokio::sync::{mpsc, oneshot};

pub(crate) struct SourceCapture {
    pub(super) events: mpsc::Receiver<Captured>,
    pub(super) installed: oneshot::Sender<CaptureGate>,
}

#[tokio::test]
async fn first_motion_after_grant_in_both_connection_roles() -> Result<()> {
    // The workspace test runs on every platform. Real injection is restricted
    // to the disposable GitHub-hosted Windows input desktop.
    let native = cfg!(windows)
        && std::env::var("GITHUB_ACTIONS").as_deref() == Ok("true")
        && std::env::var("RUNNER_ENVIRONMENT").as_deref() == Ok("github-hosted");
    for (source_dials, edge) in [(true, Edge::Right), (false, Edge::Left)] {
        exercise(source_dials, edge, native, false)
            .await
            .with_context(|| format!("source dials={source_dials}, native={native}"))?;
        if native {
            exercise(source_dials, edge, true, true)
                .await
                .with_context(|| format!("native source dials={source_dials}"))?;
        }
    }
    Ok(())
}

async fn exercise(source_dials: bool, edge: Edge, native: bool, outbound: bool) -> Result<()> {
    let a = transport::Identity::generate()?;
    let b = transport::Identity::generate()?;
    let listener = transport::endpoint("127.0.0.1:0".parse()?, &a, &b.certificate)?;
    let dialer = transport::endpoint("127.0.0.1:0".parse()?, &b, &a.certificate)?;
    let diagnostics = Arc::new(InputDiagnostics::default());
    let (stop, stopped) = tokio::sync::watch::channel(false);
    let (publish, events) = mpsc::channel(1);
    let (installed, gate) = oneshot::channel();
    let source_capture = outbound.then_some(SourceCapture { events, installed });
    let capabilities = InputCapabilities::new(&[], true, 0, false).map_err(anyhow::Error::msg)?;
    let trial = async {
        let (connected, accepted) = tokio::try_join!(
            async {
                Ok::<_, anyhow::Error>(
                    dialer
                        .connect(listener.local_addr()?, "peerglide.local")?
                        .await?,
                )
            },
            async {
                Ok::<_, anyhow::Error>(listener.accept().await.context("listener closed")?.await?)
            }
        )?;
        transport::verify_peer(&connected, &a.certificate)?;
        transport::verify_peer(&accepted, &b.certificate)?;
        let worker_dials = source_dials == outbound;
        let (worker, counterpart) = if worker_dials {
            (connected, accepted)
        } else {
            (accepted, connected)
        };
        let scripted_peer = async {
            let result = async {
                let (mut tx, mut rx) = control_stream(&counterpart, !worker_dials).await?;
                crate::exchange_hello(&mut tx, &mut rx, capabilities.clone()).await?;
                if outbound {
                    let gate = gate.await.context("wait for native capture installation")?;
                    probe_source(&counterpart, &mut tx, &mut rx, edge, gate, publish).await
                } else {
                    probe(&counterpart, &mut tx, &mut rx, edge, native, &diagnostics).await
                }
            }
            .await;
            let _ = stop.send(true);
            result
        };
        let session = async {
            let (mut tx, mut rx) = control_stream(&worker, worker_dials).await?;
            let peer = crate::exchange_hello(&mut tx, &mut rx, capabilities.clone()).await?;
            let mut reporter = crate::InputReporter::new(false);
            run_with_diagnostics(
                worker,
                tx,
                rx,
                Policy {
                    allow: true,
                    native,
                    edge: if outbound { edge } else { edge.opposite() },
                    local: capabilities.clone(),
                    peer,
                },
                stopped,
                &mut reporter,
                SessionControls {
                    diagnostics: Some(&diagnostics),
                    source_capture,
                    ..SessionControls::default()
                },
            )
            .await
        };
        let (session_result, peer_result) = tokio::join!(session, scripted_peer);
        session_result.context("control session stopped with an error")?;
        peer_result?;
        Ok::<_, anyhow::Error>(())
    };
    let result = tokio::time::timeout(Duration::from_secs(10), trial).await;
    let summary = diagnostics.snapshot(Phase::End);
    writeln!(
        std::io::stdout().lock(),
        "first motion source_dials={source_dials} native={native} outbound={outbound} {summary}"
    )?;
    result.context("control session deadline")??;
    Ok(())
}

async fn control_stream(
    connection: &quinn::Connection,
    dialed: bool,
) -> Result<(quinn::SendStream, quinn::RecvStream)> {
    if dialed {
        Ok(connection.open_bi().await?)
    } else {
        Ok(connection.accept_bi().await?)
    }
}

async fn probe_source(
    connection: &quinn::Connection,
    tx: &mut quinn::SendStream,
    rx: &mut quinn::RecvStream,
    edge: Edge,
    gate: CaptureGate,
    events: mpsc::Sender<Captured>,
) -> Result<()> {
    let layout = monitor_layout(true).context("read source display layout")?;
    let bounds = layout
        .displays()
        .iter()
        .find(|display| display.id == layout.primary())
        .context("find source primary display")?
        .bounds;
    let width = bounds.right - bounds.left;
    let height = bounds.bottom - bounds.top;
    let right = edge == Edge::Right;
    let local = match gate.route() {
        Some(route @ CaptureRoute::Local(_)) => route,
        _ => anyhow::bail!("source capture did not start Local"),
    };
    // EdgeGate starts with a zero timestamp. Keep the first edge sample out
    // of the initial dwell interval without using up any Acquire budget.
    tokio::time::sleep(Duration::from_millis(300u64.saturating_sub(now_ms()))).await;
    events
        .send(Captured::Position {
            x: if right { width - 1.0 } else { 0.0 },
            y: (height - 1.0) * 0.5,
            width,
            height,
            route: local,
            primary: None,
        })
        .await
        .context("submit source edge position")?;
    let acquire = tokio::time::timeout(Duration::from_secs(2), transport::receive(rx))
        .await
        .context("edge did not produce Acquire")??;
    let Message::Acquire {
        lease,
        edge: requested,
        along,
    } = acquire
    else {
        anyhow::bail!("expected Acquire after the edge position");
    };
    ensure!(requested == edge && along == 0.5, "unexpected Acquire route");
    ensure!(gate.is_current(local), "source committed before Granted");
    send(tx, &Message::Granted { lease })
        .await
        .context("send Granted to source")?;
    let route = tokio::time::timeout(Duration::from_millis(800), async {
        loop {
            if let Some(route @ CaptureRoute::Remote(_)) = gate.route() {
                break route;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .context("source capture remained Local after Granted")?;
    let dx = if right { width * 0.1 } else { -width * 0.1 };
    events
        .send(Captured::Motion { dx, dy: 0.0, route })
        .await
        .context("submit first routed capture motion")?;
    let bytes = tokio::time::timeout(Duration::from_millis(500), connection.read_datagram())
        .await
        .context("committed source did not send its first movement")??;
    let motion: Motion = decode(&bytes).map_err(anyhow::Error::msg)?;
    let expected_x = if right { 0.102 } else { 0.898 };
    ensure!(
        motion.lease == lease
            && motion.sequence == 1
            && (motion.x - expected_x).abs() < 1e-6
            && motion.y == 0.5,
        "first movement did not match the granted route"
    );
    send(
        tx,
        &Message::Release {
            lease,
            cursor: None,
        },
    )
    .await?;
    tokio::time::timeout(Duration::from_millis(800), async {
        loop {
            if matches!(gate.route(), Some(CaptureRoute::Local(_))) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .context("peer Release did not restore local capture")?;
    Ok(())
}

async fn probe(
    connection: &quinn::Connection,
    tx: &mut quinn::SendStream,
    rx: &mut quinn::RecvStream,
    edge: Edge,
    native: bool,
    diagnostics: &InputDiagnostics,
) -> Result<()> {
    const LEASE: u64 = 1;
    send(tx, &Message::Ping { lease: 0 })
        .await
        .context("send readiness ping")?;
    ensure!(
        matches!(transport::receive(rx).await?, Message::Pong { lease: 0 }),
        "receiver did not answer readiness ping"
    );
    send(
        tx,
        &Message::Acquire {
            lease: LEASE,
            edge,
            along: 0.25,
        },
    )
    .await
    .context("send Acquire")?;
    let grant = transport::receive(rx).await.context("wait for Granted")?;
    ensure!(
        matches!(grant, Message::Granted { lease: LEASE }),
        "receiver did not grant the lease"
    );
    let first = Motion {
        lease: LEASE,
        sequence: 1,
        x: 0.5,
        y: 0.75,
    };
    let expected = if native {
        let layout = monitor_layout(native).context("read native display layout")?;
        let bounds = layout
            .displays()
            .iter()
            .find(|display| display.id == layout.primary())
            .context("find primary display for native cursor readback")?
            .bounds;
        Some([
            bounds.left + first.x * (bounds.right - bounds.left),
            bounds.top + first.y * (bounds.bottom - bounds.top),
        ])
    } else {
        None
    };
    connection
        .send_datagram(encode(&first)?.into())
        .context("send first motion")?;
    tokio::time::timeout(Duration::from_millis(800), async {
        loop {
            let snapshot = diagnostics.snapshot(Phase::Sample).to_string();
            if snapshot
                .split_ascii_whitespace()
                .any(|field| field == "apply=1")
            {
                let moved = expected.is_none_or(|point| {
                    peerglide_platform::cursor_position().is_some_and(|current| {
                        (current[0] - point[0]).abs() <= 4.0 && (current[1] - point[1]).abs() <= 4.0
                    })
                });
                if moved {
                    return;
                }
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .context("first motion did not reach pointer application or native cursor")?;
    send(
        tx,
        &Message::Release {
            lease: LEASE,
            cursor: None,
        },
    )
    .await?;
    send(tx, &Message::Ping { lease: 0 }).await?;
    ensure!(
        matches!(transport::receive(rx).await?, Message::Pong { lease: 0 }),
        "receiver did not process Release"
    );
    Ok(())
}
