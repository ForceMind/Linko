//! Bounded framing for a future separately negotiated authenticated data stream.
//! Never route these bytes to the v5 input stream. No authentication is performed.
use crate::{clipboard::MAX_TEXT_BYTES, Manifest, CHUNK_BYTES, MAX_FILE_BYTES, MAX_MANIFEST_BYTES};
use std::io::{self, Read, Write};

const MAGIC: [u8; 4] = *b"PGD1";
const HEADER: usize = 12;

/// No Debug: variants can carry private clipboard text or file contents/names.
/// Transfer IDs are session-local routing keys, never authentication or consent.
pub enum Frame {
    Text {
        sequence: u64,
        text: String,
    },
    Offer {
        id: [u8; 16],
        manifest: Manifest,
    },
    Chunk {
        id: [u8; 16],
        index: u32,
        bytes: Vec<u8>,
    },
    Cancel {
        id: [u8; 16],
    },
}

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid data frame")
}

fn valid_length(kind: u8, len: usize) -> bool {
    match kind {
        1 => (9..=MAX_TEXT_BYTES + 8).contains(&len),
        2 => (17..=MAX_MANIFEST_BYTES + 16).contains(&len),
        3 => (21..=CHUNK_BYTES + 20).contains(&len),
        4 => len == 16,
        _ => false,
    }
}

fn valid_text(text: &str) -> bool {
    !text.is_empty() && text.len() <= MAX_TEXT_BYTES && !text.contains('\0')
}
fn valid_index(index: u32) -> bool {
    (index as u64) < MAX_FILE_BYTES.div_ceil(CHUNK_BYTES as u64)
}

impl Frame {
    /// Header and body are validated before any untrusted body allocation.
    /// Caller must also bound concurrent streams, deadlines and total session bytes.
    /// Read failures are terminal for this stream; never scan for a new magic marker.
    pub fn read(mut reader: impl Read) -> io::Result<Self> {
        let mut header = [0u8; HEADER];
        reader.read_exact(&mut header)?;
        if header[..4] != MAGIC || header[5..8] != [0, 0, 0] {
            return Err(invalid());
        }
        let kind = header[4];
        let len = u32::from_le_bytes(header[8..12].try_into().map_err(|_| invalid())?) as usize;
        if !valid_length(kind, len) {
            return Err(invalid());
        }
        let mut body = vec![0; len];
        reader.read_exact(&mut body)?;
        match kind {
            1 => {
                let sequence = u64::from_le_bytes(body[..8].try_into().map_err(|_| invalid())?);
                let text = std::str::from_utf8(&body[8..]).map_err(|_| invalid())?;
                if sequence == 0 || !valid_text(text) {
                    return Err(invalid());
                }
                Ok(Self::Text {
                    sequence,
                    text: text.to_owned(),
                })
            }
            2 => Ok(Self::Offer {
                id: body[..16].try_into().map_err(|_| invalid())?,
                manifest: Manifest::decode(&body[16..])?,
            }),
            3 => {
                let index = u32::from_le_bytes(body[16..20].try_into().map_err(|_| invalid())?);
                if !valid_index(index) {
                    return Err(invalid());
                }
                Ok(Self::Chunk {
                    id: body[..16].try_into().map_err(|_| invalid())?,
                    index,
                    bytes: body[20..].to_vec(),
                })
            }
            4 => Ok(Self::Cancel {
                id: body.try_into().map_err(|_| invalid())?,
            }),
            _ => Err(invalid()),
        }
    }

