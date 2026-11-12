use anyhow::{ensure, Result};
use peerglide_transfer::{clipboard::MAX_TEXT_BYTES, clipboard_image::MAX_PNG_BYTES};
use std::time::Duration;

pub(super) fn validate_text(text: &str) -> Result<()> {
    ensure!(
        (1..=MAX_TEXT_BYTES).contains(&text.len()) && !text.contains('\0'),
        "native drag text outside bounds"
    );
    Ok(())
}

pub(super) fn validate_png(png: &[u8]) -> Result<()> {
    ensure!(
        png.len() <= MAX_PNG_BYTES && png.starts_with(b"\x89PNG\r\n\x1a\n"),
        "native drag PNG outside bounds"
    );
    Ok(())
}

pub(super) fn drag_millis(remaining: Duration) -> Result<u32> {
    let millis = remaining.as_millis();
    ensure!((1..=60_000).contains(&millis), "native drag offer expired");
    Ok(millis as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clipboard_drag_bounds_reject_invalid_formats_and_expired_offers() {
        assert!(validate_text("屏跃").is_ok());
        assert!(validate_text("text\0").is_err());
        assert!(validate_text(&"a".repeat(MAX_TEXT_BYTES + 1)).is_err());
        assert!(validate_png(b"invalid").is_err());
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        png.resize(MAX_PNG_BYTES, 0);
        assert!(validate_png(&png).is_ok());
        png.push(0);
        assert!(validate_png(&png).is_err());
        assert_eq!(drag_millis(Duration::from_secs(60)).unwrap(), 60_000);
        assert!(drag_millis(Duration::from_secs(61)).is_err());
        assert!(drag_millis(Duration::ZERO).is_err());
    }
}
