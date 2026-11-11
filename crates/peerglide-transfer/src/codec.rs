//! Bounded framing for a future separately negotiated authenticated data stream.
//! Never route these bytes to the v5 input stream. No authentication is performed.
use crate::{clipboard::MAX_TEXT_BYTES, Manifest, CHUNK_BYTES, MAX_FILE_BYTES, MAX_MANIFEST_BYTES};
use std::io::{self, Read, Write};

const MAGIC: [u8; 4] = *b"PGD1";
pub const HEADER_BYTES: usize = 12;

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
    TextReceived {
        sequence: u64,
    },
    TextResult {
        sequence: u64,
        outcome: TextOutcome,
    },
    FileAccepted {
        id: [u8; 16],
        next_chunk: u32,
    },
    FileResult {
        id: [u8; 16],
        outcome: FileOutcome,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum TextOutcome {
    Submitted = 1,
    Changed = 2,
    Declined = 3,
    Failed = 4,
    Unknown = 5,
}

impl TextOutcome {
    fn parse(byte: u8) -> io::Result<Self> {
        match byte {
            1 => Ok(Self::Submitted),
            2 => Ok(Self::Changed),
            3 => Ok(Self::Declined),
            4 => Ok(Self::Failed),
            5 => Ok(Self::Unknown),
            _ => Err(invalid()),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum FileOutcome {
    Committed = 1,
    Declined = 2,
    Failed = 3,
    Unknown = 4,
}

impl FileOutcome {
    fn parse(byte: u8) -> io::Result<Self> {
        match byte {
            1 => Ok(Self::Committed),
            2 => Ok(Self::Declined),
            3 => Ok(Self::Failed),
            4 => Ok(Self::Unknown),
            _ => Err(invalid()),
        }
    }
}

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid data frame")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Kind {
    Text = 1,
    Offer = 2,
    Chunk = 3,
    Cancel = 4,
    TextReceived = 5,
    TextResult = 6,
    FileAccepted = 7,
    FileResult = 8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    kind: Kind,
    body_bytes: usize,
}

impl Header {
    pub fn parse(bytes: [u8; HEADER_BYTES]) -> io::Result<Self> {
        if bytes[..4] != MAGIC || bytes[5..8] != [0, 0, 0] {
            return Err(invalid());
        }
        let kind = match bytes[4] {
            1 => Kind::Text,
            2 => Kind::Offer,
            3 => Kind::Chunk,
            4 => Kind::Cancel,
            5 => Kind::TextReceived,
            6 => Kind::TextResult,
            7 => Kind::FileAccepted,
            8 => Kind::FileResult,
            _ => return Err(invalid()),
        };
        let body_bytes =
            u32::from_le_bytes(bytes[8..12].try_into().map_err(|_| invalid())?) as usize;
        if !valid_length(kind, body_bytes) {
            return Err(invalid());
        }
        Ok(Self { kind, body_bytes })
    }

    pub fn kind(self) -> Kind {
        self.kind
    }

    pub fn body_bytes(self) -> usize {
        self.body_bytes
    }
}

fn valid_length(kind: Kind, len: usize) -> bool {
    match kind {
        Kind::Text => (9..=MAX_TEXT_BYTES + 8).contains(&len),
        Kind::Offer => (17..=MAX_MANIFEST_BYTES + 16).contains(&len),
        Kind::Chunk => (21..=CHUNK_BYTES + 20).contains(&len),
        Kind::Cancel => len == 16,
        Kind::TextReceived => len == 8,
        Kind::TextResult => len == 9,
        Kind::FileAccepted => len == 20,
        Kind::FileResult => len == 17,
    }
}

fn valid_text(text: &str) -> bool {
    !text.is_empty() && text.len() <= MAX_TEXT_BYTES && !text.contains('\0')
}
const MAX_CHUNKS: u64 = MAX_FILE_BYTES.div_ceil(CHUNK_BYTES as u64);

fn valid_index(index: u32) -> bool {
    u64::from(index) < MAX_CHUNKS
}

impl Frame {
    pub fn kind(&self) -> Kind {
        match self {
            Self::Text { .. } => Kind::Text,
            Self::Offer { .. } => Kind::Offer,
            Self::Chunk { .. } => Kind::Chunk,
            Self::Cancel { .. } => Kind::Cancel,
            Self::TextReceived { .. } => Kind::TextReceived,
            Self::TextResult { .. } => Kind::TextResult,
            Self::FileAccepted { .. } => Kind::FileAccepted,
            Self::FileResult { .. } => Kind::FileResult,
        }
    }

    /// Reads a bounded frame without applying any authorization policy.
    pub fn read(reader: impl Read) -> io::Result<Self> {
        Self::read_checked(reader, |_| Ok(()))
    }

    /// Checks the validated kind/length before allocating or reading the body.
    /// The caller must bind admission to a real authenticated session.
    /// Read/admission failures are terminal; never scan for a new magic marker.
    pub fn read_checked(
        mut reader: impl Read,
        admit: impl FnOnce(Header) -> io::Result<()>,
    ) -> io::Result<Self> {
        let mut bytes = [0u8; HEADER_BYTES];
        reader.read_exact(&mut bytes)?;
        let header = Header::parse(bytes)?;
        admit(header)?;
        let mut body = vec![0; header.body_bytes()];
        reader.read_exact(&mut body)?;
        Self::decode_body(header, &body)
    }

    /// Decode an exact body after an async or blocking reader checks admission.
    pub fn decode_body(header: Header, body: &[u8]) -> io::Result<Self> {
        if body.len() != header.body_bytes() {
            return Err(invalid());
        }
        match header.kind() {
            Kind::Text => {
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
            Kind::Offer => Ok(Self::Offer {
                id: body[..16].try_into().map_err(|_| invalid())?,
                manifest: Manifest::decode(&body[16..])?,
            }),
            Kind::Chunk => {
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
            Kind::Cancel => Ok(Self::Cancel {
                id: body.try_into().map_err(|_| invalid())?,
            }),
            Kind::TextReceived => {
                let sequence = u64::from_le_bytes(body.try_into().map_err(|_| invalid())?);
                if sequence == 0 {
                    return Err(invalid());
                }
                Ok(Self::TextReceived { sequence })
            }
            Kind::TextResult => {
                let sequence = u64::from_le_bytes(body[..8].try_into().map_err(|_| invalid())?);
                if sequence == 0 {
                    return Err(invalid());
                }
                Ok(Self::TextResult {
                    sequence,
                    outcome: TextOutcome::parse(body[8])?,
                })
            }
            Kind::FileAccepted => {
                let next_chunk =
                    u32::from_le_bytes(body[16..20].try_into().map_err(|_| invalid())?);
                if u64::from(next_chunk) > MAX_CHUNKS {
                    return Err(invalid());
                }
                Ok(Self::FileAccepted {
                    id: body[..16].try_into().map_err(|_| invalid())?,
                    next_chunk,
                })
            }
            Kind::FileResult => Ok(Self::FileResult {
                id: body[..16].try_into().map_err(|_| invalid())?,
                outcome: FileOutcome::parse(body[16])?,
            }),
        }
    }

    /// Validates before writing even the header. I/O failures may leave partial
    /// bytes; close the stream and report uncertainty, do not replay blindly.
    pub fn write(&self, mut writer: impl Write) -> io::Result<()> {
        let kind = self.kind();
        let body = match self {
            Self::Text { sequence, text } => {
                if *sequence == 0 || !valid_text(text) {
                    return Err(invalid());
                }
                let mut body = Vec::with_capacity(8 + text.len());
                body.extend_from_slice(&sequence.to_le_bytes());
                body.extend_from_slice(text.as_bytes());
                body
            }
            Self::Offer { id, manifest } => {
                let encoded = manifest.encode()?;
                let mut body = Vec::with_capacity(16 + encoded.len());
                body.extend_from_slice(id);
                body.extend_from_slice(&encoded);
                body
            }
            Self::Chunk { id, index, bytes } => {
                if !valid_index(*index) || bytes.is_empty() || bytes.len() > CHUNK_BYTES {
                    return Err(invalid());
                }
                let mut body = Vec::with_capacity(20 + bytes.len());
                body.extend_from_slice(id);
                body.extend_from_slice(&index.to_le_bytes());
                body.extend_from_slice(bytes);
                body
            }
            Self::Cancel { id } => id.to_vec(),
            Self::TextReceived { sequence } => {
                if *sequence == 0 {
                    return Err(invalid());
                }
                sequence.to_le_bytes().to_vec()
            }
            Self::TextResult { sequence, outcome } => {
                if *sequence == 0 {
                    return Err(invalid());
                }
                let mut body = sequence.to_le_bytes().to_vec();
                body.push(*outcome as u8);
                body
            }
            Self::FileAccepted { id, next_chunk } => {
                if u64::from(*next_chunk) > MAX_CHUNKS {
                    return Err(invalid());
                }
                let mut body = Vec::with_capacity(20);
                body.extend_from_slice(id);
                body.extend_from_slice(&next_chunk.to_le_bytes());
                body
            }
            Self::FileResult { id, outcome } => {
                let mut body = Vec::with_capacity(17);
                body.extend_from_slice(id);
                body.push(*outcome as u8);
                body
            }
        };
        if !valid_length(kind, body.len()) {
            return Err(invalid());
        }
        let mut header = [0; HEADER_BYTES];
        header[..4].copy_from_slice(&MAGIC);
        header[4] = kind as u8;
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

    #[test]
    fn file_responses_have_exact_bounds_and_distinct_meanings() {
        let id = [42; 16];
        let last = MAX_CHUNKS as u32;
        let accepted = Frame::FileAccepted {
            id,
            next_chunk: last,
        };
        let bytes = encoded(&accepted);
        assert_eq!(bytes.len(), HEADER_BYTES + 20);
        assert!(matches!(
            Frame::read(bytes.as_slice()).unwrap(),
            Frame::FileAccepted { id: received, next_chunk } if received == id && next_chunk == last
        ));
        let mut invalid_offset = bytes;
        invalid_offset[HEADER_BYTES + 16..].copy_from_slice(&(last + 1).to_le_bytes());
        assert!(Frame::read(invalid_offset.as_slice()).is_err());
        assert!(Frame::FileAccepted {
            id,
            next_chunk: last + 1,
        }
        .write(Vec::new())
        .is_err());
        let mut declined = encoded(&Frame::FileResult {
            id,
            outcome: FileOutcome::Declined,
        });
        assert_eq!(declined.len(), HEADER_BYTES + 17);
        assert!(matches!(
            Frame::read(declined.as_slice()).unwrap(),
            Frame::FileResult { id: received, outcome: FileOutcome::Declined } if received == id
        ));
        declined[HEADER_BYTES + 16] = 0xff;
        assert!(Frame::read(declined.as_slice()).is_err());
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
            let mut header = [0; HEADER_BYTES];
            header[..4].copy_from_slice(&MAGIC);
            header[4] = kind;
            header[8..12].copy_from_slice(&len.to_le_bytes());
            let mut reader = Cursor::new(header);
            assert_eq!(
                Frame::read(&mut reader).err().unwrap().kind(),
                io::ErrorKind::InvalidData
            );
            assert_eq!(reader.position(), HEADER_BYTES as u64);
        }
        let mut flags = encoded(&Frame::Cancel { id: [1; 16] });
        flags[5] = 1;
        assert!(Frame::read(Cursor::new(flags)).is_err());
        assert!(Frame::read(Cursor::new(b"PGD2")).is_err());
    }

    #[test]
    fn rejected_admission_does_not_read_a_valid_body() {
        let payload = encoded(&Frame::Text {
            sequence: 1,
            text: "屏跃".into(),
        });
        let mut reader = Cursor::new(payload);
        let result = Frame::read_checked(&mut reader, |header| {
            assert_eq!(header.kind(), Kind::Text);
            assert_eq!(header.body_bytes(), 8 + "屏跃".len());
            Err(io::ErrorKind::PermissionDenied.into())
        });
        assert_eq!(
            result.err().unwrap().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(reader.position(), HEADER_BYTES as u64);
    }

    #[test]
    fn externally_read_body_must_match_the_validated_header_length() {
        let payload = encoded(&Frame::Cancel { id: [7; 16] });
        let bytes = payload[..HEADER_BYTES].try_into().unwrap();
        let header = Header::parse(bytes).unwrap();
        assert!(Frame::decode_body(header, &[7; 15]).is_err());
        assert!(Frame::decode_body(header, &[7; 17]).is_err());
        assert!(matches!(
            Frame::decode_body(header, &[7; 16]).unwrap(),
            Frame::Cancel { id } if id == [7; 16]
        ));
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
