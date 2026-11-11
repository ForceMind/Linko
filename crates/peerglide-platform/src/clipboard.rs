//! Explicit, local clipboard operations. The data owner supplies consent.
use peerglide_transfer::clipboard::Origin;

#[cfg(target_os = "macos")]
#[path = "clipboard_macos.rs"]
mod native;
#[cfg(windows)]
#[path = "clipboard_windows.rs"]
mod native;
pub use native::NativeClipboard;

/// An explicitly shareable snapshot. No Debug or serialization of its contents.
pub struct TextSnapshot {
    pub revision: u64,
    pub text: String,
    pub origin: Origin,
}

/// Raw native PNG snapshot. Normalize it before network transmission.
pub struct ImageSnapshot {
    pub revision: u64,
    pub png: Vec<u8>,
    pub origin: Origin,
}
