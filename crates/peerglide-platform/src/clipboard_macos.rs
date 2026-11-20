//! Explicit AppKit pasteboard access on the main thread, with change-count checks.
use super::{ImageSnapshot, TextSnapshot};
use anyhow::{Context, Result};
use objc2::{rc::Retained, MainThreadMarker};
use objc2_app_kit::{NSPasteboard, NSPasteboardTypePNG, NSPasteboardTypeString};
use objc2_foundation::{NSData, NSString};
use peerglide_transfer::{
    clipboard::{Backend, Origin, WriteResult, MAX_TEXT_BYTES},
    clipboard_image::{Image, ImageBackend, MAX_PNG_BYTES},
};

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
        if !self.only_formats(ordinary_text_type) {
            return Ok(None);
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
            origin: self.origin(revision),
        }))
    }

    pub fn image_snapshot(&self) -> Result<Option<ImageSnapshot>> {
        let Some(revision) = self.revision() else {
            return Ok(None);
        };
        if !self
            .pasteboard
            .pasteboardItems()
            .is_some_and(|items| items.count() == 1)
            || !self.only_formats(ordinary_image_type)
        {
            return Ok(None);
        }
        // SAFETY: the AppKit constant is a process-lifetime NSString.
        let kind = unsafe { NSPasteboardTypePNG };
        let Some(data) = self.pasteboard.dataForType(kind) else {
            return Ok(None);
        };
        if !(8..=MAX_PNG_BYTES).contains(&data.len()) {
            return Ok(None);
        }
        let png = data.to_vec();
        if self.revision() != Some(revision) {
            return Ok(None);
        }
        Ok(Some(ImageSnapshot {
            revision,
            png,
            origin: self.origin(revision),
        }))
    }

    fn only_formats(&self, allowed: impl Fn(&str) -> bool) -> bool {
        let Some(types) = self.pasteboard.types() else {
            return false;
        };
        if !(1..=MAX_TYPES).contains(&types.count()) {
            return false;
        }
        (0..types.count()).all(|index| {
            let kind = types.objectAtIndex(index);
            kind.length() <= MAX_TYPE_UNITS && allowed(&kind.to_string())
        })
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
        submit: impl FnOnce(&NSPasteboard) -> bool,
    ) -> WriteResult {
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
        if !submit(&self.pasteboard) || self.revision() != cleared {
            return WriteResult::UnknownOutcome;
        }
        self.imported_revision = cleared;
        WriteResult::Submitted
    }
}

impl Backend for NativeClipboard {
    fn compare_and_write(&mut self, expected_revision: u64, text: &str) -> WriteResult {
        if !valid_text(text) {
            return WriteResult::Failed;
        }
        let text = NSString::from_str(text);
        // SAFETY: the AppKit constant is a process-lifetime NSString.
        let kind = unsafe { NSPasteboardTypeString };
        self.write_payload(expected_revision, |pasteboard| {
            pasteboard.setString_forType(&text, kind)
        })
    }
}

impl ImageBackend for NativeClipboard {
    fn compare_and_write_image(&mut self, expected_revision: u64, image: &Image) -> WriteResult {
        let data = NSData::with_bytes(image.as_png());
        // SAFETY: the AppKit constant is a process-lifetime NSString.
        let kind = unsafe { NSPasteboardTypePNG };
        self.write_payload(expected_revision, |pasteboard| {
            pasteboard.setData_forType(Some(&data), kind)
        })
    }
}

fn ordinary_image_type(kind: &str) -> bool {
    matches!(kind, "public.png" | "public.tiff")
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
    fn image_whitelist_allows_png_and_native_preview_without_private_formats() {
        assert!(ordinary_image_type("public.png"));
        assert!(ordinary_image_type("public.tiff"));
        assert!(!ordinary_image_type("public.file-url"));
        assert!(!ordinary_image_type("org.nspasteboard.ConcealedType"));
        assert!(!ordinary_image_type("public.utf8-plain-text"));
    }

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
