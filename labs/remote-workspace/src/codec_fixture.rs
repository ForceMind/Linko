//! Pixel conversion for the bounded native SDR codec fixture.

pub const WIDTH: usize = 128;
pub const HEIGHT: usize = 96;
pub const NV12_BYTES: usize = WIDTH * HEIGHT * 3 / 2;

pub fn nv12_rgba(nv12: &[u8; NV12_BYTES]) -> Vec<u8> {
    // The native fixture uses the limited-range BT.601 matrix.
    let (luma, chroma) = nv12.split_at(WIDTH * HEIGHT);
    let mut rgba = Vec::with_capacity(luma.len() * 4);
    let channel = |value: i32| ((value + 128) >> 8).clamp(0, 255) as u8;
    for (index, &y) in luma.iter().enumerate() {
        let row = index / WIDTH;
        let column = index % WIDTH;
        let uv = row / 2 * WIDTH + (column & !1);
        let c = 298 * (i32::from(y) - 16);
        let d = i32::from(chroma[uv]) - 128;
        let e = i32::from(chroma[uv + 1]) - 128;
        rgba.extend_from_slice(&[
            channel(c + 409 * e),
            channel(c - 100 * d - 208 * e),
            channel(c + 516 * d),
            255,
        ]);
    }
    rgba
}
