//! One explicitly configured display destination, bound to the live v5 lease.
use anyhow::{Context, Result};
use peerglide_platform::ReceiveDisplay;
use peerglide_protocol::Edge;
use peerglide_transport::{layout_reservation as wire, Identity};
use quinn::{Connection, Endpoint};
use std::{
    net::{Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::Arc,
    time::Duration,
};
use tokio::{sync::watch, task::JoinHandle};

const SELECTED_DISPLAY: u64 = 1;
const SNAPSHOT: u64 = 1;
const OUTBOUND_BUDGET: Duration = Duration::from_millis(350);
const PEER_CLOSE: Duration = Duration::from_secs(1);

pub(super) struct Routes {
    pin: Arc<[u8]>,
    source: Option<(Endpoint, SocketAddr)>,
    target: Option<Target>,
}

struct Target {
    display: Arc<ReceiveDisplay>,
    slot: Arc<wire::ReservationSlot>,
    task: JoinHandle<()>,
}

impl Drop for Target {
    fn drop(&mut self) {
        self.task.abort();
        let _ = self.slot.clear();
    }
}

impl Routes {
    pub(super) fn start(
        identity: &Identity,
        pin: &[u8],
        input: &Connection,
        outbound: Option<SocketAddr>,
        inbound: Option<SocketAddr>,
        display: Option<Arc<ReceiveDisplay>>,
    ) -> Result<Option<Self>> {
        if outbound.is_none() && inbound.is_none() {
            return Ok(None);
        }
        let source = outbound
            .map(|address| {
                let bind = if address.is_ipv4() {
                    SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0))
                } else {
                    SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0))
                };
                wire::endpoint(bind, identity, pin).map(|endpoint| (endpoint, address))
            })
            .transpose()?;
        let pin = Arc::<[u8]>::from(pin);
        let target = if let Some(bind) = inbound {
            let display =
                display.context("secondary layout requires a selected local receive display")?;
            let listener = wire::endpoint(bind, identity, &pin)?;
            let slot = Arc::new(wire::ReservationSlot::default());
            let task = tokio::spawn(serve(
                listener,
                input.clone(),
                pin.clone(),
                display.clone(),
                slot.clone(),
            ));
            Some(Target {
                display,
                slot,
                task,
            })
        } else {
            None
        };
        Ok(Some(Self {
            pin,
            source,
            target,
        }))
    }

    pub(super) fn has_outbound(&self) -> bool {
        self.source.is_some()
    }

    pub(super) async fn confirm_outbound(
        &self,
        input: &Connection,
        stop: &mut watch::Receiver<bool>,
        lease: u64,
        edge: Edge,
        fraction: f64,
    ) -> bool {
        tokio::select! {
            biased;
            _ = super::stop_requested(stop) => false,
            confirmed = self.reserve_outbound(input, lease, edge, fraction) => confirmed,
        }
    }

    pub(super) async fn reserve_outbound(
        &self,
        input: &Connection,
        lease: u64,
        edge: Edge,
        fraction: f64,
    ) -> bool {
        let Some((endpoint, address)) = &self.source else {
            return true;
        };
        let result = async {
            let sidecar = endpoint.connect(*address, "peerglide.local")?.await?;
            let proposal = wire::Request {
                lease,
                display: SELECTED_DISPLAY,
                edge,
                fraction,
            };
            let result = wire::reserve(&sidecar, input, &self.pin, proposal).await;
            sidecar.close(0u8.into(), b"display reservation finished");
            result
        };
        matches!(tokio::time::timeout(OUTBOUND_BUDGET, result).await, Ok(Ok(Some(_))))
    }

    pub(super) fn accepts_incoming(
        &self,
        input: &Connection,
        lease: u64,
        local_edge: Edge,
        remote_edge: Edge,
        fraction: f64,
    ) -> bool {
        let Some(target) = &self.target else {
            return true;
        };
        if remote_edge != local_edge.opposite()
            || !target.display.matches_current().unwrap_or(false)
        {
            let _ = target.slot.clear();
            return false;
        }
        target
            .slot
            .consume(input, &self.pin, lease, SNAPSHOT)
            .ok()
            .flatten()
            .is_some_and(|request| matches_proposal(request, remote_edge, fraction))
    }
}

async fn serve(
    listener: Endpoint,
    input: Connection,
    pin: Arc<[u8]>,
    display: Arc<ReceiveDisplay>,
    slot: Arc<wire::ReservationSlot>,
) {
    loop {
        let incoming = tokio::select! {
            biased;
            _ = input.closed() => break,
            incoming = listener.accept() => incoming,
        };
        let Some(incoming) = incoming else {
            break;
        };
        if let Ok(Ok(sidecar)) = tokio::time::timeout(PEER_CLOSE, incoming).await {
            let offered = wire::accept_for_slot(&sidecar, &input, &pin, &slot, |request| {
                (request.display == SELECTED_DISPLAY && display.matches_current().unwrap_or(false))
                    .then_some(SNAPSHOT)
            })
            .await;
            if offered.is_ok() {
                let _ = tokio::time::timeout(PEER_CLOSE, sidecar.closed()).await;
            }
            sidecar.close(0u8.into(), b"display reservation complete");
        }
    }
    let _ = slot.clear();
}

fn matches_proposal(request: wire::Request, edge: Edge, fraction: f64) -> bool {
    request.display == SELECTED_DISPLAY
        && request.edge == edge
        && (request.fraction - fraction).abs() <= f64::EPSILON
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_reservation_cannot_authorize_another_edge_or_anchor() {
        let request = wire::Request {
            lease: 1,
            display: SELECTED_DISPLAY,
            edge: Edge::Left,
            fraction: 0.25,
        };
        assert!(matches_proposal(request, Edge::Left, 0.25));
        assert!(!matches_proposal(request, Edge::Right, 0.25));
        assert!(!matches_proposal(request, Edge::Left, 0.5));
        assert!(!matches_proposal(
            wire::Request {
                display: 2,
                ..request
            },
            Edge::Left,
            0.25
        ));
    }
}
