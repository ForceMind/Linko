//! Explicit Win32 clipboard operations on the creating UI thread.
use super::{ImageSnapshot, TextSnapshot};
use anyhow::{ensure, Context, Result};
use peerglide_transfer::{
    clipboard::{Backend, Origin, WriteResult, MAX_TEXT_BYTES},
    clipboard_image::{Image, ImageBackend, MAX_PNG_BYTES},
};
use std::{
    mem::size_of,
    ptr::{null, null_mut},
};
use windows_sys::{
    core::w,
    Win32::{
        Foundation::{GetLastError, GlobalFree, SetLastError, HGLOBAL, HWND},
        System::{
            DataExchange::{
                CloseClipboard, EmptyClipboard, EnumClipboardFormats, GetClipboardData,
                GetClipboardSequenceNumber, OpenClipboard, RegisterClipboardFormatW,
                SetClipboardData,
            },
            LibraryLoader::GetModuleHandleW,
            Memory::{
                GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE, GMEM_ZEROINIT,
            },
            Ole::{
                CF_BITMAP, CF_DIB, CF_DIBV5, CF_LOCALE, CF_OEMTEXT, CF_PALETTE, CF_TEXT,
                CF_UNICODETEXT,
            },
        },
        UI::WindowsAndMessaging::{CreateWindowExW, DestroyWindow, HWND_MESSAGE},
    },
};

const UNICODE_FORMAT: u32 = CF_UNICODETEXT as u32;
const PLAIN_FORMATS: [u32; 4] = [
    CF_TEXT as u32,
    CF_OEMTEXT as u32,
    UNICODE_FORMAT,
    CF_LOCALE as u32,
];
const MAX_UTF16_WORDS: usize = MAX_TEXT_BYTES + 1;

/// One non-visible, thread-owned window for Windows clipboard ownership.
/// This creates no listener, capture hook, clipboard subscription or timer.
pub struct NativeClipboard {
    window: HWND,
    imported_revision: Option<u64>,
}

impl NativeClipboard {
    pub fn new() -> Result<Self> {
        // SAFETY: the module handle is borrowed and STATIC is a built-in class.
        // The returned message-only HWND stays owned by this calling thread.
        let window = unsafe {
            let module = GetModuleHandleW(null());
            ensure!(!module.is_null(), "clipboard window module unavailable");
            CreateWindowExW(
                0,
                w!("STATIC"),
                null(),
                0,
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                null_mut(),
                module,
                null(),
            )
        };
        ensure!(!window.is_null(), "clipboard owner window unavailable");
        Ok(Self {
            window,
            imported_revision: None,
        })
    }

    /// Snapshot the version before waiting for a remote text frame.
    pub fn revision(&self) -> Option<u64> {
        sequence()
    }

    /// Read only ordinary text formats. Unknown formats may carry a private
    /// source marker and are kept local, even when Unicode is also available.
    pub fn snapshot(&self) -> Result<Option<TextSnapshot>> {
        let _access = Access::open(self.window).context("clipboard is busy")?;
        let Some(revision) = sequence() else {
            return Ok(None);
        };
        if !plain_text_formats() {
            return Ok(None);
        }
        // SAFETY: the clipboard is open and owns the returned handle.
        let handle = unsafe { GetClipboardData(UNICODE_FORMAT) };
        if handle.is_null() {
            return Ok(None);
        }
        // SAFETY: the borrowed clipboard HGLOBAL stays valid until CloseClipboard.
        let length = unsafe { GlobalSize(handle) };
        if length < size_of::<u16>() || !length.is_multiple_of(size_of::<u16>()) {
            return Ok(None);
        }
        let memory = LockedMemory::lock(handle).context("clipboard text is unavailable")?;
        let limit = (length / size_of::<u16>()).min(MAX_UTF16_WORDS);
        let pointer = memory.pointer.cast::<u16>();
        // SAFETY: the locked clipboard contract supplies NUL-terminated
        // UTF-16. Scan only within the allocation and stop before its padding.
        let end = (0..limit).find(|&index| unsafe { pointer.add(index).read() == 0 });
        let Some(end) = end else {
            return Ok(None);
        };
        // SAFETY: the slice includes only the observed prefix and terminator.
        // It stays within the native lock while copied into an owned string.
        let words = unsafe { std::slice::from_raw_parts(pointer, end + 1) };
        let text = plain_unicode(words);
        if sequence() != Some(revision) {
            return Ok(None);
        }
        Ok(text.map(|text| TextSnapshot {
            revision,
            text,
            origin: self.origin(revision),
        }))
    }

