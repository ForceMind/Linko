//! Input receive lifecycle and owned-injection recovery.
use anyhow::Result;
use peerglide_core::{PendingReleases, Receiver};
use peerglide_platform::{
    CaptureEpoch, ReceiveAck, ReceiveDriver, ReceiveError, ReceivePoint, ReceiveSnapshot,
};
use peerglide_protocol::{Input, Motion};

pub(super) struct Recovery {
    pub(super) receiver: Receiver,
    pub(super) native: bool,
    pub(super) pending_releases: PendingReleases,
    pub(super) driver: Option<ReceiveDriver>,
    pub(super) receive_epoch: Option<CaptureEpoch>,
}
impl Recovery {
    pub(super) fn snapshot(&self) -> ReceiveSnapshot {
        match (&self.driver, self.receive_epoch) {
            (Some(driver), Some(epoch)) => driver.snapshot(epoch),
            _ => ReceiveSnapshot::Retired,
        }
    }
    pub(super) fn outcome(&self) -> ReceiveAck {
        self.driver
            .as_ref()
            .map_or(ReceiveAck::Retired, |driver| driver.control().outcome())
    }
    pub(super) fn protection_pending(&self) -> bool {
        self.native
            && (self.snapshot() != ReceiveSnapshot::Retired
                || self.outcome() != ReceiveAck::Retired)
    }
    pub(super) async fn begin_receive(&mut self) -> Result<bool> {
        if !self.native {
            return Ok(true);
        }
        let driver = self
            .driver
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("native receive service unavailable"))?;
        match driver.begin().await {
            Ok(epoch) => {
                self.receive_epoch = Some(epoch);
                Ok(driver.is_current(epoch))
            }
            Err(ReceiveError::Busy | ReceiveError::Stale) => Ok(false),
            Err(error) => Err(error.into()),
        }
    }
    pub(super) async fn release(&mut self) -> Result<()> {
        if !self.native {
            return self.release_with(|_| Ok(()));
        }
        // Core owns protocol intent only. The Native loop is the sole OS owner,
        // including after the async worker stalls or an acknowledgement is lost.
        let _ = self.receiver.release();
        if let Some(epoch) = self.receive_epoch {
            let driver = self
                .driver
                .as_mut()
                .ok_or_else(|| anyhow::anyhow!("native receive service unavailable"))?;
            if driver.snapshot(epoch) == ReceiveSnapshot::Retired {
                self.receive_epoch = None;
                return Ok(());
            }
            let ack = driver.release(epoch, None).await?;
            anyhow::ensure!(
                ack != ReceiveAck::RecoveryRequired,
                "native receive cleanup requires recovery"
            );
            if driver.snapshot(epoch) == ReceiveSnapshot::Retired {
                self.receive_epoch = None;
            }
        }
        Ok(())
    }
    pub(super) fn release_with(&mut self, inject: impl FnMut(Input) -> Result<()>) -> Result<()> {
        self.pending_releases.extend(self.receiver.release());
        self.pending_releases.flush(inject).map_err(|remaining| {
            anyhow::anyhow!(
                "native input cleanup failed: {remaining} releases remain pending; session stopped"
            )
        })
    }
    pub(super) async fn finish(
        &mut self,
        lease: u64,
        cursor: Option<Motion>,
        now: u64,
    ) -> Result<()> {
        if !self.native {
            return self.finish_with(lease, cursor, now, |_| Ok(()), |_| Ok(()));
        }
        let Some(end) = self.receiver.finish(lease, cursor, now) else {
            return Ok(());
        };
        // Never replay Core's release list into the OS. Native already retains
        // and retires actual submitted Downs, independently of this ledger.
        if let Some(epoch) = self.receive_epoch {
            let driver = self
                .driver
                .as_mut()
                .ok_or_else(|| anyhow::anyhow!("native receive service unavailable"))?;
            if driver.snapshot(epoch) == ReceiveSnapshot::Retired {
                self.receive_epoch = None;
                return Ok(());
            }
            let terminal = end.cursor.map(|m| ReceivePoint { x: m.x, y: m.y });
            let ack = driver.release(epoch, terminal).await?;
            anyhow::ensure!(
                ack != ReceiveAck::RecoveryRequired,
                "native receive cleanup requires recovery"
            );
            if driver.snapshot(epoch) == ReceiveSnapshot::Retired {
                self.receive_epoch = None;
            }
        }
        Ok(())
    }
    pub(super) fn finish_with(
        &mut self,
        lease: u64,
        cursor: Option<Motion>,
        now: u64,
        mut pointer: impl FnMut(Motion) -> Result<()>,
        inject: impl FnMut(Input) -> Result<()>,
    ) -> Result<()> {
        let Some(end) = self.receiver.finish(lease, cursor, now) else {
            return Ok(());
        };
        self.pending_releases.extend(end.releases);
        let pointer_result = end.cursor.map(&mut pointer).unwrap_or(Ok(()));
        // Pointer failure must not bypass balanced key/button cleanup.
        self.pending_releases.flush(inject).map_err(|remaining| {
            anyhow::anyhow!(
                "native input cleanup failed: {remaining} releases remain pending; session stopped"
            )
        })?;
        pointer_result
    }
    pub(super) async fn input(
        &mut self,
        lease: u64,
        event: Input,
        cursor: Motion,
        now: u64,
    ) -> Result<bool> {
        if !self.native {
            return self.input_with(lease, event, cursor, now, |_| Ok(()), |_| Ok(()));
        }
        if !self.receiver.accepts_input(lease, event, now)
            || cursor.lease != lease
            || !self.receiver.anchor(cursor, now)
        {
            return Ok(false);
        }
        let Some(epoch) = self.receive_epoch else {
            return Ok(false);
        };
        let driver = self
            .driver
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("native receive service unavailable"))?;
        let ack = match driver
            .input(
                epoch,
                event,
                ReceivePoint {
                    x: cursor.x,
                    y: cursor.y,
                },
            )
            .await
        {
            Ok(ack) => ack,
            Err(ReceiveError::Stale)
                if matches!(
                    driver.snapshot(epoch),
                    ReceiveSnapshot::Draining | ReceiveSnapshot::Retired
                ) =>
            {
                return Ok(false)
            }
            Err(ReceiveError::Timeout)
                if matches!(
                    driver.snapshot(epoch),
                    ReceiveSnapshot::Draining | ReceiveSnapshot::Retired
                ) =>
            {
                // The expired request already asked the Native owner to retire this
                // epoch; the loop then releases the lease and confirms cleanup.
                eprintln!("Native receive input timed out; ending remote control.");
                return Ok(false);
            }
            Err(ReceiveError::Closed) if driver.snapshot(epoch) == ReceiveSnapshot::Retired => {
                return Ok(false)
            }
            Err(error) => return Err(error.into()),
        };
        anyhow::ensure!(
            ack != ReceiveAck::RecoveryRequired,
            "native receive submission requires recovery"
        );
        if ack == ReceiveAck::Submitted && driver.is_current(epoch) {
            Ok(self.receiver.input(lease, event, now))
        } else {
            Ok(false)
        }
    }
    pub(super) fn input_with(
        &mut self,
        lease: u64,
        event: Input,
        cursor: Motion,
        now: u64,
        pointer: impl FnOnce(Motion) -> Result<()>,
        inject: impl FnOnce(Input) -> Result<()>,
    ) -> Result<bool> {
        if !self.receiver.accepts_input(lease, event, now)
            || cursor.lease != lease
            || !self.receiver.anchor(cursor, now)
        {
            return Ok(false);
        }
        // Positioning can fail before injection. Never retire an owned release
        // before either operation succeeds; session cleanup must still retry it.
        pointer(cursor)?;
        self.receiver.input_with(lease, event, now, inject)
    }
    pub(super) async fn pointer(&mut self, x: f64, y: f64) -> Result<bool> {
        if !self.native {
            return Ok(true);
        }
        let Some(epoch) = self.receive_epoch else {
            return Ok(false);
        };
        let driver = self
            .driver
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("native receive service unavailable"))?;
        let ack = match driver.pointer(epoch, ReceivePoint { x, y }).await {
            Ok(ack) => ack,
            Err(ReceiveError::Stale)
                if matches!(
                    driver.snapshot(epoch),
                    ReceiveSnapshot::Draining | ReceiveSnapshot::Retired
                ) =>
            {
                return Ok(false)
            }
            Err(ReceiveError::Timeout)
                if matches!(
                    driver.snapshot(epoch),
                    ReceiveSnapshot::Draining | ReceiveSnapshot::Retired
                ) =>
            {
                eprintln!("Native receive pointer timed out; ending remote control.");
                return Ok(false);
            }
            Err(ReceiveError::Closed) if driver.snapshot(epoch) == ReceiveSnapshot::Retired => {
                return Ok(false)
            }
            Err(error) => return Err(error.into()),
        };
        anyhow::ensure!(
            ack != ReceiveAck::RecoveryRequired,
            "native receive pointer requires recovery"
        );
        Ok(ack == ReceiveAck::Submitted && driver.is_current(epoch))
    }
    pub(super) fn heartbeat(&mut self, lease: u64, now: u64) -> bool {
        if !self.receiver.accepts(lease, now) {
            return false;
        }
        if self.native {
            let (Some(driver), Some(epoch)) = (&mut self.driver, self.receive_epoch) else {
                return false;
            };
            if !driver.heartbeat(epoch) {
                return false;
            }
        }
        self.receiver.heartbeat(lease, now)
    }
}
impl Drop for Recovery {
    fn drop(&mut self) {
        if self.native {
            if let (Some(driver), Some(epoch)) = (&self.driver, self.receive_epoch) {
                let control = driver.control();
                control.request_recovery(epoch);
                control.wake_loop();
            }
        } else if self.release_with(|_| Ok(())).is_err() {
            eprintln!("Input cleanup remains incomplete; check local input state.");
        }
    }
}
