//! Bind local display segments to explicitly pinned peers before opening any connection.
use crate::transport;
#[cfg(any(windows, target_os = "macos", test))]
use crate::{exchange_hello, layout_probe::unspecified_for, probe_roundtrip};
use anyhow::{ensure, Context, Result};
use peerglide_platform::{
    display_layout::{EdgeAnchor, Layout},
    display_routes::{Link, Route, Span, Table, Target},
};
#[cfg(any(windows, target_os = "macos", test))]
use peerglide_protocol::InputCapabilities;
use peerglide_protocol::{
    route_plan::{Plan, VERSION as PLAN_VERSION},
    Edge,
};
#[cfg(any(windows, target_os = "macos", test))]
use std::time::Duration;
use std::{collections::BTreeMap, fs::File, io::Read, net::SocketAddr, path::Path};

const MAX_PLAN_BYTES: u64 = 128 * 1024;
#[cfg(any(windows, target_os = "macos", test))]
const TIMEOUT: Duration = Duration::from_secs(8);

pub(super) struct Peer {
    pub address: SocketAddr,
    pub certificate: Vec<u8>,
    pub secondary_layout: Option<SocketAddr>,
}

pub(super) struct Routes {
    table: Table,
    peers: BTreeMap<[u8; 32], Peer>,
}

impl Routes {
    #[cfg(any(windows, target_os = "macos", test))]
    pub(super) fn load(path: &Path, local: &[u8], layout: &Layout) -> Result<Self> {
        Self::load_for_launch(path, local, layout, None)
    }

    pub(super) fn load_for_launch(
        path: &Path,
        local: &[u8],
        layout: &Layout,
        expected: Option<&str>,
    ) -> Result<Self> {
        let metadata = std::fs::metadata(path).context("inspect display route plan")?;
        ensure!(
            metadata.is_file(),
            "display route plan must be a regular file"
        );
        ensure!(
            metadata.len() <= MAX_PLAN_BYTES,
            "display route plan too large"
        );
        let file = File::open(path).context("open display route plan")?;
        ensure!(
            file.metadata()?.is_file(),
            "display route plan must be a regular file"
        );
        let mut bytes = Vec::new();
        file.take(MAX_PLAN_BYTES + 1)
            .read_to_end(&mut bytes)
            .context("read display route plan")?;
        ensure!(
            bytes.len() as u64 <= MAX_PLAN_BYTES,
            "display route plan too large"
        );
        let plan = match expected {
            Some(expected) => {
                ensure!(
                    blake3::hash(&bytes).to_hex().as_str() == expected,
                    "display route plan changed after confirmation"
                );
                let value: serde_json::Value = serde_json::from_slice(&bytes)?;
                let plan: Plan = if value.get("store_version").is_some() {
                    let stored: peerglide_protocol::route_plan::StoredPlan =
                        serde_json::from_value(value)?;
                    ensure!(
                        stored.store_version == 1,
                        "unsupported desktop route draft version"
                    );
                    stored.data
                } else {
                    serde_json::from_value(value)?
                };
                ensure!(
                    plan.peers.iter().all(|peer| peer.fingerprint.is_some()),
                    "managed routes require fixed certificate fingerprints"
                );
                plan
            }
            None => serde_json::from_slice(&bytes).context("invalid display route plan")?,
        };
        let directory = path.parent().unwrap_or_else(|| Path::new("."));
        Self::bind(plan, directory, local, layout)
    }

