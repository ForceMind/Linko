//! Pair authenticated frame descriptions with decoded video timestamps.
//! The caller owns frame buffers and actual presentation. This model grants no
//! authority and never treats a match as a presentation acknowledgement.

use crate::session::{FrameStamp, SessionGate, FRAME_FRESHNESS_MS};
use crate::{advance, Error};

const MAX_PENDING: usize = 64;
// Provisional 90 kHz video clock window for the offline experiment. A larger
// discontinuity requires a new stream and anchor supplied by the adapter.
const MAX_TIMESTAMP_DELTA: i32 = 450_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MediaTrack {
    pub generation: u64,
    pub track: u32,
    pub ssrc: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MediaFrame {
    pub track: MediaTrack,
    pub timestamp: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrameMatch {
    pub stamp: FrameStamp,
    /// Deadline on the viewer's monotonic clock, from the first arrival.
    pub valid_until: u64,
}

#[derive(Clone, Copy)]
struct PendingFrame {
    timestamp: u64,
    first_seen: u64,
    stamp: Option<FrameStamp>,
    decoded: bool,
}

/// One negotiated stream in one view epoch and geometry revision. The adapter
/// must get the track and source descriptors from the authenticated control
/// binding and decoded identifiers from the associated media receiver.
pub struct FrameMatcher {
    track: MediaTrack,
    scope: FrameStamp,
    high_timestamp: u64,
    last_now: u64,
    retired_timestamp: Option<u64>,
    last_sequence: u64,
    pending: Vec<PendingFrame>,
    closed: bool,
}

impl FrameMatcher {
    pub fn new(
        gate: &SessionGate,
        track: MediaTrack,
        initial_timestamp: u32,
        now: u64,
    ) -> Result<Self, Error> {
        gate.can_view(now)?;
        if track.generation == 0 || track.track == 0 {
            return Err(Error::InvalidIdentity);
        }
        Ok(Self {
            track,
            scope: gate.frame_stamp(1),
            // Leave room on both sides of the first timestamp at an early wrap.
            high_timestamp: (1_u64 << 32) + u64::from(initial_timestamp),
            last_now: now,
            retired_timestamp: None,
            last_sequence: 0,
            pending: Vec::with_capacity(MAX_PENDING),
            closed: false,
        })
    }

    pub fn close(&mut self) {
        self.pending.clear();
        self.closed = true;
    }

    pub fn expire(&mut self, gate: &SessionGate, now: u64) -> Result<(), Error> {
        advance(&mut self.last_now, now)?;
        if self.closed || gate.frame_stamp(1) != self.scope {
            self.close();
            return Err(Error::StaleTarget);
        }
        if let Err(error) = gate.can_view(now) {
            self.close();
            return Err(error);
        }
        if let Some(expired) = self
            .pending
            .iter()
            .filter(|entry| now - entry.first_seen > FRAME_FRESHNESS_MS)
            .map(|entry| entry.timestamp)
            .max()
        {
            // An earlier frame must not be reintroduced by replaying both
            // halves after its first arrival was forgotten.
            self.retire_through(expired);
        }
        Ok(())
    }

    pub fn describe(
        &mut self,
        gate: &SessionGate,
        media: MediaFrame,
        stamp: FrameStamp,
        now: u64,
    ) -> Result<Option<FrameMatch>, Error> {
        self.observe(gate, media, Some(stamp), now)
    }

    pub fn decoded(
        &mut self,
        gate: &SessionGate,
        media: MediaFrame,
        now: u64,
    ) -> Result<Option<FrameMatch>, Error> {
        self.observe(gate, media, None, now)
    }

    fn observe(
        &mut self,
        gate: &SessionGate,
        media: MediaFrame,
        stamp: Option<FrameStamp>,
        now: u64,
    ) -> Result<Option<FrameMatch>, Error> {
        self.expire(gate, now)?;
        if media.track != self.track {
            return Err(Error::StaleTarget);
        }
        if stamp.is_some_and(|frame| frame.sequence == 0 || gate.frame_stamp(frame.sequence) != frame) {
            return Err(Error::InvalidFrame);
        }

        let delta = media.timestamp.wrapping_sub(self.high_timestamp as u32) as i32;
        if !(-MAX_TIMESTAMP_DELTA..=MAX_TIMESTAMP_DELTA).contains(&delta) {
            return Err(Error::InvalidSequence);
        }
        let timestamp = self
            .high_timestamp
            .checked_add_signed(i64::from(delta))
            .ok_or(Error::InvalidSequence)?;
        if self.retired_timestamp.is_some_and(|retired| timestamp <= retired)
            || stamp.is_some_and(|frame| frame.sequence <= self.last_sequence)
        {
            return Err(Error::StaleFrame);
        }

        let index = match self
            .pending
            .iter()
            .position(|entry| entry.timestamp == timestamp)
        {
            Some(index) => index,
            None => {
                if self.pending.len() == MAX_PENDING {
                    return Err(Error::Capacity);
                }
                self.pending.push(PendingFrame {
                    timestamp,
                    first_seen: now,
                    stamp: None,
                    decoded: false,
                });
                self.pending.len() - 1
            }
        };

        let entry = &mut self.pending[index];
        if let Some(stamp) = stamp {
            if entry.stamp.is_some_and(|old| old != stamp) {
                self.close();
                return Err(Error::InvalidFrame);
            }
            entry.stamp = Some(stamp);
        } else {
            entry.decoded = true;
        }
        self.high_timestamp = self.high_timestamp.max(timestamp);
        let Some(stamp) = entry.stamp.filter(|_| entry.decoded) else {
            return Ok(None);
        };
        let valid_until = entry.first_seen.saturating_add(FRAME_FRESHNESS_MS);
        // Late descriptions and decoded frames cannot revive an earlier candidate.
        self.retire_through(timestamp);
        if stamp.sequence <= self.last_sequence {
            return Err(Error::StaleFrame);
        }
        self.last_sequence = stamp.sequence;
        Ok(Some(FrameMatch { stamp, valid_until }))
    }

    fn retire_through(&mut self, timestamp: u64) {
        self.retired_timestamp = Some(timestamp.max(self.retired_timestamp.unwrap_or(0)));
        self.pending.retain(|entry| entry.timestamp > timestamp);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture_key;
    use crate::session::SurfaceKind;

    const TRACK: MediaTrack = MediaTrack {
        generation: 1,
        track: 1,
        ssrc: 33,
    };

    fn packet(timestamp: u32) -> MediaFrame {
        MediaFrame {
            track: TRACK,
            timestamp,
        }
    }

    fn gate() -> SessionGate {
        let mut gate = SessionGate::new(fixture_key(4), SurfaceKind::Window, 1, false).unwrap();
        gate.grant_view(0, 20_000).unwrap();
        gate
    }

    #[test]
    fn matches_both_arrival_orders_across_timestamp_wrap() {
        let gate = gate();
        let mut frames = FrameMatcher::new(&gate, TRACK, u32::MAX - 40, 0).unwrap();
        let before = packet(u32::MAX - 10);
        let after = packet(9);
        assert_eq!(frames.decoded(&gate, before, 10), Ok(None));
        assert_eq!(
            frames.describe(&gate, before, gate.frame_stamp(1), 21),
            Ok(Some(FrameMatch {
                stamp: gate.frame_stamp(1),
                valid_until: 2010,
            }))
        );
        assert_eq!(frames.describe(&gate, after, gate.frame_stamp(2), 24), Ok(None));
        assert_eq!(
            frames.decoded(&gate, after, 25),
            Ok(Some(FrameMatch {
                stamp: gate.frame_stamp(2),
                valid_until: 2024,
            }))
        );
        assert_eq!(frames.decoded(&gate, before, 26), Err(Error::StaleFrame));
    }

    #[test]
    fn rejects_another_track_generation_and_ssrc_without_disrupting_the_current_stream() {
        let gate = gate();
        let mut frames = FrameMatcher::new(&gate, TRACK, 100, 0).unwrap();
        for track in [
            MediaTrack { generation: 2, ..TRACK },
            MediaTrack { track: 2, ..TRACK },
            MediaTrack { ssrc: 34, ..TRACK },
        ] {
            assert_eq!(
                frames.decoded(&gate, MediaFrame { track, timestamp: 120 }, 5),
                Err(Error::StaleTarget)
            );
        }
        assert_eq!(frames.describe(&gate, packet(120), gate.frame_stamp(1), 6), Ok(None));
        assert!(frames.decoded(&gate, packet(120), 7).unwrap().is_some());
    }

    #[test]
    fn out_of_order_duplicate_metadata_does_not_extend_deadline() {
        let gate = gate();
        let mut frames = FrameMatcher::new(&gate, TRACK, 100, 0).unwrap();
        assert_eq!(frames.describe(&gate, packet(140), gate.frame_stamp(2), 1), Ok(None));
        assert_eq!(frames.describe(&gate, packet(110), gate.frame_stamp(1), 2), Ok(None));
        assert_eq!(frames.describe(&gate, packet(110), gate.frame_stamp(1), 1000), Ok(None));
        assert_eq!(
            frames.decoded(&gate, packet(110), 2001),
            Ok(Some(FrameMatch {
                stamp: gate.frame_stamp(1),
                valid_until: 2002,
            }))
        );
        assert_eq!(frames.decoded(&gate, packet(140), 2002), Err(Error::StaleFrame));
        assert_eq!(
            frames.describe(&gate, packet(140), gate.frame_stamp(2), 2003),
            Err(Error::StaleFrame)
        );
        assert_eq!(frames.decoded(&gate, packet(170), 2004), Ok(None));
        assert!(frames.describe(&gate, packet(170), gate.frame_stamp(3), 2005).unwrap().is_some());
    }

    #[test]
    fn rejects_stale_scope_and_never_reuses_a_closed_matcher() {
        let mut gate = gate();
        let mut frames = FrameMatcher::new(&gate, TRACK, 10, 0).unwrap();
        let mut stale = gate.frame_stamp(1);
        stale.revision += 1;
        assert_eq!(frames.describe(&gate, packet(11), stale, 5), Err(Error::InvalidFrame));
        assert_eq!(frames.decoded(&gate, packet(11), 6), Ok(None));
        gate.revoke_all(9).unwrap();
        assert_eq!(frames.expire(&gate, 9), Err(Error::StaleTarget));
        assert!(frames.pending.is_empty());
        gate.grant_view(10, 200).unwrap();
        assert_eq!(frames.decoded(&gate, packet(11), 11), Err(Error::StaleTarget));
    }

    #[test]
    fn bounds_queue_and_rejects_timestamp_discontinuities_without_resetting_it() {
        let gate = gate();
        let mut frames = FrameMatcher::new(&gate, TRACK, 0, 0).unwrap();
        for tick in 0..MAX_PENDING as u32 {
            assert_eq!(frames.decoded(&gate, packet(tick), 1), Ok(None));
        }
        assert_eq!(frames.decoded(&gate, packet(MAX_PENDING as u32), 2), Err(Error::Capacity));
        assert_eq!(frames.decoded(&gate, packet(0x8000_003F), 2), Err(Error::InvalidSequence));
        assert_eq!(frames.decoded(&gate, packet(0), 0), Err(Error::InvalidTime));
        frames.expire(&gate, 2002).unwrap();
        assert!(frames.pending.is_empty());
    }

    #[test]
    fn conflicting_source_descriptions_close_the_stream() {
        let gate = gate();
        let mut frames = FrameMatcher::new(&gate, TRACK, 0, 0).unwrap();
        assert_eq!(frames.describe(&gate, packet(10), gate.frame_stamp(1), 1), Ok(None));
        assert_eq!(
            frames.describe(&gate, packet(10), gate.frame_stamp(2), 2),
            Err(Error::InvalidFrame)
        );
        assert_eq!(frames.decoded(&gate, packet(10), 3), Err(Error::StaleTarget));
    }
}
