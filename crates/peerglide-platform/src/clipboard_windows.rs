//! Plain-text Win32 clipboard operations on the creating UI thread.
use super::TextSnapshot;
use anyhow::{ensure, Context, Result};
use peerglide_transfer::clipboard::{Backend, Origin, WriteResult, MAX_TEXT_BYTES};
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
                GetClipboardSequenceNumber, OpenClipboard, SetClipboardData,
            },
            LibraryLoader::GetModuleHandleW,
            Memory::{GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE},
            Ole::{CF_LOCALE, CF_OEMTEXT, CF_TEXT, CF_UNICODETEXT},
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
            origin: if self.imported_revision == Some(revision) {
                Origin::Imported
            } else {
                Origin::Local
            },
        }))
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
        let Some(mut payload) = OwnedMemory::encode(text) else {
            return WriteResult::Failed;
        };
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
        if unsafe { SetClipboardData(UNICODE_FORMAT, payload.0) }.is_null() {
            return WriteResult::UnknownOutcome;
        }
        payload.0 = null_mut();
        self.imported_revision = sequence();
        WriteResult::Submitted
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
        let words: Vec<_> = text.encode_utf16().chain(std::iter::once(0)).collect();
        let bytes = words.len().checked_mul(size_of::<u16>())?;
        // SAFETY: bounded native allocation, owned until transfer to the OS.
        let memory = Self(unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes) });
        if memory.0.is_null() {
            return None;
        }
        let locked = LockedMemory::lock(memory.0)?;
        // SAFETY: locked memory has exactly the requested room for the words.
        unsafe {
            std::ptr::copy_nonoverlapping(words.as_ptr(), locked.pointer.cast(), words.len());
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

fn plain_text_formats() -> bool {
    let mut format = 0;
    let mut text = false;
    // At most four known formats plus the end marker. Unknown/private formats
    // (including password manager hints) reject without reading text.
    for _ in 0..=PLAIN_FORMATS.len() {
        // SAFETY: the clipboard is held by the caller on this thread.
        unsafe {
            SetLastError(0);
            format = EnumClipboardFormats(format);
            if format == 0 {
                return GetLastError() == 0 && text;
            }
        }
        if !PLAIN_FORMATS.contains(&format) {
            return false;
        }
        text |= format != u32::from(CF_LOCALE);
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
