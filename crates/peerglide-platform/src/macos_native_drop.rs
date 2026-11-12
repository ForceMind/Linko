//! Copy-only AppKit drag from a real local mouse-down event on the UI thread.
use crate::copy_drag::{drag_millis, validate_png, validate_text};
use anyhow::{ensure, Context, Result};
use std::{ffi::c_void, path::Path, ptr::null_mut, time::Duration};

extern "C" {
    fn linko_macos_copy_drag_start(path: *const u8, bytes: usize, owner: *mut *mut c_void) -> i32;
    fn linko_macos_copy_drag_text(
        text: *const u8,
        bytes: usize,
        millis: u32,
        owner: *mut *mut c_void,
    ) -> i32;
    fn linko_macos_copy_drag_png(
        png: *const u8,
        bytes: usize,
        millis: u32,
        owner: *mut *mut c_void,
    ) -> i32;
    fn linko_macos_copy_drag_outcome(owner: *mut c_void) -> i32;
    fn linko_macos_copy_drag_cancel(owner: *mut c_void);
    fn linko_macos_copy_drag_release(owner: *mut c_void);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Copied,
    Cancelled,
    Unknown,
}

pub struct Drag {
    owner: *mut c_void,
}

impl Drag {
    /// The caller already verified the file and obtained local drop consent.
    /// AppKit requires the originating window's current mouse-down event.
    pub fn start_prepared_file(path: &Path) -> Result<Self> {
        ensure!(path.is_absolute(), "native drop requires an absolute path");
        ensure!(
            path.symlink_metadata()
                .context("prepared drop file unavailable")?
                .file_type()
                .is_file(),
            "native drop requires a regular file"
        );
        let path = bounded_path(path)?;
        Self::start(|owner| {
            // SAFETY: path is bounded UTF-8 and AppKit copies it before returning.
            unsafe { linko_macos_copy_drag_start(path.as_ptr(), path.len(), owner) }
        })
    }

    pub fn start_text(text: &str, remaining: Duration) -> Result<Self> {
        validate_text(text)?;
        let millis = drag_millis(remaining)?;
        Self::start(|owner| {
            // SAFETY: text is bounded UTF-8 and AppKit owns the data used by the drag.
            unsafe { linko_macos_copy_drag_text(text.as_ptr(), text.len(), millis, owner) }
        })
    }

    /// The caller supplies a PNG already normalized by the clipboard receive path.
    pub fn start_png(png: &[u8], remaining: Duration) -> Result<Self> {
        validate_png(png)?;
        let millis = drag_millis(remaining)?;
        Self::start(|owner| {
            // SAFETY: the bounded PNG stays alive through the call. AppKit copies it.
            unsafe { linko_macos_copy_drag_png(png.as_ptr(), png.len(), millis, owner) }
        })
    }

    fn start(call: impl FnOnce(&mut *mut c_void) -> i32) -> Result<Self> {
        let mut owner = null_mut();
        let result = call(&mut owner);
        ensure!(
            result == 0 && !owner.is_null(),
            "macOS native copy drag could not start ({result})"
        );
        Ok(Self { owner })
    }

    pub fn outcome(&self) -> Option<Outcome> {
        // SAFETY: the owner is a retained, live observation of the drag source.
        decode_outcome(unsafe { linko_macos_copy_drag_outcome(self.owner) })
    }

    /// Withdraw the copy offer. AppKit reports the final result asynchronously.
    pub fn request_cancel(&self) {
        // SAFETY: the owner stays alive through this call.
        unsafe { linko_macos_copy_drag_cancel(self.owner) };
    }
}

impl Drop for Drag {
    fn drop(&mut self) {
        // SAFETY: release exactly the retained observation owned by this value.
        unsafe { linko_macos_copy_drag_release(self.owner) };
    }
}

fn bounded_path(path: &Path) -> Result<&str> {
    let path = path.to_str().context("native drop path is not UTF-8")?;
    ensure!(
        (1..=4096).contains(&path.len()) && !path.contains('\0'),
        "native drop path outside bounds"
    );
    Ok(path)
}

fn decode_outcome(result: i32) -> Option<Outcome> {
    match result {
        0 => None,
        1 => Some(Outcome::Copied),
        2 => Some(Outcome::Cancelled),
        _ => Some(Outcome::Unknown),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_limits_and_unknown_outcomes_are_preserved() {
        assert_eq!(
            bounded_path(Path::new("/tmp/屏跃.txt")).unwrap(),
            "/tmp/屏跃.txt"
        );
        assert!(bounded_path(Path::new("/tmp/invalid\0file")).is_err());
        assert!(bounded_path(Path::new(&"x".repeat(4097))).is_err());
        assert_eq!(decode_outcome(0), None);
        assert_eq!(decode_outcome(1), Some(Outcome::Copied));
        assert_eq!(decode_outcome(2), Some(Outcome::Cancelled));
        assert_eq!(decode_outcome(-1), Some(Outcome::Unknown));
    }
}
