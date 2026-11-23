//! Layout preferences for the four offline workspace panels.
use std::path::Path;

use anyhow::{ensure, Result};
use peerglide_gui::config_store::Store;
use serde::{Deserialize, Serialize};

pub(super) const FILENAME: &str = "workspace-layout.json";
const VERSION: u16 = 1;
const SLOTS: [u8; 4] = [1, 2, 3, 4];

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Layout {
    layout_version: u16,
    pub(super) panel_order: [u8; 4],
    pub(super) focused_panel: u8,
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            layout_version: VERSION,
            panel_order: SLOTS,
            focused_panel: SLOTS[0],
        }
    }
}

impl Layout {
    fn validate(self) -> Result<()> {
        ensure!(self.layout_version == VERSION, "unknown workspace layout version");
        let mut slots = self.panel_order;
        slots.sort_unstable();
        ensure!(slots == SLOTS, "workspace layout must list each panel once");
        ensure!(
            slots.contains(&self.focused_panel),
            "workspace layout has an unknown focused panel"
        );
        Ok(())
    }
}

pub(super) struct LayoutProfile {
    store: Store,
    pub(super) notice: Option<String>,
}

impl LayoutProfile {
    pub(super) fn open(directory: &Path) -> Result<(Self, Layout)> {
        ensure!(directory.is_dir(), "workspace layout directory does not exist");
        let (store, layout) = Store::open::<Layout>(directory.join(FILENAME))?;
        layout.validate()?;
        Ok((Self { store, notice: None }, layout))
    }

    pub(super) fn save(&mut self, panel_order: [u8; 4], focused_panel: u8) -> Result<()> {
        let layout = Layout {
            layout_version: VERSION,
            panel_order,
            focused_panel,
        };
        layout.validate()?;
        self.store.save(&layout)
    }
}
