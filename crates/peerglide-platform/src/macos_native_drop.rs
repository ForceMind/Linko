//! Copy-only AppKit drag from a real local mouse-down event on the UI thread.
use crate::copy_drag::{drag_millis, validate_png, validate_text};
use anyhow::{ensure, Context, Result};
use std::{
    ffi::{c_char, c_void, CStr, OsStr},
    os::unix::ffi::OsStrExt,
    panic::{catch_unwind, AssertUnwindSafe},
    path::Path,
    ptr::null_mut,
    time::Duration,
};

type PromiseWriter = Box<dyn FnOnce(&Path, &dyn Fn() -> bool) -> Result<()> + Send>;
type ContentAdmission = Box<dyn Fn() -> bool + Send + Sync>;
type PromiseAdmission = unsafe extern "C" fn(*mut c_void) -> bool;
type NativePromiseWriter =
    extern "C" fn(*mut c_void, *const c_char, *mut c_void, PromiseAdmission) -> bool;

extern "C" {
    fn linko_macos_copy_drag_start(
        path: *const u8,
        bytes: usize,
        millis: u32,
        owner: *mut *mut c_void,
    ) -> i32;
    fn linko_macos_file_promise_start(
        name: *const u8,
        bytes: usize,
        millis: u32,
        context: *mut c_void,
        write: NativePromiseWriter,
        destroy: extern "C" fn(*mut c_void),
        owner: *mut *mut c_void,
        origin: *mut c_void,
    ) -> i32;
    fn linko_macos_file_drag_press() -> *mut c_void;
    fn linko_macos_file_drag_press_release(press: *mut c_void);
    fn linko_macos_copy_drag_content(
        content: *const u8,
        bytes: usize,
        is_png: bool,
        millis: u32,
        owner: *mut *mut c_void,
        origin: *mut c_void,
        context: *mut c_void,
        check: extern "C" fn(*mut c_void) -> bool,
        destroy: extern "C" fn(*mut c_void),
    ) -> i32;
    fn linko_macos_copy_drag_outcome(owner: *mut c_void) -> i32;
    fn linko_macos_copy_drag_started(owner: *mut c_void) -> bool;
    fn linko_macos_copy_drag_cancel(owner: *mut c_void);
    fn linko_macos_copy_drag_release(owner: *mut c_void);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Copied,
    Cancelled,
    Unknown,
}

/// Retains the actual, tagged AppKit mouse-down from the receiving window.
pub struct HandoffPress(*mut c_void);

impl HandoffPress {
    pub fn current() -> Option<Self> {
        // SAFETY: the bridge checks the main thread before accessing AppKit.
        let press = unsafe { linko_macos_file_drag_press() };
        (!press.is_null()).then(|| Self(press))
    }
}

impl Drop for HandoffPress {
    fn drop(&mut self) {
        // SAFETY: this handle owns one retain and is not transferable.
        unsafe { linko_macos_file_drag_press_release(self.0) };
    }
}

pub struct Drag {
    owner: *mut c_void,
}

impl Drag {
    /// The caller already verified the file and obtained local drop consent.
    /// AppKit requires the originating window's current mouse-down event.
    pub fn start_prepared_file(path: &Path, remaining: Duration) -> Result<Self> {
        ensure!(path.is_absolute(), "native drop requires an absolute path");
        ensure!(
            path.symlink_metadata()
                .context("prepared drop file unavailable")?
                .file_type()
                .is_file(),
            "native drop requires a regular file"
        );
        let path = bounded_path(path)?;
        let millis = drag_millis(remaining)?;
        Self::start(|owner| {
            // SAFETY: path is bounded UTF-8 and AppKit copies it before returning.
            unsafe { linko_macos_copy_drag_start(path.as_ptr(), path.len(), millis, owner) }
        })
    }

    /// AppKit invokes `write` once on a background file-promise queue. It owns
    /// the closure and the prepared content until the provider is released.
    pub fn start_file_promise(
        name: &str,
        remaining: Duration,
        write: impl FnOnce(&Path, &dyn Fn() -> bool) -> Result<()> + Send + 'static,
    ) -> Result<Self> {
        Self::file_promise(name, remaining, write, null_mut())
    }

    pub fn start_handoff_promise(
        name: &str,
        remaining: Duration,
        press: HandoffPress,
        write: impl FnOnce(&Path, &dyn Fn() -> bool) -> Result<()> + Send + 'static,
    ) -> Result<Self> {
        Self::file_promise(name, remaining, write, press.0)
    }

