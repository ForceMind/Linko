//! Convert XKB physical positions to v5 HID usages, independent of keysyms.
use anyhow::{ensure, Context, Result};
use x11rb::{
    protocol::xkb::{ConnectionExt as _, NameDetail, ID},
    rust_connection::RustConnection,
};

pub(super) struct KeyMap {
    codes: [Option<u8>; 256],
}

impl KeyMap {
    pub(super) fn load(connection: &RustConnection) -> Result<Self> {
        let extension = connection.xkb_use_extension(1, 0)?.reply()?;
        ensure!(extension.supported, "X11 XKB extension unavailable");
        let reply = connection
            .xkb_get_names(u16::from(ID::USE_CORE_KBD), NameDetail::KEY_NAMES)?
            .reply()?;
        let names = reply.value_list.key_names.context("XKB key positions unavailable")?;
        ensure!(
            names.len() == usize::from(reply.n_keys)
                && reply.first_key >= reply.min_key_code
                && reply.first_key >= 8
                && usize::from(reply.first_key) + names.len() <= usize::from(reply.max_key_code) + 1,
            "invalid XKB key position count"
        );
        Ok(Self::from_names(
            reply.first_key,
            names.into_iter().map(|key| key.name),
        ))
    }

    fn from_names(first: u8, names: impl IntoIterator<Item = [u8; 4]>) -> Self {
        let mut codes = [None; 256];
        let mut ambiguous = [false; 256];
        for (offset, name) in names.into_iter().take(256).enumerate() {
            let Ok(code) = u8::try_from(usize::from(first) + offset) else {
                break;
            };
            let Some(usage) = usage(name) else {
                continue;
            };
            let index = usize::from(usage);
            if codes[index].is_some() {
                ambiguous[index] = true;
                codes[index] = None;
            } else if !ambiguous[index] {
                codes[index] = Some(code);
            }
        }
        Self { codes }
    }

    pub(super) fn code(&self, usage: u16) -> Option<u8> {
        self.codes.get(usize::from(usage)).copied().flatten()
    }

    pub(super) fn usages(&self) -> impl Iterator<Item = u16> + '_ {
        self.codes
            .iter()
            .enumerate()
            .filter_map(|(usage, code)| code.is_some().then_some(usage as u16))
    }
}

fn usage(name: [u8; 4]) -> Option<u16> {
    const AE: [u16; 12] = [
        0x1e, 0x1f, 0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x2d, 0x2e,
    ];
    const AD: [u16; 12] = [
        0x14, 0x1a, 0x08, 0x15, 0x17, 0x1c, 0x18, 0x0c, 0x12, 0x13, 0x2f, 0x30,
    ];
    const AC: [u16; 11] = [0x04, 0x16, 0x07, 0x09, 0x0a, 0x0b, 0x0d, 0x0e, 0x0f, 0x33, 0x34];
    const AB: [u16; 10] = [0x1d, 0x1b, 0x06, 0x19, 0x05, 0x11, 0x10, 0x36, 0x37, 0x38];
    if name[2].is_ascii_digit() && name[3].is_ascii_digit() {
        let index = usize::from(name[2] - b'0') * 10 + usize::from(name[3] - b'0');
        let row: &[u16] = match &name[..2] {
            b"AE" => &AE,
            b"AD" => &AD,
            b"AC" => &AC,
            b"AB" => &AB,
            b"FK" => {
                return match index {
                    1..=12 => Some(0x39 + index as u16),
                    13..=24 => Some(0x5b + index as u16),
                    _ => None,
                };
            }
            _ => return None,
        };
        return index.checked_sub(1).and_then(|index| row.get(index).copied());
    }
    let end = name
        .iter()
        .position(|&byte| matches!(byte, 0 | b' '))
        .unwrap_or(name.len());
    Some(match &name[..end] {
        b"ESC" => 0x29,
        b"RTRN" => 0x28,
        b"BKSP" => 0x2a,
        b"TAB" => 0x2b,
        b"SPCE" => 0x2c,
        b"BKSL" => 0x31,
        b"TLDE" => 0x35,
        b"CAPS" => 0x39,
        b"INS" => 0x49,
        b"HOME" => 0x4a,
        b"PGUP" => 0x4b,
        b"DELE" => 0x4c,
        b"END" => 0x4d,
        b"PGDN" => 0x4e,
        b"RGHT" => 0x4f,
        b"LEFT" => 0x50,
        b"DOWN" => 0x51,
        b"UP" => 0x52,
        b"NMLK" => 0x53,
        b"KPDV" => 0x54,
        b"KPMU" => 0x55,
        b"KPSU" => 0x56,
        b"KPAD" => 0x57,
        b"KPEN" => 0x58,
        b"KP0" => 0x62,
        b"KP1" => 0x59,
        b"KP2" => 0x5a,
        b"KP3" => 0x5b,
        b"KP4" => 0x5c,
        b"KP5" => 0x5d,
        b"KP6" => 0x5e,
        b"KP7" => 0x5f,
        b"KP8" => 0x60,
        b"KP9" => 0x61,
        b"KPDL" => 0x63,
        b"LSGT" => 0x64,
        b"MENU" => 0x65,
        b"LCTL" => 0xe0,
        b"LFSH" => 0xe1,
        b"LALT" => 0xe2,
        b"LWIN" => 0xe3,
        b"RCTL" => 0xe4,
        b"RTSH" => 0xe5,
        b"RALT" => 0xe6,
        b"RWIN" => 0xe7,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xkb_names_select_physical_positions_and_reject_ambiguous_entries() {
        let map = KeyMap::from_names(
            40,
            [*b"AC01", *b"AD01", *b"KP1\0", *b"FK24", *b"AC01", *b"??00"],
        );
        assert_eq!(map.code(0x04), None);
        assert_eq!(map.code(0x14), Some(41));
        assert_eq!(map.code(0x59), Some(42));
        assert_eq!(map.code(0x73), Some(43));
        assert_eq!(map.usages().collect::<Vec<_>>(), [0x14, 0x59, 0x73]);
    }
}
