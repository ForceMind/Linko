//! Explicit, local text clipboard operations. The data owner supplies consent.
use peerglide_transfer::clipboard::Origin;

#[path = "clipboard_windows.rs"]
mod native;
pub use native::NativeClipboard;

/// An explicitly shareable snapshot. No Debug or serialization of its contents.
pub struct TextSnapshot {
    pub revision: u64,
    pub text: String,
    pub origin: Origin,
}
