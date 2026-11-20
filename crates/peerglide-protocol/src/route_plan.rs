//! Shared local file format for the layout editor and CLI. Not a network frame.
use crate::Edge;
use serde::{Deserialize, Serialize};
use std::{net::SocketAddr, path::PathBuf};

pub const VERSION: u8 = 1;

#[derive(Clone, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    pub version: u8,
    pub peers: Vec<PeerSpec>,
    pub routes: Vec<RouteSpec>,
}

impl Default for Plan {
    fn default() -> Self {
        Self {
            version: VERSION,
            peers: Vec::new(),
            routes: Vec::new(),
        }
    }
}

/// The desktop's local configuration envelope, accepted for pinned launches.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredPlan {
    pub store_version: u16,
    pub data: Plan,
}

#[derive(Clone, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PeerSpec {
    pub address: SocketAddr,
    pub certificate: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secondary_layout: Option<SocketAddr>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
}

#[derive(Clone, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RouteSpec {
    pub local_origin: [f64; 2],
    pub local_edge: Edge,
    pub local_span: [f64; 2],
    pub peer: usize,
    pub remote_edge: Edge,
    pub remote_span: [f64; 2],
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peer_and_secondary_layout_keep_ipv6_scope_in_local_json() {
        let peer = PeerSpec {
            address: "[fe80::22%4]:24870".parse().unwrap(),
            certificate: "peer.crt.der".into(),
            secondary_layout: Some("[fe80::22%4]:24877".parse().unwrap()),
            fingerprint: None,
        };
        let raw = serde_json::to_string(&peer).unwrap();
        assert!(raw.contains("[fe80::22%4]:24870"));
        assert!(raw.contains("[fe80::22%4]:24877"));
        let loaded: PeerSpec = serde_json::from_str(&raw).unwrap();
        assert_eq!(crate::peer_destination(loaded.address), Some(peer.address));
        assert_eq!(
            loaded.secondary_layout.and_then(crate::peer_destination),
            peer.secondary_layout
        );
        assert!(crate::peer_destination("[fe80::22]:24877".parse().unwrap()).is_none());
    }
}
