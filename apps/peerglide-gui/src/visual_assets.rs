//! Fixed, local presentation assets. No screen capture, network or native input.
use eframe::egui::{self, ColorImage, Rect, TextureHandle};
use std::sync::{Arc, OnceLock};

#[derive(Clone, Copy)]
pub(super) enum DeviceKind {
    Local,
    Peer,
}

struct DecodedDevice {
    image: Arc<ColorImage>,
    uv: Rect,
    aspect: f32,
}

static LAPTOP: OnceLock<Option<DecodedDevice>> = OnceLock::new();
static MONITOR: OnceLock<Option<DecodedDevice>> = OnceLock::new();

fn decode(bytes: &[u8]) -> Option<DecodedDevice> {
    let icon = eframe::icon_data::from_png_bytes(bytes).ok()?;
    let width = icon.width as usize;
    let height = icon.height as usize;
    if width == 0 || height == 0 || width > 2048 || height > 2048 {
        return None;
    }
    let mut minimum = [width, height];
    let mut maximum = [0, 0];
    for (index, pixel) in icon.rgba.chunks_exact(4).enumerate() {
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
        egui::pos2(minimum[0] as f32 / width as f32, minimum[1] as f32 / height as f32),
        egui::pos2(maximum[0] as f32 / width as f32, maximum[1] as f32 / height as f32),
    );
    Some(DecodedDevice {
        image: Arc::new(ColorImage::from_rgba_unmultiplied([width, height], &icon.rgba)),
        uv,
        aspect: (maximum[0] - minimum[0]) as f32 / (maximum[1] - minimum[1]) as f32,
    })
}

fn decoded(kind: DeviceKind) -> Option<&'static DecodedDevice> {
    match kind {
        DeviceKind::Local => LAPTOP.get_or_init(|| decode(include_bytes!("../assets/devices/local-laptop.png"))),
        DeviceKind::Peer => MONITOR.get_or_init(|| decode(include_bytes!("../assets/devices/desktop-monitor.png"))),
    }
    .as_ref()
}

pub(super) fn device_texture(ctx: &egui::Context, kind: DeviceKind) -> Option<TextureHandle> {
    let name = match kind {
        DeviceKind::Local => "linko-local-device-art",
        DeviceKind::Peer => "linko-peer-device-art",
    };
    let key = egui::Id::new(name);
    if let Some(texture) = ctx.data(|memory| memory.get_temp::<TextureHandle>(key)) {
        return Some(texture);
    }
    // Decode once per process, then upload once per context. Do not hold the
    // context data lock while loading a texture or re-decode on every repaint.
    let source = decoded(kind)?;
    let texture = ctx.load_texture(name, (*source.image).clone(), egui::TextureOptions::LINEAR);
    ctx.data_mut(|memory| memory.insert_temp(key, texture.clone()));
    Some(texture)
}

pub(super) fn device_uv(kind: DeviceKind) -> Rect {
    decoded(kind).map_or(Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)), |source| source.uv)
}

pub(super) fn device_aspect(kind: DeviceKind) -> f32 {
    decoded(kind).map_or(1.0, |source| source.aspect)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_device_assets_decode_to_bounded_transparent_images() {
        for kind in [DeviceKind::Local, DeviceKind::Peer] {
            let source = decoded(kind).expect("bundled device image must decode");
            assert!(source.image.width() <= 2048 && source.image.height() <= 2048);
            assert!(source.image.pixels.iter().any(|pixel| pixel.a() == 0));
            assert!(source.aspect.is_finite() && source.aspect > 0.5 && source.aspect < 3.0);
            assert!(source.uv.is_positive());
            assert!(Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)).contains_rect(source.uv));
        }
    }

    #[test]
    fn repeated_device_reads_reuse_context_texture_uploads() {
        let ctx = crate::tests::ui_context();
        let mut previous = None;
        for _ in 0..3 {
            let output = ctx.run_ui(egui::RawInput::default(), |ui| {
                let first = device_texture(ui.ctx(), DeviceKind::Local).unwrap();
                let second = device_texture(ui.ctx(), DeviceKind::Local).unwrap();
                let peer = device_texture(ui.ctx(), DeviceKind::Peer).unwrap();
                assert_eq!(first.id(), second.id());
                assert_ne!(first.id(), peer.id());
                if let Some(id) = previous {
                    assert_eq!(first.id(), id);
                }
                previous = Some(first.id());
            });
            output.drop_without_applying_deltas();
        }
    }
}
