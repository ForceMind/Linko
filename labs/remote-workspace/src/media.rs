//! Bounded RAW-RGBA reference framing and a one-frame presentation mailbox.
//! Laboratory format, NOT H.264, not PeerGlide v5, and not transport security.
//! Requires an authenticated, independently authorized channel binding. Production
//! inter-frame codecs require reference/keyframe management; do not apply this
//! raw-frame dropping policy to compressed delta frames.
use std::io::Read;

use crate::geometry::PixelSize;
use crate::session::{FrameStamp, SessionGate};
use crate::{advance, Error, SurfaceKey};

pub const HEADER_BYTES: usize = 76;
pub const MAX_FRAME_BYTES: u64 = 16 * 1024 * 1024;
pub const MAX_QUEUE_AGE_MS: u64 = 2_000;
const MAGIC: &[u8; 4] = b"LWR0";
const RAW_VERSION: u16 = 2;

/// No Debug implementation: pixel contents must not leak into diagnostic logs.
pub struct RawFrame {
    stamp: FrameStamp,
    size: PixelSize,
    pixels: Vec<u8>,
}

impl RawFrame {
    pub fn new(stamp: FrameStamp, size: PixelSize, pixels: Vec<u8>) -> Result<Self, Error> {
        stamp.key.validate()?;
        let bytes = size.rgba_bytes()?;
        if stamp.view_epoch == 0
            || stamp.revision == 0
            || stamp.sequence == 0
            || bytes > MAX_FRAME_BYTES
            || pixels.len() as u64 != bytes
        {
            return Err(Error::InvalidFrame);
        }
        Ok(Self {
            stamp,
            size,
            pixels,
        })
    }

    pub fn stamp(&self) -> FrameStamp {
        self.stamp
    }

    pub fn sequence(&self) -> u64 {
        self.stamp.sequence
    }

    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut header = [0u8; HEADER_BYTES];
        header[0..4].copy_from_slice(MAGIC);
        header[4..6].copy_from_slice(&RAW_VERSION.to_be_bytes());
        header[6..8].copy_from_slice(&1u16.to_be_bytes());
        header[8..24].copy_from_slice(&self.stamp.key.binding.nonce);
        header[24..32].copy_from_slice(&self.stamp.key.object.to_be_bytes());
        header[32..40].copy_from_slice(&self.stamp.key.generation.to_be_bytes());
        header[40..48].copy_from_slice(&self.stamp.revision.to_be_bytes());
        header[48..56].copy_from_slice(&self.stamp.sequence.to_be_bytes());
        header[56..60].copy_from_slice(&self.size.width.to_be_bytes());
        header[60..64].copy_from_slice(&self.size.height.to_be_bytes());
        header[64..68].copy_from_slice(&(self.pixels.len() as u32).to_be_bytes());
        header[68..76].copy_from_slice(&self.stamp.view_epoch.to_be_bytes());
        let mut output = Vec::with_capacity(HEADER_BYTES + self.pixels.len());
        output.extend_from_slice(&header);
        output.extend_from_slice(&self.pixels);
        output
    }
}

/// Validate a fixed-size header BEFORE allocating its bounded payload. A network
/// adapter must impose read timeout/cancellation and recheck the CURRENT gate
/// when publishing; this synchronous parser does not own a clock or a transport.
pub fn read_raw_frame(
    reader: &mut impl Read,
    gate: &SessionGate,
    expected_size: PixelSize,
    now: u64,
) -> Result<RawFrame, Error> {
    gate.can_view(now)?;
    expected_size.validate()?;
    let mut header = [0u8; HEADER_BYTES];
    reader
        .read_exact(&mut header)
        .map_err(|_| Error::TruncatedFrame)?;
    if &header[0..4] != MAGIC {
        return Err(Error::InvalidFrame);
    }
    if u16::from_be_bytes(header[4..6].try_into().unwrap()) != RAW_VERSION {
        return Err(Error::UnsupportedVersion);
    }
    if u16::from_be_bytes(header[6..8].try_into().unwrap()) != 1 {
        return Err(Error::InvalidFrame);
    }
    let key = gate.key();
    let object = u64::from_be_bytes(header[24..32].try_into().unwrap());
    let generation = u64::from_be_bytes(header[32..40].try_into().unwrap());
    let revision = u64::from_be_bytes(header[40..48].try_into().unwrap());
    let sequence = u64::from_be_bytes(header[48..56].try_into().unwrap());
    let size = PixelSize {
        width: u32::from_be_bytes(header[56..60].try_into().unwrap()),
        height: u32::from_be_bytes(header[60..64].try_into().unwrap()),
    };
    let declared = u32::from_be_bytes(header[64..68].try_into().unwrap());
    let view_epoch = u64::from_be_bytes(header[68..76].try_into().unwrap());
    if header[8..24] != key.binding.nonce
        || object != key.object
        || generation != key.generation
        || revision != gate.revision()
        || view_epoch != gate.view_epoch()
        || size != expected_size
    {
        return Err(Error::StaleTarget);
    }
    let bytes = size.rgba_bytes()?;
    if sequence == 0 || u64::from(declared) != bytes || bytes > MAX_FRAME_BYTES {
        return Err(Error::InvalidFrame);
    }
    let mut pixels = Vec::new();
    pixels
        .try_reserve_exact(declared as usize)
        .map_err(|_| Error::Capacity)?;
    pixels.resize(declared as usize, 0);
    reader
        .read_exact(&mut pixels)
        .map_err(|_| Error::TruncatedFrame)?;
    RawFrame::new(
        FrameStamp {
            key,
            view_epoch,
            revision,
            sequence,
        },
        size,
        pixels,
    )
}

