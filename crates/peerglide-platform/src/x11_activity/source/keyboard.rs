//! Keyboard ownership on the same X connection as the Xorg pointer source.
use super::{Activity, DeviceType};
use crate::x11_keymap::KeyMap;
use anyhow::{ensure, Context, Result};
use x11rb::{
    protocol::{
        xinput::{RawKeyPressEvent, XIDeviceInfo},
        xkb::{BoolCtrl, ConnectionExt as _, EventType, MapPart, PerClientFlag, SelectEventsAux, ID},
        xproto::{Allow, ConnectionExt as _, GrabMode, GrabStatus, KeyPressEvent},
    },
    CURRENT_TIME,
};

#[derive(Clone, Copy)]
struct Transition {
    detail: u32,
    time: u32,
    down: bool,
}

pub(super) struct Key {
    pub(super) usage: u16,
    pub(super) down: bool,
    pub(super) repeated: bool,
}

pub(super) struct Keyboard {
    map: KeyMap,
    devices: Vec<u16>,
    grabbed: bool,
    raw: Option<(Transition, bool)>,
    frozen: Option<(u8, bool, bool)>,
    held: [bool; 256],
}

impl Keyboard {
    pub(super) fn load(activity: &Activity, devices: &[XIDeviceInfo], pointer: u16) -> Result<Self> {
        let master = devices.iter().find(|device| device.deviceid == pointer)
            .context("Xorg paired keyboard unavailable")?.attachment;
        let devices = devices.iter().filter(|device| {
            device.enabled
                && device.type_ == DeviceType::SLAVE_KEYBOARD
                && device.attachment == master
                && !activity.xtest.contains(&device.deviceid)
        }).map(|device| device.deviceid).collect();
        let connection = &activity.connection;
        let map = KeyMap::load(connection)?;
        let device = u16::from(ID::USE_CORE_KBD);
        let flag = PerClientFlag::DETECTABLE_AUTO_REPEAT;
        let reply = connection.xkb_per_client_flags(
            device, flag, flag, BoolCtrl::default(), BoolCtrl::default(), BoolCtrl::default(),
        )?.reply()?;
        ensure!(reply.supported.contains(flag) && reply.value.contains(flag),
            "Xorg detectable key repeat unavailable");
        let events = EventType::NEW_KEYBOARD_NOTIFY | EventType::NAMES_NOTIFY | EventType::MAP_NOTIFY;
        let map_parts = MapPart::from(0xffu16);
        connection.xkb_select_events(
            device, 0u8.into(), events, map_parts, map_parts, &SelectEventsAux::default(),
        )?.check()?;
        Ok(Self {
            map,
            devices,
            grabbed: false,
            raw: None,
            frozen: None,
            held: [false; 256],
        })
    }

