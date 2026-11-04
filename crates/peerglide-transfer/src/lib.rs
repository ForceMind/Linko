//! Offline D03 storage foundation. No network, OS drop, permission grant or GUI.
//!
//! Callers must separately authenticate/authorize the sender and explicitly select
//! a private destination directory. `PeerBinding` is an expected TLS identity, not
//! an authentication mechanism. An untrusted wire decoder must cap bytes before
//! invoking any serde deserializer. Hostile local filesystem races are out of scope.
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

pub const CHUNK_BYTES: usize = 256 * 1024;
pub const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024 * 1024;
pub const MAX_MANIFEST_BYTES: usize = 5 * 1024 * 1024;
pub const VERSION: u16 = 1;
pub type Digest = [u8; 32];

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    version: u16,
    name: String,
    size: u64,
    whole: Digest,
    chunks: Vec<Digest>,
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

/// One portable, displayable basename, never a path. Reserve Windows device names
/// on every OS so an offer cannot gain different path semantics after migration.
pub fn valid_name(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    let device = ["CON", "PRN", "AUX", "NUL", "CLOCK$", "CONIN$", "CONOUT$"]
        .contains(&stem.as_str())
        || stem
            .strip_prefix("COM")
            .or_else(|| stem.strip_prefix("LPT"))
            .is_some_and(|suffix| {
                matches!(
                    suffix,
                    "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                )
            });
    !name.is_empty()
        && name.len() <= 200
        && name.trim() == name
        && name
            .split('.')
            .next()
            .is_some_and(|stem| stem.trim() == stem)
        && !name.starts_with('.')
        && !name.ends_with([' ', '.'])
        && !name.chars().any(|c| {
            c.is_control()
                || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
                || "<>:\"/\\|?*".contains(c)
        })
        && !device
}

impl Manifest {
    /// Hash a bounded stream; size changes and later chunk changes reject.
    /// This is not an atomic filesystem snapshot of a concurrently edited file.
    /// A regular file only: no directory/symlink traversal is implemented.
    pub fn from_file(path: &Path) -> io::Result<Self> {
        if !fs::symlink_metadata(path)?.file_type().is_file() {
            return Err(invalid("source must be a regular non-symlink file"));
        }
        let mut file = File::open(path)?;
        let size = file.metadata()?.len();
        if size > MAX_FILE_BYTES {
            return Err(invalid("file exceeds limit"));
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| invalid("non-Unicode basename"))?
            .to_owned();
        if !valid_name(&name) {
            return Err(invalid("unsupported basename"));
        }
        let mut chunks = Vec::with_capacity(size.div_ceil(CHUNK_BYTES as u64) as usize);
        let mut whole = blake3::Hasher::new();
        let mut buffer = vec![0; CHUNK_BYTES];
        let mut remaining = size;
        while remaining > 0 {
            let len = remaining.min(CHUNK_BYTES as u64) as usize;
            file.read_exact(&mut buffer[..len])?;
            whole.update(&buffer[..len]);
            chunks.push(*blake3::hash(&buffer[..len]).as_bytes());
            remaining -= len as u64;
        }
        if file.read(&mut [0])? != 0 || file.metadata()?.len() != size {
            return Err(invalid("source size changed"));
        }
        Ok(Self {
            version: VERSION,
            name,
            size,
            whole: *whole.finalize().as_bytes(),
            chunks,
        })
    }
    fn validate(&self) -> io::Result<()> {
        if self.version != VERSION
            || !valid_name(&self.name)
            || self.size > MAX_FILE_BYTES
            || self.chunks.len() != self.size.div_ceil(CHUNK_BYTES as u64) as usize
        {
            return Err(invalid("invalid manifest"));
        }
        if self.size == 0 && self.whole != *blake3::hash(&[]).as_bytes() {
            return Err(invalid("invalid empty digest"));
        }
        Ok(())
    }
    pub fn decode(bytes: &[u8]) -> io::Result<Self> {
        if bytes.len() > MAX_MANIFEST_BYTES {
            return Err(invalid("manifest exceeds limit"));
        }
        let value: Self =
            serde_json::from_slice(bytes).map_err(|_| invalid("malformed manifest"))?;
        value.validate()?;
        Ok(value)
    }
    pub fn encode(&self) -> io::Result<Vec<u8>> {
        self.validate()?;
        let bytes = serde_json::to_vec(self).map_err(|_| invalid("manifest serialization"))?;
        if bytes.len() > MAX_MANIFEST_BYTES {
            return Err(invalid("manifest exceeds limit"));
        }
        Ok(bytes)
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn size(&self) -> u64 {
        self.size
    }
    pub fn chunks(&self) -> usize {
        self.chunks.len()
    }
    fn chunk_len(&self, index: usize) -> io::Result<usize> {
        if index >= self.chunks.len() {
            return Err(invalid("chunk index out of range"));
        }
        Ok((self.size - index as u64 * CHUNK_BYTES as u64).min(CHUNK_BYTES as u64) as usize)
    }
    /// Sender reads from the still-open source; reject changes against frozen hashes.
    pub fn read_chunk(&self, source: &mut File, index: usize) -> io::Result<Vec<u8>> {
        let mut bytes = vec![0; self.chunk_len(index)?];
        source.seek(SeekFrom::Start(index as u64 * CHUNK_BYTES as u64))?;
        source.read_exact(&mut bytes)?;
        if blake3::hash(&bytes).as_bytes() != &self.chunks[index] {
            return Err(invalid("source changed"));
        }
        Ok(bytes)
    }
}

/// Expected authenticated peer fingerprint and random transfer ID. Neither is a
/// grant: the session layer must check current per-direction file permission.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerBinding {
    fingerprint: Digest,
    transfer_id: [u8; 16],
}
impl PeerBinding {
    pub fn new(fingerprint: Digest, transfer_id: [u8; 16]) -> Self {
        Self {
            fingerprint,
            transfer_id,
        }
    }
}

#[derive(PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    binding: PeerBinding,
    manifest: Manifest,
}

