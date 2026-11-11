//! Explicit AppKit pasteboard access on the main thread, with change-count checks.
use super::TextSnapshot;
use anyhow::{Context, Result};
use objc2::{rc::Retained, MainThreadMarker};
use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};
use objc2_foundation::NSString;
use peerglide_transfer::clipboard::{Backend, Origin, WriteResult, MAX_TEXT_BYTES};

const MAX_TYPES: usize = 16;
const MAX_TYPE_UNITS: usize = 128;

pub struct NativeClipboard {
    pasteboard: Retained<NSPasteboard>,
    imported_revision: Option<u64>,
    _main: MainThreadMarker,
}

impl NativeClipboard {
    pub fn new() -> Result<Self> {
        let main = MainThreadMarker::new().context("clipboard requires the AppKit thread")?;
        Ok(Self {
            pasteboard: NSPasteboard::generalPasteboard(),
            imported_revision: None,
            _main: main,
        })
    }

    pub fn revision(&self) -> Option<u64> {
        u64::try_from(self.pasteboard.changeCount()).ok()
    }

    pub fn snapshot(&self) -> Result<Option<TextSnapshot>> {
        let Some(revision) = self.revision() else {
            return Ok(None);
        };
        let Some(types) = self.pasteboard.types() else {
            return Ok(None);
        };
        if types.count() == 0 || types.count() > MAX_TYPES {
            return Ok(None);
        }
        for index in 0..types.count() {
            let kind = types.objectAtIndex(index);
            if kind.length() > MAX_TYPE_UNITS || !ordinary_text_type(&kind.to_string()) {
                return Ok(None);
            }
        }
        // SAFETY: the AppKit constant is a process-lifetime NSString.
        let kind = unsafe { NSPasteboardTypeString };
        let Some(text) = self.pasteboard.stringForType(kind) else {
            return Ok(None);
        };
        if text.length() == 0 || text.length() > MAX_TEXT_BYTES {
            return Ok(None);
        }
        let text = text.to_string();
        if !valid_text(&text) || self.revision() != Some(revision) {
            return Ok(None);
        }
        Ok(Some(TextSnapshot {
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

impl Backend for NativeClipboard {
    fn compare_and_write(&mut self, expected_revision: u64, text: &str) -> WriteResult {
        if !valid_text(text) {
            return WriteResult::Failed;
        }
        let text = NSString::from_str(text);
        let Some(revision) = self.revision() else {
            return WriteResult::Failed;
        };
        if revision != expected_revision {
            return WriteResult::Changed;
        }
        // AppKit has no atomic compare-and-write. Another app can replace the
        // pasteboard between these calls. Recheck before submitting and report
        // uncertainty if the count no longer proves our ownership after clear.
        let cleared = u64::try_from(self.pasteboard.clearContents()).ok();
        self.imported_revision = None;
        if cleared.is_none() || cleared == Some(revision) || self.revision() != cleared {
            return WriteResult::UnknownOutcome;
        }
        // SAFETY: the AppKit constant is a process-lifetime NSString.
        let kind = unsafe { NSPasteboardTypeString };
        if !self.pasteboard.setString_forType(&text, kind) || self.revision() != cleared {
            return WriteResult::UnknownOutcome;
        }
        self.imported_revision = cleared;
        WriteResult::Submitted
    }
}

fn ordinary_text_type(kind: &str) -> bool {
    matches!(
        kind,
        "public.utf8-plain-text"
            | "public.utf16-plain-text"
            | "public.utf16-external-plain-text"
            | "public.plain-text"
            | "NSStringPboardType"
    )
}

fn valid_text(text: &str) -> bool {
    !text.is_empty() && text.len() <= MAX_TEXT_BYTES && !text.contains('\0')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clipboard_whitelist_keeps_unrecognized_and_concealed_formats_local() {
        assert!(ordinary_text_type("public.utf8-plain-text"));
        assert!(ordinary_text_type("NSStringPboardType"));
        assert!(!ordinary_text_type("org.nspasteboard.ConcealedType"));
        assert!(!ordinary_text_type("public.html"));
        assert!(!ordinary_text_type("com.example.private"));
        assert!(valid_text("屏跃"));
        assert!(!valid_text(""));
        assert!(!valid_text("text\0"));
        assert!(!valid_text(&"x".repeat(MAX_TEXT_BYTES + 1)));
    }
}
