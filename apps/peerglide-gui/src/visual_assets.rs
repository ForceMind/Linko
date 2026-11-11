//! Fixed, local presentation assets. No screen capture, network or native input.
use eframe::egui::{self, ColorImage, Rect, TextureHandle};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DeviceKind {
    Laptop,
    Desktop,
    Unknown,
}

pub(super) fn local_device_kind() -> DeviceKind {
    super::device_hardware::local_device_kind()
}

struct DecodedDevice {
    image: ColorImage,
    uv: Rect,
    aspect: f32,
}

#[derive(Clone)]
pub(super) struct DeviceArt {
    pub texture: TextureHandle,
    pub uv: Rect,
    pub aspect: f32,
}

fn decode(bytes: &[u8]) -> Option<DecodedDevice> {
    let icon = eframe::icon_data::from_png_bytes(bytes).ok()?;
    let width = icon.width as usize;
    let height = icon.height as usize;
    if width == 0 || height == 0 || width > 2048 || height > 2048 {
        return None;
    }
    let mut minimum = [width, height];
    let mut maximum = [0, 0];
    for (index, pixel) in icon.rgba.as_chunks::<4>().0.iter().enumerate() {
        if pixel[3] >= 160 {
            let x = index % width;
            let y = index / width;
            minimum[0] = minimum[0].min(x);
            minimum[1] = minimum[1].min(y);
            maximum[0] = maximum[0].max(x + 1);
            maximum[1] = maximum[1].max(y + 1);
        }
    }
    if maximum[0] <= minimum[0] || maximum[1] <= minimum[1] {
        return None;
    }
    let uv = Rect::from_min_max(
        egui::pos2(
            minimum[0] as f32 / width as f32,
            minimum[1] as f32 / height as f32,
        ),
        egui::pos2(
            maximum[0] as f32 / width as f32,
            maximum[1] as f32 / height as f32,
        ),
    );
    Some(DecodedDevice {
        image: ColorImage::from_rgba_unmultiplied([width, height], &icon.rgba),
        uv,
        aspect: (maximum[0] - minimum[0]) as f32 / (maximum[1] - minimum[1]) as f32,
    })
}

fn asset(kind: DeviceKind) -> Option<(&'static str, &'static [u8])> {
    match kind {
        DeviceKind::Laptop => Some((
            "linko-laptop-device-art",
            include_bytes!("../assets/devices/local-laptop.png"),
        )),
        DeviceKind::Desktop => Some((
            "linko-desktop-device-art",
            include_bytes!("../assets/devices/desktop-monitor.png"),
        )),
        DeviceKind::Unknown => None,
    }
}

pub(super) fn device_art(ctx: &egui::Context, kind: DeviceKind) -> Option<DeviceArt> {
    let (name, bytes) = asset(kind)?;
    let key = egui::Id::unique(name);
    if let Some(cached) = ctx.data(|memory| memory.get_temp::<Option<DeviceArt>>(key)) {
        return cached;
    }
    // Texture upload consumes the decoded pixels. Only the handle and artwork
    // geometry remain in application state, once per egui context.
    let art = decode(bytes).map(|decoded| DeviceArt {
        texture: ctx.load_texture(name, decoded.image, egui::TextureOptions::LINEAR),
        uv: decoded.uv,
        aspect: decoded.aspect,
    });
    ctx.data_mut(|memory| memory.insert_temp(key, art.clone()));
    art
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_device_assets_decode_to_bounded_transparent_images() {
        for kind in [DeviceKind::Laptop, DeviceKind::Desktop] {
            let (_, bytes) = asset(kind).unwrap();
            let source = decode(bytes).expect("bundled device image must decode");
            assert!(source.image.width() <= 2048 && source.image.height() <= 2048);
            assert!(source.image.pixels.iter().any(|pixel| pixel.a() == 0));
            assert!(source.aspect.is_finite() && source.aspect > 0.5 && source.aspect < 3.0);
            assert!(source.uv.is_positive());
            assert!(
                Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)).contains_rect(source.uv)
            );
        }
    }

    #[test]
    fn repeated_device_reads_reuse_context_texture_uploads() {
        let ctx = crate::tests::ui_context();
        let mut previous = None;
        for _ in 0..3 {
            let output = ctx.run_ui(egui::RawInput::default(), |ui| {
                let first = device_art(ui.ctx(), DeviceKind::Laptop).unwrap();
                let second = device_art(ui.ctx(), DeviceKind::Laptop).unwrap();
                let desktop = device_art(ui.ctx(), DeviceKind::Desktop).unwrap();
                assert_eq!(first.texture.id(), second.texture.id());
                assert_ne!(first.texture.id(), desktop.texture.id());
                assert!(device_art(ui.ctx(), DeviceKind::Unknown).is_none());
                if let Some(id) = previous {
                    assert_eq!(first.texture.id(), id);
                }
                previous = Some(first.texture.id());
            });
            output.drop_without_applying_deltas();
        }
    }

    #[test]
    fn unknown_type_never_decodes_or_uploads_a_desktop_or_laptop_asset() {
        assert!(asset(DeviceKind::Unknown).is_none());
        assert!(device_art(&crate::tests::ui_context(), DeviceKind::Unknown).is_none());
    }
}
