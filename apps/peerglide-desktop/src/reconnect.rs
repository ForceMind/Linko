//! Opt-in reconnection policy; identity/permission/native errors never become retries.
use anyhow::{Context, Result};
use std::{io::Read, path::Path, time::Duration};
#[derive(Debug)]
pub struct InitialWaitExpired;
impl std::fmt::Display for InitialWaitExpired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("first connection wait expired")
    }
}
impl std::error::Error for InitialWaitExpired {}
#[derive(Debug)]
struct IdentityFilesUnavailable;
impl std::fmt::Display for IdentityFilesUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("identity files changed or are unavailable")
    }
}

/// A resource deadline, not authentication or consent. Never resets on retry.
pub struct InitialWait {
    deadline: Option<tokio::time::Instant>,
    negotiated: bool,
}
impl InitialWait {
    pub fn new(seconds: Option<u64>) -> Self {
        Self {
            deadline: seconds
                .map(|seconds| tokio::time::Instant::now() + Duration::from_secs(seconds)),
            negotiated: false,
        }
    }
    pub fn deadline(&self) -> Option<tokio::time::Instant> {
        self.deadline.filter(|_| !self.negotiated)
    }
    pub fn waiting(&self) -> bool {
        self.deadline().is_some()
    }
    pub fn check(&self) -> Result<()> {
        anyhow::ensure!(
            !self
                .deadline()
                .is_some_and(|deadline| tokio::time::Instant::now() >= deadline),
            InitialWaitExpired
        );
        Ok(())
    }
    /// Called only after actual pinned TLS/Hello and final pin checks.
    pub fn negotiated(&mut self) -> Result<()> {
        self.check()?;
        self.negotiated = true;
        Ok(())
    }
}
#[derive(Default)]
pub struct Backoff(u32);
impl Backoff {
    pub fn next_delay(&mut self) -> Duration {
        let delay = Duration::from_secs(1u64 << self.0.min(3));
        self.0 = self.0.saturating_add(1);
        delay
    }
}
pub fn retryable(error: &anyhow::Error) -> bool {
    if error.is::<IdentityFilesUnavailable>()
        || error.is::<InitialWaitExpired>()
        || error.is::<peerglide_transport::InvalidMessageFrame>()
    {
        return false;
    }
    error.chain().any(|cause| {
        if let Some(error) = cause.downcast_ref::<quinn::ConnectionError>() {
            return matches!(
                error,
                quinn::ConnectionError::TimedOut | quinn::ConnectionError::Reset
            );
        }
        if cause.is::<tokio::time::error::Elapsed>() {
            return true;
        }
        cause.downcast_ref::<std::io::Error>().is_some_and(|error| {
            matches!(
                error.kind(),
                std::io::ErrorKind::ConnectionRefused
                    | std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::TimedOut
                    | std::io::ErrorKind::NetworkUnreachable
                    | std::io::ErrorKind::HostUnreachable
            )
        })
    })
}
fn unchanged(path: &Path, expected: &[u8]) -> Result<bool> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(expected.len() as u64 + 1)
        .read_to_end(&mut bytes)?;
    Ok(bytes == expected)
}
pub fn verify_pins(identity: &Path, peer_path: &Path, local: &[u8], peer: &[u8]) -> Result<()> {
    (|| -> Result<()> {
        anyhow::ensure!(
            peerglide_transport::Identity::load(identity)
                .context("verify usable local identity before connection")?
                .certificate
                == local,
            "local identity changed during reconnect; restart explicitly"
        );
        anyhow::ensure!(
            unchanged(peer_path, peer).context("verify enrolled peer before reconnect")?,
            "enrolled identity changed during reconnect; enrollment required"
        );
        Ok(())
    })()
    .context(IdentityFilesUnavailable)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn initial_wait_is_one_budget_and_never_times_out_a_negotiated_session() {
        let mut initial = InitialWait::new(Some(10));
        let deadline = initial.deadline().unwrap();
        for _ in 0..10 {
            assert_eq!(initial.deadline(), Some(deadline));
        }
        initial.negotiated().unwrap();
        initial.deadline = Some(tokio::time::Instant::now() - Duration::from_secs(1));
        assert!(!initial.waiting() && initial.deadline().is_none());
        initial.check().unwrap();
        let mut expired = InitialWait {
            deadline: Some(tokio::time::Instant::now() - Duration::from_secs(1)),
            negotiated: false,
        };
        assert!(expired.check().is_err() && expired.negotiated().is_err());
        assert!(expired.waiting());
        assert!(!retryable(&InitialWaitExpired.into()));
        assert!(!retryable(
            &anyhow::Error::from(std::io::Error::from(std::io::ErrorKind::TimedOut))
                .context(peerglide_transport::InvalidMessageFrame)
        ));
        let disk_error = anyhow::Error::from(std::io::Error::from(std::io::ErrorKind::TimedOut))
            .context(IdentityFilesUnavailable);
        assert!(!retryable(&disk_error));
        assert!(!InitialWait::new(None).waiting());
    }
    #[test]
    fn delays_are_rate_bounded_and_do_not_overflow() {
        let mut policy = Backoff::default();
        for expected in [1, 2, 4, 8, 8] {
            assert_eq!(policy.next_delay(), Duration::from_secs(expected));
        }
        for _ in 0..1000 {
            assert_eq!(policy.next_delay(), Duration::from_secs(8));
        }
    }
    #[test]
    fn security_and_native_failures_are_not_network_retries() {
        assert!(retryable(&quinn::ConnectionError::TimedOut.into()));
        assert!(retryable(&quinn::ConnectionError::Reset.into()));
        assert!(retryable(
            &quinn::ReadError::ConnectionLost(quinn::ConnectionError::TimedOut).into()
        ));
        assert!(!retryable(&quinn::ConnectionError::LocallyClosed.into()));
        assert!(!retryable(&anyhow::anyhow!(
            "peer certificate differs from enrolled identity"
        )));
        assert!(!retryable(&anyhow::anyhow!("native input cleanup failed")));
        assert!(!retryable(
            &std::io::Error::from(std::io::ErrorKind::PermissionDenied).into()
        ));
    }
    #[test]
    fn pin_changes_and_deletion_stop_retry() -> Result<()> {
        let id = peerglide_transport::Identity::generate()?;
        let root = std::env::temp_dir().join(format!("peerglide-pins-{}", id.fingerprint()));
        std::fs::create_dir(&root)?;
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(root.clone());
        let peer = root.join("peer.der");
        let local = peerglide_transport::Identity::create(&root)?.certificate;
        std::fs::write(&peer, b"peer")?;
        verify_pins(&root, &peer, &local, b"peer")?;
        std::fs::write(&peer, b"replacement")?;
        assert!(verify_pins(&root, &peer, &local, b"peer").is_err());
        std::fs::remove_file(&peer)?;
        assert!(verify_pins(&root, &peer, &local, b"peer").is_err());
        std::fs::write(&peer, b"peer")?;
        std::fs::remove_file(root.join("node.key.der"))?;
        assert!(verify_pins(&root, &peer, &local, b"peer").is_err());
        Ok(())
    }
}