    fn bind(plan: Plan, directory: &Path, local: &[u8], layout: &Layout) -> Result<Self> {
        ensure!(
            plan.version == PLAN_VERSION,
            "unsupported display route plan version"
        );
        ensure!(
            (1..=transport::trusted_listener::MAX_TRUSTED_PEERS).contains(&plan.peers.len()),
            "display route peer count outside listener limits"
        );
        ensure!(
            (1..=peerglide_platform::display_routes::MAX_LINKS).contains(&plan.routes.len()),
            "display route count outside layout limits"
        );
        let mut peers = BTreeMap::new();
        let mut peer_ids = Vec::with_capacity(plan.peers.len());
        for spec in plan.peers {
            ensure!(
                spec.address.port() != 0 && !spec.address.ip().is_unspecified(),
                "display route peer needs a destination address and port"
            );
            ensure!(
                spec.secondary_layout
                    .is_none_or(|address| address.port() != 0 && !address.ip().is_unspecified()),
                "secondary layout needs a destination address and port"
            );
            let certificate = transport::read_certificate(&directory.join(spec.certificate))?;
            ensure!(
                spec.fingerprint
                    .as_ref()
                    .is_none_or(|expected| expected
                        .eq_ignore_ascii_case(&transport::fingerprint(&certificate))),
                "display route peer certificate differs from its configured fingerprint"
            );
            ensure!(
                certificate != local,
                "display route peer matches the local identity"
            );
            let id = *blake3::hash(&certificate).as_bytes();
            ensure!(
                peers
                    .insert(
                        id,
                        Peer {
                            address: spec.address,
                            certificate,
                            secondary_layout: spec.secondary_layout,
                        }
                    )
                    .is_none(),
                "display route peer certificate is repeated"
            );
            peer_ids.push(id);
        }
        let links = plan
            .routes
            .into_iter()
            .map(|spec| {
                let [x, y] = spec.local_origin;
                let mut candidates = layout
                    .displays()
                    .iter()
                    .filter(|display| display.bounds.left == x && display.bounds.top == y);
                let display = candidates
                    .next()
                    .context("display route local origin is missing")?;
                ensure!(
                    candidates.next().is_none(),
                    "display route local origin is ambiguous"
                );
                let peer = *peer_ids
                    .get(spec.peer)
                    .context("display route peer index is missing")?;
                Ok(Link {
                    source_display: display.id,
                    source_edge: spec.local_edge,
                    source_span: span(spec.local_span),
                    target: Target {
                        peer,
                        display: None,
                        edge: spec.remote_edge,
                    },
                    target_span: span(spec.remote_span),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let table = Table::new(layout, links)
            .map_err(|error| anyhow::anyhow!("invalid display route segments: {error:?}"))?;
        Ok(Self { table, peers })
    }

    pub(super) fn resolve(
        &self,
        layout: &Layout,
        edge: Edge,
        (x, y): (f64, f64),
    ) -> Option<(EdgeAnchor, Route, &Peer)> {
        let anchor = layout.edge_at(edge, x, y, 1.0)?;
        let route = self.table.resolve(layout, anchor)?;
        let peer = self.peers.get(&route.target.peer)?;
        Some((anchor, route, peer))
    }

    pub(super) fn select_at(
        &self,
        layout: &Layout,
        point: (f64, f64),
    ) -> Option<(EdgeAnchor, Route, &Peer)> {
        let mut matches = [Edge::Left, Edge::Right, Edge::Top, Edge::Bottom]
            .into_iter()
            .filter_map(|edge| self.resolve(layout, edge, point));
        let selection = matches.next()?;
        // Two adjacent configured edges can meet at a corner. No implicit priority.
        matches.next().is_none().then_some(selection)
    }
}

fn span([start, end]: [f64; 2]) -> Span {
    Span { start, end }
}

#[cfg(any(windows, target_os = "macos", test))]
async fn probe(identity: &transport::Identity, peer: &Peer) -> Result<()> {
    let endpoint = transport::endpoint(unspecified_for(peer.address), identity, &peer.certificate)?;
    let result = tokio::time::timeout(TIMEOUT, async {
        let connection = endpoint.connect(peer.address, "peerglide.local")?.await?;
        transport::verify_peer(&connection, &peer.certificate)?;
        let (mut tx, mut rx) = connection.open_bi().await?;
        exchange_hello(&mut tx, &mut rx, InputCapabilities::default()).await?;
        probe_roundtrip(&mut tx, &mut rx).await
    })
    .await;
    endpoint.close(0u8.into(), b"display route probe ended");
    result?
}

#[cfg(any(windows, target_os = "macos"))]
pub(super) async fn run(identity: &Path, path: &Path, point: (f64, f64), edge: Edge) -> Result<()> {
    let identity = transport::Identity::load(identity)?;
    let layout = peerglide_platform::read_display_layout()?;
    let routes = Routes::load(path, &identity.certificate, &layout)?;
    let (_, route, peer) = routes
        .resolve(&layout, edge, point)
        .context("no route on that exposed display edge")?;
    probe(&identity, peer)
        .await
        .context("selected route peer is unavailable or could not be verified")?;
    println!(
        "PASS: verified v5 route peer {}, entry edge {:?}.",
        transport::fingerprint(&peer.certificate),
        route.target.edge
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use peerglide_platform::display_layout::{Bounds, Display, DisplayId, Rotation};
    use peerglide_protocol::Message;
    use std::{
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Result<Self> {
            let time = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
            let path =
                std::env::temp_dir().join(format!("linko-route-{}-{time}", std::process::id()));
            std::fs::create_dir(&path)?;
            Ok(Self(path))
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn test_layout() -> Layout {
        let source = Display {
            id: DisplayId(1),
            bounds: Bounds {
                left: -100.0,
                top: 0.0,
                right: 0.0,
                bottom: 100.0,
            },
            scale: 1.5,
            rotation: Rotation::Upright,
        };
        let attached = Display {
            id: DisplayId(2),
            bounds: Bounds {
                left: 0.0,
                top: 0.0,
                right: 50.0,
                bottom: 25.0,
            },
            ..source
        };
        Layout::new(attached.id, vec![source, attached]).unwrap()
    }

    async fn answer(endpoint: quinn::Endpoint, pin: Vec<u8>) -> Result<()> {
        let connection = endpoint.accept().await.context("listener closed")?.await?;
        transport::verify_peer(&connection, &pin)?;
        let (mut tx, mut rx) = connection.accept_bi().await?;
        let caps = exchange_hello(&mut tx, &mut rx, InputCapabilities::default()).await?;
        ensure!(!caps.motion(), "probe must advertise no native input");
        ensure!(matches!(
            transport::receive(&mut rx).await?,
            Message::Ping { lease: 0 }
        ));
        transport::send(&mut tx, &Message::Pong { lease: 0 }).await?;
        connection.closed().await;
        Ok(())
    }

    #[tokio::test]
    async fn adjacent_routes_probe_only_the_selected_certificate() -> Result<()> {
        let fixture = Fixture::new()?;
        let local = transport::Identity::generate()?;
        let first = transport::Identity::generate()?;
        let second = transport::Identity::generate()?;
        let bind = SocketAddr::from(([127, 0, 0, 1], 0));
        let first_endpoint = transport::endpoint(bind, &first, &local.certificate)?;
        let second_endpoint = transport::endpoint(bind, &second, &local.certificate)?;
        let first_address = first_endpoint.local_addr()?;
        let second_address = second_endpoint.local_addr()?;
        std::fs::write(fixture.0.join("first.der"), &first.certificate)?;
        std::fs::write(fixture.0.join("second.der"), &second.certificate)?;
        let mut plan = serde_json::json!({
            "version": 1,
            "peers": [
                {"certificate": "first.der", "address": first_address},
                {"certificate": "second.der", "address": second_address, "secondary_layout": "127.0.0.1:24877"},
            ],
            "routes": [
                {"local_origin": [-100, 0], "local_edge": "Right", "local_span": [0, 0.5],
                 "peer": 0, "remote_edge": "Left", "remote_span": [0, 1]},
                {"local_origin": [-100, 0], "local_edge": "Right", "local_span": [0.5, 1],
                 "peer": 1, "remote_edge": "Top", "remote_span": [0.25, 0.75]},
            ],
        });
        let path = fixture.0.join("routes.json");
        std::fs::write(&path, serde_json::to_vec(&plan)?)?;
        let layout = test_layout();
        let routes = Routes::load(&path, &local.certificate, &layout)?;
        assert!(routes.resolve(&layout, Edge::Right, (-1.0, 10.0)).is_none());
        assert!(routes
            .resolve(&layout, Edge::Right, (-10.0, 35.0))
            .is_none());
        assert!(routes
            .resolve(&test_layout(), Edge::Right, (-1.0, 60.0))
            .is_none());
        let (_, first_route, first_peer) =
            routes.resolve(&layout, Edge::Right, (-1.0, 30.0)).unwrap();
        let (_, second_route, second_peer) =
            routes.resolve(&layout, Edge::Right, (-1.0, 50.0)).unwrap();
        assert_eq!(first_peer.certificate, first.certificate);
        assert_eq!(second_peer.certificate, second.certificate);
        assert_eq!(first_peer.secondary_layout, None);
        assert_eq!(
            second_peer.secondary_layout,
            Some("127.0.0.1:24877".parse()?)
        );
        assert_eq!(first_route.target.edge, Edge::Left);
        assert_eq!(second_route.target.edge, Edge::Top);
        assert_eq!(second_route.fraction, 0.25);
        plan["peers"][1]["fingerprint"] = serde_json::json!(first.fingerprint());
        std::fs::write(&path, serde_json::to_vec(&plan)?)?;
        assert!(Routes::load(&path, &local.certificate, &layout).is_err());
        plan["peers"][1]["fingerprint"] = serde_json::json!(second.fingerprint());
        plan["peers"][0]["fingerprint"] = serde_json::json!(first.fingerprint());
        let mut draft =
            serde_json::to_vec(&serde_json::json!({"store_version": 1, "data": &plan}))?;
        let expected = blake3::hash(&draft).to_hex();
        std::fs::write(&path, &draft)?;
        Routes::load_for_launch(&path, &local.certificate, &layout, Some(expected.as_str()))?;
        draft.push(b'\n');
        std::fs::write(&path, &draft)?;
        assert!(Routes::load_for_launch(
            &path,
            &local.certificate,
            &layout,
            Some(expected.as_str())
        )
        .is_err());
        let serve_first = tokio::spawn(answer(first_endpoint.clone(), local.certificate.clone()));
        let serve_second = tokio::spawn(answer(second_endpoint.clone(), local.certificate.clone()));
        probe(&local, first_peer).await?;
        probe(&local, second_peer).await?;
        tokio::time::timeout(TIMEOUT, serve_first).await???;
        tokio::time::timeout(TIMEOUT, serve_second).await???;

        let incorrect = tokio::spawn(async move {
            let incoming = second_endpoint.accept().await.context("listener closed")?;
            ensure!(incoming.await.is_err(), "wrong pin completed TLS");
            Ok::<_, anyhow::Error>(())
        });
        let wrong = Peer {
            address: second_address,
            certificate: first.certificate,
            secondary_layout: None,
        };
        assert!(probe(&local, &wrong).await.is_err());
        tokio::time::timeout(TIMEOUT, incorrect).await???;
        first_endpoint.close(0u8.into(), b"offline test");
        assert!(probe(&local, first_peer).await.is_err());

        plan["routes"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "local_origin": [-100, 0], "local_edge": "Bottom", "local_span": [0, 1],
                "peer": 0, "remote_edge": "Top", "remote_span": [0, 1],
            }));
        std::fs::write(&path, serde_json::to_vec(&plan)?)?;
        let corners = Routes::load(&path, &local.certificate, &layout)?;
        assert!(corners
            .resolve(&layout, Edge::Right, (-1.0, 99.0))
            .is_some());
        assert!(corners
            .resolve(&layout, Edge::Bottom, (-1.0, 99.0))
            .is_some());
        assert!(corners.select_at(&layout, (-1.0, 99.0)).is_none());

        plan["routes"][1]["local_span"] = serde_json::json!([0.4, 1]);
        std::fs::write(&path, serde_json::to_vec(&plan)?)?;
        assert!(Routes::load(&path, &local.certificate, &layout).is_err());
        Ok(())
    }
}
