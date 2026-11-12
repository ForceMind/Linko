//! Xorg pointer source using the receiver's existing XInput2 observation owner.
use super::{unique, Activity, DeviceType, Event, MAX_EVENTS};
use anyhow::{ensure, Context, Result};
use peerglide_protocol::Edge;
use std::collections::BTreeMap;
use x11rb::{
    connection::Connection as _,
    protocol::{
        shape::{ConnectionExt as _, SK, SO},
        xinput::{
            ConnectionExt as _, DeviceClassData, Fp3232, RawMotionEvent, ValuatorMode, XIDeviceInfo,
        },
        xproto::{
            Allow, ChangeWindowAttributesAux, ClipOrdering, ConnectionExt as _, CreateWindowAux,
            EventMask, GrabMode, GrabStatus, Window, WindowClass,
        },
    },
    CURRENT_TIME, NONE,
};

pub enum PointerSample {
    Idle,
    Motion { dx: f64, dy: f64 },
    LocalInput,
}

pub struct PointerSource {
    activity: Activity,
    master: u16,
    axes: BTreeMap<u16, [u16; 2]>,
    bounds: (i16, i16),
    confinement: Option<Window>,
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
        })
    }

    /// Drain one bounded batch. Unsupported input stops an active lease.
    /// Relative motion is coalesced before crossing the worker boundary.
    pub fn poll(&self) -> Result<PointerSample> {
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
                Event::XinputRawButtonPress(event) | Event::XinputRawButtonRelease(event) => {
                    local_input |= !self.activity.xtest.contains(&event.sourceid);
                }
                Event::ButtonPress(_) | Event::ButtonRelease(_) if self.confinement.is_some() => {
                    local_input = true;
                }
                Event::XinputHierarchy(_) | Event::ConfigureNotify(_) | Event::Error(_) => {
                    anyhow::bail!("Xorg input or root geometry changed");
                }
                _ => (),
            }
        }
        anyhow::bail!("Xorg input observation backlog exceeded its bound")
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
            self.release()?;
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
            .shape_rectangles(SO::SET, SK::INPUT, ClipOrdering::UNSORTED, window, 0, 0, &[])?
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

    pub fn release(&mut self) -> Result<()> {
        let Some(window) = self.confinement.take() else {
            return Ok(());
        };
        let connection = &self.activity.connection;
        // SyncPointer freezes on the first button event. Let the server
        // reprocess that same event locally, with its original button pair.
        // ReplayPointer is a no-op when a button did not freeze the grab.
        let replay = connection
            .allow_events(Allow::REPLAY_POINTER, CURRENT_TIME)?
            .check();
        connection.ungrab_pointer(CURRENT_TIME)?.check()?;
        connection.destroy_window(window)?.check()?;
        replay?;
        Ok(())
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
                Fp3232 { integral: 7, frac: 0 },
            ],
            ..RawMotionEvent::default()
        };
        assert_eq!(axis_value(&event, 2), -2.5);
        assert_eq!(axis_value(&event, 35), 7.0);
        assert_eq!(axis_value(&event, 0), 0.0);
    }
}
