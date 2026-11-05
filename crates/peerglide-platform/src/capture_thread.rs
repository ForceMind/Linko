//! Cooperative native-loop shutdown with a bounded join; never waits in a hook.
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc,
};
use std::time::Duration;

#[cfg(any(windows, target_os = "macos", test))]
use anyhow::Context;
#[cfg(any(windows, target_os = "macos", test))]
use std::{
    fs::{File, Metadata, OpenOptions},
    path::Path,
};

#[cfg(any(windows, target_os = "macos", test))]
static RESERVED: AtomicBool = AtomicBool::new(false);

/// Cooperating processes use this one empty inode in their configured temp root.
/// TMPDIR/TEMP configuration determines the scope: different temp roots and
/// malicious local file replacement are not an authenticated security boundary.
#[cfg(any(windows, target_os = "macos", test))]
pub(super) struct Reservation {
    file: File,
    release_healthy: Arc<AtomicBool>,
    #[cfg(test)]
    fail_unlock: bool,
}
#[cfg(any(windows, target_os = "macos", test))]
struct RollbackReservation(bool);
#[cfg(any(windows, target_os = "macos", test))]
impl Drop for RollbackReservation {
    fn drop(&mut self) {
        if self.0 {
            RESERVED.store(false, Ordering::Release);
        }
    }
}
#[cfg(any(windows, target_os = "macos", test))]
fn validate_lock_metadata(metadata: &Metadata) -> anyhow::Result<()> {
    anyhow::ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink() && metadata.len() == 0,
        "native capture lock must be an empty regular non-symlink file"
    );
    Ok(())
}
#[cfg(any(windows, target_os = "macos", test))]
fn validate_lock_file(file: &File, path: &Path, before: Option<&Metadata>) -> anyhow::Result<()> {
    let opened = file
        .metadata()
        .context("inspect opened native capture lock")?;
    let current = std::fs::symlink_metadata(path).context("inspect native capture lock path")?;
    validate_lock_metadata(&opened)?;
    validate_lock_metadata(&current)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        anyhow::ensure!(
            opened.dev() == current.dev()
                && opened.ino() == current.ino()
                && before.is_none_or(
                    |metadata| metadata.dev() == opened.dev() && metadata.ino() == opened.ino()
                ),
            "native capture lock path changed during opening"
        );
    }
    #[cfg(not(unix))]
    let _ = before;
    Ok(())
}
#[cfg(any(windows, target_os = "macos", test))]
fn open_lock_file(path: &Path) -> anyhow::Result<File> {
    let mut create = OpenOptions::new();
    create.read(true).write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        create.mode(0o600);
    }
    let (file, before) = match create.open(path) {
        Ok(file) => (file, None),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let before =
                std::fs::symlink_metadata(path).context("inspect existing native capture lock")?;
            validate_lock_metadata(&before)?;
            // Never truncate, overwrite, remove or replace an existing object.
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .open(path)
                .context("open existing native capture lock")?;
            (file, Some(before))
        }
        Err(error) => return Err(error).context("exclusively create native capture lock"),
    };
    validate_lock_file(&file, path, before.as_ref())?;
    Ok(file)
}
#[cfg(any(windows, target_os = "macos", test))]
impl Reservation {
    #[cfg(any(windows, target_os = "macos"))]
    pub(super) fn acquire() -> anyhow::Result<Self> {
        Self::acquire_at(&std::env::temp_dir().join("peerglide-native-input-capture.lock"))
    }

    /// All filesystem checks and nonblocking locking occur before hook install.
    /// Tests use an exclusively owned directory without changing process TMPDIR.
    fn acquire_at(path: &Path) -> anyhow::Result<Self> {
        anyhow::ensure!(
            !RESERVED.swap(true, Ordering::AcqRel),
            "previous native capture is still running"
        );
        let mut rollback = RollbackReservation(true);
        let file = open_lock_file(path)?;
        file.try_lock()
            .context("native input capture lock unavailable")?;
        let reservation = Self {
            file,
            release_healthy: Arc::new(AtomicBool::new(true)),
            #[cfg(test)]
            fail_unlock: false,
        };
        rollback.0 = false;
        // From here Reservation owns both guards, including all failure paths.
        validate_lock_file(&reservation.file, path, None)?;
        Ok(reservation)
    }
}
#[cfg(any(windows, target_os = "macos", test))]
impl Drop for Reservation {
    fn drop(&mut self) {
        // Explicit unlock is necessary even if another reference/duplicated fd
        // still exists. Only a successfully acquired Reservation reaches here.
        #[cfg(not(test))]
        let inject_failure = false;
        #[cfg(test)]
        let inject_failure = self.fail_unlock;
        if !inject_failure && self.file.unlock().is_ok() {
            RESERVED.store(false, Ordering::Release);
        } else {
            self.release_healthy.store(false, Ordering::Release);
            // Retain the process guard on an uncertain OS-lock release. Closing
            // this File is still attempted normally; never unlock another file.
            eprintln!(
                "Native capture lock release failed; new capture remains blocked in this process."
            );
        }
    }
}

