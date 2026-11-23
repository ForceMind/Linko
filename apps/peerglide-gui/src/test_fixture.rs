use anyhow::{Context, Result};
use std::path::PathBuf;

pub(super) struct Fixture {
    pub(super) path: PathBuf,
    live: bool,
}

impl Fixture {
    pub(super) fn new() -> Result<Self> {
        static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        for _ in 0..100 {
            let path = std::env::temp_dir().join(format!(
                "peerglide-ui-{}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)?
                    .as_nanos(),
                NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            match std::fs::create_dir(&path) {
                Ok(()) => return Ok(Self { path, live: true }),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => {
                    return Err(e).with_context(|| format!("create GUI fixture {}", path.display()))
                }
            }
        }
        anyhow::bail!("unable to reserve a unique GUI test directory")
    }

    pub(super) fn close(mut self) -> Result<()> {
        std::fs::remove_dir_all(&self.path)
            .with_context(|| format!("cleanup GUI fixture {}", self.path.display()))?;
        self.live = false;
        Ok(())
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if self.live {
            if let Err(e) = std::fs::remove_dir_all(&self.path) {
                eprintln!("GUI fixture cleanup failed at {}: {e}", self.path.display());
            }
        }
    }
}
