//! Xorg pointer source using the receiver's existing XInput2 observation owner.
use super::{unique, Activity, DeviceType, Event, MAX_EVENTS};
use anyhow::{ensure, Context, Result};
use peerglide_protocol::Edge;
use std::{collections::BTreeMap, time::Duration};
use x11rb::{
    connection::Connection as _,
    protocol::{
        shape::{ConnectionExt as _, SK, SO},
        xinput::{
            ConnectionExt as _, DeviceClassData, Fp3232, RawButtonPressEvent, RawMotionEvent,
            ValuatorMode, XIDeviceInfo,
        },
        xproto::{
            Allow, ButtonPressEvent, ChangeWindowAttributesAux, ClipOrdering, ConnectionExt as _,
            CreateWindowAux, EventMask, GrabMode, GrabStatus, Mapping, Window, WindowClass,
        },
    },
    CURRENT_TIME, NONE,
};

pub enum PointerSample {
    Idle,
    Motion { dx: f64, dy: f64 },
    Button { dx: f64, dy: f64, number: u8, down: bool },
    LocalInput,
}

#[derive(Clone, Copy)]
struct RawButton {
    detail: u32,
    time: u32,
    down: bool,
    physical: bool,
}

impl RawButton {
    fn matches(self, core: &ButtonPressEvent, down: bool) -> bool {
        self.physical
            && core.response_type & 0x80 == 0
            && self.detail == u32::from(core.detail)
            && self.time == core.time
            && self.down == down
    }
}

pub struct PointerSource {
    activity: Activity,
    master: u16,
    axes: BTreeMap<u16, [u16; 2]>,
    bounds: (i16, i16),
    confinement: Option<Window>,
    raw_button: Option<RawButton>,
    frozen_button: Option<(u8, bool, bool)>,
    suppressed: [bool; 256],
}

impl PointerSource {
    pub fn open() -> Result<Self> {
        let (activity, devices) = Activity::open()?;
        let connection = &activity.connection;
        ensure!(
            connection.setup().roots.len() == 1,
            "Xorg pointer sending requires a single X screen"
        );
        let master = unique(
            devices
                .iter()
                .filter(|device| device.enabled && device.type_ == DeviceType::MASTER_POINTER)
                .map(|device| device.deviceid),
        )
        .context("Xorg master pointer is ambiguous")?;
        let rel_x = connection.intern_atom(true, b"Rel X")?.reply()?.atom;
        let rel_y = connection.intern_atom(true, b"Rel Y")?.reply()?.atom;
        let axes = devices
            .iter()
            .filter(|device| {
                device.enabled
                    && device.type_ == DeviceType::SLAVE_POINTER
                    && device.attachment == master
                    && !activity.xtest.contains(&device.deviceid)
            })
            .filter_map(|device| {
                relative_axes(device, [rel_x, rel_y]).map(|axes| (device.deviceid, axes))
            })
            .collect();
        connection
            .change_window_attributes(
                activity.root,
                &ChangeWindowAttributesAux::new().event_mask(EventMask::STRUCTURE_NOTIFY),
            )?
            .check()?;
        let geometry = connection.get_geometry(activity.root)?.reply()?;
        let bounds = (
            i16::try_from(
                geometry
                    .width
                    .checked_sub(1)
                    .context("Xorg width is zero")?,
            )?,
            i16::try_from(
                geometry
                    .height
                    .checked_sub(1)
                    .context("Xorg height is zero")?,
            )?,
        );
        ensure!(bounds.0 > 1 && bounds.1 > 1, "Xorg root is too small");
        Ok(Self {
            activity,
            master,
            axes,
            bounds,
            confinement: None,
            raw_button: None,
            frozen_button: None,
            suppressed: [false; 256],
        })
    }

