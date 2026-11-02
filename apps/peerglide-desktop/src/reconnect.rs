//! Opt-in reconnection policy; identity/permission/native errors never become retries.
use anyhow::{Context, Result};
use std::{io::Read, path::Path, time::Duration};
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
    anyhow::ensure!(
        unchanged(&identity.join("node.crt.der"), local)
            .context("verify local identity before reconnect")?,
        "local identity changed during reconnect; restart explicitly"
    );
    anyhow::ensure!(
        unchanged(peer_path, peer).context("verify enrolled peer before reconnect")?,
        "enrolled identity changed during reconnect; enrollment required"
    );
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
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
        std::fs::write(root.join("node.crt.der"), b"local")?;
        std::fs::write(&peer, b"peer")?;
        verify_pins(&root, &peer, b"local", b"peer")?;
        std::fs::write(&peer, b"replacement")?;
        assert!(verify_pins(&root, &peer, b"local", b"peer").is_err());
        std::fs::remove_file(&peer)?;
        assert!(verify_pins(&root, &peer, b"local", b"peer").is_err());
        Ok(())
    }
}
