//! Read-only local chassis classification. Cache only the coarse result, never
//! model/serial strings; no network, configuration or peer metadata is involved.
use super::visual_assets::DeviceKind;
use std::sync::OnceLock;

static LOCAL_KIND: OnceLock<DeviceKind> = OnceLock::new();

pub(super) fn local_device_kind() -> DeviceKind {
    detect_once(&LOCAL_KIND, probe)
}

fn detect_once(cache: &OnceLock<DeviceKind>, probe: impl FnOnce() -> DeviceKind) -> DeviceKind {
    *cache.get_or_init(probe)
}

#[cfg(any(windows, target_os = "linux", test))]
fn chassis_kind(value: u8) -> DeviceKind {
    // DMTF DSP0134 3.8 table 17. Tablet, detachable, server and other ambiguous
    // forms have no matching artwork and deliberately remain Unknown.
    match value {
        8..=10 | 14 | 31 => DeviceKind::Laptop,
        3..=7 | 13 | 15 | 35 | 36 => DeviceKind::Desktop,
        _ => DeviceKind::Unknown,
    }
}

#[cfg(any(target_os = "macos", test))]
fn mac_model_kind(model: &str) -> DeviceKind {
    if model.len() > 127 {
        return DeviceKind::Unknown;
    }
    for (prefix, kind) in [
        ("MacBookPro", DeviceKind::Laptop),
        ("MacBookAir", DeviceKind::Laptop),
        ("MacBook", DeviceKind::Laptop),
        ("iMacPro", DeviceKind::Desktop),
        ("iMac", DeviceKind::Desktop),
        ("Macmini", DeviceKind::Desktop),
        ("MacStudio", DeviceKind::Desktop),
        ("MacPro", DeviceKind::Desktop),
    ] {
        if model
            .strip_prefix(prefix)
            .and_then(|suffix| suffix.split_once(','))
            .is_some_and(|(generation, revision)| {
                !generation.is_empty()
                    && !revision.is_empty()
                    && generation.bytes().all(|byte| byte.is_ascii_digit())
                    && revision.bytes().all(|byte| byte.is_ascii_digit())
            })
        {
            return kind;
        }
    }
    // Apple silicon identifiers such as Mac14,2 do not encode a form factor.
    DeviceKind::Unknown
}

#[cfg(target_os = "macos")]
fn probe() -> DeviceKind {
    use std::{ffi::c_void, ptr};
    #[link(name = "System")]
    unsafe extern "C" {
        fn sysctlbyname(
            name: *const std::ffi::c_char,
            output: *mut c_void,
            length: *mut usize,
            new_value: *mut c_void,
            new_length: usize,
        ) -> i32;
    }
    let mut bytes = [0u8; 128];
    let mut length = bytes.len();
    // hw.model only; no IORegistry, serial number or hardware UUID queries.
    let status = unsafe {
        sysctlbyname(
            c"hw.model".as_ptr(),
            bytes.as_mut_ptr().cast(),
            &mut length,
            ptr::null_mut(),
            0,
        )
    };
    let kind = if status == 0 && length > 0 && length <= bytes.len() {
        let value = &bytes[..length];
        let value = value.strip_suffix(&[0]).unwrap_or(value);
        std::str::from_utf8(value).map_or(DeviceKind::Unknown, mac_model_kind)
    } else {
        DeviceKind::Unknown
    };
    bytes.fill(0);
    kind
}

#[cfg(any(windows, test))]
const MAX_FIRMWARE_BYTES: usize = 1024 * 1024;

#[cfg(any(windows, test))]
fn raw_smbios_kind(raw: &[u8]) -> DeviceKind {
    // RawSMBIOSData: four version/calling-method bytes, little-endian Length,
    // followed by the SMBIOS structures. Only Type 3 offset 5 is interpreted.
    if !(8..=MAX_FIRMWARE_BYTES).contains(&raw.len()) {
        return DeviceKind::Unknown;
    }
    let length = u32::from_le_bytes([raw[4], raw[5], raw[6], raw[7]]) as usize;
    if length != raw.len() - 8 {
        return DeviceKind::Unknown;
    }
    smbios_kind(&raw[8..]).unwrap_or(DeviceKind::Unknown)
}

