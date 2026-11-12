//! Translate v5 HID keyboard positions to evdev codes for an EIS keyboard.
//! Reference: USB HID Usage Tables page 0x07 and Linux input-event-codes.h.
const LETTERS: [u32; 26] = [
    30, 48, 46, 32, 18, 33, 34, 35, 23, 36, 37, 38, 50, 49, 24, 25, 16, 19, 31, 20, 22, 47, 17, 45,
    21, 44,
];

// The non-US hash position aliases the ordinary backslash evdev code. Offer
// only one HID usage for a Linux key so either release has a unique owner.
const PUNCTUATION: [u32; 18] = [
    28, 1, 14, 15, 57, 12, 13, 26, 27, 43, 0, 39, 40, 41, 51, 52, 53, 58,
];

// Print Screen through keypad equal. The power usage stays unadvertised.
const NAVIGATION_AND_KEYPAD: [u32; 34] = [
    99, 70, 119, 110, 102, 104, 111, 107, 109, 106, 105, 108, 103, 69, 98, 55, 74, 78, 96, 79, 80,
    81, 75, 76, 77, 71, 72, 73, 82, 83, 86, 127, 0, 117,
];
const MODIFIERS: [u32; 8] = [29, 42, 56, 125, 97, 54, 100, 126];

fn code(usage: u16) -> Option<u32> {
    let code = match usage {
        0x04..=0x1d => LETTERS[usize::from(usage - 0x04)],
        0x1e..=0x27 => u32::from(usage - 0x1e) + 2,
        0x28..=0x39 => PUNCTUATION[usize::from(usage - 0x28)],
        0x3a..=0x43 => u32::from(usage - 0x3a) + 59,
        0x44..=0x45 => u32::from(usage - 0x44) + 87,
        0x46..=0x67 => NAVIGATION_AND_KEYPAD[usize::from(usage - 0x46)],
        0x68..=0x73 => u32::from(usage - 0x68) + 183,
        0xe0..=0xe7 => MODIFIERS[usize::from(usage - 0xe0)],
        _ => return None,
    };
    (code != 0).then_some(code)
}

pub(super) fn usages() -> impl Iterator<Item = u16> {
    (0x04..=0x73)
        .chain(0xe0..=0xe7)
        .filter(|&usage| code(usage).is_some())
}

pub(super) struct Keys {
    held: [bool; 256],
}

impl Default for Keys {
    fn default() -> Self {
        Self { held: [false; 256] }
    }
}

impl Keys {
    pub(super) fn transition(&mut self, usage: u16, down: bool) -> Option<u32> {
        let code = code(usage)?;
        let held = &mut self.held[usize::from(usage)];
        if *held == down {
            return None;
        }
        *held = down;
        Some(code)
    }

    pub(super) fn has_held(&self) -> bool {
        self.held.contains(&true)
    }

    pub(super) fn release(&mut self) -> impl Iterator<Item = u32> + '_ {
        self.held
            .iter_mut()
            .enumerate()
            .filter_map(|(usage, down)| {
                if std::mem::take(down) {
                    code(usage as u16)
                } else {
                    None
                }
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peerglide_protocol::Input;
    use std::collections::BTreeSet;

    #[test]
    fn advertised_positions_have_unique_evdev_codes() {
        let mut codes = BTreeSet::new();
        for usage in usages() {
            assert!(Input::Key { usage, down: true }.valid());
            assert!(codes.insert(code(usage).unwrap()));
        }
        assert_eq!(code(0x04), Some(30));
        assert_eq!(code(0x28), Some(28));
        assert_eq!(code(0x58), Some(96));
        assert_eq!(code(0x73), Some(194));
        assert_eq!(code(0xe4), Some(97));
        assert!(code(0x32).is_none());
        assert!(code(0x66).is_none());
    }

    #[test]
    fn cleanup_releases_only_submitted_keys_and_never_repeats_them() {
        let mut keys = Keys::default();
        assert_eq!(keys.transition(0xe4, false), None);
        assert_eq!(keys.transition(0xe4, true), Some(97));
        assert_eq!(keys.transition(0xe4, true), None);
        assert_eq!(keys.transition(0x04, true), Some(30));
        assert_eq!(keys.transition(0x04, false), Some(30));
        assert_eq!(keys.transition(0x58, true), Some(96));
        assert!(keys.has_held());
        assert_eq!(keys.release().collect::<Vec<_>>(), [96, 97]);
        assert!(!keys.has_held());
        assert!(keys.release().next().is_none());
    }
}
