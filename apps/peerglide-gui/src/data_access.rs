//! Local authorization and address checks shared by explicit data jobs.
use super::{transport, App, Identity};
use anyhow::{Context, Result};
use std::net::{IpAddr, SocketAddr};

pub(super) fn ready(app: &App) -> bool {
    app.writable()
        && !app.enrollment_blocked
        && !app.pairing_pending()
        && !app.close_requested
        && !app.exit_confirmed
        && app.config.fingerprint.is_some()
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct Pin {
    local: String,
    peer: String,
}

impl Pin {
    pub(super) fn matches(&self, app: &App) -> bool {
        ready(app) && app.config.fingerprint.as_deref() == Some(self.peer.as_str())
    }

    #[cfg(any(all(windows, target_env = "msvc"), target_os = "macos", test))]
    pub(super) fn recheck(&self, app: &App) -> Result<()> {
        let (current, _, _) = Self::load(app)?;
        anyhow::ensure!(&current == self, "本机或已登记的身份发生变化");
        Ok(())
    }

    #[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
    pub(super) fn authenticates(&self, origin: transport::file_transfer::DropOrigin) -> bool {
        origin.matches_peer(&self.peer)
    }

    pub(super) fn load(app: &App) -> Result<(Self, Identity, Vec<u8>)> {
        anyhow::ensure!(ready(app), "请先完成设备配对并打开可写窗口");
        app.ensure_pairing_ready().context("请先完成设备配对核验")?;
        let identity = Identity::load(&app.identity_dir()).context("无法读取本机身份")?;
        let peer = transport::read_certificate(&app.dir.join("trusted-peer.crt.der"))
            .context("无法读取已登记的证书")?;
        let fingerprint = transport::fingerprint(&peer);
        anyhow::ensure!(
            app.config.fingerprint.as_deref() == Some(fingerprint.as_str()),
            "已登记证书发生变化，请重新核验"
        );
        Ok((
            Self {
                local: identity.fingerprint(),
                peer: fingerprint,
            },
            identity,
            peer,
        ))
    }
}

pub(super) fn parse_address(text: &str) -> Result<SocketAddr> {
    let address: SocketAddr = text.trim().parse()?;
    anyhow::ensure!(
        address.port() != 0 && !address.ip().is_multicast() && !is_broadcast(address.ip()),
        "需要有效的单播地址和非零端口"
    );
    Ok(address)
}

fn is_broadcast(address: IpAddr) -> bool {
    matches!(address, IpAddr::V4(ip) if ip.is_broadcast())
}

pub(super) fn saved_target(saved: &str, port: u16) -> Option<SocketAddr> {
    let mut address = parse_address(saved).ok()?;
    if address.ip().is_unspecified() {
        return None;
    }
    address.set_port(port);
    Some(address)
}

pub(super) fn send_address(text: &str, saved: &str, port: u16) -> Result<SocketAddr> {
    let address = if text.trim().is_empty() {
        saved_target(saved, port).context("请输入接收端 IP 地址和 UDP 端口")?
    } else {
        parse_address(text).context("请输入接收端 IP 地址和 UDP 端口")?
    };
    anyhow::ensure!(!address.ip().is_unspecified(), "请输入接收端的实际 IP 地址");
    Ok(address)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_pin_checks_real_certificates_without_granting_input() -> Result<()> {
        let fixture = super::super::tests::Fixture::new()?;
        let mut app = App::load(fixture.path.clone());
        let local = Identity::create(&app.identity_dir())?;
        let peer = Identity::generate()?;
        let peer_path = app.dir.join("trusted-peer.crt.der");
        std::fs::write(&peer_path, &peer.certificate)?;
        assert!(Pin::load(&app).is_err());

        app.config.fingerprint = Some(peer.fingerprint());
        let (pin, _, _) = Pin::load(&app)?;
        assert_eq!(pin.local, local.fingerprint());
        assert_eq!(pin.peer, peer.fingerprint());
        assert!(!app.config.allow && !app.config.native);

        std::fs::write(&peer_path, Identity::generate()?.certificate)?;
        assert!(Pin::load(&app).is_err());
        assert!(pin.recheck(&app).is_err());

        std::fs::write(peer_path, &peer.certificate)?;
        pin.recheck(&app)?;
        std::fs::rename(app.identity_dir(), fixture.path.join("retired-identity"))?;
        Identity::create(&app.identity_dir())?;
        assert!(pin.matches(&app));
        assert!(pin.recheck(&app).is_err());
        drop(app);
        fixture.close()
    }

    #[test]
    fn explicit_target_and_saved_address_use_distinct_data_port_rules() -> Result<()> {
        assert_eq!(
            send_address("", "[2001:db8::2]:24871", 24872)?,
            "[2001:db8::2]:24872".parse()?
        );
        assert_eq!(
            send_address("192.0.2.1:9000", "invalid", 24872)?,
            "192.0.2.1:9000".parse()?
        );
        assert!(send_address("", "0.0.0.0:24871", 24872).is_err());
        assert!(send_address("[::]:24872", "", 24872).is_err());
        assert!(send_address("224.0.0.1:24872", "", 24872).is_err());
        Ok(())
    }
}