pub struct Receiver {
    manifest: Manifest,
    dir: PathBuf,
    partial: File,
    present: Vec<bool>,
    verified_bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Progress {
    pub verified_bytes: u64,
    pub total_bytes: u64,
    pub chunks_complete: bool,
}

impl Receiver {
    /// Explicitly chosen existing directory. Never creates a peer-supplied path.
    /// Leaves partials after failures for explicit recovery, never overwrites one.
    pub fn create(root: &Path, binding: PeerBinding, manifest: Manifest) -> io::Result<Self> {
        manifest.validate()?;
        require_dir(root)?;
        let receipt = Receipt {
            binding,
            manifest: manifest.clone(),
        };
        let bytes = serde_json::to_vec(&receipt).map_err(|_| invalid("receipt serialization"))?;
        let dir = root.join(format!(".peerglide-{}", blake3::hash(&bytes).to_hex()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            fs::DirBuilder::new().mode(0o700).create(&dir)?;
        }
        #[cfg(not(unix))]
        fs::create_dir(&dir)?;
        let mut receipt_file = exclusive(&dir.join("receipt.json"))?;
        receipt_file.write_all(&bytes)?;
        receipt_file.sync_all()?;
        let partial = exclusive(&dir.join("partial"))?;
        partial
            .try_lock()
            .map_err(|_| invalid("partial already in use"))?;
        partial.set_len(manifest.size)?;
        partial.sync_all()?;
        sync_dir(&dir)?;
        sync_dir(root)?;
        Ok(Self {
            present: vec![false; manifest.chunks.len()],
            manifest,
            dir,
            partial,
            verified_bytes: 0,
        })
    }
    /// Re-verify every partial chunk rather than trusting persisted counters.
    /// Same binding, exact canonical manifest and regular private files required.
    pub fn resume(root: &Path, binding: PeerBinding, manifest: Manifest) -> io::Result<Self> {
        manifest.validate()?;
        require_dir(root)?;
        let expected = serde_json::to_vec(&Receipt {
            binding,
            manifest: manifest.clone(),
        })
        .map_err(|_| invalid("receipt serialization"))?;
        let dir = root.join(format!(".peerglide-{}", blake3::hash(&expected).to_hex()));
        require_dir(&dir)?;
        if dir.join("committing").try_exists()? {
            return Err(invalid("commit started; inspect destination manually"));
        }
        require_regular(&dir.join("receipt.json"))?;
        let mut actual = Vec::new();
        File::open(dir.join("receipt.json"))?
            .take((MAX_MANIFEST_BYTES + 1024) as u64 + 1)
            .read_to_end(&mut actual)?;
        if actual != expected {
            return Err(invalid("resume identity or manifest mismatch"));
        }
        require_regular(&dir.join("partial"))?;
        let partial = OpenOptions::new()
            .read(true)
            .write(true)
            .open(dir.join("partial"))?;
        partial
            .try_lock()
            .map_err(|_| invalid("partial already in use"))?;
        // Recheck under the partial lock so a competing commit cannot be resumed.
        if dir.join("committing").try_exists()? {
            return Err(invalid("commit started; inspect destination manually"));
        }
        if partial.metadata()?.len() != manifest.size {
            return Err(invalid("partial size changed"));
        }
        let mut receiver = Self {
            present: vec![false; manifest.chunks.len()],
            manifest,
            dir,
            partial,
            verified_bytes: 0,
        };
        let mut buffer = vec![0; CHUNK_BYTES];
        for index in 0..receiver.manifest.chunks.len() {
            let len = receiver.manifest.chunk_len(index)?;
            receiver.partial.read_exact(&mut buffer[..len])?;
            if blake3::hash(&buffer[..len]).as_bytes() == &receiver.manifest.chunks[index] {
                receiver.present[index] = true;
                receiver.verified_bytes += len as u64;
            }
        }
        Ok(receiver)
    }
    pub fn progress(&self) -> Progress {
        Progress {
            verified_bytes: self.verified_bytes,
            total_bytes: self.manifest.size,
            chunks_complete: self.present.iter().all(|p| *p),
        }
    }
    pub fn missing(&self) -> impl Iterator<Item = usize> + '_ {
        self.present
            .iter()
            .enumerate()
            .filter_map(|(i, present)| (!present).then_some(i))
    }
    /// Input slice is bounded and hashed before touching disk; a duplicate valid
    /// chunk is idempotent. The future wire reader must cap allocation itself.
    pub fn write_chunk(&mut self, index: usize, bytes: &[u8]) -> io::Result<Progress> {
        if bytes.len() != self.manifest.chunk_len(index)?
            || blake3::hash(bytes).as_bytes() != &self.manifest.chunks[index]
        {
            return Err(invalid("chunk length or digest mismatch"));
        }
        if !self.present[index] {
            self.partial
                .seek(SeekFrom::Start(index as u64 * CHUNK_BYTES as u64))?;
            self.partial.write_all(bytes)?;
            self.partial.sync_all()?;
            self.present[index] = true;
            self.verified_bytes += bytes.len() as u64;
        }
        Ok(self.progress())
    }
    /// Recheck whole content, then atomically add a non-overwriting hard link in
    /// the same directory/filesystem. No execute/open, no overwrite rename fallback.
    /// On errors after publication, the final file may exist: callers inspect it,
    /// never automatically replay. Filesystems without hard links are unsupported.
    pub fn commit(self, root: &Path) -> io::Result<PathBuf> {
        self.commit_checked(root, || Ok(()))
    }
    /// Checks current local permission before verification, marker and publication.
    /// Callback failure preserves staged data and never retries publication.
    pub fn commit_checked(
        mut self,
        root: &Path,
        mut check: impl FnMut() -> io::Result<()>,
    ) -> io::Result<PathBuf> {
        check()?;
        if !self.progress().chunks_complete {
            return Err(invalid("transfer incomplete"));
        }
        require_dir(root)?;
        if self.dir.parent() != Some(root) {
            return Err(invalid("destination root changed"));
        }
        self.partial.seek(SeekFrom::Start(0))?;
        let mut hasher = blake3::Hasher::new();
        let mut buffer = vec![0; CHUNK_BYTES];
        let mut bounded = (&mut self.partial).take(self.manifest.size + 1);
        loop {
            let n = bounded.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            hasher.update(&buffer[..n]);
        }
        if self.partial.metadata()?.len() != self.manifest.size
            || hasher.finalize().as_bytes() != &self.manifest.whole
        {
            return Err(invalid("whole file verification failed"));
        }
        self.partial.sync_all()?;
        let destination = root.join(&self.manifest.name);
        // Persist a terminal marker before publication. Resume must never mutate
        // a hard-linked final file, including after a crash/move by the user.
        check()?;
        let marker = exclusive(&self.dir.join("committing"))?;
        marker.sync_all()?;
        sync_dir(&self.dir)?;
        check()?;
        fs::hard_link(self.dir.join("partial"), &destination)?;
        sync_dir(root)?;
        fs::remove_file(self.dir.join("partial"))?;
        sync_dir(&self.dir)?;
        Ok(destination)
    }
}
fn require_dir(path: &Path) -> io::Result<()> {
    if !fs::symlink_metadata(path)?.file_type().is_dir() {
        return Err(invalid("destination is not a regular directory"));
    }
    Ok(())
}
fn require_regular(path: &Path) -> io::Result<()> {
    if !fs::symlink_metadata(path)?.file_type().is_file() {
        return Err(invalid("resume file is not regular"));
    }
    Ok(())
}
fn exclusive(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}
fn sync_dir(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    File::open(path)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

impl Drop for Receiver {
    fn drop(&mut self) {
        let _ = self.partial.unlock();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!(
                "peerglide-transfer-{}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&p).unwrap();
            Self(p)
        }
        fn source(&self, len: usize) -> (PathBuf, Vec<u8>, Manifest) {
            let path = self.0.join("多语言-😀.bin");
            let bytes = (0..len)
                .map(|i| ((i * 31 + i / 7) % 251) as u8)
                .collect::<Vec<_>>();
            fs::write(&path, &bytes).unwrap();
            let manifest = Manifest::from_file(&path).unwrap();
            (path, bytes, manifest)
        }
        fn destination(&self) -> PathBuf {
            let p = self.0.join("received");
            fs::create_dir(&p).unwrap();
            p
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn binding() -> PeerBinding {
        PeerBinding::new([23; 32], [71; 16])
    }

    #[test]
    fn interrupted_out_of_order_transfer_resumes_verified_chunks_and_commits() {
        let fixture = Fixture::new();
        let (path, bytes, manifest) = fixture.source(CHUNK_BYTES * 3 + 19);
        let root = fixture.destination();
        let mut source = File::open(path).unwrap();
        let mut receiver = Receiver::create(&root, binding(), manifest.clone()).unwrap();
        for i in [2, 0, 2] {
            receiver
                .write_chunk(i, &manifest.read_chunk(&mut source, i).unwrap())
                .unwrap();
        }
        assert_eq!(receiver.progress().verified_bytes, 2 * CHUNK_BYTES as u64);
        assert_eq!(receiver.missing().collect::<Vec<_>>(), vec![1, 3]);
        drop(receiver);
        let mut resumed = Receiver::resume(&root, binding(), manifest.clone()).unwrap();
        assert_eq!(resumed.missing().collect::<Vec<_>>(), vec![1, 3]);
        for i in [3, 1] {
            resumed
                .write_chunk(i, &manifest.read_chunk(&mut source, i).unwrap())
                .unwrap();
        }
        assert!(resumed.progress().chunks_complete);
        let result = resumed.commit(&root).unwrap();
        assert_eq!(fs::read(result).unwrap(), bytes);
        assert!(Receiver::resume(&root, binding(), manifest).is_err());
    }
    #[test]
    fn malformed_oversized_future_and_unsafe_name_offers_reject() {
        let fixture = Fixture::new();
        let (_, _, mut m) = fixture.source(1);
        assert!(Manifest::decode(&vec![b' '; MAX_MANIFEST_BYTES + 1]).is_err());
        assert!(Manifest::decode(b"{}").is_err());
        let original = m.encode().unwrap();
        assert_eq!(
            Manifest::decode(&original).unwrap().encode().unwrap(),
            original
        );
        m.version += 1;
        assert!(m.encode().is_err());
        m.version = VERSION;
        m.size = MAX_FILE_BYTES + 1;
        assert!(m.encode().is_err());
        m.size = 1;
        m.chunks.clear();
        assert!(m.encode().is_err());
        for name in [
            "",
            ".",
            "..",
            "../outside",
            "/abs",
            "a\\b",
            "C:evil",
            "NUL.txt",
            "CON .txt",
            "COM1",
            "LPT³.txt",
            "CONIN$",
            "a.",
            "a ",
            " x",
            "a\n",
            "a\u{202e}txt",
        ] {
            assert!(!valid_name(name), "{name:?}");
        }
        assert!(valid_name("屏跃 emoji😀.txt"));
    }
    #[test]
    fn invalid_chunks_never_advance_and_cannot_publish_partial() {
        let f = Fixture::new();
        let (_, bytes, m) = f.source(19);
        let root = f.destination();
        let mut r = Receiver::create(&root, binding(), m.clone()).unwrap();
        assert!(r.write_chunk(usize::MAX, &bytes).is_err());
        assert!(r.write_chunk(0, &bytes[..18]).is_err());
        assert!(r.write_chunk(0, &[0; 19]).is_err());
        assert_eq!(r.progress().verified_bytes, 0);
        assert!(r.commit(&root).is_err());
        assert!(!root.join(m.name()).exists());
    }
    #[test]
    fn empty_file_commit_and_name_collision_do_not_overwrite() {
        let f = Fixture::new();
        let (_, _, m) = f.source(0);
        let root = f.destination();
        let r = Receiver::create(&root, binding(), m.clone()).unwrap();
        assert!(r.progress().chunks_complete);
        fs::write(root.join(m.name()), b"existing user file").unwrap();
        assert!(r.commit(&root).is_err());
        assert_eq!(
            fs::read(root.join(m.name())).unwrap(),
            b"existing user file"
        );
        assert!(Receiver::resume(&root, binding(), m.clone()).is_err());
        let other = f.0.join("empty-destination");
        fs::create_dir(&other).unwrap();
        let r = Receiver::create(&other, binding(), m.clone()).unwrap();
        assert_eq!(fs::metadata(r.commit(&other).unwrap()).unwrap().len(), 0);
    }
    #[test]
    fn corrupted_partial_is_requested_again_and_bad_whole_hash_never_publishes() {
        let f = Fixture::new();
        let (_, bytes, mut m) = f.source(CHUNK_BYTES + 3);
        let root = f.destination();
        m.whole = [0; 32];
        let mut r = Receiver::create(&root, binding(), m.clone()).unwrap();
        r.write_chunk(0, &bytes[..CHUNK_BYTES]).unwrap();
        let partial = r.dir.join("partial");
        drop(r);
        OpenOptions::new()
            .write(true)
            .open(partial)
            .unwrap()
            .write_all(b"corrupted")
            .unwrap();
        let mut r = Receiver::resume(&root, binding(), m.clone()).unwrap();
        assert_eq!(r.missing().collect::<Vec<_>>(), vec![0, 1]);
        r.write_chunk(0, &bytes[..CHUNK_BYTES]).unwrap();
        r.write_chunk(1, &bytes[CHUNK_BYTES..]).unwrap();
        assert!(r.commit(&root).is_err());
        assert!(!root.join(m.name()).exists());
    }
    #[test]
    fn changing_identity_manifest_or_receipt_cannot_resume_and_duplicate_create_preserves_data() {
        let f = Fixture::new();
        let (_, bytes, m) = f.source(3);
        let root = f.destination();
        let mut r = Receiver::create(&root, binding(), m.clone()).unwrap();
        r.write_chunk(0, &bytes).unwrap();
        assert!(Receiver::create(&root, binding(), m.clone()).is_err());
        let receipt = r.dir.join("receipt.json");
        drop(r);
        assert!(Receiver::resume(&root, PeerBinding::new([24; 32], [71; 16]), m.clone()).is_err());
        assert!(Receiver::resume(&root, PeerBinding::new([23; 32], [72; 16]), m.clone()).is_err());
        let mut changed = m.clone();
        changed.name = "other.bin".into();
        assert!(Receiver::resume(&root, binding(), changed).is_err());
        fs::write(receipt, b"{} ").unwrap();
        assert!(Receiver::resume(&root, binding(), m).is_err());
    }
    #[test]
    fn concurrent_receiver_and_terminal_commit_marker_reject_resume() {
        let f = Fixture::new();
        let (_, _, m) = f.source(3);
        let root = f.destination();
        let r = Receiver::create(&root, binding(), m.clone()).unwrap();
        assert!(Receiver::resume(&root, binding(), m.clone()).is_err());
        let marker = r.dir.join("committing");
        drop(r);
        fs::write(marker, b"").unwrap();
        assert!(Receiver::resume(&root, binding(), m).is_err());
    }
    #[test]
    fn sender_rechecks_content_and_exact_last_chunk_bounds() {
        let f = Fixture::new();
        let (path, _, m) = f.source(CHUNK_BYTES + 7);
        let mut file = File::open(&path).unwrap();
        assert_eq!(m.read_chunk(&mut file, 1).unwrap().len(), 7);
        assert!(m.read_chunk(&mut file, 2).is_err());
        fs::write(path, b"changed").unwrap();
        assert!(m.read_chunk(&mut file, 0).is_err());
    }
    #[test]
    fn maximum_size_manifest_is_encodable_with_bounded_metadata() {
        let m = Manifest {
            version: VERSION,
            name: "large.bin".into(),
            size: MAX_FILE_BYTES,
            whole: [255; 32],
            chunks: vec![[255; 32]; (MAX_FILE_BYTES / CHUNK_BYTES as u64) as usize],
        };
        let bytes = m.encode().unwrap();
        assert!(bytes.len() <= MAX_MANIFEST_BYTES);
        assert_eq!(Manifest::decode(&bytes).unwrap().chunks(), m.chunks());
    }
    #[cfg(unix)]
    #[test]
    fn symlink_source_root_and_partial_are_rejected() {
        use std::os::unix::fs::symlink;
        let f = Fixture::new();
        let (path, _, m) = f.source(3);
        let alias = f.0.join("alias.bin");
        symlink(&path, &alias).unwrap();
        assert!(Manifest::from_file(&alias).is_err());
        let root = f.destination();
        let root_alias = f.0.join("root-alias");
        symlink(&root, &root_alias).unwrap();
        assert!(Receiver::create(&root_alias, binding(), m.clone()).is_err());
        let r = Receiver::create(&root, binding(), m.clone()).unwrap();
        let partial = r.dir.join("partial");
        drop(r);
        fs::remove_file(&partial).unwrap();
        symlink(path, partial).unwrap();
        assert!(Receiver::resume(&root, binding(), m).is_err());
    }
}

#[cfg(test)]
mod commit_regressions {
    use super::*;
    #[test]
    fn commit_rehashes_disk_even_when_all_chunks_were_previously_verified() {
        let root = std::env::temp_dir().join(format!(
            "peerglide-commit-corrupt-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        let path = root.join("source.bin");
        fs::write(&path, b"valid original").unwrap();
        let m = Manifest::from_file(&path).unwrap();
        let dest = root.join("dest");
        fs::create_dir(&dest).unwrap();
        let binding = PeerBinding::new([1; 32], [2; 16]);
        let mut r = Receiver::create(&dest, binding, m).unwrap();
        r.write_chunk(0, b"valid original").unwrap();
        r.partial.seek(SeekFrom::Start(0)).unwrap();
        r.partial.write_all(b"wrong").unwrap();
        assert!(r.progress().chunks_complete);
        assert!(r.commit(&dest).is_err());
        assert!(!dest.join("source.bin").exists());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn crash_after_hard_link_never_resumes_a_moved_final_inode() {
        let root = std::env::temp_dir().join(format!(
            "peerglide-commit-crash-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        let path = root.join("source.bin");
        fs::write(&path, b"data").unwrap();
        let m = Manifest::from_file(&path).unwrap();
        let dest = root.join("dest");
        fs::create_dir(&dest).unwrap();
        let binding = PeerBinding::new([1; 32], [2; 16]);
        let mut r = Receiver::create(&dest, binding.clone(), m.clone()).unwrap();
        r.write_chunk(0, b"data").unwrap();
        // Emulate a crash in the publication window: marker and link durable,
        // partial not yet removed. User then moves the published file elsewhere.
        fs::write(r.dir.join("committing"), b"").unwrap();
        fs::hard_link(r.dir.join("partial"), dest.join(m.name())).unwrap();
        drop(r);
        fs::rename(dest.join(m.name()), root.join("moved.bin")).unwrap();
        assert!(Receiver::resume(&dest, binding, m).is_err());
        assert_eq!(fs::read(root.join("moved.bin")).unwrap(), b"data");
        fs::remove_dir_all(root).unwrap();
    }
}

pub mod clipboard;
pub mod data_policy;
pub mod manager;
pub mod codec;