#[cfg(any(windows, test))]
fn smbios_kind(table: &[u8]) -> Option<DeviceKind> {
    let mut offset = 0usize;
    let mut kind = None;
    while offset < table.len() {
        let header = table.get(offset..offset.checked_add(4)?)?;
        let length = usize::from(header[1]);
        if length < 4 {
            return None;
        }
        let end = offset.checked_add(length)?;
        let formatted = table.get(offset..end)?;
        // Skip the unformatted string set without decoding, retaining or logging
        // any strings (which may contain serial/asset identifiers).
        let terminator = table.get(end..)?.windows(2).position(|pair| pair == [0, 0])?;
        let next = end.checked_add(terminator)?.checked_add(2)?;
        if header[0] == 127 {
            return if length == 4 && terminator == 0 { kind } else { None };
        }
        if header[0] == 3 {
            if length < 9 {
                return None;
            }
            let found = chassis_kind(formatted[5] & 0x7f); // bit 7 is the chassis-lock flag
            if kind.is_some_and(|previous| previous != found) {
                return None;
            }
            kind = Some(found);
        }
        offset = next;
    }
    None // Missing end-of-table is not proof of a complete firmware snapshot.
}

#[cfg(windows)]
fn probe() -> DeviceKind {
    use std::{ffi::c_void, ptr};
    #[link(name = "kernel32")]
    unsafe extern "system" {
        #[link_name = "GetSystemFirmwareTable"]
        fn get_system_firmware_table(provider: u32, table: u32, buffer: *mut c_void, size: u32) -> u32;
    }
    const RSMB: u32 = 0x5253_4d42; // C provider signature 'RSMB'; table id is zero.
    let size = unsafe { get_system_firmware_table(RSMB, 0, ptr::null_mut(), 0) } as usize;
    if !(8..=MAX_FIRMWARE_BYTES).contains(&size) {
        return DeviceKind::Unknown;
    }
    let mut raw = vec![0u8; size];
    let written = unsafe {
        get_system_firmware_table(RSMB, 0, raw.as_mut_ptr().cast(), size as u32)
    } as usize;
    let kind = if written == size { raw_smbios_kind(&raw) } else { DeviceKind::Unknown };
    raw.fill(0); // Best-effort temporary-buffer clearing, not a secure-memory claim.
    kind
}

#[cfg(any(target_os = "linux", test))]
fn sysfs_chassis_kind(bytes: &[u8]) -> DeviceKind {
    if bytes.len() > 32 {
        return DeviceKind::Unknown;
    }
    let Ok(value) = std::str::from_utf8(bytes) else {
        return DeviceKind::Unknown;
    };
    let value = value.trim();
    if value.is_empty() || value.len() > 3 || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return DeviceKind::Unknown;
    }
    value.parse::<u8>().map_or(DeviceKind::Unknown, chassis_kind)
}

