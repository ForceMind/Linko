//! The single local XInput2 observation owner. Never logs input payloads.
use super::{capture_thread::Reservation, x11_inject::local_connection};
use anyhow::{ensure, Context, Result};
use x11rb::{
    connection::Connection as _,
    protocol::{
        xinput::{ConnectionExt as _, DeviceType, EventMask, XIDeviceInfo, XIEventMask},
        xproto::Window,
        Event,
    },
    rust_connection::RustConnection,
};

mod source;
pub use source::{PointerSample, PointerSource};

const ALL_DEVICES: u16 = 0;
const ALL_MASTERS: u16 = 1;
const MAX_EVENTS: usize = 64;
const XTEST_POINTER: &[u8] = b"Virtual core XTEST pointer";
const XTEST_KEYBOARD: &[u8] = b"Virtual core XTEST keyboard";

pub struct Activity {
    connection: RustConnection,
    root: Window,
    xtest: [u16; 2],
    _reservation: Reservation,
}

impl Activity {
    /// The receiver observes activity without grabbing input.
    pub fn open_for_local_takeover() -> Result<Self> {
        Self::open().map(|(activity, _)| activity)
    }

    fn open() -> Result<(Self, Vec<XIDeviceInfo>)> {
        let reservation = Reservation::acquire()?;
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
            unique(pointer.map(|device| device.deviceid))
                .context("XTEST pointer source ambiguous")?,
            unique(keyboard.map(|device| device.deviceid))
                .context("XTEST keyboard source ambiguous")?,
        ];
        ensure!(xtest[0] != xtest[1], "XTEST sources are ambiguous");
        let masks = [
            EventMask {
                deviceid: ALL_MASTERS,
                mask: vec![
                    XIEventMask::RAW_KEY_PRESS
                        | XIEventMask::RAW_KEY_RELEASE
                        | XIEventMask::RAW_BUTTON_PRESS
                        | XIEventMask::RAW_BUTTON_RELEASE
                        | XIEventMask::RAW_MOTION,
                ],
            },
            EventMask {
                deviceid: ALL_DEVICES,
                mask: vec![XIEventMask::HIERARCHY],
            },
        ];
        connection.xinput_xi_select_events(root, &masks)?.check()?;
        Ok((
            Self {
                connection,
                root,
                xtest,
                _reservation: reservation,
            },
            reply.infos,
        ))
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
    #[ignore = "requires the dedicated GitHub Actions Xvfb server"]
    fn isolated_xvfb_pointer_moves_only_after_local_admission() -> Result<()> {
        use crate::{x11_inject::Injector, ReceivePoint};
        use x11rb::protocol::xproto::{
            ChangeWindowAttributesAux, ConnectionExt as _, EventMask as CoreEventMask,
        };

        ensure!(
            std::env::var("GITHUB_ACTIONS").ok().as_deref() == Some("true")
                && std::env::var("LINKO_TEST_ISOLATED_X11").ok().as_deref() == Some("1"),
            "this test requires the dedicated CI display"
        );
        let activity = Activity::open_for_local_takeover()?;
        let mut injector = Injector::open_after_explicit_consent()?;
        let roots = &activity.connection.setup().roots;
        ensure!(roots.len() == 1, "CI display must have one screen");
        let root = roots[0].root;
        let bounds = activity.connection.get_geometry(root)?.reply()?;
        ensure!(
            bounds.width == 1024 && bounds.height == 768,
            "unexpected CI display geometry"
        );
        let before = activity.connection.query_pointer(root)?.reply()?;
        let (point, expected) = if before.root_x == 0 && before.root_y == 0 {
            (ReceivePoint { x: 1.0, y: 1.0 }, (1023, 767))
        } else {
            (ReceivePoint { x: 0.0, y: 0.0 }, (0, 0))
        };
        assert!(!injector.pointer_if(point, || Ok(false))?);
        let blocked = activity.connection.query_pointer(root)?.reply()?;
        assert_eq!(
            (blocked.root_x, blocked.root_y),
            (before.root_x, before.root_y)
        );
        assert!(injector.pointer_if(point, || Ok(true))?);
        let after = activity.connection.query_pointer(root)?.reply()?;
        assert_eq!((after.root_x, after.root_y), expected);
        assert!(!activity.local_activity()?);

        let keys_before = activity.connection.query_keymap()?.reply()?.keys;
        let buttons_before = after.mask;
        let usage = injector
            .key_usages()
            .next()
            .context("CI XKB key mapping unavailable")?;
        assert!(injector.key(usage, true)?);
        assert!(injector.button(1, true)?);
        assert_ne!(
            activity.connection.query_keymap()?.reply()?.keys,
            keys_before
        );
        assert_ne!(
            activity.connection.query_pointer(root)?.reply()?.mask,
            buttons_before
        );
        assert!(!activity.local_activity()?);
        injector.release_owned()?;
        assert_eq!(
            activity.connection.query_keymap()?.reply()?.keys,
            keys_before
        );
        assert_eq!(
            activity.connection.query_pointer(root)?.reply()?.mask,
            buttons_before
        );
        assert!(!activity.local_activity()?);

        activity
            .connection
            .change_window_attributes(
                root,
                &ChangeWindowAttributesAux::new()
                    .event_mask(CoreEventMask::BUTTON_PRESS | CoreEventMask::BUTTON_RELEASE),
            )?
            .check()?;
        assert!(injector.scroll(1, -1, || Ok(true))?);
        let mut pairs = Vec::new();
        for _ in 0..16 {
            match activity.connection.poll_for_event()? {
                Some(Event::ButtonPress(event)) => pairs.push((event.detail, true)),
                Some(Event::ButtonRelease(event)) => pairs.push((event.detail, false)),
                Some(_) => (),
                None => break,
            }
        }
        assert_eq!(pairs, [(5, true), (5, false), (7, true), (7, false)]);

        drop(activity);
        let observer = local_connection()?.0;
        observer
            .change_window_attributes(
                root,
                &ChangeWindowAttributesAux::new()
                    .event_mask(CoreEventMask::BUTTON_PRESS | CoreEventMask::BUTTON_RELEASE),
            )?
            .check()?;
        let mut source = PointerSource::open()?;
        injector.pointer(ReceivePoint { x: 1.0, y: 0.5 })?;
        assert!(matches!(source.poll()?, PointerSample::Idle));
        let edge = peerglide_protocol::Edge::Right;
        let along = source
            .edge_fraction(edge)?
            .context("right edge not reached")?;
        assert!(source.claim(edge, along)?);
        assert!(injector.button(2, true)?);
        assert!(matches!(source.poll()?, PointerSample::LocalInput));
        assert!(observer.poll_for_event()?.is_none());
        source.release(|| {})?;
        assert!(injector.button(2, false)?);
        let mut local_pair = Vec::new();
        for _ in 0..16 {
            match observer.poll_for_event()? {
                Some(Event::ButtonPress(event)) => local_pair.push((event.detail, true)),
                Some(Event::ButtonRelease(event)) => local_pair.push((event.detail, false)),
                Some(_) => (),
                None => break,
            }
        }
        assert_eq!(local_pair, [(3, true), (3, false)]);
        Ok(())
    }

    #[test]
    fn takeover_uses_only_source_and_topology_without_accessing_input_payloads() {
        let xtest = [5, 6];
        for sourceid in [5, 6] {
            assert!(!takeover(
                &Event::XinputRawMotion(RawMotionEvent {
                    sourceid,
                    ..Default::default()
                }),
                xtest
            ));
        }
        for sourceid in [0, 14] {
            assert!(takeover(
                &Event::XinputRawKeyPress(RawKeyPressEvent {
                    sourceid,
                    ..Default::default()
                }),
                xtest
            ));
        }
        assert!(takeover(
            &Event::XinputHierarchy(HierarchyEvent::default()),
            xtest
        ));
        assert_eq!(unique([5].into_iter()), Some(5));
        assert_eq!(unique([5, 6].into_iter()), None);
        assert_eq!(unique([0].into_iter()), None);
    }
}
