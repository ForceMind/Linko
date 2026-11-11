//! Capture-thread ownership of matched input pairs.
use crate::{CaptureEpoch, CaptureRoute};

#[derive(Clone, Copy, Default)]
pub(super) enum PairOwner {
    #[default]
    Empty,
    Local(CaptureEpoch),
    Remote(CaptureEpoch),
    Consumed(CaptureRoute),
    // Protected pairs are fixed-table holds, separate from the physical hold sets.
    Protected(CaptureRoute),
}
impl PairOwner {
    pub(super) fn route(self, fallback: CaptureRoute) -> CaptureRoute {
        match self {
            Self::Local(e) => CaptureRoute::Local(e),
            Self::Remote(e) => CaptureRoute::Remote(e),
            Self::Consumed(r) | Self::Protected(r) => r,
            Self::Empty => CaptureRoute::Local(fallback.epoch()),
        }
    }
    pub(super) fn suppressed(self) -> bool {
        matches!(
            self,
            Self::Remote(_) | Self::Consumed(_) | Self::Protected(_)
        )
    }
}
pub(super) struct PairLedger {
    pub(super) keys: [PairOwner; 256],
    pub(super) buttons: [PairOwner; 256],
    pub(super) unknown: [u64; 1024],
    pub(super) extra_held: usize,
    pub(super) force_armed: bool,
    pub(super) force_pending: bool,
    pub(super) debt: usize,
    pub(super) mouse_debt: usize,
    pub(super) unknown_debt: usize,
}
impl Default for PairLedger {
    fn default() -> Self {
        Self {
            keys: [PairOwner::Empty; 256],
            buttons: [PairOwner::Empty; 256],
            unknown: [0; 1024],
            extra_held: 0,
            force_armed: false,
            force_pending: false,
            debt: 0,
            mouse_debt: 0,
            unknown_debt: 0,
        }
    }
}
impl PairLedger {
    pub(super) fn replace(&mut self, key: bool, index: usize, owner: PairOwner) {
        let slot = if key {
            &mut self.keys[index]
        } else {
            &mut self.buttons[index]
        };
        self.extra_held = self.extra_held - usize::from(matches!(slot, PairOwner::Protected(_)))
            + usize::from(matches!(owner, PairOwner::Protected(_)));
        let old_debt = usize::from(slot.suppressed());
        let new_debt = usize::from(owner.suppressed());
        self.debt = self.debt - old_debt + new_debt;
        if !key {
            self.mouse_debt = self.mouse_debt - old_debt + new_debt;
        }
        *slot = owner;
    }
    pub(super) fn mouse_debt(&self) -> bool {
        self.mouse_debt != 0
    }
}
