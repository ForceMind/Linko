//! Bounded PCM playout for one locally authorized audio source.
//! No OS device, input grant, network or media-content diagnostics.
pub use peerglide_protocol::audio::{Frame, CHANNELS, SAMPLES, SAMPLE_RATE};

const WINDOW: usize = 16;
const PREFILL: usize = 3;
const START_WAIT: u8 = 4;
const REBUFFER_GAP: u8 = 4;

/// Local output generation issued by the one audio owner.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct OutputEpoch(u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admission {
    Queued,
    Inactive,
    Late,
    Duplicate,
    TooFarAhead,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Playout {
    Audio,
    Buffering,
    Gap,
    Inactive,
}

struct Active {
    epoch: OutputEpoch,
    next: u64,
    queued: usize,
    started: bool,
    warmup: u8,
    gaps: u8,
}

/// One queue per audio source. Normal operations need no allocation.
pub struct Buffer {
    generation: u64,
    active: Option<Active>,
    frames: [Option<Frame>; WINDOW],
}

impl Default for Buffer {
    fn default() -> Self {
        Self {
            generation: 0,
            active: None,
            frames: [const { None }; WINDOW],
        }
    }
}

impl Buffer {
    /// Called after local audio authorization or an explicit output change.
    /// A fresh stream begins with sequence one and no old queued audio.
    pub fn start(&mut self) -> Option<OutputEpoch> {
        self.stop();
        self.generation = self.generation.checked_add(1)?;
        let epoch = OutputEpoch(self.generation);
        self.active = Some(Active {
            epoch,
            next: 1,
            queued: 0,
            started: false,
            warmup: 0,
            gaps: 0,
        });
        Some(epoch)
    }

    /// Stops admission and drops buffered samples. The output owner supplies
    /// silence when render is called with a stopped or retired generation.
    pub fn stop(&mut self) {
        self.active = None;
        self.frames = [const { None }; WINDOW];
    }

    pub fn push(&mut self, epoch: OutputEpoch, frame: Frame) -> Admission {
        let Some(active) = self.active.as_mut().filter(|active| active.epoch == epoch) else {
            return Admission::Inactive;
        };
        let Some(distance) = frame.sequence.checked_sub(active.next) else {
            return Admission::Late;
        };
        if distance >= WINDOW as u64 {
            return Admission::TooFarAhead;
        }
        let slot = &mut self.frames[(frame.sequence % WINDOW as u64) as usize];
        if slot.is_some() {
            return Admission::Duplicate;
        }
        *slot = Some(frame);
        active.queued += 1;
        Admission::Queued
    }

    /// Consumes one five-millisecond period. A missing packet never reuses old
    /// samples. After a run of gaps, prefill again to absorb short interruptions.
    pub fn render(&mut self, epoch: OutputEpoch, output: &mut [i16; SAMPLES]) -> Playout {
        output.fill(0);
        let Some(active) = self.active.as_mut().filter(|active| active.epoch == epoch) else {
            return Playout::Inactive;
        };
        if !active.started {
            if active.queued == 0 {
                return Playout::Buffering;
            }
            active.warmup = active.warmup.saturating_add(1);
            if active.queued < PREFILL && active.warmup < START_WAIT {
                return Playout::Buffering;
            }
            active.started = true;
        }
        let index = (active.next % WINDOW as u64) as usize;
        let Some(next) = active.next.checked_add(1) else {
            self.stop();
            return Playout::Inactive;
        };
        active.next = next;
        if let Some(frame) = self.frames[index].take() {
            active.queued -= 1;
            active.gaps = 0;
            *output = frame.samples;
            return Playout::Audio;
        }
        active.gaps = active.gaps.saturating_add(1);
        if active.gaps >= REBUFFER_GAP {
            active.started = false;
            active.warmup = 0;
            active.gaps = 0;
        }
        Playout::Gap
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(sequence: u64, value: i16) -> Frame {
        Frame {
            sequence,
            samples: [value; SAMPLES],
        }
    }

    #[test]
    fn bounded_reordering_and_silence_for_a_missing_period() {
        let mut buffer = Buffer::default();
        let epoch = buffer.start().unwrap();
        let mut output = [99; SAMPLES];
        assert_eq!(buffer.push(epoch, frame(3, 3)), Admission::Queued);
        assert_eq!(buffer.render(epoch, &mut output), Playout::Buffering);
        assert_eq!(output, [0; SAMPLES]);
        assert_eq!(buffer.push(epoch, frame(1, 1)), Admission::Queued);
        assert_eq!(buffer.push(epoch, frame(3, 8)), Admission::Duplicate);
        assert_eq!(buffer.push(epoch, frame(4, 4)), Admission::Queued);
        assert_eq!(buffer.push(epoch, frame(17, 5)), Admission::TooFarAhead);
        assert_eq!(buffer.render(epoch, &mut output), Playout::Audio);
        assert_eq!(output, [1; SAMPLES]);
        assert_eq!(buffer.push(epoch, frame(1, 7)), Admission::Late);
        assert_eq!(buffer.render(epoch, &mut output), Playout::Gap);
        assert_eq!(output, [0; SAMPLES]);
        assert_eq!(buffer.render(epoch, &mut output), Playout::Audio);
        assert_eq!(output, [3; SAMPLES]);
        assert_eq!(buffer.render(epoch, &mut output), Playout::Audio);
        assert_eq!(output, [4; SAMPLES]);
    }

    #[test]
    fn output_change_and_stop_clear_samples_and_reject_old_generations() {
        let mut buffer = Buffer::default();
        let old = buffer.start().unwrap();
        assert_eq!(buffer.push(old, frame(1, 1)), Admission::Queued);
        let current = buffer.start().unwrap();
        let mut output = [99; SAMPLES];
        assert_eq!(buffer.render(old, &mut output), Playout::Inactive);
        assert_eq!(output, [0; SAMPLES]);
        assert_eq!(buffer.push(old, frame(2, 2)), Admission::Inactive);
        assert_eq!(buffer.render(current, &mut output), Playout::Buffering);
        assert_eq!(buffer.push(current, frame(1, 3)), Admission::Queued);
        for _ in 0..START_WAIT - 1 {
            assert_eq!(buffer.render(current, &mut output), Playout::Buffering);
        }
        assert_eq!(buffer.render(current, &mut output), Playout::Audio);
        assert_eq!(output, [3; SAMPLES]);
        buffer.stop();
        assert_eq!(buffer.render(current, &mut output), Playout::Inactive);
        assert_eq!(output, [0; SAMPLES]);
    }
}
