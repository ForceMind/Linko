//! Bounded, versioned local JSON with compare-before-save and cooperating-writer
//! OS locks. No key/trust-file transaction or hostile-filesystem guarantee.
use anyhow::{Context, Result};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};
pub const MAX_BYTES: usize = 64 * 1024;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope<T> {
    store_version: u16,
    data: T,
}
#[derive(Serialize)]
struct EnvelopeRef<'a, T> {
    store_version: u16,
    data: &'a T,
}
pub struct Store {
    path: PathBuf,
    expected: Option<blake3::Hash>,
}
fn read(path: &Path) -> Result<Option<Vec<u8>>> {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let metadata = file.metadata()?;
    anyhow::ensure!(
        metadata.is_file() && metadata.len() <= MAX_BYTES as u64,
        "configuration must be a regular file under 64 KiB"
    );
    let mut bytes = Vec::new();
    file.take(MAX_BYTES as u64 + 1).read_to_end(&mut bytes)?;
    anyhow::ensure!(bytes.len() <= MAX_BYTES, "configuration exceeds 64 KiB");
    Ok(Some(bytes))
}
fn options() -> OpenOptions {
    let mut o = OpenOptions::new();
    o.write(true).read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o
}
pub struct Lock(File);
impl Drop for Lock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}
pub fn lock(path: &Path) -> Result<Lock> {
    let file = options().create(true).truncate(false).open(path)?;
    file.try_lock()
        .map_err(|e| anyhow::anyhow!("another PeerGlide operation owns this configuration: {e}"))?;
    Ok(Lock(file))
}
impl Store {
    pub fn open<T: DeserializeOwned + Default>(path: PathBuf) -> Result<(Self, T)> {
        let bytes = read(&path)?;
        let expected = bytes.as_deref().map(blake3::hash);
        let data = match bytes {
            None => T::default(),
            Some(bytes) => {
                let value: serde_json::Value = serde_json::from_slice(&bytes)
                    .context("invalid configuration JSON; original preserved")?;
                if value.get("store_version").is_some() {
                    let e: Envelope<T> = serde_json::from_value(value)?;
                    anyhow::ensure!(
                        e.store_version == 1,
                        "unsupported configuration version; original preserved"
                    );
                    e.data
                } else {
                    serde_json::from_value(value)
                        .context("invalid legacy configuration; original preserved")?
                }
            }
        };
        Ok((Self { path, expected }, data))
    }
    pub fn save<T: Serialize>(&mut self, data: &T) -> Result<()> {
        let path = self.path.clone();
        let _lock = lock(&path.with_extension("lock"))?;
        let previous = read(&path)?;
        anyhow::ensure!(
            previous.as_deref().map(blake3::hash) == self.expected,
            "configuration changed in another process; reload before saving"
        );
        self.commit(data, previous)
    }
    /// Only invoked by an explicit GUI reset, never automatic on parse/version error.
    pub fn reset<T: Serialize + Default>(path: PathBuf) -> Result<(Self, T)> {
        let _lock = lock(&path.with_extension("lock"))?;
        let previous = read(&path)?;
        let mut store = Self {
            path,
            expected: previous.as_deref().map(blake3::hash),
        };
        let data = T::default();
        store.commit(&data, previous)?;
        Ok((store, data))
    }
    fn commit<T: Serialize>(&mut self, data: &T, previous: Option<Vec<u8>>) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(&EnvelopeRef {
            store_version: 1,
            data,
        })?;
        anyhow::ensure!(bytes.len() <= MAX_BYTES, "configuration exceeds 64 KiB");
        if let Some(previous) = previous {
            let backup = self
                .path
                .with_extension(format!("bak-{}", blake3::hash(&previous).to_hex()));
            match options().create_new(true).open(&backup) {
                Ok(mut f) => {
                    f.write_all(&previous)?;
                    f.sync_all()?;
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    anyhow::ensure!(
                        read(&backup)?.as_deref() == Some(previous.as_slice()),
                        "configuration backup differs; refusing overwrite"
                    );
                }
                Err(e) => return Err(e).context("create configuration backup"),
            }
        }
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let parent = self.path.parent().context("configuration parent")?;
        let mut temporary = None;
        for _ in 0..16 {
            let path = parent.join(format!(
                ".peerglide-config-{}-{}.tmp",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match options().create_new(true).open(&path) {
                Ok(file) => {
                    temporary = Some((path, file));
                    break;
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e).context("create configuration temporary file"),
            }
        }
        let (path, mut file) = temporary.context("reserve configuration temporary file")?;
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }
        let cleanup = Cleanup(path);
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&cleanup.0, &self.path).context("replace configuration atomically")?;
        self.expected = Some(blake3::hash(&bytes));
        #[cfg(unix)]
        File::open(parent)?
            .sync_all()
            .context("configuration replaced but directory sync failed")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Debug, Default, Serialize, Deserialize, PartialEq)]
    #[serde(default, deny_unknown_fields)]
    struct Settings {
        enabled: bool,
        n: u32,
    }
    fn fixture() -> Result<super::super::tests::Fixture> {
        super::super::tests::Fixture::new()
    }
    #[test]
    fn legacy_migrates_with_exact_backup_and_new_defaults_off() -> Result<()> {
        let f = fixture()?;
        let path = f.path.join("settings.json");
        let old = br#"{"n":7}"#;
        std::fs::write(&path, old)?;
        let (mut store, mut s) = Store::open::<Settings>(path.clone())?;
        assert!(!s.enabled);
        assert_eq!(s.n, 7);
        s.n = 8;
        store.save(&s)?;
        assert_eq!(Store::open::<Settings>(path.clone())?.1, s);
        let backup = path.with_extension(format!("bak-{}", blake3::hash(old).to_hex()));
        assert_eq!(std::fs::read(backup)?, old);
        Ok(())
    }
    #[test]
    fn corrupt_future_and_oversized_configuration_is_preserved_until_explicit_reset() -> Result<()>
    {
        let f = fixture()?;
        let path = f.path.join("settings.json");
        for bytes in [
            b"broken".to_vec(),
            br#"{"store_version":2,"data":{}}"#.to_vec(),
            vec![0; MAX_BYTES + 1],
        ] {
            std::fs::write(&path, &bytes)?;
            assert!(Store::open::<Settings>(path.clone()).is_err());
            assert_eq!(std::fs::read(&path)?, bytes);
        }
        std::fs::write(&path, b"broken")?;
        let (_, s) = Store::reset::<Settings>(path.clone())?;
        assert!(!s.enabled);
        assert_eq!(Store::open::<Settings>(path)?.1, s);
        Ok(())
    }
    #[test]
    fn concurrent_snapshot_cannot_overwrite_and_os_lock_releases_on_drop() -> Result<()> {
        let f = fixture()?;
        let path = f.path.join("settings.json");
        let (mut a, _) = Store::open::<Settings>(path.clone())?;
        let (mut b, _) = Store::open::<Settings>(path.clone())?;
        a.save(&Settings {
            n: 1,
            ..Default::default()
        })?;
        assert!(b
            .save(&Settings {
                n: 2,
                ..Default::default()
            })
            .is_err());
        assert_eq!(Store::open::<Settings>(path.clone())?.1.n, 1);
        let guard = lock(&path.with_extension("lock"))?;
        assert!(a.save(&Settings::default()).is_err());
        let inherited = guard.0.try_clone()?;
        drop(guard);
        a.save(&Settings::default())?;
        drop(inherited);
        Ok(())
    }
}
