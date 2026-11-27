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

    /// Held values in `start..end`, counted from the fixed words only.
    pub(super) fn count_range(&self, start: usize, end: usize) -> usize {
        let end = end.min(WORDS * 64);
        let mut index = start;
        let mut total = 0;
        while index < end {
            let offset = index % 64;
            let width = (64 - offset).min(end - index);
            let mask = if width == 64 {
                u64::MAX
            } else {
                ((1u64 << width) - 1) << offset
            };
            total += (self.words[index / 64] & mask).count_ones() as usize;
            index += width;
        }
        total
    }
}

impl<T: Copy + Into<usize>, const WORDS: usize> Extend<T> for HeldSet<T, WORDS> {
    fn extend<I: IntoIterator<Item = T>>(&mut self, values: I) {
        for value in values {
            self.insert(value);
        }
    }
}

impl<T: Copy + Into<usize>, const WORDS: usize> FromIterator<T> for HeldSet<T, WORDS> {
    fn from_iter<I: IntoIterator<Item = T>>(values: I) -> Self {
        let mut held = Self::default();
        held.extend(values);
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

    #[test]
    fn range_counts_split_categories_without_allocation() {
        let keys: HeldKeys = [4, 0x3f, 0x40, 0xe0, 0xe7, 0x8004].into_iter().collect();
        let buttons: HeldButtons = [1, 3, 15, 200].into_iter().collect();
        let allocated = crate::allocation_checks::count(|| {
            assert_eq!(keys.count_range(0, 0x100), 5);
            assert_eq!(keys.count_range(0xe0, 0xe8), 2);
            assert_eq!(keys.count_range(0x3f, 0x41), 2);
            assert_eq!(keys.count_range(0x100, usize::MAX), 1);
            assert_eq!(keys.count_range(5, 5), 0);
            assert_eq!(buttons.count_range(0, 15), 2);
            assert_eq!(buttons.count_range(15, 256), 2);
        });
        assert_eq!(allocated, 0);
    }
}
