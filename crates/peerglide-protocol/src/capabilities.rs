//! Finite receiver capabilities. Metadata never grants authorization.
use crate::Input;
use serde::{Deserialize, Serialize};
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputCapabilities {
    keys: [u8; 32],
    buttons: u8,
    motion: bool,
    scroll: bool,
}
impl InputCapabilities {
    pub fn new(
        keys: &[u16],
        motion: bool,
        buttons: u8,
        scroll: bool,
    ) -> Result<Self, &'static str> {
        let mut result = Self {
            motion,
            buttons,
            scroll,
            ..Self::default()
        };
        for &usage in keys {
            if !(Input::Key { usage, down: true }).valid() {
                return Err("invalid key capability");
            }
            result.keys[usize::from(usage / 8)] |= 1 << (usage % 8);
        }
        if !result.valid() {
            return Err("invalid input capabilities");
        }
        Ok(result)
    }
    pub fn valid(&self) -> bool {
        self.buttons & !7 == 0
            && (0..256u16).all(|usage| {
                self.keys[usize::from(usage / 8)] & (1 << (usage % 8)) == 0
                    || Input::Key { usage, down: true }.valid()
            })
    }
    pub fn motion(&self) -> bool {
        self.motion
    }
    pub fn supports(&self, event: Input) -> bool {
        if !event.valid() {
            return false;
        }
        match event {
            Input::Key { usage, .. } => self.keys[usize::from(usage / 8)] & (1 << (usage % 8)) != 0,
            Input::Button { button, .. } => self.buttons & (1 << (button - 1)) != 0,
            Input::Scroll { .. } => self.scroll,
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn finite_capabilities_do_not_imply_permission_or_expand_inputs() {
        let caps = InputCapabilities::new(&[4, 0xe0], true, 1, false).unwrap();
        assert!(caps.valid());
        assert!(caps.supports(Input::Key {
            usage: 4,
            down: false
        }));
        assert!(!caps.supports(Input::Key {
            usage: 5,
            down: true
        }));
        assert!(!caps.supports(Input::Button {
            button: 0,
            down: true
        }));
        assert!(!caps.supports(Input::Button {
            button: 2,
            down: true
        }));
        assert!(!caps.supports(Input::Scroll { x: 0, y: 1 }));
        assert!(!caps.supports(Input::Key {
            usage: u16::MAX,
            down: true
        }));
        assert!(!InputCapabilities::default().motion());
        assert!(InputCapabilities::new(&[0], true, 7, true).is_err());
        assert!(InputCapabilities::new(&[], true, 8, true).is_err());
    }
    #[test]
    fn malformed_wire_capability_bits_are_rejected() {
        let mut caps = InputCapabilities::default();
        caps.keys[0] = 1;
        let decoded: InputCapabilities =
            serde_json::from_slice(&serde_json::to_vec(&caps).unwrap()).unwrap();
        assert!(!decoded.valid());
        assert!(serde_json::from_str::<InputCapabilities>(
            "{\"keys\":[],\"buttons\":0,\"motion\":true,\"scroll\":false}"
        )
        .is_err());
    }
}
