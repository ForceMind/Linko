//! Native OLE copy drag on the local UI owner, never an input callback.
use crate::copy_drag::{drag_millis, validate_png, validate_text};
use anyhow::{ensure, Context, Result};
use std::{
    ffi::c_void,
    os::windows::ffi::OsStrExt,
    panic::{catch_unwind, AssertUnwindSafe},
    path::Path,
    sync::mpsc::{self, SyncSender},
    time::Duration,
};

const READ_PIECE: usize = 64 * 1024;
const READ_TIMEOUT: Duration = Duration::from_secs(2);
type ReadDropFile = extern "C" fn(*mut c_void, u64, *mut u8, usize) -> bool;
type ReleaseDropFile = extern "C" fn(*mut c_void);
type CheckDrag = extern "C" fn(*const c_void, u8) -> bool;
type DragControl<'a> = &'a dyn Fn(u8) -> bool;

extern "C" {
    fn linko_windows_copy_drag(path: *const u16, units: usize, millis: u32) -> i32;
    fn linko_windows_verified_file_drag(
        name: *const u16,
        units: usize,
        bytes: u64,
        millis: u32,
        owner: *mut c_void,
        read: ReadDropFile,
        release: ReleaseDropFile,
        control: *const c_void,
        check: Option<CheckDrag>,
    ) -> i32;
    fn linko_windows_copy_drag_text(text: *const u16, units: usize, millis: u32) -> i32;
    fn linko_windows_copy_drag_png(png: *const u8, bytes: usize, millis: u32) -> i32;
}

struct ReadRequest {
    offset: u64,
    length: usize,
    reply: SyncSender<Option<Vec<u8>>>,
}

/// Publish one read-only, delayed file stream to a synchronous Shell drop.
/// The reader runs on a storage worker. The OLE owner revokes it on return.
pub fn drag_verified_file(
    name: &str,
    bytes: u64,
    remaining: Duration,
    read: impl FnMut(u64, &mut [u8]) -> Result<()> + Send + 'static,
) -> Result<Outcome> {
    verified_file_drag(name, bytes, remaining, read, None)
}

/// OLE owns the UI loop. Check the local handoff at startup, while active,
/// and again before admitting the content stream for a completed release.
pub fn drag_handoff_file(
    name: &str,
    bytes: u64,
    remaining: Duration,
    read: impl FnMut(u64, &mut [u8]) -> Result<()> + Send + 'static,
    control: DragControl<'_>,
) -> Result<Outcome> {
    verified_file_drag(name, bytes, remaining, read, Some(control))
}

fn verified_file_drag(
    name: &str,
    bytes: u64,
    remaining: Duration,
    mut read: impl FnMut(u64, &mut [u8]) -> Result<()> + Send + 'static,
    control: Option<DragControl<'_>>,
) -> Result<Outcome> {
    ensure!(
        peerglide_transfer::valid_name(name),
        "invalid native drop name"
    );
    ensure!(
        bytes <= peerglide_transfer::MAX_FILE_BYTES,
        "native drop file exceeds limit"
    );
    let millis = drag_millis(remaining)?;
    let units: Vec<_> = name.encode_utf16().collect();
    let (send, receive) = mpsc::sync_channel::<ReadRequest>(1);
    std::thread::Builder::new()
        .name("linko-ole-file-reader".into())
        .spawn(move || {
            while let Ok(request) = receive.recv() {
                let mut bytes = vec![0; request.length];
                let verified = read(request.offset, &mut bytes).is_ok();
                if request.reply.try_send(verified.then_some(bytes)).is_err() || !verified {
                    break;
                }
            }
        })?;
    let owner = Box::into_raw(Box::new(send)).cast();
    // SAFETY: the name slice stays live through the call. The native side owns
    // the boxed channel on every path and serializes reading with revocation.
    decode_outcome(unsafe {
        linko_windows_verified_file_drag(
            units.as_ptr(),
            units.len(),
            bytes,
            millis,
            owner,
            request_file_piece,
            release_file_reader,
            (&control as *const Option<DragControl<'_>>).cast(),
            control.map(|_| check_handoff as CheckDrag),
        )
    })
}

extern "C" fn check_handoff(control: *const c_void, phase: u8) -> bool {
    catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: OLE uses the borrowed callback on the same thread and only
        // during the synchronous native call.
        unsafe { &*control.cast::<Option<DragControl<'_>>>() }
            .is_some_and(|check| check(phase))
    })).unwrap_or(false)
}

extern "C" fn request_file_piece(
    owner: *mut c_void,
    offset: u64,
    output: *mut u8,
    length: usize,
) -> bool {
    catch_unwind(AssertUnwindSafe(|| {
        if owner.is_null() || output.is_null() || length > READ_PIECE {
            return false;
        }
        // SAFETY: the native owner keeps the sender and writable buffer alive
        // until this call returns, and closes the sender under the same lock.
        let sender = unsafe { &*owner.cast::<SyncSender<ReadRequest>>() };
        let (reply, result) = mpsc::sync_channel(1);
        if sender
            .try_send(ReadRequest {
                offset,
                length,
                reply,
            })
            .is_err()
        {
            return false;
        }
        let Ok(Some(piece)) = result.recv_timeout(READ_TIMEOUT) else {
            return false;
        };
        if piece.len() != length {
            return false;
        }
        // SAFETY: the two buffers have the same length and distinct ownership.
        unsafe { std::ptr::copy_nonoverlapping(piece.as_ptr(), output, length) };
        true
    }))
    .unwrap_or(false)
}

extern "C" fn release_file_reader(owner: *mut c_void) {
    // SAFETY: each native owner closes the boxed sender exactly once.
    unsafe { drop(Box::from_raw(owner.cast::<SyncSender<ReadRequest>>())) };
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Cancelled,
    Copied,
    Unknown,
}

/// Only a locally authorized UI gesture supplies a previously verified path.
/// This call owns an OLE UI message loop until the user drops or cancels.
pub fn drag_prepared_file(path: &Path, remaining: Duration) -> Result<Outcome> {
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
    let millis = drag_millis(remaining)?;
    // SAFETY: the bounded UTF-16 slice remains alive through the call. OLE
    // receives an owned copy of the path and only allows the copy effect.
    decode_outcome(unsafe { linko_windows_copy_drag(units.as_ptr(), units.len(), millis) })
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

    extern "C" {
        fn linko_windows_file_stream_regression() -> i32;
    }

    #[test]
    fn native_file_stream_opens_only_for_the_drop_and_revokes_retained_streams() {
        // SAFETY: the native fixture owns only in-memory data and COM handles.
        assert_eq!(unsafe { linko_windows_file_stream_regression() }, 0);
    }

    #[test]
    fn native_drop_path_rejects_nul_and_oversize() {
        assert!(bounded_path(Path::new("C:\\received\\screen.txt")).is_ok());
        assert!(bounded_path(Path::new("C:\\invalid\0file")).is_err());
        assert!(bounded_path(Path::new(&"x".repeat(32_768))).is_err());
    }
}