    /// Drain one bounded batch. Unsupported input stops an active lease.
    /// Relative motion is coalesced before crossing the worker boundary.
    pub fn poll(&mut self) -> Result<PointerSample> {
        ensure!(self.frozen_button.is_none(), "Xorg button needs a decision");
        let mut motion = (0.0, 0.0);
        let mut local_input = false;
        for _ in 0..MAX_EVENTS {
            let Some(event) = self.activity.connection.poll_for_event()? else {
                return Ok(if local_input {
                    PointerSample::LocalInput
                } else if motion != (0.0, 0.0) {
                    PointerSample::Motion {
                        dx: motion.0 / f64::from(self.bounds.0),
                        dy: motion.1 / f64::from(self.bounds.1),
                    }
                } else {
                    PointerSample::Idle
                });
            };
            match event {
                Event::XinputRawMotion(event) if !self.activity.xtest.contains(&event.sourceid) => {
                    match self.axes.get(&event.sourceid) {
                        Some([x, y]) => {
                            motion.0 += axis_value(&event, *x);
                            motion.1 += axis_value(&event, *y);
                        }
                        None => local_input = true,
                    }
                }
                Event::XinputRawKeyPress(event) | Event::XinputRawKeyRelease(event) => {
                    local_input |= !self.activity.xtest.contains(&event.sourceid);
                }
                Event::XinputRawButtonPress(event) => {
                    local_input |= self.remember_button(&event, true);
                }
                Event::XinputRawButtonRelease(event) => {
                    local_input |= self.remember_button(&event, false);
                }
                Event::ButtonPress(event) if self.confinement.is_some() => {
                    return Ok(self.freeze_button(&event, true, motion, local_input));
                }
                Event::ButtonRelease(event) if self.confinement.is_some() => {
                    return Ok(self.freeze_button(&event, false, motion, local_input));
                }
                Event::MappingNotify(event) if event.request == Mapping::POINTER => {
                    anyhow::bail!("Xorg pointer mapping changed");
                }
                Event::XinputHierarchy(_) | Event::ConfigureNotify(_) | Event::Error(_) => {
                    anyhow::bail!("Xorg input mapping or root geometry changed");
                }
                _ => (),
            }
        }
        anyhow::bail!("Xorg input observation backlog exceeded its bound")
    }

    fn remember_button(&mut self, event: &RawButtonPressEvent, down: bool) -> bool {
        if self.confinement.is_none() {
            return !self.activity.xtest.contains(&event.sourceid);
        }
        let ambiguous = self.raw_button.is_some_and(|previous| {
            previous.time == event.time && previous.detail == event.detail && previous.down == down
        });
        self.raw_button = Some(RawButton {
            detail: event.detail,
            time: event.time,
            down,
            physical: !ambiguous
                && !self.activity.xtest.contains(&event.sourceid)
                && self.axes.contains_key(&event.sourceid),
        });
        false
    }

    fn freeze_button(
        &mut self,
        event: &ButtonPressEvent,
        down: bool,
        motion: (f64, f64),
        local_input: bool,
    ) -> PointerSample {
        if event.response_type & 0x80 != 0 || event.event != self.activity.root {
            return PointerSample::LocalInput;
        }
        let raw = self.raw_button.take();
        let admitted = !local_input && raw.is_some_and(|raw| raw.matches(event, down));
        self.frozen_button = Some((event.detail, down, admitted));
        if !admitted {
            return PointerSample::LocalInput;
        }
        PointerSample::Button {
            dx: motion.0 / f64::from(self.bounds.0),
            dy: motion.1 / f64::from(self.bounds.1),
            number: event.detail,
            down,
        }
    }

    pub fn check_button_mapping(&self, buttons: bool, wheel: bool) -> Result<()> {
        if buttons || wheel {
            let mapping = self.activity.connection.get_pointer_mapping()?.reply()?.map;
            ensure!(
                (!buttons || mapping.get(..3) == Some(&[1, 2, 3]))
                    && (!wheel || mapping.get(3..7) == Some(&[4, 5, 6, 7])),
                "Xorg pointer forwarding requires a standard button mapping"
            );
        }
        Ok(())
    }

    /// Consume the current server-frozen event and keep ownership of its pair.
    pub fn accept_button(&mut self, number: u8, down: bool) -> Result<()> {
        ensure!(
            self.frozen_button == Some((number, down, true))
                && self.suppressed[usize::from(number)] != down,
            "Xorg button pair ownership changed"
        );
        self.consume_button()
    }

    fn consume_button(&mut self) -> Result<()> {
        let (number, down, _) = self.frozen_button.take().context("Xorg button is not frozen")?;
        self.suppressed[usize::from(number)] = down;
        self.activity
            .connection
            .allow_events(Allow::SYNC_POINTER, CURRENT_TIME)?
            .check()?;
        Ok(())
    }

    fn has_suppressed_buttons(&self) -> bool {
        self.suppressed.iter().any(|down| *down)
    }

    pub fn edge_fraction(&self, edge: Edge) -> Result<Option<f64>> {
        let point = self.point()?;
        Ok(fraction(self.bounds, point, edge))
    }

    /// Claim only after the pinned peer grants this lease and the native worker
    /// rechecks that the original edge position has not changed.
    pub fn claim(&mut self, edge: Edge, along: f64) -> Result<bool> {
        ensure!(self.confinement.is_none(), "Xorg pointer is already owned");
        let point = self.point()?;
        if fraction(self.bounds, point, edge) != Some(along) || !self.no_local_input()? {
            return Ok(false);
        }
        if !self.grab_at(point)? {
            return Ok(false);
        }
        let admitted = (|| -> Result<bool> {
            if !self.no_local_input()? || !matches!(self.poll()?, PointerSample::Idle) {
                return Ok(false);
            }
            self.activity
                .connection
                .allow_events(Allow::SYNC_POINTER, CURRENT_TIME)?
                .check()?;
            Ok(true)
        })();
        if !matches!(admitted, Ok(true)) {
            self.release(|| {})?;
        }
        admitted
    }

