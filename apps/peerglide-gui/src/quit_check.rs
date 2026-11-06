//! Opt-in isolated AppKit integration check. Never enables input or networking.
use anyhow::{Context, Result};
use peerglide_platform::application_menu::Owner;
use serde::Serialize;
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::Path,
    time::{Duration, Instant},
};

pub(super) struct Check {
    file: File,
    started: Instant,
    turns: u32,
    requested: bool,
    returned: bool,
    quit_received: bool,
    running_before: Option<bool>,
    error: Option<&'static str>,
}
impl Check {
    pub(super) fn prepare(dir: &Path, report: &Path) -> Result<Self> {
        anyhow::ensure!(cfg!(target_os = "macos"), "此退出检查仅支持 macOS");
        match fs::read_dir(dir) {
            Ok(mut entries) => {
                anyhow::ensure!(entries.next().is_none(), "退出检查只接受空的独立资料目录")
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error).context("无法检查独立资料目录"),
        }
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        // An empty reserved file is deliberately not a success receipt. Existing
        // reports/user files reject before the GUI starts and are never replaced.
        let file = options.open(report).context("退出检查报告必须是新文件")?;
        Ok(Self {
            file,
            started: Instant::now(),
            turns: 0,
            requested: false,
            returned: false,
            quit_received: false,
            running_before: None,
            error: None,
        })
    }
    /// True requests a normal GUI close after a failed check. No synthetic Quit
    /// is posted: a success must come from the real application delegate bridge.
    pub(super) fn tick(&mut self, owner: Option<&Owner>, unsafe_context: bool) -> bool {
        if self.error.is_some() {
            return true;
        }
        if unsafe_context {
            self.error = Some("check requires no peer, native preference or worker");
            return true;
        }
        if self.started.elapsed() > Duration::from_secs(10) {
            self.error = Some("AppKit termination did not complete within deadline");
            return true;
        }
        self.turns = self.turns.saturating_add(1);
        if !self.requested
            && self.turns >= 2
            && self.started.elapsed() >= Duration::from_millis(100)
        {
            self.requested = true;
            let Some(owner) = owner else {
                self.error = Some("real application exit owner is missing");
                return true;
            };
            match owner.request_termination_for_check() {
                Ok(running) => {
                    self.running_before = Some(running);
                    self.returned = true;
                }
                Err(_) => {
                    self.error = Some("real AppKit termination request failed");
                    return true;
                }
            }
        }
        false
    }
    pub(super) fn received_quit(&mut self) {
        self.quit_received = true;
    }
    pub(super) fn finish(
        mut self,
        no_worker_or_input: bool,
        exit_owner_healthy: bool,
    ) -> Result<()> {
        #[derive(Serialize)]
        struct Receipt {
            schema_version: u16,
            scope: &'static str,
            terminate_returned: bool,
            same_mailbox_quit_received: bool,
            application_running_before: Option<bool>,
            no_worker_or_input: bool,
            exit_owner_healthy: bool,
            closing_in_gui_logic: bool,
            success: bool,
            error: Option<&'static str>,
        }
        let receipt = Receipt {
            schema_version: 1,
            scope: "isolated_real_appkit_terminate_to_owned_quit",
            terminate_returned: self.returned,
            same_mailbox_quit_received: self.quit_received,
            application_running_before: self.running_before,
            no_worker_or_input,
            exit_owner_healthy,
            closing_in_gui_logic: true,
            success: self.requested
                && self.returned
                && self.quit_received
                && no_worker_or_input
                && exit_owner_healthy
                && self.error.is_none(),
            error: self.error,
        };
        self.file.write_all(&serde_json::to_vec(&receipt)?)?;
        self.file.sync_all()?;
        Ok(())
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    #[test]
    fn used_profile_and_existing_report_are_never_overwritten() -> Result<()> {
        let f = crate::tests::Fixture::new()?;
        let profile = f.path.join("profile");
        fs::create_dir(&profile)?;
        let report = f.path.join("report.json");
        fs::write(&report, b"existing report")?;
        assert!(Check::prepare(&profile, &report).is_err());
        assert_eq!(fs::read(&report)?, b"existing report");
        fs::write(profile.join("settings.json"), b"existing settings")?;
        let other = f.path.join("other.json");
        assert!(Check::prepare(&profile, &other).is_err());
        assert!(!other.exists());
        assert_eq!(
            fs::read(profile.join("settings.json"))?,
            b"existing settings"
        );
        Ok(())
    }
}
