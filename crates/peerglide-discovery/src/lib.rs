//! Opt-in LAN hints only. Nothing here authenticates or grants remote control.
use anyhow::Result;
use mdns_sd::{DaemonEvent, DaemonStatus, Receiver, ServiceDaemon, ServiceEvent, ServiceInfo};
use std::{
    collections::BTreeMap,
    net::{Ipv4Addr, SocketAddr},
    time::{Duration, Instant},
};
pub const SERVICE: &str = "_peerglide._udp.local.";
pub const PAIRING_SERVICE: &str = "_peerglide-pair._tcp.local.";
#[derive(Clone, Copy, PartialEq, Eq)]
enum Purpose {
    Input,
    Pairing,
}
impl Purpose {
    fn service(self) -> &'static str {
        match self {
            Self::Input => SERVICE,
            Self::Pairing => PAIRING_SERVICE,
        }
    }
}
const MAX_HINTS: usize = 64;
const MAX_EVENTS: usize = 128;
const EXPIRY: Duration = Duration::from_secs(90);
#[derive(Clone, Debug)]
pub struct UntrustedHint {
    pub name: String,
    pub fingerprint: String,
    pub addresses: Vec<SocketAddr>,
    expires: Instant,
}
fn fingerprint_valid(fp: &str) -> bool {
    fp.len() == 64 && fp.bytes().all(|b| b.is_ascii_hexdigit())
}
impl UntrustedHint {
    pub fn new(
        name: &str,
        fingerprint: &str,
        version: &str,
        addresses: impl IntoIterator<Item = Ipv4Addr>,
        port: u16,
        now: Instant,
    ) -> Option<Self> {
        Self::for_purpose(
            Purpose::Input,
            name,
            fingerprint,
            version,
            addresses,
            port,
            now,
        )
    }
    /// Pairing-v2 hints are addresses only, never authenticated identities.
    pub fn pairing(
        name: &str,
        fingerprint: &str,
        input_version: &str,
        pairing_version: &str,
        addresses: impl IntoIterator<Item = Ipv4Addr>,
        port: u16,
        now: Instant,
    ) -> Option<Self> {
        if pairing_version != "2" {
            return None;
        }
        Self::for_purpose(
            Purpose::Pairing,
            name,
            fingerprint,
            input_version,
            addresses,
            port,
            now,
        )
    }
    fn for_purpose(
        purpose: Purpose,
        name: &str,
        fingerprint: &str,
        version: &str,
        addresses: impl IntoIterator<Item = Ipv4Addr>,
        port: u16,
        now: Instant,
    ) -> Option<Self> {
        if name.len() > 255
            || name.chars().any(char::is_control)
            || !name.to_ascii_lowercase().ends_with(purpose.service())
            || !fingerprint_valid(fingerprint)
            || (version.len() > 5
                || version.parse::<u16>().ok() != Some(peerglide_protocol::VERSION))
            || port == 0
        {
            return None;
        }
        let mut addresses: Vec<_> = addresses
            .into_iter()
            .take(64)
            .filter(|a| a.is_private() || a.is_link_local())
            .map(|a| SocketAddr::from((a, port)))
            .take(16)
            .collect();
        addresses.sort();
        addresses.dedup();
        addresses.truncate(4);
        if addresses.is_empty() {
            return None;
        }
        Some(Self {
            name: name.into(),
            fingerprint: fingerprint.to_ascii_lowercase(),
            addresses,
            expires: now.checked_add(EXPIRY)?,
        })
    }
}
#[derive(Default)]
pub struct HintTable(BTreeMap<String, UntrustedHint>);
impl HintTable {
    pub fn update(&mut self, hint: UntrustedHint, now: Instant) {
        self.expire(now);
        if self.0.len() < MAX_HINTS || self.0.contains_key(&hint.name) {
            self.0.insert(hint.name.clone(), hint);
        }
    }
    pub fn expire(&mut self, now: Instant) {
        self.0.retain(|_, h| h.expires > now);
    }
    pub fn hints(&self) -> impl Iterator<Item = &UntrustedHint> {
        self.0.values()
    }
    pub fn clear(&mut self) {
        self.0.clear();
    }
}
pub struct Discovery {
    purpose: Purpose,
    daemon: ServiceDaemon,
    events: Receiver<ServiceEvent>,
    monitor: Receiver<DaemonEvent>,
    published: Option<String>,
    refresh: Instant,
    pub table: HintTable,
}
pub struct PendingStop(Receiver<DaemonStatus>);
impl PendingStop {
    pub fn finished(&self) -> bool {
        self.0.try_recv().is_ok() || self.0.is_disconnected()
    }
}
impl Discovery {
    /// Explicit opt-in. No service or machine name is published at browse start.
    pub fn start() -> Result<Self> {
        let daemon = ServiceDaemon::new()?;
        Self::with_daemon(daemon)
    }
    /// Separate opt-in subscription. No code, identity or grant is published.
    pub fn start_pairing() -> Result<Self> {
        Self::for_purpose(ServiceDaemon::new()?, Purpose::Pairing)
    }
    fn with_daemon(daemon: ServiceDaemon) -> Result<Self> {
        Self::for_purpose(daemon, Purpose::Input)
    }
    fn for_purpose(daemon: ServiceDaemon, purpose: Purpose) -> Result<Self> {
        let setup = (|| -> Result<_> {
            let monitor = daemon.monitor()?;
            let events = daemon.browse(purpose.service())?;
            Ok((monitor, events))
        })();
        match setup {
            Ok((monitor, events)) => Ok(Self {
                purpose,
                daemon,
                events,
                monitor,
                published: None,
                refresh: Instant::now(),
                table: HintTable::default(),
            }),
            Err(error) => {
                let _ = daemon.shutdown();
                Err(error)
            }
        }
    }
    pub fn poll(&mut self, own_fingerprint: Option<&str>) -> Result<()> {
        let now = Instant::now();
        self.table.expire(now);
        for _ in 0..MAX_EVENTS {
            let Ok(event) = self.monitor.try_recv() else {
                break;
            };
            if let DaemonEvent::Error(_) = event {
                anyhow::bail!("mDNS network error; stop/retry discovery or use a manual address");
            }
        }
        for _ in 0..MAX_EVENTS {
            let Ok(event) = self.events.try_recv() else {
                break;
            };
            match event {
                ServiceEvent::ServiceResolved(info) => {
                    if self.purpose == Purpose::Pairing
                        && info.get_property_val_str("p") != Some("2")
                    {
                        continue;
                    }
                    if let Some(hint) = UntrustedHint::for_purpose(
                        self.purpose,
                        info.get_fullname(),
                        info.get_property_val_str("fp").unwrap_or(""),
                        info.get_property_val_str("v").unwrap_or(""),
                        info.get_addresses_v4(),
                        info.get_port(),
                        now,
                    ) {
                        if own_fingerprint != Some(hint.fingerprint.as_str()) {
                            self.table.update(hint, now);
                        }
                    }
                }
                ServiceEvent::ServiceRemoved(_, name) => {
                    self.table.0.remove(&name);
                }
                ServiceEvent::SearchStopped(_) => {
                    anyhow::bail!("discovery subscription stopped; retry or use a manual address")
                }
                _ => (),
            }
        }
        anyhow::ensure!(
            !self.events.is_disconnected(),
            "discovery event queue disconnected; stop/retry"
        );
        if now.duration_since(self.refresh) > Duration::from_secs(30) {
            self.daemon.stop_browse(self.purpose.service())?;
            self.events = self.daemon.browse(self.purpose.service())?;
            self.refresh = now;
        }
        Ok(())
    }
    /// Explicitly publish a public fingerprint for an already running listener.
    /// Never publishes the OS hostname, username, private key, permission or file.
    pub fn advertise(&mut self, fp: &str, port: u16) -> Result<()> {
        anyhow::ensure!(
            fingerprint_valid(fp) && port != 0,
            "valid public fingerprint/listener port required"
        );
        anyhow::ensure!(self.published.is_none(), "already advertising");
        let prefix = &fp[..12];
        let instance = format!("PeerGlide-{prefix}");
        let host = format!("peerglide-{prefix}.local.");
        let version = peerglide_protocol::VERSION.to_string();
        let mut properties = vec![("v", version.as_str()), ("fp", fp)];
        if self.purpose == Purpose::Pairing {
            properties.push(("p", "2"));
        }
        let info = ServiceInfo::new(
            self.purpose.service(),
            &instance,
            &host,
            "",
            port,
            &properties[..],
        )?
        .enable_addr_auto();
        let name = info.get_fullname().to_string();
        self.daemon.register(info)?;
        self.published = Some(name);
        Ok(())
    }
    pub fn is_advertising(&self) -> bool {
        self.published.is_some()
    }
    pub fn unpublish(&mut self) -> Result<()> {
        if let Some(name) = &self.published {
            self.daemon.unregister(name)?;
            self.published = None;
        }
        Ok(())
    }
    pub fn request_stop(&mut self) -> Result<PendingStop> {
        self.unpublish()?;
        let ack = self.daemon.shutdown()?;
        self.table.clear();
        Ok(PendingStop(ack))
    }
}
impl Drop for Discovery {
    fn drop(&mut self) {
        let _ = self.unpublish();
        let _ = self.daemon.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn hint(name: &str, now: Instant) -> UntrustedHint {
        UntrustedHint::new(
            name,
            &"a".repeat(64),
            "5",
            [Ipv4Addr::new(192, 168, 1, 2)],
            24870,
            now,
        )
        .unwrap()
    }
    #[test]
    fn hint_metadata_is_bounded_versioned_and_lan_only() {
        let now = Instant::now();
        let name = format!("test.{SERVICE}");
        assert!(UntrustedHint::new(&name, "abc", "5", [Ipv4Addr::LOCALHOST], 1, now).is_none());
        assert!(UntrustedHint::new(
            &name,
            &"a".repeat(64),
            "4",
            [Ipv4Addr::new(192, 168, 1, 2)],
            1,
            now
        )
        .is_none());
        assert!(UntrustedHint::new(
            &name,
            &"a".repeat(64),
            "5",
            [Ipv4Addr::new(8, 8, 8, 8)],
            1,
            now
        )
        .is_none());
        assert!(UntrustedHint::new(
            &format!("bad\n.{SERVICE}"),
            &"a".repeat(64),
            "5",
            [Ipv4Addr::new(192, 168, 1, 2)],
            1,
            now
        )
        .is_none());
        let addresses = (1..50).map(|n| Ipv4Addr::new(192, 168, 1, n));
        assert_eq!(
            UntrustedHint::new(&name, &"A".repeat(64), "5", addresses, 1, now)
                .unwrap()
                .addresses
                .len(),
            4
        );
    }
    #[test]
    fn table_flood_and_exact_expiry_do_not_grow_or_authorize() {
        let now = Instant::now();
        let mut table = HintTable::default();
        for i in 0..1000 {
            table.update(hint(&format!("node{i}.{SERVICE}"), now), now);
        }
        assert_eq!(table.hints().count(), 64);
        table.update(
            hint(&format!("node0.{SERVICE}"), now + Duration::from_secs(1)),
            now,
        );
        table.expire(now + EXPIRY);
        assert_eq!(table.hints().count(), 1);
        table.expire(now + EXPIRY + Duration::from_secs(1));
        assert_eq!(table.hints().count(), 0);
    }
    #[test]
    fn pairing_hints_reject_input_services_old_versions_and_non_lan_addresses() {
        let now = Instant::now();
        let name = format!("node.{PAIRING_SERVICE}");
        let fp = "a".repeat(64);
        let lan = [Ipv4Addr::new(192, 168, 1, 2)];
        assert!(UntrustedHint::new(&name, &fp, "5", lan, 24871, now).is_none());
        assert!(
            UntrustedHint::pairing(&format!("node.{SERVICE}"), &fp, "5", "2", lan, 24870, now)
                .is_none()
        );
        for version in ["", "1", "3", "02", "2\n"] {
            assert!(UntrustedHint::pairing(&name, &fp, "5", version, lan, 24871, now).is_none());
        }
        assert!(UntrustedHint::pairing(&name, &fp, "4", "2", lan, 24871, now).is_none());
        assert!(
            UntrustedHint::pairing(&name, &fp, "5", "2", [Ipv4Addr::LOCALHOST], 24871, now)
                .is_none()
        );
        assert!(UntrustedHint::pairing(&name, &fp, "5", "2", lan, 0, now).is_none());
        let mut table = HintTable::default();
        for i in 0..1000 {
            let hint = UntrustedHint::pairing(
                &format!("node{i}.{PAIRING_SERVICE}"),
                &fp,
                "5",
                "2",
                lan,
                24871,
                now,
            )
            .unwrap();
            table.update(hint, now);
        }
        assert_eq!(table.hints().count(), MAX_HINTS);
        table.expire(now + EXPIRY);
        assert_eq!(table.hints().count(), 0);
    }
    #[test]
    fn daemon_lifecycle_with_disabled_interfaces_withdraws_and_stops() -> Result<()> {
        for purpose in [Purpose::Input, Purpose::Pairing] {
            let socket = std::net::UdpSocket::bind("127.0.0.1:0")?;
            let port = socket.local_addr()?.port();
            drop(socket);
            let daemon = ServiceDaemon::new_with_port(port)?;
            // Ordered before browse/register; no interface can send a query or advertisement.
            daemon.disable_interface(mdns_sd::IfKind::All)?;
            let mut discovery = Discovery::for_purpose(daemon, purpose)?;
            assert!(!discovery.is_advertising());
            discovery.advertise(&"a".repeat(64), 24871)?;
            assert!(discovery.is_advertising());
            discovery.unpublish()?;
            assert!(!discovery.is_advertising());
            let stop = discovery.request_stop()?;
            assert!(matches!(
                stop.0.recv_timeout(Duration::from_secs(2)),
                Ok(DaemonStatus::Shutdown)
            ));
        }
        Ok(())
    }
}
