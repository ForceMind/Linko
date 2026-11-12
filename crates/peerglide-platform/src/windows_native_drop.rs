//! Native OLE copy drag on the local UI owner, never an input callback.
use crate::copy_drag::{drag_millis, validate_png, validate_text};
use anyhow::{ensure, Context, Result};
use std::{os::windows::ffi::OsStrExt, path::Path, time::Duration};

extern "C" {
    fn linko_windows_copy_drag(path: *const u16, units: usize) -> i32;
    fn linko_windows_copy_drag_text(text: *const u16, units: usize, millis: u32) -> i32;
    fn linko_windows_copy_drag_png(png: *const u8, bytes: usize, millis: u32) -> i32;
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
    ensure!(
        path.is_absolute(),
        "native drop requires a local absolute path"
    );
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
    decode_outcome(unsafe { linko_windows_copy_drag(units.as_ptr(), units.len()) })
}

pub fn drag_text(text: &str, remaining: Duration) -> Result<Outcome> {
    validate_text(text)?;
    let millis = drag_millis(remaining)?;
    let units: Vec<_> = text.encode_utf16().collect();
    // SAFETY: the bounded UTF-16 slice stays alive through the call and OLE gets an owned copy.
    decode_outcome(unsafe { linko_windows_copy_drag_text(units.as_ptr(), units.len(), millis) })
}

/// The caller supplies a PNG already normalized by the clipboard receive path.
pub fn drag_png(png: &[u8], remaining: Duration) -> Result<Outcome> {
    validate_png(png)?;
    let millis = drag_millis(remaining)?;
    // SAFETY: the bounded byte slice stays alive through the call and OLE gets an owned copy.
    decode_outcome(unsafe { linko_windows_copy_drag_png(png.as_ptr(), png.len(), millis) })
}

fn decode_outcome(result: i32) -> Result<Outcome> {
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
