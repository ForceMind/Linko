//! Offline file admission with durable root-wide cumulative reservations.
//! No networking, automatic refunds, cleanup or filesystem free-space guarantee.
use crate::{
    data_policy::{Capability, DataSession, Denied, Permissions, Permit},
    admission::Ledger, Manifest, PeerBinding, Progress, Receiver,
};
use std::{
    collections::BTreeMap,
    io,
    path::{Path, PathBuf},
};

pub const MAX_ACTIVE: usize = 4;
pub const MAX_ATTEMPTS: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Receiving,
    Suspended,
    Cancelled,
    Failed,
    Committed,
    UnknownOutcome,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Summary {
    pub state: State,
    pub verified_bytes: u64,
    pub total_bytes: u64,
}

#[derive(Debug)]
pub enum Error {
    Permission(Denied),
    Quota,
    Duplicate,
    UnknownId,
    Inactive,
    Incomplete,
    Io(io::Error),
}
impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

struct Entry {
    receiver: Option<Receiver>,
    permit: Permit,
    summary: Summary,
}

/// Every admission is an explicit local per-offer accept (or explicit resume).
/// Opening inventories retained storage and holds an OS lock for this receive
/// root. Reservations survive manager/session/process lifetimes. No cleanup.
pub struct Manager {
    root: PathBuf,
    session: DataSession,
    budget: u64,
    entries: BTreeMap<[u8; 16], Entry>,
    // Drop last: receivers must close before another manager can acquire the root.
    ledger: Ledger,
}
impl Manager {
    pub fn new(root: &Path, session: DataSession, budget: u64) -> io::Result<Self> {
        let ledger = Ledger::open(root)?;
        Ok(Self {
            root: root.to_owned(),
            session,
            budget,
            entries: BTreeMap::new(),
            ledger,
        })
    }

    fn suspend(&mut self) {
        for entry in self.entries.values_mut() {
            if entry.receiver.take().is_some() {
                entry.summary.state = State::Suspended;
            }
        }
    }

    pub fn consent(
        &mut self,
        permissions: Permissions,
        lifetime_ms: u64,
        now: u64,
    ) -> Result<(), Denied> {
        self.suspend();
        self.session.consent(permissions, lifetime_ms, now)
    }

    pub fn revoke(&mut self) {
        self.session.revoke();
        self.suspend();
    }

    pub fn close(&mut self) {
        self.session.close();
        self.suspend();
    }

    /// binding must be independently authenticated, manifest must be shown to the
    /// user, and the local user must select accept/resume before calling.
    pub fn accept(
        &mut self,
        binding: PeerBinding,
        manifest: Manifest,
        resume: bool,
        now: u64,
    ) -> Result<(), Error> {
        manifest.validate()?;
        let permit = self
            .session
            .authorize_peer(binding.fingerprint, Capability::FileReceive, now)
            .map_err(Error::Permission)?;
        let id = binding.transfer_id;
        if self.entries.contains_key(&id) {
            return Err(Error::Duplicate);
        }
        let active = self
            .entries
            .values()
            .filter(|entry| entry.receiver.is_some())
            .count();
        if active >= MAX_ACTIVE || self.entries.len() >= MAX_ATTEMPTS {
            return Err(Error::Quota);
        }
        // Sync the durable reservation before creating or extending staged data.
        if !self.ledger.reserve(binding.clone(), &manifest, resume, self.budget)? {
            return Err(Error::Quota);
        }
        let total_bytes = manifest.size();
        let result = if resume {
            Receiver::resume(&self.root, binding, manifest)
        } else {
            Receiver::create(&self.root, binding, manifest)
        };
        let summary = match &result {
            Ok(receiver) => Summary {
                state: State::Receiving,
                verified_bytes: receiver.progress().verified_bytes,
                total_bytes,
            },
            Err(_) => Summary {
                state: State::Failed,
                verified_bytes: 0,
                total_bytes,
            },
        };
        let failure = result
            .as_ref()
            .err()
            .map(|e| io::Error::new(e.kind(), "file admission failed"));
        self.entries.insert(
            id,
            Entry {
                receiver: result.ok(),
                permit,
                summary,
            },
        );
        if let Some(error) = failure {
            return Err(Error::Io(error));
        }
        Ok(())
    }