    fn point(&self) -> Result<(i16, i16)> {
        let position = self
            .activity
            .connection
            .query_pointer(self.activity.root)?
            .reply()?;
        ensure!(position.same_screen, "Xorg pointer left the source screen");
        Ok((position.root_x, position.root_y))
    }

    fn no_local_input(&self) -> Result<bool> {
        Ok(self.buttons_up()?
            && self
                .activity
                .connection
                .query_keymap()?
                .reply()?
                .keys
                .iter()
                .all(|key| *key == 0))
    }

    fn buttons_up(&self) -> Result<bool> {
        let reply = self
            .activity
            .connection
            .xinput_xi_query_pointer(self.activity.root, self.master)?
            .reply()?;
        Ok(reply.buttons.iter().all(|bits| *bits == 0))
    }

    fn grab_at(&mut self, point: (i16, i16)) -> Result<bool> {
        let connection = &self.activity.connection;
        let root = self.activity.root;
        let window = connection.generate_id()?;
        connection
            .create_window(
                0,
                window,
                root,
                point.0,
                point.1,
                1,
                1,
                0,
                WindowClass::INPUT_ONLY,
                0,
                &CreateWindowAux::new().override_redirect(1),
            )?
            .check()?;
        // The confinement window must not become the local click target.
        connection
            .shape_rectangles(
                SO::SET,
                SK::INPUT,
                ClipOrdering::UNSORTED,
                window,
                0,
                0,
                &[],
            )?
            .check()?;
        connection.map_window(window)?.check()?;
        let reply = connection
            .grab_pointer(
                false,
                root,
                EventMask::BUTTON_PRESS | EventMask::BUTTON_RELEASE,
                GrabMode::SYNC,
                GrabMode::ASYNC,
                window,
                NONE,
                CURRENT_TIME,
            )?
            .reply()?;
        if reply.status == GrabStatus::SUCCESS {
            self.confinement = Some(window);
            Ok(true)
        } else {
            connection.destroy_window(window)?.check()?;
            Ok(false)
        }
    }

