//! Fixed storage for physical holds, including startup aliases and unknown keys.
use std::marker::PhantomData;

pub(super) type HeldKeys = HeldSet<u16, 1024>;
pub(super) type HeldButtons = HeldSet<u8, 4>;

pub(super) struct HeldSet<T, const WORDS: usize> {
    words: [u64; WORDS],
    count: usize,
    input: PhantomData<T>,
}

impl<T, const WORDS: usize> Default for HeldSet<T, WORDS> {
    fn default() -> Self {
        Self {
            words: [0; WORDS],
            count: 0,
            input: PhantomData,
        }
    }
}

impl<T: Copy + Into<usize>, const WORDS: usize> HeldSet<T, WORDS> {
    fn location(value: T) -> (usize, u64) {
        let index = value.into();
        (index / 64, 1u64 << (index % 64))
    }

    pub(super) fn contains(&self, value: &T) -> bool {
        let (word, bit) = Self::location(*value);
        self.words[word] & bit != 0
    }

    pub(super) fn insert(&mut self, value: T) -> bool {
        let (word, bit) = Self::location(value);
        let bits = &mut self.words[word];
        if *bits & bit != 0 {
            return false;
        }
        *bits |= bit;
        self.count += 1;
        true
    }

    pub(super) fn remove(&mut self, value: &T) -> bool {
        let (word, bit) = Self::location(*value);
        let bits = &mut self.words[word];
        if *bits & bit == 0 {
            return false;
        }
        *bits &= !bit;
        self.count -= 1;
        true
    }

    pub(super) fn len(&self) -> usize {
        self.count
    }

    pub(super) fn is_empty(&self) -> bool {
        self.count == 0
    }
}

impl<T: Copy + Into<usize>, const WORDS: usize> FromIterator<T> for HeldSet<T, WORDS> {
    fn from_iter<I: IntoIterator<Item = T>>(values: I) -> Self {
        let mut held = Self::default();
        for value in values {
            held.insert(value);
        }
        held
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn holds_track_duplicates_and_full_code_ranges_without_allocation() {
        let allocated = crate::allocation_checks::count(|| {
            let mut keys: HeldKeys = [0xe0, 0x4027, 0xffff].into_iter().collect();
            assert_eq!(keys.len(), 3);
            assert!(keys.contains(&0x4027));
            assert!(!keys.insert(0x4027));
            assert!(keys.remove(&0x4027));
            assert!(!keys.remove(&0x4027));
            assert!(keys.remove(&0xffff));
            assert!(keys.remove(&0xe0));
            assert!(keys.is_empty());

            let mut buttons: HeldButtons = [1, 32, u8::MAX].into_iter().collect();
            assert_eq!(buttons.len(), 3);
            assert!(buttons.contains(&u8::MAX));
            assert!(!buttons.insert(32));
            assert!(buttons.remove(&32));
            assert!(!buttons.remove(&32));
            assert_eq!(buttons.len(), 2);
        });
        assert_eq!(allocated, 0);
    }
}
