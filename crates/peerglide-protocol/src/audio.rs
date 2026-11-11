//! Fixed PCM datagram for a future independently authenticated audio session.
//! No input messages, audio grant, device selection or playback are encoded here.
use std::mem::size_of;

pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: usize = 2;
/// Five milliseconds of interleaved stereo samples.
pub const SAMPLES: usize = 240 * CHANNELS;
const MAGIC: &[u8; 4] = b"PGA1";
const HEADER: usize = MAGIC.len() + size_of::<u64>();
pub const FRAME_BYTES: usize = HEADER + SAMPLES * size_of::<i16>();

/// Audio samples stay out of Debug, errors and logs.
pub struct Frame {
    pub sequence: u64,
    pub samples: [i16; SAMPLES],
}

impl Frame {
    pub fn encode(&self) -> Option<[u8; FRAME_BYTES]> {
        if self.sequence == 0 {
            return None;
        }
        let mut bytes = [0; FRAME_BYTES];
        bytes[..MAGIC.len()].copy_from_slice(MAGIC);
        bytes[MAGIC.len()..HEADER].copy_from_slice(&self.sequence.to_le_bytes());
        let (words, _) = bytes[HEADER..].as_chunks_mut::<2>();
        for (word, sample) in words.iter_mut().zip(self.samples.iter()) {
            *word = sample.to_le_bytes();
        }
        Some(bytes)
    }

    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != FRAME_BYTES || !bytes.starts_with(MAGIC) {
            return None;
        }
        let sequence = u64::from_le_bytes(bytes[MAGIC.len()..HEADER].try_into().ok()?);
        if sequence == 0 {
            return None;
        }
        let mut samples = [0; SAMPLES];
        let (words, _) = bytes[HEADER..].as_chunks::<2>();
        for (sample, word) in samples.iter_mut().zip(words) {
            *sample = i16::from_le_bytes(*word);
        }
        Some(Self { sequence, samples })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_pcm_frame_preserves_signed_samples_and_sequence() {
        let mut samples = [0; SAMPLES];
        samples[0] = i16::MIN;
        samples[1] = i16::MAX;
        samples[SAMPLES - 1] = -297;
        let frame = Frame {
            sequence: 48,
            samples,
        };
        let encoded = frame.encode().unwrap();
        let decoded = Frame::decode(&encoded).unwrap();
        assert_eq!(encoded.len(), 972);
        assert_eq!(decoded.sequence, 48);
        assert_eq!(decoded.samples, frame.samples);
    }

    #[test]
    fn wrong_size_magic_and_zero_sequence_reject() {
        let mut frame = Frame {
            sequence: 0,
            samples: [0; SAMPLES],
        };
        assert!(frame.encode().is_none());
        frame.sequence = 1;
        let mut bytes = frame.encode().unwrap();
        assert!(Frame::decode(&bytes[..FRAME_BYTES - 1]).is_none());
        assert!(Frame::decode(&[0; FRAME_BYTES + 1]).is_none());
        bytes[MAGIC.len()..HEADER].fill(0);
        assert!(Frame::decode(&bytes).is_none());
        let mut bytes = frame.encode().unwrap();
        bytes[0] ^= 0xff;
        assert!(Frame::decode(&bytes).is_none());
    }
}