    pub fn image_snapshot(&self) -> Result<Option<ImageSnapshot>> {
        let png = png_format().context("PNG clipboard format unavailable")?;
        let _access = Access::open(self.window).context("clipboard is busy")?;
        let Some(revision) = sequence() else {
            return Ok(None);
        };
        let allowed = [
            png,
            u32::from(CF_BITMAP),
            u32::from(CF_DIB),
            u32::from(CF_DIBV5),
            u32::from(CF_PALETTE),
        ];
        if !only_formats(&allowed, |format| format == png) {
            return Ok(None);
        }
        // SAFETY: registered clipboard data is a borrowed HGLOBAL valid during Access.
        let handle = unsafe { GetClipboardData(png) };
        if handle.is_null() {
            return Ok(None);
        }
        // SAFETY: the clipboard continues to own this live allocation.
        let allocation = unsafe { GlobalSize(handle) }.min(MAX_PNG_BYTES);
        let memory = LockedMemory::lock(handle).context("clipboard image unavailable")?;
        let pointer = memory.pointer.cast::<u8>();
        // SAFETY: the parser reads header bytes only within the bounded lock.
        let length = png_length(allocation, |index| unsafe { pointer.add(index).read() });
        let Some(length) = length else {
            return Ok(None);
        };
        // SAFETY: only the declared PNG through IEND is copied. Padding after
        // it is outside the format and never enters the snapshot.
        let bytes = unsafe { std::slice::from_raw_parts(pointer, length) };
        let png = bytes.to_vec();
        if sequence() != Some(revision) {
            return Ok(None);
        }
        Ok(Some(ImageSnapshot {
            revision,
            png,
            origin: self.origin(revision),
        }))
    }

    fn origin(&self, revision: u64) -> Origin {
        if self.imported_revision == Some(revision) {
            Origin::Imported
        } else {
            Origin::Local
        }
    }

    fn write_payload(
        &mut self,
        expected_revision: u64,
        format: u32,
        mut payload: OwnedMemory,
    ) -> WriteResult {
        let Some(_access) = Access::open(self.window) else {
            return WriteResult::Failed;
        };
        let Some(revision) = sequence() else {
            return WriteResult::Failed;
        };
        if revision != expected_revision {
            return WriteResult::Changed;
        }
        // SAFETY: the current thread holds the clipboard lock with its live HWND.
        // EmptyClipboard transfers ownership to that window. SetClipboardData
        // receives an unlocked GMEM_MOVEABLE allocation, transferred only on success.
        if unsafe { EmptyClipboard() } == 0 {
            return WriteResult::Failed;
        }
        self.imported_revision = None;
        if unsafe { SetClipboardData(format, payload.0) }.is_null() {
            return WriteResult::UnknownOutcome;
        }
        payload.0 = null_mut();
        self.imported_revision = sequence();
        WriteResult::Submitted
    }
}

impl Drop for NativeClipboard {
    fn drop(&mut self) {
        // SAFETY: this instance owns the live window on its creating thread.
        unsafe { DestroyWindow(self.window) };
    }
}

impl Backend for NativeClipboard {
    fn compare_and_write(&mut self, expected_revision: u64, text: &str) -> WriteResult {
        if text.is_empty() || text.len() > MAX_TEXT_BYTES || text.contains('\0') {
            return WriteResult::Failed;
        }
        let Some(payload) = OwnedMemory::encode(text) else {
            return WriteResult::Failed;
        };
        self.write_payload(expected_revision, UNICODE_FORMAT, payload)
    }
}

impl ImageBackend for NativeClipboard {
    fn compare_and_write_image(&mut self, expected_revision: u64, image: &Image) -> WriteResult {
        let Some(format) = png_format() else {
            return WriteResult::Failed;
        };
        let Some(payload) = OwnedMemory::copy(image.as_png()) else {
            return WriteResult::Failed;
        };
        self.write_payload(expected_revision, format, payload)
    }
}

struct Access;
impl Access {
    fn open(window: HWND) -> Option<Self> {
        // SAFETY: callers pass the live window they own on this thread.
        if unsafe { OpenClipboard(window) } != 0 {
            Some(Self)
        } else {
            None
        }
    }
}
impl Drop for Access {
    fn drop(&mut self) {
        // SAFETY: each guard represents one successful OpenClipboard.
        unsafe { CloseClipboard() };
    }
}

struct OwnedMemory(HGLOBAL);
impl OwnedMemory {
    fn encode(text: &str) -> Option<Self> {
        let bytes: Vec<_> = text
            .encode_utf16()
            .chain(std::iter::once(0))
            .flat_map(u16::to_ne_bytes)
            .collect();
        Self::copy(&bytes)
    }