    pub fn write_chunk(
        &mut self,
        id: [u8; 16],
        index: usize,
        bytes: &[u8],
        now: u64,
    ) -> Result<Progress, Error> {
        let entry = self.entries.get_mut(&id).ok_or(Error::UnknownId)?;
        if let Err(error) = self
            .session
            .validate(&entry.permit, Capability::FileReceive, now)
        {
            entry.receiver.take();
            if entry.summary.state == State::Receiving {
                entry.summary.state = State::Suspended;
            }
            return Err(Error::Permission(error));
        }
        let receiver = entry.receiver.as_mut().ok_or(Error::Inactive)?;
        let progress = receiver.write_chunk(index, bytes)?;
        entry.summary.verified_bytes = progress.verified_bytes;
        Ok(progress)
    }

    /// A fresh clock is queried at every low-level publication boundary, after
    /// potentially slow whole-file hashing/sync. Never resets the grant deadline.
    pub fn commit(
        &mut self,
        id: [u8; 16],
        mut clock: impl FnMut() -> u64,
    ) -> Result<PathBuf, Error> {
        let entry = self.entries.get_mut(&id).ok_or(Error::UnknownId)?;
        if let Err(error) = self
            .session
            .validate(&entry.permit, Capability::FileReceive, clock())
        {
            entry.receiver.take();
            if entry.summary.state == State::Receiving {
                entry.summary.state = State::Suspended;
            }
            return Err(Error::Permission(error));
        }
        let receiver = entry.receiver.as_ref().ok_or(Error::Inactive)?;
        if !receiver.progress().chunks_complete {
            return Err(Error::Incomplete);
        }
        let receiver = entry.receiver.take().ok_or(Error::Inactive)?;
        entry.summary.state = State::UnknownOutcome;
        let result = receiver.commit_checked(&self.root, || {
            self.session
                .validate(&entry.permit, Capability::FileReceive, clock())
                .map_err(|_| io::Error::new(io::ErrorKind::PermissionDenied, "file grant expired"))
        });
        match result {
            Ok(path) => {
                entry.summary.state = State::Committed;
                Ok(path)
            }
            Err(error) => Err(Error::Io(error)),
        }
    }

    /// Releases handles only. Does not delete staged or published user data.
    pub fn cancel(&mut self, id: [u8; 16]) -> Result<(), Error> {
        let entry = self.entries.get_mut(&id).ok_or(Error::UnknownId)?;
        if entry.summary.state != State::Receiving {
            return Err(Error::Inactive);
        }
        entry.receiver.take();
        entry.summary.state = State::Cancelled;
        Ok(())
    }

    pub fn summary(&self, id: [u8; 16]) -> Option<Summary> {
        self.entries.get(&id).map(|entry| entry.summary)
    }

    /// Startup inventory plus durable admissions; retained/terminal counts are
    /// the startup snapshot, not a live progress or publication-success report.
    pub fn inventory(&self) -> crate::Inventory {
        self.ledger.inventory()
    }