    pub fn release(&mut self, mut progress: impl FnMut()) -> Result<()> {
        let Some(window) = self.confinement else {
            return Ok(());
        };
        // The network lease is already retired. Keep only the native owner
        // until presses consumed by this grab have their matching release.
        let drained = (|| -> Result<()> {
            while self.has_suppressed_buttons() {
                progress();
                if self.frozen_button.is_none() {
                    self.poll()?;
                }
                if self.frozen_button.is_some() {
                    self.consume_button()?;
                } else {
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
            Ok(())
        })();
        self.confinement = None;
        let connection = &self.activity.connection;
        // SyncPointer freezes on the first button event. Let the server
        // reprocess that same event locally, with its original button pair.
        // ReplayPointer is a no-op when a button did not freeze the grab.
        let restore = if drained.is_ok() {
            connection.allow_events(Allow::REPLAY_POINTER, CURRENT_TIME)?.check()
        } else {
            // An ambiguous release must never be replayed into a local app.
            Ok(())
        };
        connection.ungrab_pointer(CURRENT_TIME)?.check()?;
        connection.destroy_window(window)?.check()?;
        restore?;
        drained
    }
}

fn fraction(bounds: (i16, i16), (x, y): (i16, i16), edge: Edge) -> Option<f64> {
    let (right, bottom) = bounds;
    if !(0..=right).contains(&x) || !(0..=bottom).contains(&y) {
        return None;
    }
    match edge {
        Edge::Left if x == 0 => Some(f64::from(y) / f64::from(bottom)),
        Edge::Right if x == right => Some(f64::from(y) / f64::from(bottom)),
        Edge::Top if y == 0 => Some(f64::from(x) / f64::from(right)),
        Edge::Bottom if y == bottom => Some(f64::from(x) / f64::from(right)),
        _ => None,
    }
}

fn relative_axes(device: &XIDeviceInfo, labels: [u32; 2]) -> Option<[u16; 2]> {
    let axis = |label| {
        let axes = device.classes.iter().filter_map(|class| match &class.data {
            DeviceClassData::Valuator(axis)
                if axis.mode == ValuatorMode::RELATIVE && label != NONE && axis.label == label =>
            {
                Some(axis.number)
            }
            _ => None,
        });
        let mut axes = axes;
        let number = axes.next()?;
        axes.next().is_none().then_some(number)
    };
    let [x, y] = [axis(labels[0])?, axis(labels[1])?];
    (x != y).then_some([x, y])
}

fn axis_value(event: &RawMotionEvent, axis: u16) -> f64 {
    let word = usize::from(axis / 32);
    let flag = 1u32 << (axis % 32);
    let Some(mask) = event
        .valuator_mask
        .get(word)
        .filter(|mask| *mask & flag != 0)
    else {
        return 0.0;
    };
    let preceding: u32 = event.valuator_mask[..word]
        .iter()
        .map(|mask| mask.count_ones())
        .sum();
    let index = preceding + (*mask & (flag - 1)).count_ones();
    event
        .axisvalues
        .get(index as usize)
        .map_or(0.0, |Fp3232 { integral, frac }| {
            f64::from(*integral) + f64::from(*frac) / 4_294_967_296.0
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_button_requires_a_physical_matching_xinput_event() {
        let core = ButtonPressEvent {
            response_type: x11rb::protocol::xproto::BUTTON_PRESS_EVENT,
            detail: 3,
            time: 99,
            ..ButtonPressEvent::default()
        };
        let raw = RawButton { detail: 3, time: 99, down: true, physical: true };
        assert!(raw.matches(&core, true));
        assert!(!raw.matches(&core, false));
        assert!(!RawButton { time: 98, ..raw }.matches(&core, true));
        assert!(!RawButton { physical: false, ..raw }.matches(&core, true));
        let synthetic = ButtonPressEvent { response_type: core.response_type | 0x80, ..core };
        assert!(!raw.matches(&synthetic, true));
    }

    #[test]
    #[ignore = "requires the dedicated GitHub Actions Xvfb server"]
    fn xorg_drains_a_consumed_pair_without_replaying_its_release() -> Result<()> {
        use crate::{x11_inject::{local_connection, Injector}, ReceivePoint};
        use std::sync::mpsc;

        ensure!(
            std::env::var("GITHUB_ACTIONS").ok().as_deref() == Some("true")
                && std::env::var("LINKO_TEST_ISOLATED_X11").ok().as_deref() == Some("1"),
            "this test requires the dedicated CI display"
        );
        let mut source = PointerSource::open()?;
        let mut injector = Injector::open_after_explicit_consent()?;
        let observer = local_connection()?.0;
        observer.change_window_attributes(
            source.activity.root,
            &ChangeWindowAttributesAux::new().event_mask(EventMask::BUTTON_PRESS | EventMask::BUTTON_RELEASE),
        )?.check()?;
        injector.pointer(ReceivePoint { x: 1.0, y: 0.5 })?;
        let along = source.edge_fraction(Edge::Right)?.context("right edge not reached")?;
        assert!(source.claim(Edge::Right, along)?);
        assert!(injector.button(2, true)?);
        assert!(matches!(source.poll()?, PointerSample::LocalInput));
        // Runtime admission rejects XTEST. Exercise the local cleanup of a
        // pair already consumed by the native owner in this isolated test.
        assert!(source.accept_button(3, true).is_err());
        source.consume_button()?;
        assert!(source.has_suppressed_buttons());
        let (draining, started) = mpsc::channel();
        let mut draining = Some(draining);
        let mut injector = std::thread::scope(|scope| -> Result<Injector> {
            let release = scope.spawn(move || -> Result<Injector> {
                started.recv_timeout(Duration::from_secs(1))?;
                assert!(injector.button(2, false)?);
                Ok(injector)
            });
            source.release(|| {
                if let Some(draining) = draining.take() {
                    let _ = draining.send(());
                }
            })?;
            release.join().expect("isolated release thread")
        })?;
        assert!(!source.has_suppressed_buttons());
        assert!(observer.poll_for_event()?.is_none());
        assert!(injector.button(2, true)?);
        assert!(injector.button(2, false)?);
        let down = observer.poll_for_event()?;
        let up = observer.poll_for_event()?;
        assert!(matches!(down, Some(Event::ButtonPress(event)) if event.detail == 3));
        assert!(matches!(up, Some(Event::ButtonRelease(event)) if event.detail == 3));
        Ok(())
    }

    #[test]
    fn x11_source_uses_exact_outer_pixels_and_sparse_signed_axes() {
        assert_eq!(fraction((999, 500), (999, 250), Edge::Right), Some(0.5));
        assert_eq!(fraction((999, 500), (998, 250), Edge::Right), None);
        assert_eq!(fraction((999, 500), (999, -1), Edge::Right), None);
        let event = RawMotionEvent {
            valuator_mask: vec![1 << 2, 1 << 3],
            axisvalues: vec![
                Fp3232 {
                    integral: -3,
                    frac: 1 << 31,
                },
                Fp3232 {
                    integral: 7,
                    frac: 0,
                },
            ],
            ..RawMotionEvent::default()
        };
        assert_eq!(axis_value(&event, 2), -2.5);
        assert_eq!(axis_value(&event, 35), 7.0);
        assert_eq!(axis_value(&event, 0), 0.0);
    }
}
