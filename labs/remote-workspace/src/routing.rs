//! Single-target routing with a mandatory cleanup acknowledgement barrier.
//! This module never injects input. The existing native runtime remains the
//! owner of pressed-key/button ledgers and the sole capture/injection lifecycle.
use crate::geometry::{map_pointer, Camera, MappedPoint, Point, Rect, SourceGeometry};
use crate::session::SessionGate;
use crate::{advance, Error, SurfaceKey};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    pub view_id: u64,
    /// Advance on ANY camera, panel, crop, or presentation mapping change.
    pub revision: u64,
    pub camera: Camera,
    pub panel: Rect,
}

impl Placement {
    pub fn validate(self) -> Result<(), Error> {
        if self.view_id == 0 || self.revision == 0 {
            return Err(Error::InvalidGeometry);
        }
        self.camera.validate()?;
        self.panel.validate()
    }
}

/// Current mapping supplied at submission, not a snapshot from the old ticket.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PointerSample {
    pub placement: Placement,
    pub geometry: SourceGeometry,
    pub position: Point,
}

/// Process-local routing ticket, not a serialized network authorization token.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InputTicket {
    key: SurfaceKey,
    policy_epoch: u64,
    source_revision: u64,
    focus_epoch: u64,
    placement: Placement,
}

impl InputTicket {
    pub fn target(self) -> SurfaceKey {
        self.key
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReleaseRequest {
    key: SurfaceKey,
    epoch: u64,
}

impl ReleaseRequest {
    pub fn target(self) -> SurfaceKey {
        self.key
    }
}

struct Active {
    ticket: InputTicket,
    sequence: u64,
}

/// One active input target per local controller. No broadcast mode exists.
#[derive(Default)]
pub struct InputRouter {
    active: Option<Active>,
    pending: Option<ReleaseRequest>,
    epoch: u64,
    last_now: u64,
}

impl InputRouter {
    pub fn focus(
        &mut self,
        gate: &SessionGate,
        placement: Placement,
        now: u64,
    ) -> Result<InputTicket, Error> {
        advance(&mut self.last_now, now)?;
        if self.pending.is_some() {
            return Err(Error::ReleasePending);
        }
        if let Some(active) = &self.active {
            let ticket = active.ticket;
            if ticket.key != gate.key()
                || ticket.policy_epoch != gate.policy_epoch()
                || ticket.source_revision != gate.revision()
                || ticket.placement != placement
            {
                // Selecting another panel stops the old target even if the new
                // target will subsequently fail its own permission check.
                self.stop();
                return Err(Error::ReleasePending);
            }
            if let Err(error) = gate.can_control(now) {
                self.stop();
                return Err(error);
            }
            // Repeated focus must NOT reset the replay watermark.
            return Ok(ticket);
        }
        placement.validate()?;
        gate.can_control(now)?;
        self.epoch = self.epoch.checked_add(1).ok_or(Error::Capacity)?;
        let ticket = InputTicket {
            key: gate.key(),
            policy_epoch: gate.policy_epoch(),
            source_revision: gate.revision(),
            focus_epoch: self.epoch,
            placement,
        };
        self.active = Some(Active {
            ticket,
            sequence: 0,
        });
        Ok(ticket)
    }

    /// Stop/blur closes routing immediately. Retain the cleanup request until the
    /// input owner confirms actual releases or verifies receiver lease cleanup.
    /// An arbitrary network timeout is NOT proof of receiver cleanup.
    pub fn stop(&mut self) -> Option<ReleaseRequest> {
        if let Some(active) = self.active.take() {
            self.pending = Some(ReleaseRequest {
                key: active.ticket.key,
                epoch: active.ticket.focus_epoch,
            });
        }
        self.pending
    }

    pub fn pending_release(&self) -> Option<ReleaseRequest> {
        self.pending
    }

    /// LOCAL backend acknowledgement only. An untrusted peer boolean must not
    /// call this on the backend's behalf. Native cleanup remains outside this lab.
    pub fn acknowledge_release(&mut self, request: ReleaseRequest) -> Result<(), Error> {
        if self.pending != Some(request) {
            return Err(Error::WrongAcknowledgement);
        }
        self.pending = None;
        Ok(())
    }

    /// Run from a runtime watchdog while input is idle too. A library cannot
    /// independently discover revoked OS permission or release held native keys.
    pub fn poll(&mut self, gate: &SessionGate, now: u64) -> Result<(), Error> {
        advance(&mut self.last_now, now)?;
        let Some(active) = &self.active else {
            return Ok(());
        };
        if active.ticket.key != gate.key() {
            return Err(Error::StaleTarget);
        }
        let check = if active.ticket.policy_epoch != gate.policy_epoch()
            || active.ticket.source_revision != gate.revision()
        {
            Err(Error::StaleTarget)
        } else {
            gate.can_control(now)
        };
        if check.is_err() {
            self.stop();
        }
        check
    }

    /// Preflight immediately before submission to the existing runtime. Success
    /// means policy accepted, NOT that an OS or application acted on the input.
    pub fn authorize(
        &mut self,
        ticket: InputTicket,
        gate: &SessionGate,
        placement: Placement,
        sequence: u64,
        now: u64,
    ) -> Result<SurfaceKey, Error> {
        self.check_mapping(ticket, gate, placement, now)?;
        let active = self.active.as_mut().ok_or(Error::StaleTarget)?;
        if sequence == 0 || sequence <= active.sequence {
            return Err(Error::InvalidSequence);
        }
        active.sequence = sequence;
        Ok(ticket.key)
    }

    fn check_mapping(
        &mut self,
        ticket: InputTicket,
        gate: &SessionGate,
        placement: Placement,
        now: u64,
    ) -> Result<(), Error> {
        advance(&mut self.last_now, now)?;
        let active = self.active.as_ref().ok_or(Error::StaleTarget)?;
        if active.ticket != ticket || ticket.key != gate.key() {
            // Late events must not tear down a new, unrelated active target.
            return Err(Error::StaleTarget);
        }
        if ticket.placement != placement {
            self.stop();
            return Err(Error::StaleTarget);
        }
        self.poll(gate, now)
    }

    pub fn pointer(
        &mut self,
        ticket: InputTicket,
        gate: &SessionGate,
        sample: PointerSample,
        sequence: u64,
        now: u64,
    ) -> Result<MappedPoint, Error> {
        self.check_mapping(ticket, gate, sample.placement, now)?;
        if sample.geometry.revision != ticket.source_revision {
            self.stop();
            return Err(Error::StaleTarget);
        }
        let point = map_pointer(
            sample.placement.camera,
            sample.placement.panel,
            sample.geometry,
            sample.position,
        )?;
        self.authorize(ticket, gate, sample.placement, sequence, now)?;
        Ok(point)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture_key;
    use crate::geometry::{PixelSize, Rotation};
    use crate::session::{ControlScope, SurfaceKind};

    fn gate(seed: u8) -> SessionGate {
        let mut g = SessionGate::new(fixture_key(seed), SurfaceKind::Display, 1, false).unwrap();
        g.grant_view(0, 10_000).unwrap();
        g.grant_control(ControlScope::WholeDesktop, 0, 10_000)
            .unwrap();
        g.presented(g.frame_stamp(1), 0).unwrap();
        g
    }

    fn placement() -> Placement {
        Placement {
            view_id: 1,
            revision: 1,
            camera: Camera {
                origin: Point { x: 0.0, y: 0.0 },
                zoom: 1.0,
            },
            panel: Rect {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 100.0,
            },
        }
    }

    fn sample() -> PointerSample {
        PointerSample {
            placement: placement(),
            geometry: SourceGeometry {
                pixels: PixelSize {
                    width: 100,
                    height: 100,
                },
                logical: placement().panel,
                rotation: Rotation::None,
                revision: 1,
            },
            position: Point { x: 50.0, y: 50.0 },
        }
    }

    #[test]
    fn switching_requires_acknowledged_cleanup_and_old_input_cannot_cross() {
        let a = gate(1);
        let b = gate(2);
        let mut r = InputRouter::default();
        let ta = r.focus(&a, placement(), 0).unwrap();
        assert_eq!(r.focus(&b, placement(), 1), Err(Error::ReleasePending));
        let release = r.pending_release().unwrap();
        assert_eq!(release.target(), a.key());
        assert_eq!(r.focus(&b, placement(), 2), Err(Error::ReleasePending));
        r.acknowledge_release(release).unwrap();
        let tb = r.focus(&b, placement(), 3).unwrap();
        assert_eq!(
            r.authorize(ta, &a, placement(), 1, 4),
            Err(Error::StaleTarget)
        );
        assert_eq!(r.authorize(tb, &b, placement(), 1, 5), Ok(b.key()));
        assert_eq!(r.pending_release(), None);
    }

    #[test]
    fn wrong_or_duplicate_cleanup_acknowledgements_reject() {
        let a = gate(1);
        let mut r = InputRouter::default();
        r.focus(&a, placement(), 0).unwrap();
        let release = r.stop().unwrap();
        let wrong = ReleaseRequest {
            epoch: release.epoch + 1,
            ..release
        };
        assert_eq!(
            r.acknowledge_release(wrong),
            Err(Error::WrongAcknowledgement)
        );
        assert_eq!(r.pending_release(), Some(release));
        r.acknowledge_release(release).unwrap();
        assert_eq!(
            r.acknowledge_release(release),
            Err(Error::WrongAcknowledgement)
        );
    }

    #[test]
    fn repeated_focus_does_not_reset_replay_sequence() {
        let a = gate(1);
        let mut r = InputRouter::default();
        let t = r.focus(&a, placement(), 0).unwrap();
        r.authorize(t, &a, placement(), 10, 1).unwrap();
        assert_eq!(r.focus(&a, placement(), 2).unwrap(), t);
        for seq in [0, 9, 10] {
            assert_eq!(
                r.authorize(t, &a, placement(), seq, 3),
                Err(Error::InvalidSequence)
            );
        }
        r.authorize(t, &a, placement(), 11, 4).unwrap();
    }

    #[test]
    fn consent_revocation_stops_even_without_new_input() {
        let mut a = gate(1);
        let mut r = InputRouter::default();
        r.focus(&a, placement(), 0).unwrap();
        a.revoke_control(1).unwrap();
        assert_eq!(r.poll(&a, 1), Err(Error::StaleTarget));
        assert_eq!(r.pending_release().unwrap().target(), a.key());
    }

    #[test]
    fn stale_video_stops_idle_control() {
        let a = gate(1);
        let mut r = InputRouter::default();
        r.focus(&a, placement(), 0).unwrap();
        assert_eq!(r.poll(&a, 2000), Err(Error::StaleFrame));
        assert!(r.pending_release().is_some());
    }

    #[test]
    fn changing_layout_or_camera_invalidates_old_mapping() {
        let a = gate(1);
        let mut r = InputRouter::default();
        let t = r.focus(&a, placement(), 0).unwrap();
        let mut changed = placement();
        changed.camera.zoom = 2.0;
        changed.revision = 2;
        assert_eq!(r.authorize(t, &a, changed, 1, 1), Err(Error::StaleTarget));
        assert!(r.pending_release().is_some());
    }

    #[test]
    fn backwards_clock_cannot_extend_control() {
        let a = gate(1);
        let mut r = InputRouter::default();
        let t = r.focus(&a, placement(), 5).unwrap();
        assert_eq!(
            r.authorize(t, &a, placement(), 1, 4),
            Err(Error::InvalidTime)
        );
    }

    #[test]
    fn pointer_checks_current_mapping_not_the_tickets_old_snapshot() {
        let a = gate(1);
        let mut r = InputRouter::default();
        let t = r.focus(&a, placement(), 0).unwrap();
        let mut changed = sample();
        changed.placement.camera.zoom = 2.0;
        assert_eq!(r.pointer(t, &a, changed, 1, 1), Err(Error::StaleTarget));
        assert!(r.pending_release().is_some());
    }

    #[test]
    fn invalid_pointer_does_not_consume_event_sequence() {
        let a = gate(1);
        let mut r = InputRouter::default();
        let t = r.focus(&a, placement(), 0).unwrap();
        let mut invalid = sample();
        invalid.position.x = f64::NAN;
        assert_eq!(r.pointer(t, &a, invalid, 1, 1), Err(Error::InvalidGeometry));
        let point = r.pointer(t, &a, sample(), 1, 2).unwrap();
        assert_eq!((point.pixel_x, point.pixel_y), (50, 50));
    }

    #[test]
    fn selecting_unauthorized_panel_still_stops_previous_target() {
        let a = gate(1);
        let b = SessionGate::new(fixture_key(2), SurfaceKind::Display, 1, false).unwrap();
        let mut r = InputRouter::default();
        let old = r.focus(&a, placement(), 0).unwrap();
        assert_eq!(r.focus(&b, placement(), 1), Err(Error::ReleasePending));
        assert_eq!(
            r.authorize(old, &a, placement(), 1, 2),
            Err(Error::StaleTarget)
        );
        r.acknowledge_release(r.pending_release().unwrap()).unwrap();
        assert_eq!(r.focus(&b, placement(), 3), Err(Error::PermissionDenied));
    }

    #[test]
    fn repeated_focus_on_stale_video_stops_control() {
        let a = gate(1);
        let mut r = InputRouter::default();
        r.focus(&a, placement(), 0).unwrap();
        assert_eq!(r.focus(&a, placement(), 2000), Err(Error::StaleFrame));
        assert!(r.pending_release().is_some());
    }
}
