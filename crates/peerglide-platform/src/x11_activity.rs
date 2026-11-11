//! Read-only XInput2 activity for local takeover. Never logs input payloads.
use super::x11_inject::local_connection;
use anyhow::{ensure, Context, Result};
use x11rb::{
    connection::Connection as _,
    protocol::{
        xinput::{ConnectionExt as _, DeviceType, EventMask, XIEventMask},
        Event,
    },
    rust_connection::RustConnection,
};

const ALL_DEVICES: u16 = 0;
const ALL_MASTERS: u16 = 1;
const MAX_EVENTS: usize = 64;
const XTEST_POINTER: &[u8] = b"Virtual core XTEST pointer";
const XTEST_KEYBOARD: &[u8] = b"Virtual core XTEST keyboard";

pub struct Activity {
    connection: RustConnection,
    xtest: [u16; 2],
}

impl Activity {
    /// This is the Linux receiver's only native input observation subscription.
    /// It never grabs input and opens only inside the local X11 session.
    pub fn open_for_local_takeover() -> Result<Self> {
        let (connection, screen) = local_connection()?;
        let version = connection.xinput_xi_query_version(2, 2)?.reply()?;
        ensure!(
            version.major_version > 2 || (version.major_version == 2 && version.minor_version >= 2),
            "XInput2 source identification unavailable"
        );
        let root = connection
            .setup()
            .roots
            .get(screen)
            .context("local X11 root unavailable")?
            .root;
        let reply = connection.xinput_xi_query_device(ALL_DEVICES)?.reply()?;
        let pointer = reply.infos.iter().filter(|device| {
            device.enabled
                && device.type_ == DeviceType::SLAVE_POINTER
                && device.name == XTEST_POINTER
        });
        let keyboard = reply.infos.iter().filter(|device| {
            device.enabled
                && device.type_ == DeviceType::SLAVE_KEYBOARD
                && device.name == XTEST_KEYBOARD
        });
        let xtest = [
            unique(pointer.map(|device| device.deviceid)).context("XTEST pointer source ambiguous")?,
            unique(keyboard.map(|device| device.deviceid)).context("XTEST keyboard source ambiguous")?,
        ];
        ensure!(xtest[0] != xtest[1], "XTEST sources are ambiguous");
        let masks = [
            EventMask {
                deviceid: ALL_MASTERS,
                mask: vec![XIEventMask::RAW_KEY_PRESS
                    | XIEventMask::RAW_KEY_RELEASE
                    | XIEventMask::RAW_BUTTON_PRESS
                    | XIEventMask::RAW_BUTTON_RELEASE
                    | XIEventMask::RAW_MOTION],
            },
            EventMask {
                deviceid: ALL_DEVICES,
                mask: vec![XIEventMask::HIERARCHY],
            },
        ];
        connection.xinput_xi_select_events(root, &masks)?.check()?;
        Ok(Self { connection, xtest })
    }

    /// True means reclaim the local computer before the next remote effect.
    /// A busy or changed device stream also requires local recovery.
    pub fn local_activity(&self) -> Result<bool> {
        for _ in 0..MAX_EVENTS {
            let Some(event) = self.connection.poll_for_event()? else {
                return Ok(false);
            };
            if takeover(&event, self.xtest) {
                return Ok(true);
            }
        }
        Ok(true)
    }
}

fn unique(mut devices: impl Iterator<Item = u16>) -> Option<u16> {
    let id = devices.next()?;
    (id != 0 && devices.next().is_none()).then_some(id)
}

fn takeover(event: &Event, xtest: [u16; 2]) -> bool {
    let source = match event {
        Event::XinputRawKeyPress(event) | Event::XinputRawKeyRelease(event) => event.sourceid,
        Event::XinputRawButtonPress(event)
        | Event::XinputRawButtonRelease(event)
        | Event::XinputRawMotion(event) => event.sourceid,
        Event::XinputHierarchy(_) | Event::Error(_) => return true,
        _ => return false,
    };
    !xtest.contains(&source)
}

#[cfg(test)]
mod tests {
    use super::*;
    use x11rb::protocol::xinput::{HierarchyEvent, RawKeyPressEvent, RawMotionEvent};

    #[test]
    fn takeover_uses_only_source_and_topology_without_accessing_input_payloads() {
        let xtest = [5, 6];
        for sourceid in [5, 6] {
            assert!(!takeover(&Event::XinputRawMotion(RawMotionEvent {
                sourceid,
                ..Default::default()
            }), xtest));
        }
        for sourceid in [0, 14] {
            assert!(takeover(&Event::XinputRawKeyPress(RawKeyPressEvent {
                sourceid,
                ..Default::default()
            }), xtest));
        }
        assert!(takeover(&Event::XinputHierarchy(HierarchyEvent::default()), xtest));
        assert_eq!(unique([5].into_iter()), Some(5));
        assert_eq!(unique([5, 6].into_iter()), None);
        assert_eq!(unique([0].into_iter()), None);
    }
}