    /// Validates before writing even the header. I/O failures may leave partial
    /// bytes; close the stream and report uncertainty, do not replay blindly.
    pub fn write(&self, mut writer: impl Write) -> io::Result<()> {
        let (kind, body) = match self {
            Self::Text { sequence, text } => {
                if *sequence == 0 || !valid_text(text) {
                    return Err(invalid());
                }
                let mut body = Vec::with_capacity(8 + text.len());
                body.extend_from_slice(&sequence.to_le_bytes());
                body.extend_from_slice(text.as_bytes());
                (1, body)
            }
            Self::Offer { id, manifest } => {
                let encoded = manifest.encode()?;
                let mut body = Vec::with_capacity(16 + encoded.len());
                body.extend_from_slice(id);
                body.extend_from_slice(&encoded);
                (2, body)
            }
            Self::Chunk { id, index, bytes } => {
                if !valid_index(*index) || bytes.is_empty() || bytes.len() > CHUNK_BYTES {
                    return Err(invalid());
                }
                let mut body = Vec::with_capacity(20 + bytes.len());
                body.extend_from_slice(id);
                body.extend_from_slice(&index.to_le_bytes());
                body.extend_from_slice(bytes);
                (3, body)
            }
            Self::Cancel { id } => (4, id.to_vec()),
        };
        if !valid_length(kind, body.len()) {
            return Err(invalid());
        }
        let mut header = [0; HEADER];
        header[..4].copy_from_slice(&MAGIC);
        header[4] = kind;
        header[8..12].copy_from_slice(&(body.len() as u32).to_le_bytes());
        writer.write_all(&header)?;
        writer.write_all(&body)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn encoded(frame: &Frame) -> Vec<u8> {
        let mut bytes = Vec::new();
        frame.write(&mut bytes).unwrap();
        bytes
    }

    struct Fragmented(Cursor<Vec<u8>>);
    impl Read for Fragmented {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            let len = bytes.len().min(3);
            self.0.read(&mut bytes[..len])
        }
    }

    #[test]
    fn fragmented_unicode_and_back_to_back_frames() {
        let mut bytes = encoded(&Frame::Text {
            sequence: 5,
            text: "屏跃 🌎".into(),
        });
        bytes.extend(encoded(&Frame::Cancel { id: [9; 16] }));
        let mut reader = Fragmented(Cursor::new(bytes));
        match Frame::read(&mut reader).unwrap() {
            Frame::Text { sequence, text } => {
                assert_eq!(sequence, 5);
                assert_eq!(text, "屏跃 🌎");
            }
            _ => panic!("wrong frame"),
        }
        assert!(matches!(
            Frame::read(&mut reader).unwrap(),
            Frame::Cancel { id } if id == [9; 16]
        ));
    }

    #[test]
    fn invalid_header_rejects_before_reading_payload() {
        for (kind, len) in [(1, u32::MAX), (2, u32::MAX), (3, 20), (4, 17), (99, 16)] {
            let mut header = [0; HEADER];
            header[..4].copy_from_slice(&MAGIC);
            header[4] = kind;
            header[8..12].copy_from_slice(&len.to_le_bytes());
            let mut reader = Cursor::new(header);
            assert_eq!(
                Frame::read(&mut reader).err().unwrap().kind(),
                io::ErrorKind::InvalidData
            );
            assert_eq!(reader.position(), HEADER as u64);
        }
        let mut flags = encoded(&Frame::Cancel { id: [1; 16] });
        flags[5] = 1;
        assert!(Frame::read(Cursor::new(flags)).is_err());
        assert!(Frame::read(Cursor::new(b"PGD2")).is_err());
    }

    #[test]
    fn truncation_invalid_text_and_invalid_chunks_fail_closed() {
        let bytes = encoded(&Frame::Text {
            sequence: 1,
            text: "test".into(),
        });
        for end in 0..bytes.len() {
            assert!(Frame::read(Cursor::new(&bytes[..end])).is_err());
        }
        for frame in [
            Frame::Text {
                sequence: 0,
                text: "x".into(),
            },
            Frame::Text {
                sequence: 1,
                text: "\0".into(),
            },
            Frame::Chunk {
                id: [1; 16],
                index: u32::MAX,
                bytes: vec![1],
            },
            Frame::Chunk {
                id: [1; 16],
                index: 0,
                bytes: vec![0; CHUNK_BYTES + 1],
            },
        ] {
            let mut output = Vec::new();
            assert!(frame.write(&mut output).is_err());
            assert!(output.is_empty());
        }
        let mut invalid_utf8 = bytes;
        *invalid_utf8.last_mut().unwrap() = 0xff;
        assert!(Frame::read(Cursor::new(invalid_utf8)).is_err());
    }

    #[test]
    fn maximum_chunk_roundtrip_preserves_bytes() {
        let input = Frame::Chunk {
            id: [7; 16],
            index: 32767,
            bytes: vec![19; CHUNK_BYTES],
        };
        match Frame::read(Cursor::new(encoded(&input))).unwrap() {
            Frame::Chunk { id, index, bytes } => {
                assert_eq!(id, [7; 16]);
                assert_eq!(index, 32767);
                assert_eq!(bytes, vec![19; CHUNK_BYTES]);
            }
            _ => panic!("wrong frame"),
        }
    }
}