struct Queued {
    frame: RawFrame,
    received_at: u64,
}

/// A single raw/decoded frame slot; replacement is bounded and non-blocking.
/// Recreate on view-consent, geometry or session changes, never from disk.
pub struct LatestFrame {
    key: SurfaceKey,
    view_epoch: u64,
    revision: u64,
    size: PixelSize,
    watermark: u64,
    last_now: u64,
    queued: Option<Queued>,
}

impl LatestFrame {
    pub fn new(gate: &SessionGate, size: PixelSize) -> Result<Self, Error> {
        if size.rgba_bytes()? > MAX_FRAME_BYTES {
            return Err(Error::Capacity);
        }
        Ok(Self {
            key: gate.key(),
            view_epoch: gate.view_epoch(),
            revision: gate.revision(),
            size,
            watermark: 0,
            last_now: 0,
            queued: None,
        })
    }

    fn check_gate(&mut self, gate: &SessionGate, now: u64) -> Result<(), Error> {
        advance(&mut self.last_now, now)?;
        let result = if gate.key() != self.key
            || gate.revision() != self.revision
            || gate.view_epoch() != self.view_epoch
        {
            Err(Error::StaleTarget)
        } else {
            gate.can_view(now)
        };
        if result.is_err() {
            self.queued = None;
        }
        result
    }

    /// True means an older raw frame was replaced; useful for aggregate metrics.
    pub fn push(&mut self, frame: RawFrame, gate: &SessionGate, now: u64) -> Result<bool, Error> {
        self.check_gate(gate, now)?;
        if frame.stamp.key != self.key
            || frame.stamp.revision != self.revision
            || frame.stamp.view_epoch != self.view_epoch
            || frame.size != self.size
        {
            return Err(Error::StaleTarget);
        }
        if frame.stamp.sequence <= self.watermark {
            return Err(Error::InvalidSequence);
        }
        self.watermark = frame.stamp.sequence;
        let replaced = self.queued.is_some();
        self.queued = Some(Queued {
            frame,
            received_at: now,
        });
        Ok(replaced)
    }

    /// The renderer must acknowledge actual presentation separately. Taking a
    /// frame does not grant control and does not mean the GPU displayed it.
    pub fn take(&mut self, gate: &SessionGate, now: u64) -> Result<Option<RawFrame>, Error> {
        self.check_gate(gate, now)?;
        let Some(queued) = self.queued.take() else {
            return Ok(None);
        };
        if now - queued.received_at >= MAX_QUEUE_AGE_MS {
            return Err(Error::StaleFrame);
        }
        Ok(Some(queued.frame))
    }

    pub fn clear(&mut self) {
        self.queued = None;
    }

