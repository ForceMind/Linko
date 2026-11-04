//! Durable, conservative reservations for one explicitly selected receive root.
//! No cleanup or budget refund: retained/published/moved files keep reservations.
use crate::{
    exclusive, invalid, require_dir, require_regular, sync_dir, Digest, Manifest, PeerBinding,
    Receipt,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

const PREFIX: &str = ".peerglide-admission-";
const MAX_RECORD_BYTES: u64 = 2048;
const MAX_ROOT_ENTRIES: usize = 4096;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Reservation {
    version: u16,
    binding: PeerBinding,
    manifest: Digest,
    receipt: Digest,
    bytes: u64,
}
impl Reservation {
    fn new(binding: PeerBinding, manifest: &Manifest) -> io::Result<Self> {
        manifest.validate()?;
        let receipt = serde_json::to_vec(&Receipt {
            binding: binding.clone(),
            manifest: manifest.clone(),
        })
        .map_err(|_| invalid("reservation serialization"))?;
        Ok(Self {
            version: 1,
            binding,
            manifest: *blake3::hash(&manifest.encode()?).as_bytes(),
            receipt: *blake3::hash(&receipt).as_bytes(),
            bytes: manifest.size(),
        })
    }
    fn key(&self) -> io::Result<Digest> {
        let bytes =
            serde_json::to_vec(&self.binding).map_err(|_| invalid("binding serialization"))?;
        Ok(*blake3::hash(&bytes).as_bytes())
    }
    fn path(&self, root: &Path) -> io::Result<PathBuf> {
        Ok(root.join(format!(
            "{PREFIX}{}",
            blake3::Hash::from_bytes(self.key()?).to_hex()
        )))
    }
}

/// Aggregate metadata only; does not expose peer IDs, names, paths or digests.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Inventory {
    pub reserved_bytes: u64,
    pub reservations: usize,
    pub retained_partials: usize,
    /// A committing marker means publication may have happened, never proof of success.
    pub terminal_records: usize,
}

