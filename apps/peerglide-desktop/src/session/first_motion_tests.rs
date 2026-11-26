use super::{monitor_layout, run_with_diagnostics, send, transport, Policy, SessionControls};
use anyhow::{ensure, Context, Result};
use peerglide_platform::input_diagnostics::{InputDiagnostics, Phase};
use peerglide_protocol::{encode, Edge, InputCapabilities, Message, Motion};
use std::{io::Write, sync::Arc, time::Duration};

#[tokio::test]
async fn first_motion_after_grant_in_both_connection_roles() -> Result<()> {
    // The workspace test runs on every platform. Real injection is restricted
    // to the disposable GitHub-hosted Windows input desktop.
    let native = cfg!(windows)
        && std::env::var("GITHUB_ACTIONS").as_deref() == Ok("true")
        && std::env::var("RUNNER_ENVIRONMENT").as_deref() == Ok("github-hosted");
    for (source_dials, edge) in [(true, Edge::Right), (false, Edge::Left)] {
        exercise(source_dials, edge, native)
            .await
            .with_context(|| format!("source dials={source_dials}, native={native}"))?;
    }
    Ok(())
}

async fn exercise(source_dials: bool, edge: Edge, native: bool) -> Result<()> {
    let a = transport::Identity::generate()?;
    let b = transport::Identity::generate()?;
    let listener = transport::endpoint("127.0.0.1:0".parse()?, &a, &b.certificate)?;
    let dialer = transport::endpoint("127.0.0.1:0".parse()?, &b, &a.certificate)?;
    let diagnostics = Arc::new(InputDiagnostics::default());
    let (stop, stopped) = tokio::sync::watch::channel(false);
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
        let (source, target) = if source_dials {
            (connected, accepted)
        } else {
            (accepted, connected)
        };
        let originate = async {
            let result = async {
                let (mut tx, mut rx) = control_stream(&source, source_dials).await?;
                crate::exchange_hello(&mut tx, &mut rx, capabilities.clone()).await?;
                probe(&source, &mut tx, &mut rx, edge, native, &diagnostics).await
            }
            .await;
            let _ = stop.send(true);
            result
        };
        let receive = async {
            let (mut tx, mut rx) = control_stream(&target, !source_dials).await?;
            let peer = crate::exchange_hello(&mut tx, &mut rx, capabilities.clone()).await?;
            let mut reporter = crate::InputReporter::new(false);
            run_with_diagnostics(
                target,
                tx,
                rx,
                Policy {
                    allow: true,
                    native,
                    edge: edge.opposite(),
                    local: capabilities.clone(),
                    peer,
                },
                stopped,
                &mut reporter,
                SessionControls {
                    diagnostics: Some(&diagnostics),
                    ..SessionControls::default()
                },
            )
            .await
        };
        let (receiver, sender) = tokio::join!(receive, originate);
        receiver.context("control receiver stopped with an error")?;
        sender?;
        Ok::<_, anyhow::Error>(())
    };
    let result = tokio::time::timeout(Duration::from_secs(10), trial).await;
    let summary = diagnostics.snapshot(Phase::End);
    writeln!(
        std::io::stdout().lock(),
        "first motion source_dials={source_dials} native={native} {summary}"
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
