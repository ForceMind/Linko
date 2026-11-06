//! Exercise the GUI's actual process owner against the actual CLI, not forged
//! Authenticated fixtures. CARGO_BIN_EXE is build provenance, never peer auth.
#[path = "../../peerglide-gui/src/pairing_supervisor.rs"]
pub mod pairing_supervisor;
use anyhow::{Context, Result};
use pairing_supervisor::{Mode, Phase, Supervisor};
use peerglide_protocol::pairing_worker::{Failure, StopReason};
use peerglide_transport::Identity;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Result<Self> {
        let path = std::env::temp_dir().join(format!(
            "peerglide-parent-{}",
            Identity::generate()?.fingerprint()
        ));
        std::fs::create_dir(&path)?;
        Ok(Self(path))
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn wait(
    a: &mut Supervisor,
    b: &mut Supervisor,
    ready: impl Fn(&Supervisor, &Supervisor) -> bool,
) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        a.poll();
        b.poll();
        if ready(a, b) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    anyhow::bail!("pairing parent process deadline exceeded")
}
fn start_offer(
    a: &mut Supervisor,
    b: &mut Supervisor,
    binary: &Path,
    identity: &Path,
) -> Result<(String, std::net::SocketAddr)> {
    a.start(
        binary,
        identity,
        Mode::Offer("127.0.0.1:0".parse()?),
        Arc::new(|| {}),
    )?;
    wait(a, b, |a, _| a.displayed_code().is_some())?;
    let (code, address) = a.displayed_code().context("missing private display code")?;
    Ok((code.to_owned(), address))
}

#[test]
fn owned_children_authenticate_cancel_reap_and_never_reuse_old_observations() -> Result<()> {
    let fixture = Fixture::new()?;
    let aa = fixture.0.join("a");
    let bb = fixture.0.join("b");
    let aid = Identity::create(&aa)?;
    let bid = Identity::create(&bb)?;
    let akey = std::fs::read(aa.join("node.key.der"))?;
    let bkey = std::fs::read(bb.join("node.key.der"))?;
    let binary = Path::new(env!("CARGO_BIN_EXE_peerglide"));
    let mut a = Supervisor::default();
    let mut b = Supervisor::default();
    let (code, address) = start_offer(&mut a, &mut b, binary, &aa)?;
    b.start(binary, &bb, Mode::connect(address, code)?, Arc::new(|| {}))?;
    wait(&mut a, &mut b, |a, b| {
        a.observation().is_some() && b.observation().is_some()
    })?;
    let first_context = a.observation().unwrap().context();
    assert!(a.observation().unwrap().certificate() == bid.certificate);
    assert!(b.observation().unwrap().certificate() == aid.certificate);
    assert_eq!(first_context, b.observation().unwrap().context());
    assert!(a.local_certificate_snapshot() == Some(aid.certificate.as_slice()));
    let first_generation = a.generation();
    a.cancel();
    assert!(a.observation().is_none() && a.displayed_code().is_none());
    assert!(a.generation() > first_generation);
    assert!(a
        .start(
            binary,
            &aa,
            Mode::Offer("127.0.0.1:0".parse()?),
            Arc::new(|| {})
        )
        .is_err());
    wait(&mut a, &mut b, |a, b| !a.needs_poll() && !b.needs_poll())?;
    assert!(matches!(a.phase(), Phase::Stopped(StopReason::Cancelled)));
    assert!(matches!(
        b.phase(),
        Phase::Stopped(StopReason::PeerDisconnected)
    ));
    assert!(a.observation().is_none() && b.observation().is_none());

    let (code, address) = start_offer(&mut a, &mut b, binary, &aa)?;
    b.start(binary, &bb, Mode::connect(address, code)?, Arc::new(|| {}))?;
    wait(&mut a, &mut b, |a, b| {
        a.observation().is_some() && b.observation().is_some()
    })?;
    assert_ne!(first_context, a.observation().unwrap().context());
    a.stop_and_reap();
    b.stop_and_reap();
    assert!(
        !a.needs_poll()
            && !b.needs_poll()
            && a.observation().is_none()
            && b.observation().is_none()
    );
    assert!(std::fs::read(aa.join("node.key.der"))? == akey);
    assert!(std::fs::read(bb.join("node.key.der"))? == bkey);
    assert!(!fixture.0.join("settings.json").exists());
    assert!(!aa.join("trusted-peer.crt.der").exists() && !bb.join("trusted-peer.crt.der").exists());
    Ok(())
}

#[test]
fn failed_code_and_missing_identity_never_produce_an_observation() -> Result<()> {
    let fixture = Fixture::new()?;
    let aa = fixture.0.join("a");
    let bb = fixture.0.join("b");
    Identity::create(&aa)?;
    Identity::create(&bb)?;
    let binary = Path::new(env!("CARGO_BIN_EXE_peerglide"));
    let mut a = Supervisor::default();
    let mut b = Supervisor::default();
    let (code, address) = start_offer(&mut a, &mut b, binary, &aa)?;
    let wrong = if code == "12345678" {
        "87654321"
    } else {
        "12345678"
    };
    b.start(
        binary,
        &bb,
        Mode::connect(address, wrong.into())?,
        Arc::new(|| {}),
    )?;
    wait(&mut a, &mut b, |_, b| !b.needs_poll())?;
    assert!(matches!(b.phase(), Phase::Failed(Failure::Authentication)));
    assert!(b.observation().is_none());
    a.stop_and_reap();
    assert!(a
        .start(
            binary,
            &fixture.0.join("missing"),
            Mode::Offer(address),
            Arc::new(|| {})
        )
        .is_err());
    assert!(!a.needs_poll() && a.observation().is_none());
    Ok(())
}