pub(crate) struct Ledger {
    root: PathBuf,
    lock: File,
    reservations: BTreeMap<Digest, Reservation>,
    inventory: Inventory,
    healthy: bool,
}
impl Drop for Ledger {
    fn drop(&mut self) {
        let _ = self.lock.unlock();
    }
}
fn bounded_read(path: &Path, limit: u64) -> io::Result<Vec<u8>> {
    require_regular(path)?;
    let file = File::open(path)?;
    if file.metadata()?.len() > limit {
        return Err(invalid("storage record exceeds limit"));
    }
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(invalid("storage record exceeds limit"));
    }
    Ok(bytes)
}
impl Ledger {
    pub(crate) fn open(root: &Path) -> io::Result<Self> {
        require_dir(root)?;
        let path = root.join(".peerglide-manager.lock");
        match fs::symlink_metadata(&path) {
            Ok(metadata) if !metadata.file_type().is_file() => {
                return Err(invalid("invalid receive root lock"))
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let lock = options.open(path)?;
        lock.try_lock()
            .map_err(|_| invalid("receive root already managed"))?;
        let mut ledger = Self {
            root: root.to_owned(),
            lock,
            reservations: BTreeMap::new(),
            inventory: Inventory::default(),
            healthy: true,
        };
        let mut legacy = Vec::new();
        for (count, entry) in fs::read_dir(root)?.enumerate() {
            if count >= MAX_ROOT_ENTRIES {
                return Err(invalid("receive root entry limit"));
            }
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if name.starts_with(PREFIX) {
                let bytes = bounded_read(&entry.path(), MAX_RECORD_BYTES)?;
                let record: Reservation = serde_json::from_slice(&bytes)
                    .map_err(|_| invalid("invalid admission record"))?;
                if record.version != 1
                    || record.bytes > crate::MAX_FILE_BYTES
                    || record.path(root)? != entry.path()
                {
                    return Err(invalid("unsupported or mismatched admission record"));
                }
                ledger.insert(record)?;
            } else if name.starts_with(".peerglide-") && name != ".peerglide-manager.lock" {
                // Legacy receiver directories are authoritative retained usage.
                require_dir(&entry.path())?;
                let bytes = bounded_read(
                    &entry.path().join("receipt.json"),
                    (crate::MAX_MANIFEST_BYTES + 1024) as u64,
                )?;
                let receipt: Receipt = serde_json::from_slice(&bytes)
                    .map_err(|_| invalid("invalid retained receipt"))?;
                let record = Reservation::new(receipt.binding, &receipt.manifest)?;
                if name
                    != format!(
                        ".peerglide-{}",
                        blake3::Hash::from_bytes(record.receipt).to_hex()
                    )
                {
                    return Err(invalid("retained directory binding mismatch"));
                }
                let mut partial_present = false;
                let mut terminal_present = false;
                for child in fs::read_dir(entry.path())? {
                    let child = child?;
                    if !["receipt.json", "partial", "committing"]
                        .iter()
                        .any(|name| child.file_name() == *name)
                    {
                        return Err(invalid("unknown retained storage entry"));
                    }
                    require_regular(&child.path())?;
                    let len = child.metadata()?.len();
                    if child.file_name() == "partial" {
                        if len != record.bytes { return Err(invalid("partial size differs from reservation")); }
                        partial_present = true;
                        ledger.inventory.retained_partials += 1;
                    } else if child.file_name() == "committing" {
                        if len != 0 {
                            return Err(invalid("invalid terminal marker"));
                        }
                        terminal_present = true;
                        ledger.inventory.terminal_records += 1;
                    }
                }
                if !partial_present && !terminal_present { return Err(invalid("incomplete retained initialization")); }
                if legacy.len() >= super::manager::MAX_ATTEMPTS {
                    return Err(invalid("retained transfer limit"));
                }
                legacy.push(record);
            }
        }
        // Validate the full inventory before persisting any legacy migration.
        let mut combined = ledger.reservations.clone();
        for record in &legacy {
            if let Some(previous) = combined.insert(record.key()?, record.clone()) {
                if previous != *record {
                    return Err(invalid("conflicting retained reservation"));
                }
            }
        }
        if combined.len() > super::manager::MAX_ATTEMPTS {
            return Err(invalid("reservation limit"));
        }
        for record in legacy {
            if !ledger.reservations.contains_key(&record.key()?) {
                ledger.persist(record)?;
            }
        }
        Ok(ledger)
    }

    fn insert(&mut self, record: Reservation) -> io::Result<()> {
        if self.reservations.len() >= super::manager::MAX_ATTEMPTS {
            return Err(invalid("reservation limit"));
        }
        let size = self
            .inventory
            .reserved_bytes
            .checked_add(record.bytes)
            .ok_or_else(|| invalid("reservation overflow"))?;
        if self.reservations.insert(record.key()?, record).is_some() {
            return Err(invalid("duplicate reservation"));
        }
        self.inventory.reserved_bytes = size;
        self.inventory.reservations = self.reservations.len();
        Ok(())
    }
    fn persist(&mut self, record: Reservation) -> io::Result<()> {
        if !self.has_capacity(1)? { return Err(invalid("receive root entry limit")); }
        // Poison before I/O: any failed/uncertain persistence blocks this owner.
        self.healthy = false;
        let bytes =
            serde_json::to_vec(&record).map_err(|_| invalid("reservation serialization"))?;
        if bytes.len() as u64 > MAX_RECORD_BYTES {
            return Err(invalid("reservation exceeds limit"));
        }
        let mut file = exclusive(&record.path(&self.root)?)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        sync_dir(&self.root)?;
        self.insert(record)?;
        self.healthy = true;
        Ok(())
    }
    pub(crate) fn reserve(
        &mut self,
        binding: PeerBinding,
        manifest: &Manifest,
        resume: bool,
        budget: u64,
    ) -> io::Result<bool> {
        if !self.healthy {
            return Err(invalid("reservation persistence failed; inspect storage"));
        }
        if self.inventory.reserved_bytes > budget {
            return Ok(false);
        }
        let record = Reservation::new(binding, manifest)?;
        if let Some(previous) = self.reservations.get(&record.key()?) {
            return if resume && previous == &record {
                Ok(true)
            } else {
                Err(invalid("transfer ID already reserved"))
            };
        }
        if resume { return Err(invalid("no retained reservation to resume")); }
        if self.reservations.len() >= super::manager::MAX_ATTEMPTS || record.bytes > budget - self.inventory.reserved_bytes || !self.has_capacity(3)? { return Ok(false); }
        self.persist(record)?;
        Ok(true)
    }
    pub(crate) fn has_capacity(&self, additional: usize) -> io::Result<bool> {
        let mut count = additional;
        for entry in fs::read_dir(&self.root)? {
            entry?;
            count += 1;
            if count > MAX_ROOT_ENTRIES { return Ok(false); }
        }
        Ok(true)
    }
    pub(crate) fn inventory(&self) -> Inventory {
        self.inventory
    }
}
