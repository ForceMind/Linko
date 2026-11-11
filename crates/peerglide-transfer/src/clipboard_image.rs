//! Canonical clipboard PNG. Pixel data is bounded and metadata stays local.
use png::{BitDepth, Decoder, Encoder, Limits, Transformations};
use std::io::{self, Cursor, Write};

pub const MAX_PNG_BYTES: usize = 4 * 1024 * 1024;
const MAX_EDGE: u32 = 2048;
const MAX_PIXELS: usize = MAX_EDGE as usize * MAX_EDGE as usize;
const MAX_DECODED_BYTES: usize = MAX_PIXELS * 4;

/// A single verified frame containing pixels only. No Debug or serialization.
pub struct Image {
    width: u32,
    height: u32,
    png: Vec<u8>,
}

impl Image {
    /// Decode fully within the limits and re-encode to discard private chunks,
    /// EXIF, profiles, text and original container bytes.
    pub fn normalize(input: &[u8]) -> io::Result<Self> {
        if input.is_empty() || input.len() > MAX_PNG_BYTES {
            return Err(invalid("clipboard PNG size outside limits"));
        }
        let limits = Limits {
            bytes: MAX_DECODED_BYTES,
        };
        let mut decoder = Decoder::new_with_limits(Cursor::new(input), limits);
        decoder.set_ignore_text_chunk(true);
        decoder.set_ignore_iccp_chunk(true);
        decoder.set_transformations(Transformations::normalize_to_color8());
        let header = decoder
            .read_header_info()
            .map_err(|_| invalid("invalid clipboard PNG header"))?;
        if !valid_edge(header.width) || !valid_edge(header.height) {
            return Err(invalid("clipboard PNG dimensions outside limits"));
        }
        let mut reader = decoder
            .read_info()
            .map_err(|_| invalid("invalid clipboard PNG metadata"))?;
        if reader.info().animation_control.is_some() || reader.info().frame_control.is_some() {
            return Err(invalid("animated clipboard PNG is unsupported"));
        }
        let size = reader
            .output_buffer_size()
            .filter(|size| *size <= MAX_DECODED_BYTES)
            .ok_or_else(|| invalid("decoded clipboard PNG exceeds limit"))?;
        let mut pixels = vec![0; size];
        let frame = reader
            .next_frame(&mut pixels)
            .map_err(|_| invalid("invalid clipboard PNG pixels"))?;
        reader
            .finish()
            .map_err(|_| invalid("incomplete clipboard PNG"))?;
        if frame.bit_depth != BitDepth::Eight
            || reader.info().animation_control.is_some()
            || reader.info().frame_control.is_some()
        {
            return Err(invalid("unsupported clipboard PNG format"));
        }
        let mut output = BoundedOutput(Vec::new());
        let mut encoder = Encoder::new(&mut output, frame.width, frame.height);
        encoder.set_depth(frame.bit_depth);
        encoder.set_color(frame.color_type);
        let mut writer = encoder
            .write_header()
            .map_err(|_| invalid("clipboard PNG encoding failed"))?;
        let pixels = pixels
            .get(..frame.buffer_size())
            .ok_or_else(|| invalid("decoded clipboard PNG exceeds limit"))?;
        writer
            .write_image_data(pixels)
            .map_err(|_| invalid("clipboard PNG encoding failed"))?;
        writer
            .finish()
            .map_err(|_| invalid("clipboard PNG encoding failed"))?;
        Ok(Self {
            width: frame.width,
            height: frame.height,
            png: output.0,
        })
    }

    pub fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub fn as_png(&self) -> &[u8] {
        &self.png
    }
}

fn valid_edge(edge: u32) -> bool {
    (1..=MAX_EDGE).contains(&edge)
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

struct BoundedOutput(Vec<u8>);

impl Write for BoundedOutput {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_PNG_BYTES - self.0.len() {
            return Err(invalid("encoded clipboard PNG exceeds limit"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use png::ColorType;

    fn fixture(width: u32, height: u32, animated: bool) -> Vec<u8> {
        let mut bytes = Vec::new();
        let mut encoder = Encoder::new(&mut bytes, width, height);
        encoder.set_color(ColorType::Rgba);
        encoder.set_depth(BitDepth::Eight);
        encoder
            .add_text_chunk("Comment".into(), "private metadata".into())
            .unwrap();
        if animated {
            encoder.set_animated(2, 0).unwrap();
        }
        let mut writer = encoder.write_header().unwrap();
        let pixels = vec![31; (width * height * 4) as usize];
        writer.write_image_data(&pixels).unwrap();
        if animated {
            writer.write_image_data(&pixels).unwrap();
        }
        writer.finish().unwrap();
        bytes
    }

    #[test]
    fn preserves_pixels_but_discards_text_metadata() {
        let source = fixture(3, 2, false);
        let image = Image::normalize(&source).unwrap();
        assert_eq!(image.dimensions(), (3, 2));
        let mut reader = Decoder::new(Cursor::new(image.as_png())).read_info().unwrap();
        assert!(reader.info().uncompressed_latin1_text.is_empty());
        let mut pixels = [0; 24];
        reader.next_frame(&mut pixels).unwrap();
        assert_eq!(pixels, [31; 24]);
    }

    #[test]
    fn invalid_large_and_animated_pngs_are_rejected() {
        let source = fixture(3, 2, false);
        assert!(Image::normalize(&source[..source.len() / 2]).is_err());
        assert!(Image::normalize(&fixture(MAX_EDGE + 1, 1, false)).is_err());
        assert!(Image::normalize(&fixture(3, 2, true)).is_err());
        assert!(Image::normalize(&vec![0; MAX_PNG_BYTES + 1]).is_err());
    }
}