    fn copy(bytes: &[u8]) -> Option<Self> {
        // SAFETY: bounded native allocation, owned until transfer to the OS.
        let memory = Self(unsafe { GlobalAlloc(GMEM_MOVEABLE | GMEM_ZEROINIT, bytes.len()) });
        if memory.0.is_null() {
            return None;
        }
        let locked = LockedMemory::lock(memory.0)?;
        // SAFETY: locked memory has room for all of the requested bytes.
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), locked.pointer.cast(), bytes.len());
        }
        drop(locked);
        Some(memory)
    }
}
impl Drop for OwnedMemory {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: this allocation has not been transferred to Windows.
            unsafe { GlobalFree(self.0) };
        }
    }
}

struct LockedMemory {
    handle: HGLOBAL,
    pointer: *mut std::ffi::c_void,
}
impl LockedMemory {
    fn lock(handle: HGLOBAL) -> Option<Self> {
        // SAFETY: callers pass live owned memory or a borrowed clipboard handle.
        let pointer = unsafe { GlobalLock(handle) };
        if pointer.is_null() {
            None
        } else {
            Some(Self { handle, pointer })
        }
    }
}
impl Drop for LockedMemory {
    fn drop(&mut self) {
        // SAFETY: every guard represents one successful GlobalLock.
        unsafe { GlobalUnlock(self.handle) };
    }
}

fn sequence() -> Option<u64> {
    // SAFETY: no pointers or ownership transfer.
    let revision = unsafe { GetClipboardSequenceNumber() };
    (revision != 0).then_some(u64::from(revision))
}

/// Find the declared end without examining pixels, metadata or trailing
/// allocation padding. Full PNG validation runs on the data worker.
fn png_length(allocation: usize, read: impl Fn(usize) -> u8) -> Option<usize> {
    const SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
    if allocation < SIGNATURE.len()
        || SIGNATURE
            .iter()
            .enumerate()
            .any(|(index, expected)| read(index) != *expected)
    {
        return None;
    }
    let mut offset = SIGNATURE.len();
    for _ in 0..4096 {
        let header_end = offset.checked_add(8)?;
        if header_end > allocation {
            return None;
        }
        let size = u32::from_be_bytes(std::array::from_fn(|index| read(offset + index)));
        let kind: [u8; 4] = std::array::from_fn(|index| read(offset + 4 + index));
        let end = header_end.checked_add(size as usize)?.checked_add(4)?;
        if end > allocation {
            return None;
        }
        if &kind == b"IEND" {
            return (size == 0).then_some(end);
        }
        offset = end;
    }
    None
}

fn png_format() -> Option<u32> {
    // SAFETY: the format name is a process-lifetime NUL-terminated wide string.
    let format = unsafe { RegisterClipboardFormatW(w!("PNG")) };
    (format != 0).then_some(format)
}

fn plain_text_formats() -> bool {
    only_formats(&PLAIN_FORMATS, |format| format != u32::from(CF_LOCALE))
}

fn only_formats(allowed: &[u32], is_content: impl Fn(u32) -> bool) -> bool {
    let mut format = 0;
    let mut content = false;
    // Unknown/private formats (including password-manager hints) stay local.
    for _ in 0..=allowed.len() {
        // SAFETY: the clipboard is held by the caller on this thread.
        unsafe {
            SetLastError(0);
            format = EnumClipboardFormats(format);
            if format == 0 {
                return GetLastError() == 0 && content;
            }
        }
        if !allowed.contains(&format) {
            return false;
        }
        content |= is_content(format);
    }
    false
}

fn plain_unicode(words: &[u16]) -> Option<String> {
    let end = words.iter().position(|word| *word == 0)?;
    let text = String::from_utf16(&words[..end]).ok()?;
    (!text.is_empty() && text.len() <= MAX_TEXT_BYTES).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_extent_stops_before_padding_and_rejects_truncated_chunks() {
        let png = b"\x89PNG\r\n\x1a\n\0\0\0\0IEND\0\0\0\0padding";
        assert_eq!(png_length(png.len(), |index| png[index]), Some(20));
        assert!(png_length(19, |index| png[index]).is_none());
        let oversized = b"\x89PNG\r\n\x1a\n\xff\xff\xff\xffIDAT";
        assert!(png_length(oversized.len(), |index| oversized[index]).is_none());
    }

    #[test]
    fn clipboard_unicode_requires_termination_and_bounds_utf8() {
        assert_eq!(plain_unicode(&[0x5c4f, 0, 0xffff]).as_deref(), Some("屏"));
        assert!(plain_unicode(&[65]).is_none());
        assert!(plain_unicode(&[0]).is_none());
        assert!(plain_unicode(&[0xd800, 0]).is_none());
        let mut oversized = vec![0x5c4f; MAX_TEXT_BYTES / 3 + 1];
        oversized.push(0);
        assert!(plain_unicode(&oversized).is_none());
    }
}
