//! Local mixing with one bounded jitter buffer per authorized source.
use crate::audio_playout::{Admission, Buffer, Frame, OutputEpoch, Playout, SAMPLES};
use std::sync::atomic::{AtomicU64, Ordering};

pub const MAX_SOURCES: usize = 4;
static NEXT_MIX: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy)]
pub struct Source {
    owner: u64,
    slot: usize,
    epoch: OutputEpoch,
}

#[derive(Default)]
struct Input {
    buffer: Buffer,
    active: Option<OutputEpoch>,
}

pub struct Mix {
    owner: u64,
    inputs: [Input; MAX_SOURCES],
}

impl Default for Mix {
    fn default() -> Self {
        let owner = NEXT_MIX
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .unwrap_or(0);
        Self {
            owner,
            inputs: Default::default(),
        }
    }
}

impl Mix {
    /// Admit one local source slot after the authenticated audio grant.
    pub fn start(&mut self, slot: usize) -> Option<Source> {
        let input = self.inputs.get_mut(slot)?;
        if self.owner == 0 || input.active.is_some() {
            return None;
        }
        let epoch = input.buffer.start()?;
        input.active = Some(epoch);
        Some(Source {
            owner: self.owner,
            slot,
            epoch,
        })
    }

    pub fn push(&mut self, source: Source, frame: Frame) -> Admission {
        if source.owner != self.owner {
            return Admission::Inactive;
        }
        self.inputs[source.slot].buffer.push(source.epoch, frame)
    }

    pub fn stop(&mut self, source: Source) -> bool {
        let input = &mut self.inputs[source.slot];
        if source.owner != self.owner || input.active != Some(source.epoch) {
            return false;
        }
        input.active = None;
        input.buffer.stop();
        true
    }

    /// Mix one period. Each missing period contributes silence for that source.
    /// Widen before adding, then clamp once for deterministic clipping.
    pub fn render(&mut self, output: &mut [i16; SAMPLES]) -> bool {
        let mut combined = [0i32; SAMPLES];
        let mut decoded = [0; SAMPLES];
        let mut audible = false;
        for input in &mut self.inputs {
            let Some(epoch) = input.active else {
                continue;
            };
            if input.buffer.render(epoch, &mut decoded) == Playout::Audio {
                audible = true;
                for (total, sample) in combined.iter_mut().zip(decoded) {
                    *total += i32::from(sample);
                }
            }
        }
        for (sample, total) in output.iter_mut().zip(combined) {
            *sample = total.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16;
        }
        audible
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prefill(mix: &mut Mix, source: Source, value: i16) {
        for sequence in 1..=3 {
            assert_eq!(
                mix.push(
                    source,
                    Frame {
                        sequence,
                        samples: [value; SAMPLES]
                    }
                ),
                Admission::Queued
            );
        }
    }

    #[test]
    fn mixes_independent_sources_without_overflow_or_replaying_a_disconnected_source() {
        let mut mix = Mix::default();
        let left = mix.start(0).unwrap();
        let mut another_owner = Mix::default();
        assert!(another_owner.start(0).is_some());
        assert!(!another_owner.stop(left));
        assert_eq!(
            another_owner.push(
                left,
                Frame {
                    sequence: 1,
                    samples: [9; SAMPLES]
                }
            ),
            Admission::Inactive
        );
        let right = mix.start(1).unwrap();
        assert!(mix.start(MAX_SOURCES).is_none());
        assert!(mix.start(0).is_none());
        prefill(&mut mix, left, 20_000);
        prefill(&mut mix, right, 25_000);
        let mut output = [0; SAMPLES];
        assert!(mix.render(&mut output));
        assert_eq!(output, [i16::MAX; SAMPLES]);
        assert!(mix.stop(right));
        assert!(mix.render(&mut output));
        assert_eq!(output, [20_000; SAMPLES]);
        assert!(mix.stop(left));
        assert!(!mix.render(&mut output));
        assert_eq!(output, [0; SAMPLES]);

        let replacement = mix.start(0).unwrap();
        let independent = mix.start(1).unwrap();
        assert!(!mix.stop(left));
        assert_eq!(
            mix.push(
                left,
                Frame {
                    sequence: 1,
                    samples: [9; SAMPLES]
                }
            ),
            Admission::Inactive
        );
        prefill(&mut mix, replacement, -18_000);
        prefill(&mut mix, independent, -20_000);
        assert!(mix.render(&mut output));
        assert_eq!(output, [i16::MIN; SAMPLES]);
    }
}