    fn file_promise(
        name: &str,
        remaining: Duration,
        write: impl FnOnce(&Path, &dyn Fn() -> bool) -> Result<()> + Send + 'static,
        origin: *mut c_void,
    ) -> Result<Self> {
        ensure!(
            peerglide_transfer::valid_name(name),
            "invalid native drop name"
        );
        let millis = drag_millis(remaining)?;
        let writer: Box<Option<PromiseWriter>> = Box::new(Some(Box::new(write)));
        let context = Box::into_raw(writer).cast();
        Self::start(|owner| {
            // SAFETY: the native call copies the name and takes ownership of
            // the boxed context on every path, including failure to start.
            unsafe {
                linko_macos_file_promise_start(
                    name.as_ptr(),
                    name.len(),
                    millis,
                    context,
                    write_promise,
                    drop_promise,
                    owner,
                    origin,
                )
            }
        })
    }

    pub fn start_text(text: &str, remaining: Duration) -> Result<Self> {
        validate_text(text)?;
        Self::content(text.as_bytes(), false, remaining, None)
    }

    /// Advertise text from this tagged press. The native provider consults the
    /// local admission check each time it is asked to deliver the bytes.
    pub fn start_handoff_text(
        text: &str,
        remaining: Duration,
        press: HandoffPress,
        admit: impl Fn() -> bool + Send + Sync + 'static,
    ) -> Result<Self> {
        validate_text(text)?;
        Self::content(
            text.as_bytes(),
            false,
            remaining,
            Some((press, Box::new(admit))),
        )
    }

    /// The caller supplies a PNG already normalized by the clipboard receive path.
    pub fn start_png(png: &[u8], remaining: Duration) -> Result<Self> {
        validate_png(png)?;
        Self::content(png, true, remaining, None)
    }

    /// Use the same release check for a PNG prepared by the clipboard worker.
    pub fn start_handoff_png(
        png: &[u8],
        remaining: Duration,
        press: HandoffPress,
        admit: impl Fn() -> bool + Send + Sync + 'static,
    ) -> Result<Self> {
        validate_png(png)?;
        Self::content(png, true, remaining, Some((press, Box::new(admit))))
    }

    fn content(
        bytes: &[u8],
        is_png: bool,
        remaining: Duration,
        handoff: Option<(HandoffPress, ContentAdmission)>,
    ) -> Result<Self> {
        let millis = drag_millis(remaining)?;
        let (press, context) = match handoff {
            Some((press, check)) => (Some(press), Box::into_raw(Box::new(check)).cast()),
            None => (None, null_mut()),
        };
        let origin = press.as_ref().map_or(null_mut(), |press| press.0);
        Self::start(|owner| {
            // SAFETY: AppKit copies the bounded bytes and consumes context on
            // success and failure. The press remains retained through the call.
            unsafe {
                linko_macos_copy_drag_content(
                    bytes.as_ptr(),
                    bytes.len(),
                    is_png,
                    millis,
                    owner,
                    origin,
                    context,
                    check_content,
                    drop_content,
                )
            }
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

    pub fn started(&self) -> bool {
        // SAFETY: the owner remains retained for the lifetime of the drag.
        unsafe { linko_macos_copy_drag_started(self.owner) }
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

extern "C" fn check_content(context: *mut c_void) -> bool {
    catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: the provider keeps its boxed admission alive until deallocation.
        let admit = unsafe { &*context.cast::<ContentAdmission>() };
        admit()
    }))
    .unwrap_or(false)
}

extern "C" fn drop_content(context: *mut c_void) {
    // SAFETY: the native provider transfers ownership back once.
    unsafe { drop(Box::from_raw(context.cast::<ContentAdmission>())) }
}

extern "C" fn write_promise(
    context: *mut c_void,
    destination: *const c_char,
    owner: *mut c_void,
    can_write: PromiseAdmission,
) -> bool {
    catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: AppKit's provider owns this box, calls this bridge only once,
        // and keeps the destination C string and admission owner live during it.
        let writer = unsafe { &mut *context.cast::<Option<PromiseWriter>>() }.take();
        let destination = unsafe { CStr::from_ptr(destination) };
        let path = Path::new(OsStr::from_bytes(destination.to_bytes()));
        let allowed = || unsafe { can_write(owner) };
        writer.is_some_and(|write| write(path, &allowed).is_ok())
    }))
    .unwrap_or(false)
}

extern "C" fn drop_promise(context: *mut c_void) {
    // SAFETY: the native provider destroys its one boxed writer exactly once.
    let _ = catch_unwind(AssertUnwindSafe(|| unsafe {
        drop(Box::from_raw(context.cast::<Option<PromiseWriter>>()));
    }));
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

    extern "C" {
        fn linko_macos_guarded_copy_regression() -> i32;
    }

    #[test]
    fn guarded_appkit_copy_rejects_early_and_retired_reads() {
        // SAFETY: the fixture only exercises a pasteboard item's in-memory provider.
        assert_eq!(unsafe { linko_macos_guarded_copy_regression() }, 0);
    }

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