#[cfg(target_os = "linux")]
fn probe() -> DeviceKind {
    use std::{fs::File, io::Read};
    let Ok(file) = File::open("/sys/class/dmi/id/chassis_type") else {
        return DeviceKind::Unknown;
    };
    let mut bytes = Vec::with_capacity(33);
    if file.take(33).read_to_end(&mut bytes).is_err() {
        return DeviceKind::Unknown;
    }
    sysfs_chassis_kind(&bytes)
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
fn probe() -> DeviceKind {
    DeviceKind::Unknown
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(table: &[u8]) -> Vec<u8> {
        let mut bytes = vec![0, 3, 8, 0];
        bytes.extend_from_slice(&(table.len() as u32).to_le_bytes());
        bytes.extend_from_slice(table);
        bytes
    }

    fn table(chassis: u8) -> Vec<u8> {
        // Synthetic Type 3: no manufacturer, model, serial or asset strings.
        vec![3, 9, 1, 0, 0, chassis, 0, 0, 0, 0, 0, 127, 4, 2, 0, 0, 0]
    }

    #[test]
    fn explicit_mac_families_are_classified_without_guessing_generic_mac_ids() {
        for model in ["MacBook10,1", "MacBookPro18,3", "MacBookAir10,1"] {
            assert_eq!(mac_model_kind(model), DeviceKind::Laptop);
        }
        for model in ["iMac20,1", "iMacPro1,1", "Macmini9,1", "MacStudio1,1", "MacPro7,1"] {
            assert_eq!(mac_model_kind(model), DeviceKind::Desktop);
        }
        for model in ["Mac14,2", "Mac15,7", "MacBook", "MacBookProX,1", "iMac1,", "Other"] {
            assert_eq!(mac_model_kind(model), DeviceKind::Unknown);
        }
    }

    #[test]
    fn smbios_chassis_and_lock_bit_use_only_the_declared_form_factor() {
        for chassis in [8, 9, 10, 14, 31, 0x89] {
            assert_eq!(raw_smbios_kind(&raw(&table(chassis))), DeviceKind::Laptop);
        }
        for chassis in [3, 4, 5, 6, 7, 13, 15, 35, 36, 0x83] {
            assert_eq!(raw_smbios_kind(&raw(&table(chassis))), DeviceKind::Desktop);
        }
        for chassis in [0, 1, 2, 11, 12, 17, 23, 30, 32, 127, 255] {
            assert_eq!(raw_smbios_kind(&raw(&table(chassis))), DeviceKind::Unknown);
        }
    }

    #[test]
    fn truncated_and_inconsistent_firmware_never_certifies_a_type() {
        let complete = raw(&table(9));
        for length in 0..complete.len() {
            assert_eq!(raw_smbios_kind(&complete[..length]), DeviceKind::Unknown);
        }
        let structures = table(9);
        for length in 0..structures.len() {
            // Also exercise truncated records with a self-consistent outer Length.
            assert_eq!(raw_smbios_kind(&raw(&structures[..length])), DeviceKind::Unknown);
        }
        let mut bad = complete.clone();
        bad[4] += 1;
        assert_eq!(raw_smbios_kind(&bad), DeviceKind::Unknown);
        assert_eq!(raw_smbios_kind(&vec![0; MAX_FIRMWARE_BYTES + 1]), DeviceKind::Unknown);
        for table in [vec![3, 3, 0, 0], vec![3, 255, 0, 0], vec![3, 5, 0, 0, 0, 0, 0]] {
            assert_eq!(raw_smbios_kind(&raw(&table)), DeviceKind::Unknown);
        }
    }

    #[test]
    fn missing_or_conflicting_type_three_records_stay_unknown() {
        assert_eq!(raw_smbios_kind(&raw(&[127, 4, 0, 0, 0, 0])), DeviceKind::Unknown);
        let mut conflict = table(9);
        conflict.truncate(11);
        conflict.extend_from_slice(&table(3));
        assert_eq!(raw_smbios_kind(&raw(&conflict)), DeviceKind::Unknown);
        let mut missing_end = table(9);
        missing_end.truncate(11);
        assert_eq!(raw_smbios_kind(&raw(&missing_end)), DeviceKind::Unknown);
    }

    #[test]
    fn bounded_linux_chassis_text_never_defaults_on_failure() {
        assert_eq!(sysfs_chassis_kind(b"9\n"), DeviceKind::Laptop);
        assert_eq!(sysfs_chassis_kind(b"3\n"), DeviceKind::Desktop);
        for bytes in [&b""[..], &b"unknown\n"[..], &b"+3"[..], &b"30\n"[..], &b"\xff"[..], &[b'9'; 33][..]] {
            assert_eq!(sysfs_chassis_kind(bytes), DeviceKind::Unknown);
        }
    }

    #[test]
    fn failed_probe_result_is_cached_once_without_reading_hardware_in_tests() {
        let cache = OnceLock::new();
        let calls = std::cell::Cell::new(0);
        for _ in 0..3 {
            assert_eq!(detect_once(&cache, || {
                calls.set(calls.get() + 1);
                DeviceKind::Unknown
            }), DeviceKind::Unknown);
        }
        assert_eq!(calls.get(), 1);
        assert_eq!(detect_once(&cache, || DeviceKind::Laptop), DeviceKind::Unknown);
    }
}
