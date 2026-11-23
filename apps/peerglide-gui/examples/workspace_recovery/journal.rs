//! Source-local persistence for the window handoff experiment.
use std::path::{Path, PathBuf};

use anyhow::{ensure, Context, Result};
use linko_workspace_lab::geometry::Rect;
use peerglide_gui::config_store::{lock, Lock, Store};
use serde::{Deserialize, Serialize};

pub(super) const FILENAME: &str = "window-recovery.json";
const VERSION: u16 = 1;

/// An incarnation attested by the local backend across journal process
/// restarts. An HWND, PID, title or viewer-side token cannot supply this value.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WindowInstance {
    pub(super) backend: [u8; 16],
    pub(super) object: u64,
}

impl WindowInstance {
    fn validate(self) -> Result<()> {
        ensure!(
            self.backend != [0; 16] && self.object != 0,
            "missing native window incarnation"
        );
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RecoveryRecord {
    pub(super) transaction: u64,
    pub(super) window: WindowInstance,
    bounds: [f64; 4],
}

impl RecoveryRecord {
    pub(super) fn original_bounds(self) -> Rect {
        let [x, y, width, height] = self.bounds;
        Rect {
            x,
            y,
            width,
            height,
        }
    }
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Ledger {
    version: u16,
    last_transaction: u64,
    pending: Option<RecoveryRecord>,
}

impl Default for Ledger {
    fn default() -> Self {
        Self {
            version: VERSION,
            last_transaction: 0,
            pending: None,
        }
    }
}

impl Ledger {
    fn validate(self) -> Result<()> {
        ensure!(
            self.version == VERSION,
            "unsupported recovery journal version"
        );
        if let Some(record) = self.pending {
            ensure!(
                record.transaction != 0 && record.transaction == self.last_transaction,
                "invalid recovery transaction"
            );
            record.window.validate()?;
            record.original_bounds().validate()?;
        }
        Ok(())
    }
}

pub(super) struct Journal {
    path: PathBuf,
    store: Option<Store>,
    ledger: Ledger,
    _owner: Lock,
}

impl Journal {
    pub(super) fn open(directory: &Path) -> Result<Self> {
        ensure!(directory.is_dir(), "recovery journal directory must exist");
        // Hold a single source-owner lock across the platform operation and
        // its acknowledgement. Store::save uses a different write lock.
        let owner = lock(&directory.join("window-recovery-owner.lock"))?;
        let path = directory.join(FILENAME);
        let (store, ledger) = Store::open::<Ledger>(path.clone())?;
        ledger.validate()?;
        Ok(Self {
            path,
            store: Some(store),
            ledger,
            _owner: owner,
        })
    }

    pub(super) fn pending(&self) -> Result<Option<RecoveryRecord>> {
        ensure!(self.store.is_some(), "reopen the recovery journal");
        Ok(self.ledger.pending)
    }

    /// Called after the current handoff policy has requested a hide. Persist
    /// the obligation before dispatching the action to the platform.
    pub(super) fn arm(&mut self, window: WindowInstance, bounds: Rect) -> Result<RecoveryRecord> {
        window.validate()?;
        bounds.validate()?;
        ensure!(
            self.pending()?.is_none(),
            "previous recovery is still pending"
        );
        let transaction = self
            .ledger
            .last_transaction
            .checked_add(1)
            .context("recovery transaction number exhausted")?;
        let record = RecoveryRecord {
            transaction,
            window,
            bounds: [bounds.x, bounds.y, bounds.width, bounds.height],
        };
        self.commit(Ledger {
            version: VERSION,
            last_transaction: transaction,
            pending: Some(record),
        })?;
        Ok(record)
    }

    /// The backend must verify the same live incarnation and resolve the
    /// original bounds against the current native display topology.
    pub(super) fn recovery_for(&self, observed: WindowInstance) -> Result<RecoveryRecord> {
        let record = self.pending()?.context("no recovery is pending")?;
        ensure!(
            record.window == observed,
            "native window incarnation changed"
        );
        Ok(record)
    }

    /// Only the local backend can confirm that this exact obligation has been
    /// resolved. Successful submission of a restore request is insufficient.
    pub(super) fn confirm(&mut self, observed: WindowInstance, transaction: u64) -> Result<()> {
        let record = self.recovery_for(observed)?;
        ensure!(
            record.transaction == transaction,
            "stale recovery acknowledgement"
        );
        self.commit(Ledger {
            pending: None,
            ..self.ledger
        })
    }

    fn commit(&mut self, candidate: Ledger) -> Result<()> {
        let mut store = self.store.take().context("reopen the recovery journal")?;
        if let Err(error) = store.save_without_backup(&candidate) {
            // A directory sync can fail after an atomic rename. Re-read only
            // the authoritative record under the same owner lock. The failed
            // call never gives permission to dispatch a hide.
            if let Ok((actual_store, actual)) = Store::open::<Ledger>(self.path.clone()) {
                if actual.validate().is_ok() {
                    self.ledger = actual;
                    self.store = Some(actual_store);
                }
            }
            return Err(error);
        }
        self.store = Some(store);
        self.ledger = candidate;
        Ok(())
    }
}
