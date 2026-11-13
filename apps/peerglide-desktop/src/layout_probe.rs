//! Explicit, read-only display metadata. Does not acquire or inject input.
use crate::{exchange_hello, transport};
use anyhow::{Context, Result};
use peerglide_protocol::InputCapabilities;
use quinn::{Connection, Endpoint};
use std::{
    net::{Ipv4Addr, Ipv6Addr, SocketAddr},
    path::Path,
    time::Duration,
};
use transport::layout_reservation::{self, inventory};

const TIMEOUT: Duration = Duration::from_secs(8);

pub(super) async fn probe(
    identity: &Path,
    certificate: &Path,
    address: SocketAddr,
    layout_address: SocketAddr,
) -> Result<()> {
    let identity = transport::Identity::load(identity)?;
    let pin = transport::read_certificate(certificate)?;
    let input = transport::endpoint(unspecified_for(address), &identity, &pin)?;
    let sidecar = layout_reservation::endpoint(unspecified_for(layout_address), &identity, &pin)?;
    let work = async {
        let input = connect(&input, address).await?;
        hello(&input, &pin, true).await?;
        let sidecar = connect(&sidecar, layout_address).await?;
        let displays = inventory::query(&sidecar, &input, &pin)
            .await?
            .context("the verified computer did not offer a display inventory")?;
        println!("{}", serde_json::to_string_pretty(&displays)?);
        sidecar.close(0u8.into(), b"display query complete");
        input.close(0u8.into(), b"display query complete");
        Ok(())
    };
    tokio::select! {
        biased;
        result = tokio::signal::ctrl_c() => result.map_err(Into::into),
        result = work => result,
    }
}

#[cfg(any(windows, target_os = "macos"))]
pub(super) async fn listen(
    identity: &Path,
    certificate: &Path,
    bind: SocketAddr,
    layout_bind: SocketAddr,
) -> Result<()> {
    let identity = transport::Identity::load(identity)?;
    let pin = transport::read_certificate(certificate)?;
    let input = transport::endpoint(bind, &identity, &pin)?;
    let sidecar = layout_reservation::endpoint(layout_bind, &identity, &pin)?;
    println!(
        "Waiting for one display query on {} and {}.",
        input.local_addr()?,
        sidecar.local_addr()?
    );
    let work = async {
        let input = accept(&input, Duration::from_secs(60)).await?;
        hello(&input, &pin, false).await?;
        let sidecar = accept(&sidecar, TIMEOUT).await?;
        let offered = inventory::answer(&sidecar, &input, &pin, || match local_inventory() {
            Ok(snapshot) => Some(snapshot),
            Err(error) => {
                eprintln!("Could not read display geometry: {error}");
                None
            }
        })
        .await?;
        // Keep the short reply alive until the requester closes or times out.
        let _ = tokio::time::timeout(TIMEOUT, sidecar.closed()).await;
        anyhow::ensure!(offered, "the display inventory was not offered");
        Ok(())
    };
    tokio::select! {
        biased;
        result = tokio::signal::ctrl_c() => result.map_err(Into::into),
        result = work => result,
    }
}

pub(super) fn unspecified_for(address: SocketAddr) -> SocketAddr {
    if address.is_ipv4() {
        SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0))
    } else {
        SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0))
    }
}

async fn connect(endpoint: &Endpoint, address: SocketAddr) -> Result<Connection> {
    Ok(tokio::time::timeout(TIMEOUT, endpoint.connect(address, "peerglide.local")?).await??)
}

#[cfg(any(windows, target_os = "macos"))]
async fn accept(endpoint: &Endpoint, timeout: Duration) -> Result<Connection> {
    let incoming = tokio::time::timeout(timeout, endpoint.accept())
        .await?
        .context("display listener closed")?;
    Ok(tokio::time::timeout(TIMEOUT, incoming).await??)
}

async fn hello(connection: &Connection, pin: &[u8], connector: bool) -> Result<()> {
    transport::verify_peer(connection, pin)?;
    let (mut send, mut receive) = if connector {
        tokio::time::timeout(TIMEOUT, connection.open_bi()).await??
    } else {
        tokio::time::timeout(TIMEOUT, connection.accept_bi()).await??
    };
    exchange_hello(&mut send, &mut receive, InputCapabilities::default()).await?;
    Ok(())
}

#[cfg(any(windows, target_os = "macos"))]
fn local_inventory() -> Result<inventory::Inventory> {
    use peerglide_platform::display_layout::Rotation;
    let layout = peerglide_platform::read_display_layout()?;
    let displays = layout
        .displays()
        .iter()
        .enumerate()
        .map(|(index, display)| {
            let bounds = display.bounds;
            let left = coordinate(bounds.left)?;
            let top = coordinate(bounds.top)?;
            let right = coordinate(bounds.right)?;
            let bottom = coordinate(bounds.bottom)?;
            let scale = (display.scale * 1000.0).round();
            anyhow::ensure!(
                (1.0..=f64::from(u16::MAX)).contains(&scale),
                "display scale outside metadata range"
            );
            Ok(inventory::Display {
                id: u8::try_from(index + 1)?,
                left,
                top,
                width: right.abs_diff(left),
                height: bottom.abs_diff(top),
                scale_milli: scale as u16,
                quarter_turns: match display.rotation {
                    Rotation::Upright => 0,
                    Rotation::Clockwise90 => 1,
                    Rotation::Clockwise180 => 2,
                    Rotation::Clockwise270 => 3,
                },
                primary: display.id == layout.primary(),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    // This is one immutable snapshot for this one authenticated query.
    Ok(inventory::Inventory {
        generation: 1,
        displays,
    })
}

#[cfg(any(windows, target_os = "macos"))]
fn coordinate(value: f64) -> Result<i32> {
    anyhow::ensure!(
        value.fract() == 0.0 && (f64::from(i32::MIN)..=f64::from(i32::MAX)).contains(&value),
        "display origin or extent outside integer metadata range"
    );
    Ok(value as i32)
}

#[cfg(all(test, any(windows, target_os = "macos")))]
mod tests {
    #[test]
    fn display_listener_requires_explicit_local_consent() {
        use clap::Parser;
        assert!(crate::Cli::try_parse_from([
            "peerglide",
            "listen-displays",
            "--peer-cert",
            "peer.der"
        ])
        .is_err());
    }
}