pub(super) struct CaptureThread {
    stop: Arc<AtomicBool>,
    done: mpsc::Receiver<bool>,
    completed: Option<bool>,
}
impl CaptureThread {
    #[cfg(test)]
    pub(super) fn spawn(task: impl FnOnce(Arc<AtomicBool>) + Send + 'static) -> Self {
        Self::spawn_owned(task, (), None)
    }
    #[cfg(any(windows, target_os = "macos", test))]
    pub(super) fn spawn_reserved(
        task: impl FnOnce(Arc<AtomicBool>) + Send + 'static,
        owner: Reservation,
    ) -> Self {
        let health = owner.release_healthy.clone();
        Self::spawn_owned(task, owner, Some(health))
    }
    fn spawn_owned(
        task: impl FnOnce(Arc<AtomicBool>) + Send + 'static,
        owner: impl Send + 'static,
        release_healthy: Option<Arc<AtomicBool>>,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let (tx, done) = mpsc::sync_channel(1);
        let thread = std::thread::spawn(move || task(flag));
        // Only the reaper can block on join. Its ownership guard survives all
        // native thread-local destructors, even if the caller times out/drops.
        std::thread::spawn(move || {
            let clean = thread.join().is_ok();
            drop(owner);
            let _ = tx
                .send(clean && release_healthy.is_none_or(|health| health.load(Ordering::Acquire)));
        });
        Self {
            stop,
            done,
            completed: None,
        }
    }
    /// True until a shutdown poll has received the reaper's final result.
    pub(super) fn pending(&self) -> bool {
        self.completed.is_none()
    }
    pub(super) fn shutdown(&mut self, timeout: Duration) -> bool {
        self.stop.store(true, Ordering::Release);
        if let Some(clean) = self.completed {
            return clean;
        }
        match self.done.recv_timeout(timeout) {
            Ok(clean) => {
                self.completed = Some(clean);
                clean
            }
            Err(mpsc::RecvTimeoutError::Timeout) => false,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                self.completed = Some(false);
                false
            }
        }
    }
}
impl Drop for CaptureThread {
    fn drop(&mut self) {
        if !self.shutdown(Duration::from_millis(500)) {
            eprintln!("Native capture thread did not stop cleanly within its shutdown budget.");
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        path::PathBuf,
        process::Command,
        sync::{atomic::AtomicU64, Mutex},
    };

    static RESERVATION_TESTS: Mutex<()> = Mutex::new(());
    static FIXTURE_SEQUENCE: AtomicU64 = AtomicU64::new(0);
    const CHILD_PATH: &str = "PEERGLIDE_TEST_CAPTURE_LOCK_PATH";
    const CHILD_EXPECTATION: &str = "PEERGLIDE_TEST_CAPTURE_LOCK_EXPECTATION";

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            // Exclusive directory creation never cleans/reuses another test's
            // files, even after an earlier process left a fixture behind.
            for _ in 0..100 {
                let sequence = FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
                let path = std::env::temp_dir().join(format!(
                    "peerglide-capture-lock-test-{}-{sequence}",
                    std::process::id(),
                ));
                match std::fs::create_dir(&path) {
                    Ok(()) => return Self(path),
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
                    Err(error) => panic!("create isolated capture-lock fixture: {error}"),
                }
            }
            panic!("could not reserve an isolated capture-lock fixture");
        }
        fn path(&self) -> PathBuf {
            self.0.join("peerglide-native-input-capture.lock")
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            // Only this test's exclusively created directory is removed. The
            // production lock path and arbitrary existing objects are untouched.
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn child_attempt(path: &Path, expectation: &str) {
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "capture_thread::tests::reservation_child_process",
                "--nocapture",
            ])
            .env(CHILD_PATH, path)
            .env(CHILD_EXPECTATION, expectation)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "child lock check failed: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        let marker = format!("PEERGLIDE_CAPTURE_LOCK_CHECK:{expectation}");
        assert!(
            String::from_utf8_lossy(&output.stdout).contains(&marker),
            "child helper did not execute its lock assertion"
        );
    }

    #[test]
    fn reservation_child_process() {
        let Some(path) = std::env::var_os(CHILD_PATH) else {
            return;
        };
        let expectation = std::env::var(CHILD_EXPECTATION).unwrap();
        if expectation == "release_failed" {
            let mut owner = Reservation::acquire_at(Path::new(&path)).unwrap();
            owner.fail_unlock = true;
            let mut thread = CaptureThread::spawn_reserved(|_| {}, owner);
            assert!(!thread.shutdown(Duration::from_secs(2)));
            assert_eq!(thread.completed, Some(false));
            assert!(Reservation::acquire_at(Path::new(&path)).is_err());
            println!("PEERGLIDE_CAPTURE_LOCK_CHECK:release_failed");
            return;
        }
        match (
            expectation.as_str(),
            Reservation::acquire_at(Path::new(&path)),
        ) {
            ("blocked", Err(error)) => assert!(
                matches!(
                    error.downcast_ref::<std::fs::TryLockError>(),
                    Some(std::fs::TryLockError::WouldBlock)
                ),
                "child was rejected for a reason other than lock contention: {error:#}"
            ),
            ("acquired", Ok(owner)) => drop(owner),
            ("blocked", Ok(_)) => panic!("child acquired a lock still held by parent"),
            ("acquired", Err(error)) => panic!("child failed to acquire released lock: {error:#}"),
            _ => panic!("invalid child expectation"),
        }
        assert!(!RESERVED.load(Ordering::Acquire));
        println!("PEERGLIDE_CAPTURE_LOCK_CHECK:{expectation}");
    }

    #[test]
    fn unlock_failure_reaches_shutdown_and_keeps_process_reservation_poisoned() {
        let fixture = Fixture::new();
        child_attempt(&fixture.path(), "release_failed");
    }

    #[test]
    fn reservation_excludes_other_processes_and_unlocks_with_a_cloned_file_alive() {
        let _serial = RESERVATION_TESTS.lock().unwrap();
        let fixture = Fixture::new();
        let path = fixture.path();
        let owner = Reservation::acquire_at(&path).unwrap();
        let cloned = owner.file.try_clone().unwrap();
        child_attempt(&path, "blocked");
        drop(owner);
        // Closing only the original File would retain an open-file-description
        // lock on Unix. The still-open clone must not retain capture ownership.
        child_attempt(&path, "acquired");
        assert!(path.is_file());
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);
        drop(cloned);
        assert!(Reservation::acquire_at(&path).is_ok());
    }

    #[test]
    fn invalid_existing_objects_are_preserved_and_failed_acquisition_rolls_back() {
        let _serial = RESERVATION_TESTS.lock().unwrap();
        let fixture = Fixture::new();
        let nonempty = fixture.0.join("nonempty");
        std::fs::write(&nonempty, b"preserve these bytes").unwrap();
        assert!(Reservation::acquire_at(&nonempty).is_err());
        assert_eq!(std::fs::read(&nonempty).unwrap(), b"preserve these bytes");
        let directory = fixture.0.join("directory");
        std::fs::create_dir(&directory).unwrap();
        std::fs::write(directory.join("sentinel"), b"keep").unwrap();
        assert!(Reservation::acquire_at(&directory).is_err());
        assert_eq!(std::fs::read(directory.join("sentinel")).unwrap(), b"keep");
        // Each failure must release only our process flag, allowing a valid
        // reservation without deleting/replacing either rejected object.
        assert!(Reservation::acquire_at(&fixture.path()).is_ok());
    }

    #[test]
    fn existing_symlink_is_rejected_without_touching_its_empty_target() {
        let _serial = RESERVATION_TESTS.lock().unwrap();
        let fixture = Fixture::new();
        let target = fixture.0.join("empty-target");
        File::create_new(&target).unwrap();
        let link = fixture.path();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, &link).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(&target, &link).unwrap();
        assert!(Reservation::acquire_at(&link).is_err());
        assert!(std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(std::fs::metadata(&target).unwrap().len(), 0);
        assert!(Reservation::acquire_at(&target).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn newly_created_lock_is_private_to_its_user() {
        use std::os::unix::fs::PermissionsExt;
        let _serial = RESERVATION_TESTS.lock().unwrap();
        let fixture = Fixture::new();
        let owner = Reservation::acquire_at(&fixture.path()).unwrap();
        assert_eq!(
            owner.file.metadata().unwrap().permissions().mode() & 0o077,
            0
        );
    }

    #[test]
    fn reservation_refuses_overlap_and_releases_after_owner_exit() {
        let _serial = RESERVATION_TESTS.lock().unwrap();
        let fixture = Fixture::new();
        let owner = Reservation::acquire_at(&fixture.path()).unwrap();
        assert!(Reservation::acquire_at(&fixture.path()).is_err());
        drop(owner);
        assert!(Reservation::acquire_at(&fixture.path()).is_ok());
    }
    #[test]
    fn repeated_start_stop_joins_and_is_idempotent() {
        for _ in 0..20 {
            let mut t = CaptureThread::spawn(|flag| {
                while !flag.load(Ordering::Acquire) {
                    std::thread::yield_now();
                }
            });
            assert!(t.pending());
            assert!(t.shutdown(Duration::from_secs(1)));
            assert!(!t.pending());
            assert_eq!(t.completed, Some(true));
            assert!(t.shutdown(Duration::ZERO));
        }
    }
    #[test]
    fn partial_initialization_exit_is_joined() {
        let mut t = CaptureThread::spawn(|_| ());
        assert!(t.shutdown(Duration::from_secs(1)));
    }
    #[test]
    fn completion_notice_does_not_make_blocked_thread_local_cleanup_joinable() {
        let _serial = RESERVATION_TESTS.lock().unwrap();
        let fixture = Fixture::new();
        let reservation = Reservation::acquire_at(&fixture.path()).unwrap();
        struct OnExit(mpsc::SyncSender<()>, mpsc::Receiver<()>);
        impl Drop for OnExit {
            fn drop(&mut self) {
                let _ = self.0.send(());
                let _ = self.1.recv();
            }
        }
        thread_local! { static EXIT: std::cell::RefCell<Option<OnExit>> = const { std::cell::RefCell::new(None) }; }
        let (started, waiting) = mpsc::sync_channel(1);
        let (release, blocked) = mpsc::sync_channel(1);
        struct Owned(Arc<AtomicBool>);
        impl Drop for Owned {
            fn drop(&mut self) {
                self.0.store(false, Ordering::Release);
            }
        }
        let owned = Arc::new(AtomicBool::new(true));
        let mut t = CaptureThread::spawn_owned(
            move |_| EXIT.set(Some(OnExit(started, blocked))),
            (Owned(owned.clone()), reservation),
            None,
        );
        waiting.recv_timeout(Duration::from_secs(1)).unwrap();
        // Rescue prevents a regression from hanging the test process forever.
        let (kick, rescue_wait) = mpsc::sync_channel(1);
        let rescue = std::thread::spawn(move || {
            let _ = rescue_wait.recv_timeout(Duration::from_secs(1));
            let _ = release.send(());
        });
        let stopped = t.shutdown(Duration::ZERO);
        let retained = owned.load(Ordering::Acquire);
        let capture_still_reserved = Reservation::acquire_at(&fixture.path()).is_err();
        let _ = kick.send(());
        rescue.join().unwrap();
        assert!(!stopped, "a completed task is not a completed OS thread");
        assert!(
            retained,
            "capture ownership must survive thread-local cleanup"
        );
        assert!(
            capture_still_reserved,
            "OS capture guard must survive thread-local cleanup"
        );
        assert!(t.shutdown(Duration::from_secs(1)));
        assert!(!owned.load(Ordering::Acquire));
        assert!(Reservation::acquire_at(&fixture.path()).is_ok());
    }
    #[test]
    fn blocked_backend_reports_timeout_without_waiting_forever() {
        let (tx, rx) = mpsc::sync_channel(1);
        let mut t = CaptureThread::spawn(move |_| {
            let _ = rx.recv();
        });
        assert!(!t.shutdown(Duration::ZERO));
        tx.send(()).unwrap();
        assert!(t.shutdown(Duration::from_secs(1)));
    }
}
