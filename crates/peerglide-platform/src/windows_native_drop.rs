//! Native OLE copy drag on the local UI owner, never an input callback.
use anyhow::{ensure, Context, Result};
use std::{os::windows::ffi::OsStrExt, path::Path};

extern "C" {
    fn linko_windows_copy_drag(path: *const u16, units: usize) -> i32;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Cancelled,
    Copied,
    Unknown,
}

/// Only a locally authorized UI gesture supplies a previously verified path.
/// This call owns an OLE UI message loop until the user drops or cancels.
pub fn drag_prepared_file(path: &Path) -> Result<Outcome> {
    ensure!(path.is_absolute(), "native drop requires a local absolute path");
    ensure!(
        path.symlink_metadata()
            .context("prepared drop file unavailable")?
            .file_type()
            .is_file(),
        "native drop requires a regular file"
    );
    let units = bounded_path(path)?;
    // SAFETY: the bounded UTF-16 slice remains alive through the call. OLE
    // receives an owned copy of the path and only allows the copy effect.
    let result = unsafe { linko_windows_copy_drag(units.as_ptr(), units.len()) };
    match result {
        0 => Ok(Outcome::Cancelled),
        1 => Ok(Outcome::Copied),
        2 => Ok(Outcome::Unknown),
        _ => anyhow::bail!("Windows native copy drag failed ({result:#010x})"),
    }
}

fn bounded_path(path: &Path) -> Result<Vec<u16>> {
    let words: Vec<_> = path.as_os_str().encode_wide().take(32_768).collect();
    ensure!(
        (1..=32_767).contains(&words.len()) && !words.contains(&0),
        "native drop path outside bounds"
    );
    Ok(words)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_drop_path_rejects_nul_and_oversize() {
        assert!(bounded_path(Path::new("C:\\received\\screen.txt")).is_ok());
        assert!(bounded_path(Path::new("C:\\invalid\0file")).is_err());
        assert!(bounded_path(Path::new(&"x".repeat(32_768))).is_err());
    }
}