    pub(super) fn usages(&self) -> impl Iterator<Item = u16> + '_ {
        self.map.usages()
    }

    pub(super) fn grabbed(&self) -> bool {
        self.grabbed
    }

    pub(super) fn claim(&mut self, activity: &Activity) -> Result<bool> {
        let reply = activity.connection.grab_keyboard(
            false, activity.root, CURRENT_TIME, GrabMode::ASYNC, GrabMode::SYNC,
        )?.reply()?;
        self.grabbed = reply.status == GrabStatus::SUCCESS;
        Ok(self.grabbed)
    }

    pub(super) fn resume(&self, activity: &Activity) -> Result<()> {
        activity.connection.allow_events(Allow::SYNC_KEYBOARD, CURRENT_TIME)?.check()?;
        Ok(())
    }

    pub(super) fn remember(&mut self, event: &RawKeyPressEvent, down: bool) {
        let transition = Transition { detail: event.detail, time: event.time, down };
        let duplicate = self.raw.is_some_and(|(raw, _)| {
            raw.detail == transition.detail && raw.time == transition.time && raw.down == down
        });
        self.raw = Some((transition, !duplicate && self.devices.contains(&event.sourceid)));
    }

    pub(super) fn freeze(
        &mut self,
        activity: &Activity,
        event: &KeyPressEvent,
        down: bool,
        local_input: bool,
    ) -> Option<Key> {
        if event.response_type & 0x80 != 0 || event.event != activity.root {
            return None;
        }
        let raw = self.raw.take();
        let trusted = raw.is_some_and(|(raw, physical)| {
            physical && raw.detail == u32::from(event.detail) && raw.time == event.time && raw.down == down
        });
        let code = event.detail;
        let repeated = down && self.held[usize::from(code)];
        let usage = self.map.usage(code);
        let admitted = !local_input && (trusted || repeated) && usage.is_some_and(|usage| {
            !self.emergency(usage, down) && (down || self.held[usize::from(code)])
        });
        self.frozen = Some((code, down, admitted));
        usage.filter(|_| admitted).map(|usage| Key { usage, down, repeated })
    }

    fn emergency(&self, usage: u16, down: bool) -> bool {
        let pressed = |usages: [u16; 2]| usages.into_iter().any(|usage| {
            self.map.code(usage).is_some_and(|code| self.held[usize::from(code)])
        });
        down && usage == 0x29 && pressed([0xe0, 0xe4]) && pressed([0xe2, 0xe6])
    }

    pub(super) fn accept(&mut self, activity: &Activity, usage: u16, down: bool) -> Result<()> {
        let code = self.map.code(usage).context("Xorg key position unavailable")?;
        ensure!(self.frozen == Some((code, down, true)), "Xorg key ownership changed");
        self.consume(activity)
    }

    pub(super) fn pending(&self) -> bool {
        self.frozen.is_some()
    }

    pub(super) fn has_held(&self) -> bool {
        self.held.contains(&true)
    }

    pub(super) fn consume(&mut self, activity: &Activity) -> Result<()> {
        let (code, down, _) = self.frozen.context("Xorg key is not frozen")?;
        self.resume(activity)?;
        self.frozen = None;
        self.held[usize::from(code)] = down;
        Ok(())
    }

    pub(super) fn release(&mut self, activity: &Activity, replay: bool) -> Result<()> {
        if !self.grabbed {
            return Ok(());
        }
        let restore = if replay {
            activity.connection.allow_events(Allow::REPLAY_KEYBOARD, CURRENT_TIME)?.check()
        } else {
            Ok(())
        };
        activity.connection.ungrab_keyboard(CURRENT_TIME)?.check()?;
        self.grabbed = false;
        restore?;
        self.raw = None;
        self.frozen = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::{PointerSample, PointerSource};
    use crate::{x11_inject::{local_connection, Injector}, ReceivePoint};
    use peerglide_protocol::Edge;
    use x11rb::{
        connection::Connection as _,
        protocol::{xproto::{ChangeWindowAttributesAux, EventMask, InputFocus, KEY_PRESS_EVENT}, Event},
    };

    #[test]
    #[ignore = "requires the dedicated GitHub Actions Xvfb server"]
    fn xorg_keyboard_retires_captured_modifiers_and_restores_local_keys() -> Result<()> {
        ensure!(
            std::env::var("GITHUB_ACTIONS").ok().as_deref() == Some("true")
                && std::env::var("LINKO_TEST_ISOLATED_X11").ok().as_deref() == Some("1"),
            "this test requires the dedicated CI display"
        );
        let mut source = PointerSource::open(true)?;
        let mut injector = Injector::open_after_explicit_consent()?;
        let observer = local_connection()?.0;
        let root = source.activity.root;
        observer.change_window_attributes(
            root, &ChangeWindowAttributesAux::new().event_mask(EventMask::KEY_PRESS | EventMask::KEY_RELEASE),
        )?.check()?;
        observer.set_input_focus(InputFocus::PARENT, root, CURRENT_TIME)?.check()?;
        injector.pointer(ReceivePoint { x: 1.0, y: 0.5 })?;
        let along = source.edge_fraction(Edge::Right)?.context("right edge not reached")?;
        assert!(source.claim(Edge::Right, along, true)?);

        for usage in [0xe0, 0xe2] {
            assert!(injector.key(usage, true)?);
            assert!(matches!(source.poll()?, PointerSample::LocalInput));
            assert!(source.accept_key(usage, true).is_err());
            // This fixture prepares an already consumed pair without changing
            // the production rejection of injected keyboard input.
            source.keyboard.as_mut().unwrap().consume(&source.activity)?;
        }
        let keyboard = source.keyboard.as_mut().unwrap();
        assert!(keyboard.emergency(0x29, true));
        assert!(!keyboard.emergency(0x29, false));
        let code = keyboard.map.code(0xe0).context("control unavailable")?;
        let repeat = KeyPressEvent { response_type: KEY_PRESS_EVENT, detail: code, event: root, ..KeyPressEvent::default() };
        let key = keyboard.freeze(&source.activity, &repeat, true, false).context("held key repeat rejected")?;
        assert!(key.repeated && key.usage == 0xe0);
        keyboard.frozen = None;
        assert!(observer.poll_for_event()?.is_none());

        assert!(injector.key(0x29, true)?);
        assert!(matches!(source.poll()?, PointerSample::LocalInput));
        assert!(source.accept_key(0x29, true).is_err());
        for usage in [0x29, 0xe2, 0xe0] {
            assert!(injector.key(usage, false)?);
        }
        source.release(|| {})?;
        assert!(observer.poll_for_event()?.is_none());
        assert!(injector.key(0x29, true)?);
        assert!(injector.key(0x29, false)?);
        assert!(matches!(observer.poll_for_event()?, Some(Event::KeyPress(_))));
        assert!(matches!(observer.poll_for_event()?, Some(Event::KeyRelease(_))));
        Ok(())
    }
}