    pub fn has_frame(&self) -> bool {
        self.queued.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture_key;
    use crate::session::{ControlScope, SurfaceKind};
    use std::io::Cursor;

    fn size() -> PixelSize {
        PixelSize {
            width: 2,
            height: 2,
        }
    }

    fn gate() -> SessionGate {
        let mut g = SessionGate::new(fixture_key(1), SurfaceKind::Display, 1, false).unwrap();
        g.grant_view(0, 10_000).unwrap();
        g
    }

    fn frame(gate: &SessionGate, sequence: u64) -> RawFrame {
        RawFrame::new(gate.frame_stamp(sequence), size(), vec![17; 16]).unwrap()
    }

    #[test]
    fn raw_frame_roundtrip_preserves_exact_pixels() {
        let g = gate();
        let packet = frame(&g, 1).encode();
        let decoded = read_raw_frame(&mut Cursor::new(packet), &g, size(), 0).unwrap();
        assert_eq!(decoded.pixels(), &[17; 16]);
        assert_eq!(decoded.sequence(), 1);
    }

    #[test]
    fn oversized_length_rejects_before_reading_payload() {
        let g = gate();
        let mut packet = frame(&g, 1).encode();
        packet[64..68].copy_from_slice(&u32::MAX.to_be_bytes());
        let mut reader = Cursor::new(packet);
        assert!(matches!(
            read_raw_frame(&mut reader, &g, size(), 0),
            Err(Error::InvalidFrame)
        ));
        assert_eq!(reader.position(), HEADER_BYTES as u64);
    }

    #[test]
    fn every_truncated_prefix_rejects() {
        let g = gate();
        let packet = frame(&g, 1).encode();
        for end in 0..packet.len() {
            assert!(matches!(
                read_raw_frame(&mut Cursor::new(&packet[..end]), &g, size(), 0),
                Err(Error::TruncatedFrame)
            ));
        }
    }

    #[test]
    fn wrong_nonce_generation_revision_size_or_consent_reject() {
        let g = gate();
        for offset in [8, 31, 39, 47, 59, 63, 75] {
            let mut packet = frame(&g, 1).encode();
            packet[offset] ^= 1;
            assert!(matches!(
                read_raw_frame(&mut Cursor::new(packet), &g, size(), 0),
                Err(Error::StaleTarget)
            ));
        }
    }

    #[test]
    fn unknown_version_codec_and_zero_sequence_reject() {
        let g = gate();
        for offset in [0, 5, 7, 55] {
            let mut packet = frame(&g, 1).encode();
            packet[offset] ^= 1;
            assert!(read_raw_frame(&mut Cursor::new(packet), &g, size(), 0).is_err());
        }
    }

    #[test]
    fn unauthorized_parser_does_not_read_even_header() {
        let g = SessionGate::new(fixture_key(1), SurfaceKind::Display, 1, false).unwrap();
        let mut reader = Cursor::new(frame(&gate(), 1).encode());
        assert!(matches!(
            read_raw_frame(&mut reader, &g, size(), 0),
            Err(Error::PermissionDenied)
        ));
        assert_eq!(reader.position(), 0);
    }

    #[test]
    fn mailbox_is_one_slot_and_watermark_survives_consumption() {
        let g = gate();
        let mut queue = LatestFrame::new(&g, size()).unwrap();
        assert!(!queue.push(frame(&g, 1), &g, 0).unwrap());
        assert!(queue.push(frame(&g, 2), &g, 1).unwrap());
        assert_eq!(queue.take(&g, 2).unwrap().unwrap().sequence(), 2);
        assert!(!queue.has_frame());
        assert_eq!(queue.push(frame(&g, 1), &g, 3), Err(Error::InvalidSequence));
    }

    #[test]
    fn revocation_clears_queued_pixels_and_old_queue_stays_invalid() {
        let mut g = gate();
        let mut queue = LatestFrame::new(&g, size()).unwrap();
        queue.push(frame(&g, 1), &g, 0).unwrap();
        g.revoke_all(1).unwrap();
        assert!(queue.take(&g, 1).is_err());
        assert!(!queue.has_frame());
        g.grant_view(2, 100).unwrap();
        assert_eq!(queue.push(frame(&g, 2), &g, 2), Err(Error::StaleTarget));
    }

    #[test]
    fn reconsent_rejects_old_packet_even_with_fresh_mailbox() {
        let mut g = gate();
        let old_packet = frame(&g, 1).encode();
        let old_frame = frame(&g, 2);
        g.revoke_all(1).unwrap();
        g.grant_view(2, 100).unwrap();
        let mut queue = LatestFrame::new(&g, size()).unwrap();
        assert_eq!(queue.push(old_frame, &g, 2), Err(Error::StaleTarget));
        assert!(matches!(
            read_raw_frame(&mut Cursor::new(old_packet), &g, size(), 2),
            Err(Error::StaleTarget)
        ));
        queue.push(frame(&g, 1), &g, 3).unwrap();
    }

    #[test]
    fn input_grant_does_not_reset_video_queue() {
        let mut g = gate();
        let mut queue = LatestFrame::new(&g, size()).unwrap();
        queue.push(frame(&g, 1), &g, 0).unwrap();
        g.grant_control(ControlScope::WholeDesktop, 1, 99).unwrap();
        assert!(queue.take(&g, 1).unwrap().is_some());
    }

    #[test]
    fn exact_age_expiry_discards_stale_frame() {
        let g = gate();
        let mut queue = LatestFrame::new(&g, size()).unwrap();
        queue.push(frame(&g, 1), &g, 0).unwrap();
        assert!(matches!(queue.take(&g, 2000), Err(Error::StaleFrame)));
        assert!(!queue.has_frame());
    }
}
