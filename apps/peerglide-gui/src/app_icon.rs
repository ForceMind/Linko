//! Native window/taskbar icon, separate from the in-window brand mark.
use eframe::egui::IconData;

#[cfg(target_os = "macos")]
const PNG: &[u8] = include_bytes!("../assets/brand/app-icon-macos-256.png");
#[cfg(not(target_os = "macos"))]
const PNG: &[u8] = include_bytes!("../assets/brand/app-icon-256.png");

pub(super) fn icon() -> IconData {
    eframe::icon_data::from_png_bytes(PNG).expect("bundled Linko window icon must decode")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_native_icon_is_rgba_with_visible_and_transparent_pixels() {
        let icon = icon();
        assert_eq!([icon.width, icon.height], [256, 256]);
        assert_eq!(icon.rgba.len(), 256 * 256 * 4);
        assert!(icon.rgba.iter().skip(3).step_by(4).any(|alpha| *alpha == 0));
        assert!(icon
            .rgba
            .iter()
            .skip(3)
            .step_by(4)
            .any(|alpha| *alpha == 255));
    }
}
