//! Shared local file format for the layout editor and CLI. Not a network frame.
use crate::Edge;
use serde::{Deserialize, Serialize};
use std::{net::SocketAddr, path::PathBuf};

pub const VERSION: u8 = 1;

#[derive(Clone, Deserialize, Serialize)]
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

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PeerSpec {
    pub address: SocketAddr,
    pub certificate: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secondary_layout: Option<SocketAddr>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RouteSpec {
    pub local_origin: [f64; 2],
    pub local_edge: Edge,
    pub local_span: [f64; 2],
    pub peer: usize,
    pub remote_edge: Edge,
    pub remote_span: [f64; 2],
}
