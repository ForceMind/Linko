//! Application-local gain shared by the native output owners.
pub(super) fn scale_sample(sample: i16, volume_percent: u8) -> i16 {
    (i32::from(sample) * i32::from(volume_percent) / 100) as i16
}