    pub fn admitted_bytes(&self) -> u64 {
        self.ledger.inventory().reserved_bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        sync::atomic::{AtomicU64, Ordering},
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "peerglide-manager-{}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&root).unwrap();
            Self(root)
        }
        fn setup(&self, budget: u64) -> (Manager, Manifest) {
            let source = self.0.join("source.txt");
            fs::write(&source, b"file data").unwrap();
            let manifest = Manifest::from_file(&source).unwrap();
            let root = self.0.join("received");
            fs::create_dir(&root).unwrap();
            let manager =
                Manager::new(&root, DataSession::new([1; 32], [2; 32], 0), budget).unwrap();
            (manager, manifest)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn grant(manager: &mut Manager) {
        manager
            .consent(
                Permissions {
                    file_receive: true,
                    ..Permissions::default()
                },
                1000,
                0,
            )
            .unwrap();
    }
    fn binding(id: u8) -> PeerBinding {
        PeerBinding::new([1; 32], [id; 16])
    }

    #[test]
    fn grant_binding_and_budget_precede_disk_admission() {
        let fixture = Fixture::new();
        let (mut manager, manifest) = fixture.setup(9);
        assert!(manager
            .accept(binding(1), manifest.clone(), false, 0)
            .is_err());
        assert_eq!(fs::read_dir(&manager.root).unwrap().count(), 1);
        grant(&mut manager);
        assert!(manager
            .accept(
                PeerBinding::new([9; 32], [1; 16]),
                manifest.clone(),
                false,
                0
            )
            .is_err());
        assert_eq!(fs::read_dir(&manager.root).unwrap().count(), 1);
        manager
            .accept(binding(1), manifest.clone(), false, 0)
            .unwrap();
        assert!(matches!(
            manager.accept(binding(1), manifest.clone(), false, 0),
            Err(Error::Duplicate)
        ));
        manager.cancel([1; 16]).unwrap();
        assert_eq!(manager.admitted_bytes(), 9);
        assert!(matches!(
            manager.accept(binding(2), manifest, false, 0),
            Err(Error::Quota)
        ));
    }

    #[test]
    fn verified_file_commit_and_revoke_prevent_further_effects() {
        let fixture = Fixture::new();
        let (mut manager, manifest) = fixture.setup(100);
        grant(&mut manager);
        manager
            .accept(binding(1), manifest.clone(), false, 0)
            .unwrap();
        assert!(matches!(
            manager.commit([1; 16], || 1),
            Err(Error::Incomplete)
        ));
        manager.write_chunk([1; 16], 0, b"file data", 1).unwrap();
        let path = manager.commit([1; 16], || 2).unwrap();
        assert_eq!(fs::read(path).unwrap(), b"file data");
        assert_eq!(manager.summary([1; 16]).unwrap().state, State::Committed);
        manager.accept(binding(2), manifest, false, 2).unwrap();
        manager.revoke();
        assert_eq!(manager.summary([2; 16]).unwrap().state, State::Suspended);
        assert!(manager.write_chunk([2; 16], 0, b"file data", 3).is_err());
        assert!(manager.commit([2; 16], || 3).is_err());
    }

    #[test]
    fn permission_is_rechecked_after_hashing_before_publication() {
        let fixture = Fixture::new();
        let (mut manager, manifest) = fixture.setup(100);
        grant(&mut manager);
        manager.accept(binding(1), manifest, false, 0).unwrap();
        manager.write_chunk([1; 16], 0, b"file data", 1).unwrap();
        let mut checks = 0;
        assert!(manager
            .commit([1; 16], || {
                checks += 1;
                if checks < 3 {
                    2
                } else {
                    1000
                }
            })
            .is_err());
        assert!(!manager.root.join("source.txt").exists());
        assert_eq!(
            manager.summary([1; 16]).unwrap().state,
            State::UnknownOutcome
        );
        assert!(manager.commit([1; 16], || 1001).is_err());
    }

    #[test]
    fn active_limit_cancellation_and_explicit_disk_resume() {
        let fixture = Fixture::new();
        let (mut manager, manifest) = fixture.setup(100);
        grant(&mut manager);
        for id in 1..=4 {
            manager
                .accept(binding(id), manifest.clone(), false, 0)
                .unwrap();
        }
        assert!(matches!(
            manager.accept(binding(5), manifest.clone(), false, 0),
            Err(Error::Quota)
        ));
        manager.write_chunk([1; 16], 0, b"file data", 1).unwrap();
        manager.cancel([1; 16]).unwrap();
        manager
            .accept(binding(5), manifest.clone(), false, 1)
            .unwrap();
        let root = manager.root.clone();
        manager.close();
        drop(manager);
        let mut resumed = Manager::new(&root, DataSession::new([1; 32], [3; 32], 0), 100).unwrap();
        assert_eq!(resumed.admitted_bytes(), 45);
        grant(&mut resumed);
        resumed.accept(binding(1), manifest, true, 0).unwrap();
        assert_eq!(resumed.summary([1; 16]).unwrap().verified_bytes, 9);
        assert!(resumed.commit([1; 16], || 1).is_ok());
    }

    #[test]
    fn codec_offer_and_chunk_flow_to_verified_disk_commit() {
        use crate::codec::Frame;
        use std::io::Cursor;
        let fixture = Fixture::new();
        let (mut manager, manifest) = fixture.setup(100);
        grant(&mut manager);
        let mut wire = Vec::new();
        Frame::Offer {
            id: [1; 16],
            manifest,
        }
        .write(&mut wire)
        .unwrap();
        Frame::Chunk {
            id: [1; 16],
            index: 0,
            bytes: b"file data".to_vec(),
        }
        .write(&mut wire)
        .unwrap();
        let mut input = Cursor::new(wire);
        match Frame::read(&mut input).unwrap() {
            Frame::Offer { id, manifest } => manager
                .accept(PeerBinding::new([1; 32], id), manifest, false, 0)
                .unwrap(),
            _ => panic!("wrong offer"),
        }
        match Frame::read(&mut input).unwrap() {
            Frame::Chunk { id, index, bytes } => {
                manager.write_chunk(id, index as usize, &bytes, 1).unwrap();
            }
            _ => panic!("wrong chunk"),
        }
        let path = manager.commit([1; 16], || 2).unwrap();
        assert_eq!(fs::read(path).unwrap(), b"file data");
    }

    #[test]
    fn restart_preserves_cancelled_and_published_reservations() {
        let fixture = Fixture::new();
        let (mut manager, manifest) = fixture.setup(18);
        grant(&mut manager);
        manager.accept(binding(1), manifest.clone(), false, 0).unwrap();
        manager.cancel([1; 16]).unwrap();
        manager.accept(binding(2), manifest.clone(), false, 0).unwrap();
        manager.write_chunk([2; 16], 0, b"file data", 1).unwrap();
        let published = manager.commit([2; 16], || 2).unwrap();
        // Moving the user's published file must not refund admission credit.
        fs::rename(published, fixture.0.join("moved.txt")).unwrap();
        let root = manager.root.clone();
        drop(manager);
        let mut manager = Manager::new(&root, DataSession::new([1; 32], [3; 32], 0), 18).unwrap();
        assert_eq!(manager.admitted_bytes(), 18);
        assert_eq!(manager.inventory().reservations, 2);
        assert_eq!(manager.inventory().retained_partials, 1);
        assert_eq!(manager.inventory().terminal_records, 1);
        // A new session still starts without consent.
        assert!(matches!(manager.accept(binding(1), manifest.clone(), true, 0), Err(Error::Permission(_))));
        grant(&mut manager);
        assert!(matches!(manager.accept(binding(3), manifest.clone(), false, 0), Err(Error::Quota)));
        manager.accept(binding(1), manifest, true, 0).unwrap();
        assert_eq!(manager.admitted_bytes(), 18);
        assert_eq!(fs::read(fixture.0.join("moved.txt")).unwrap(), b"file data");
    }

    #[test]
    fn legacy_inventory_is_imported_without_refunding_or_double_charging() {
        let fixture = Fixture::new();
        let (manager, manifest) = fixture.setup(100);
        let root = manager.root.clone();
        drop(manager);
        let mut legacy = Receiver::create(&root, binding(1), manifest.clone()).unwrap();
        legacy.write_chunk(0, b"file data").unwrap();
        drop(legacy);
        let mut manager = Manager::new(&root, DataSession::new([1; 32], [3; 32], 0), 8).unwrap();
        assert_eq!(manager.admitted_bytes(), 9);
        grant(&mut manager);
        assert!(matches!(manager.accept(binding(1), manifest.clone(), true, 0), Err(Error::Quota)));
        drop(manager);
        let mut manager = Manager::new(&root, DataSession::new([1; 32], [4; 32], 0), 9).unwrap();
        grant(&mut manager);
        manager.accept(binding(1), manifest, true, 0).unwrap();
        assert_eq!(manager.summary([1; 16]).unwrap().verified_bytes, 9);
        assert_eq!(manager.admitted_bytes(), 9);
        manager.commit([1; 16], || 1).unwrap();
    }

    #[test]
    fn exclusive_manager_lock_survives_session_close_and_releases_on_drop() {
        let fixture = Fixture::new();
        let (mut manager, _) = fixture.setup(100);
        let root = manager.root.clone();
        assert!(Manager::new(&root, DataSession::new([1; 32], [3; 32], 0), 100).is_err());
        manager.close();
        assert!(Manager::new(&root, DataSession::new([1; 32], [3; 32], 0), 100).is_err());
        drop(manager);
        assert!(Manager::new(&root, DataSession::new([1; 32], [3; 32], 0), 100).is_ok());
    }

    #[test]
    fn admission_io_failure_keeps_durable_reservation() {
        let fixture = Fixture::new();
        let (mut manager, manifest) = fixture.setup(9);
        let root = manager.root.clone();
        let receipt = serde_json::to_vec(&crate::Receipt { binding: binding(1), manifest: manifest.clone() }).unwrap();
        let staging = root.join(format!(".peerglide-{}", blake3::hash(&receipt).to_hex()));
        fs::create_dir(&staging).unwrap();
        grant(&mut manager);
        assert!(manager.accept(binding(1), manifest.clone(), false, 0).is_err());
        assert_eq!(manager.admitted_bytes(), 9);
        drop(manager);
        // A partial initialization blocks reopening; the original is preserved.
        assert!(Manager::new(&root, DataSession::new([1; 32], [3; 32], 0), 9).is_err());
        assert!(staging.is_dir());
        // Explicit fixture recovery does not delete the durable reservation.
        fs::remove_dir(&staging).unwrap();
        let mut manager = Manager::new(&root, DataSession::new([1; 32], [3; 32], 0), 9).unwrap();
        grant(&mut manager);
        assert_eq!(manager.admitted_bytes(), 9);
        assert!(matches!(manager.accept(binding(2), manifest, false, 0), Err(Error::Quota)));
    }

    #[test]
    fn damaged_and_future_storage_records_are_preserved_and_rejected() {
        let fixture = Fixture::new();
        let (mut manager, manifest) = fixture.setup(100);
        let root = manager.root.clone();
        grant(&mut manager);
        manager.accept(binding(1), manifest, false, 0).unwrap();
        drop(manager);
        let record = fs::read_dir(&root).unwrap().map(|entry| entry.unwrap().path()).find(|path| path.file_name().unwrap().to_str().unwrap().starts_with(".peerglide-admission-")).unwrap();
        let original = fs::read(&record).unwrap();
        let mut value: serde_json::Value = serde_json::from_slice(&original).unwrap();
        value["version"] = 2.into();
        let future = serde_json::to_vec(&value).unwrap();
        for bytes in [b"{".to_vec(), future, vec![0; 2049]] {
            fs::write(&record, &bytes).unwrap();
            assert!(Manager::new(&root, DataSession::new([1; 32], [3; 32], 0), 100).is_err());
            assert_eq!(fs::read(&record).unwrap(), bytes);
        }
        fs::write(&record, original).unwrap();
        assert_eq!(Manager::new(&root, DataSession::new([1; 32], [3; 32], 0), 100).unwrap().admitted_bytes(), 9);
    }

    #[test]
    fn restart_rejects_changed_manifest_or_reused_id_but_retains_quota() {
        let fixture = Fixture::new();
        let (mut manager, manifest) = fixture.setup(100);
        let root = manager.root.clone();
        grant(&mut manager);
        manager.accept(binding(1), manifest.clone(), false, 0).unwrap();
        drop(manager);
        fs::write(fixture.0.join("source.txt"), b"different").unwrap();
        let changed = Manifest::from_file(&fixture.0.join("source.txt")).unwrap();
        let mut manager = Manager::new(&root, DataSession::new([1; 32], [3; 32], 0), 100).unwrap();
        grant(&mut manager);
        assert!(manager.accept(binding(1), changed, true, 0).is_err());
        assert!(manager.accept(binding(1), manifest.clone(), false, 0).is_err());
        manager.accept(binding(1), manifest, true, 0).unwrap();
        assert_eq!(manager.admitted_bytes(), 9);
    }
}
