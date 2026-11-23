//! RW02: an offline coordinator over the existing policy/media/input models.
//!
//! All callbacks carry an opaque, process-local ViewId. Do not recover a callback's
//! destination by looking up a window title, PID, OS handle, or newest connection.
//! This is NOT authentication, capture, native input, a renderer, or persistence.
//! The production adapter must serialize calls, supply a monotonic local clock,
//! run poll while idle, and acknowledge actual effects rather than queued work.
use std::collections::BTreeMap;
use std::sync::Arc;

use crate::frame_binding::FrameMatch;
use crate::geometry::{Camera, MappedPoint, Point, Rect, SourceGeometry};
use crate::handoff::{Command, Handoff, State};
use crate::media::{LatestFrame, RawFrame, MAX_FRAME_BYTES, MAX_QUEUE_AGE_MS};
use crate::routing::{InputRouter, InputTicket, Placement, PointerSample, ReleaseRequest};
use crate::scheduling::{self, Budget, Plan, ViewDemand};
use crate::session::{ContentCheck, ControlScope, FrameStamp, SessionGate, SurfaceKind};
use crate::{advance, Binding, Error, SurfaceKey};

pub const MAX_SURFACES: usize = 16;

/// Local lifetime handle, never reused within a coordinator. Not a wire credential.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ViewId(u64);

/// An additional local display of one authorized view. It carries no input route.
#[derive(Clone)]
pub struct MirrorHandle {
    view: ViewId,
    instance: Arc<()>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SurfaceSpec {
    pub key: SurfaceKey,
    pub kind: SurfaceKind,
    pub geometry: SourceGeometry,
    pub camera: Camera,
    pub panel: Rect,
}

/// Non-sensitive fault category; no desktop content or native handle is stored.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Fault {
    ConnectionLost,
    MediaWorkerExited,
    SourceClosed,
    PermissionLost,
    LocalTakeover,
    ScreenLocked,
}

/// Issued only when a frame leaves the mailbox for the presenter. The deadline
/// starts at RECEIPT, not dequeue, so rendering cannot refresh an old frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PresentationTicket {
    view: ViewId,
    stamp: FrameStamp,
    received_at: u64,
    mirror: bool,
}

impl PresentationTicket {
    pub fn stamp(self) -> FrameStamp {
        self.stamp
    }
}

/// Intentionally not Debug/Clone: pixel contents do not belong in diagnostics.
pub struct Presentation {
    pub ticket: PresentationTicket,
    pub frame: RawFrame,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HandoffToken {
    view: ViewId,
    attempt: u64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WindowIntent {
    pub token: HandoffToken,
    pub command: Command,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Snapshot {
    pub connected: bool,
    pub viewing_allowed: bool,
    pub control_policy_ready: bool,
    pub input_active: bool,
    pub release_pending: bool,
    pub frame_queued: bool,
    pub presentation_pending: bool,
    pub handoff: Option<State>,
    pub fault: Option<Fault>,
}

struct Surface {
    gate: SessionGate,
    geometry: SourceGeometry,
    placement: Placement,
    frames: LatestFrame,
    queued_at: Option<u64>,
    presenting: Option<PresentationTicket>,
    mirror: Option<Mirror>,
    connected: bool,
    fault: Option<Fault>,
    parking_allowed: bool,
    handoff: Option<(HandoffToken, Handoff)>,
}

struct Mirror {
    instance: Arc<()>,
    panel: Rect,
}

impl Surface {
    fn mirror_mut(&mut self, handle: &MirrorHandle) -> Result<&mut Mirror, Error> {
        self.mirror
            .as_mut()
            .filter(|mirror| Arc::ptr_eq(&mirror.instance, &handle.instance))
            .ok_or(Error::StaleTarget)
    }

    fn cancel_mirror_presentation(&mut self) {
        if self.presenting.is_some_and(|ticket| ticket.mirror) {
            self.presenting = None;
        }
    }

    fn clear_media(&mut self) {
        self.frames.clear();
        self.queued_at = None;
        self.presenting = None;
    }

    fn cancel_handoff(&mut self) {
        if let Some((_, handoff)) = &mut self.handoff {
            handoff.cancel();
        }
    }

    fn transaction(&mut self, token: HandoffToken) -> Result<&mut Handoff, Error> {
        match self.handoff.as_mut() {
            Some((current, handoff)) if *current == token => Ok(handoff),
            _ => Err(Error::WrongAcknowledgement),
        }
    }
}

/// One local viewer, bounded surfaces, and ONE input router. No broadcast path.
/// Slots are retired only after terminal closure and acknowledged cleanup.
/// Reconnection requires a new authenticated binding and a new registration.
/// ViewIds protect callback lifetimes; they do not authenticate a remote peer.
pub struct Coordinator {
    viewer: [u8; 32],
    limit: usize,
    surfaces: BTreeMap<ViewId, Surface>,
    input: InputRouter,
    active: Option<ViewId>,
    next_view: u64,
    next_attempt: u64,
    last_now: u64,
}

impl Coordinator {
    pub fn new(viewer: [u8; 32], limit: usize) -> Result<Self, Error> {
        if viewer == [0; 32] {
            return Err(Error::InvalidIdentity);
        }
        if limit == 0 || limit > MAX_SURFACES {
            return Err(Error::Capacity);
        }
        Ok(Self {
            viewer,
            limit,
            surfaces: BTreeMap::new(),
            input: InputRouter::default(),
            active: None,
            next_view: 0,
            next_attempt: 0,
            last_now: 0,
        })
    }

    fn clock(&mut self, now: u64) -> Result<(), Error> {
        if let Err(error) = advance(&mut self.last_now, now) {
            // A broken clock must not leave an active input route open.
            self.stop_input();
            return Err(error);
        }
        Ok(())
    }

    fn surface(&self, view: ViewId) -> Result<&Surface, Error> {
        self.surfaces.get(&view).ok_or(Error::StaleTarget)
    }

    fn surface_mut(&mut self, view: ViewId) -> Result<&mut Surface, Error> {
        self.surfaces.get_mut(&view).ok_or(Error::StaleTarget)
    }

    fn cancel_active_handoff(&mut self) {
        if let Some(view) = self.active.take() {
            if let Some(s) = self.surfaces.get_mut(&view) {
                s.cancel_handoff();
            }
        }
    }

    fn stop_input(&mut self) -> Option<ReleaseRequest> {
        self.cancel_active_handoff();
        self.input.stop()
    }

    fn stop_view_input(&mut self, view: ViewId) {
        if self.active == Some(view) {
            self.stop_input();
        }
    }

    fn sync_input(&mut self) {
        if self.input.pending_release().is_some() {
            self.cancel_active_handoff();
        }
    }

    pub fn register(&mut self, spec: SurfaceSpec, now: u64) -> Result<ViewId, Error> {
        self.clock(now)?;
        spec.key.validate()?;
        spec.geometry.validate()?;
        if spec.key.binding.viewer != self.viewer {
            return Err(Error::InvalidIdentity);
        }
        if self.surfaces.len() >= self.limit {
            return Err(Error::Capacity);
        }
        if self.surfaces.values().any(|s| s.gate.key() == spec.key) {
            return Err(Error::InvalidState);
        }
        if spec.geometry.pixels.rgba_bytes()? > MAX_FRAME_BYTES {
            return Err(Error::Capacity);
        }
        let id = self.next_view.checked_add(1).ok_or(Error::Capacity)?;
        let placement = Placement {
            view_id: id,
            revision: 1,
            camera: spec.camera,
            panel: spec.panel,
        };
        placement.validate()?;
        // No platform has demonstrated window-only input isolation in this lab.
        let gate = SessionGate::new(spec.key, spec.kind, spec.geometry.revision, false)?;
        let frames = LatestFrame::new(&gate, spec.geometry.pixels)?;
        let view = ViewId(id);
        self.surfaces.insert(
            view,
            Surface {
                gate,
                geometry: spec.geometry,
                placement,
                frames,
                queued_at: None,
                presenting: None,
                mirror: None,
                connected: true,
                fault: None,
                parking_allowed: false,
                handoff: None,
            },
        );
        self.next_view = id;
        Ok(view)
    }

    /// Explicit local viewing consent; no input consent is inherited.
    pub fn grant_view(&mut self, view: ViewId, now: u64, duration: u64) -> Result<(), Error> {
        self.clock(now)?;
        let s = self.surface_mut(view)?;
        s.gate.grant_view(now, duration)?;
        s.clear_media();
        s.frames = LatestFrame::new(&s.gate, s.geometry.pixels)?;
        s.parking_allowed = false;
        s.cancel_handoff();
        self.stop_view_input(view);
        Ok(())
    }

    pub fn grant_control(
        &mut self,
        view: ViewId,
        scope: ControlScope,
        now: u64,
        duration: u64,
    ) -> Result<(), Error> {
        self.clock(now)?;
        let s = self.surface_mut(view)?;
        s.gate.grant_control(scope, now, duration)?;
        s.cancel_handoff();
        self.stop_view_input(view);
        Ok(())
    }

    pub fn revoke_control(&mut self, view: ViewId, now: u64) -> Result<(), Error> {
        self.clock(now)?;
        let s = self.surface_mut(view)?;
        s.gate.revoke_control(now)?;
        s.cancel_handoff();
        self.stop_view_input(view);
        Ok(())
    }

    pub fn revoke_view(&mut self, view: ViewId, now: u64) -> Result<(), Error> {
        self.clock(now)?;
        let s = self.surface_mut(view)?;
        s.gate.revoke_all(now)?;
        s.clear_media();
        s.parking_allowed = false;
        s.cancel_handoff();
        self.stop_view_input(view);
        Ok(())
    }

    /// End a decoder lifetime while retaining the existing consent deadline.
    /// Adapters must stop forwarding this instance and retain its original stamps.
    pub fn invalidate_media(&mut self, view: ViewId, now: u64) -> Result<(), Error> {
        self.clock(now)?;
        let s = self.surface_mut(view)?;
        s.gate.invalidate_media(now)?;
        s.clear_media();
        s.frames = LatestFrame::new(&s.gate, s.geometry.pixels)?;
        s.parking_allowed = false;
        s.cancel_handoff();
        self.stop_view_input(view);
        Ok(())
    }

    /// Trusted source geometry only. Invalid input is rejected before mutation.
    pub fn revise_geometry(
        &mut self,
        view: ViewId,
        geometry: SourceGeometry,
        now: u64,
    ) -> Result<(), Error> {
        self.clock(now)?;
        geometry.validate()?;
        if geometry.pixels.rgba_bytes()? > MAX_FRAME_BYTES {
            return Err(Error::Capacity);
        }
        let s = self.surface_mut(view)?;
        s.gate.revise_geometry(geometry.revision, now)?;
        s.geometry = geometry;
        s.clear_media();
        s.frames = LatestFrame::new(&s.gate, geometry.pixels)?;
        s.parking_allowed = false;
        s.cancel_handoff();
        self.stop_view_input(view);
        Ok(())
    }

    pub fn relayout(
        &mut self,
        view: ViewId,
        camera: Camera,
        panel: Rect,
        now: u64,
    ) -> Result<(), Error> {
        self.clock(now)?;
        let s = self.surface_mut(view)?;
        let placement = Placement {
            camera,
            panel,
            revision: s.placement.revision.checked_add(1).ok_or(Error::Capacity)?,
            ..s.placement
        };
        placement.validate()?;
        if camera == s.placement.camera && panel == s.placement.panel {
            return Ok(());
        }
        s.placement = placement;
        s.cancel_handoff();
        self.stop_view_input(view);
        Ok(())
    }

    /// Add one read-only presentation region to this already authorized source.
    /// Both regions use the same source gate, mailbox and decoder allocation.
    pub fn attach_mirror(
        &mut self,
        view: ViewId,
        panel: Rect,
        now: u64,
    ) -> Result<MirrorHandle, Error> {
        self.clock(now)?;
        panel.validate()?;
        let s = self.surface_mut(view)?;
        s.gate.can_view(now)?;
        if s.mirror.is_some() {
            return Err(Error::Capacity);
        }
        let instance = Arc::new(());
        s.mirror = Some(Mirror {
            instance: Arc::clone(&instance),
            panel,
        });
        Ok(MirrorHandle { view, instance })
    }

    pub fn relayout_mirror(
        &mut self,
        handle: &MirrorHandle,
        panel: Rect,
        now: u64,
    ) -> Result<(), Error> {
        self.clock(now)?;
        panel.validate()?;
        let s = self.surface_mut(handle.view)?;
        let mirror = s.mirror_mut(handle)?;
        if mirror.panel != panel {
            mirror.panel = panel;
            s.cancel_mirror_presentation();
        }
        Ok(())
    }

    pub fn detach_mirror(&mut self, handle: &MirrorHandle, now: u64) -> Result<(), Error> {
        self.clock(now)?;
        let s = self.surface_mut(handle.view)?;
        s.mirror_mut(handle)?;
        s.mirror = None;
        s.cancel_mirror_presentation();
        Ok(())
    }

    /// Plan the current canvas viewport using registered geometry and viewing grants.
    /// Viewing focus is a quality preference independent of the active input route.
    /// Apply the plan with current authorization and actual decoder resource accounting.
    pub fn plan_viewport(
        &self,
        viewport: Rect,
        viewing_focus: Option<ViewId>,
        budget: Budget,
        now: u64,
    ) -> Result<Plan, Error> {
        if now < self.last_now {
            return Err(Error::InvalidTime);
        }
        if let Some(view) = viewing_focus {
            self.surface(view)?;
        }
        let demands: Vec<_> = self
            .surfaces
            .iter()
            .flat_map(|(view, surface)| {
                std::iter::once(surface.placement.panel)
                    .chain(surface.mirror.iter().map(|mirror| mirror.panel))
                    .map(move |panel| ViewDemand {
                        gate: &surface.gate,
                        pixels: surface.geometry.pixels,
                        canvas_rect: panel,
                        focused: viewing_focus == Some(*view),
                    })
            })
            .collect();
        scheduling::plan(&demands, viewport, budget, now)
    }

    /// Read-only policy for a trusted media adapter. Only the coordinator can
    /// grant, revoke or advance this view's policy.
    pub fn view_gate(&self, view: ViewId) -> Result<&SessionGate, Error> {
        Ok(&self.surface(view)?.gate)
    }

    /// Metadata for a trusted capture/decoder adapter, NOT permission to capture.
    pub fn frame_stamp(&self, view: ViewId, sequence: u64) -> Result<FrameStamp, Error> {
        Ok(self.view_gate(view)?.frame_stamp(sequence))
    }

    pub fn receive(&mut self, view: ViewId, frame: RawFrame, now: u64) -> Result<bool, Error> {
        self.receive_at(view, frame, now, now)
    }

    /// Queue the decoded buffer matched by the adapter. Use the first local
    /// arrival so that matching, decoder and renderer delays share one budget.
    pub fn receive_matched(
        &mut self,
        view: ViewId,
        frame: RawFrame,
        matched: FrameMatch,
        now: u64,
    ) -> Result<bool, Error> {
        if frame.stamp() != matched.stamp() {
            return Err(Error::InvalidFrame);
        }
        self.receive_at(view, frame, matched.first_seen(), now)
    }

    fn receive_at(
        &mut self,
        view: ViewId,
        frame: RawFrame,
        received_at: u64,
        now: u64,
    ) -> Result<bool, Error> {
        self.poll(now)?;
        let s = self.surface_mut(view)?;
        let replaced = s.frames.push_observed(frame, &s.gate, received_at, now)?;
        s.queued_at = Some(received_at);
        Ok(replaced)
    }

    /// At most one outstanding presentation receipt per view. Native adapters
    /// must also bound buffers they own; this library cannot free their textures.
    pub fn take_for_present(
        &mut self,
        view: ViewId,
        now: u64,
    ) -> Result<Option<Presentation>, Error> {
        self.take_for_region(view, None, now)
    }

    /// Use only when presenting to the mirror alone. A frame visible at both
    /// regions uses one primary presentation and one renderer-owned texture.
    pub fn take_for_mirror(
        &mut self,
        handle: &MirrorHandle,
        now: u64,
    ) -> Result<Option<Presentation>, Error> {
        self.take_for_region(handle.view, Some(handle), now)
    }

    fn take_for_region(
        &mut self,
        view: ViewId,
        mirror: Option<&MirrorHandle>,
        now: u64,
    ) -> Result<Option<Presentation>, Error> {
        self.poll(now)?;
        let s = self.surface_mut(view)?;
        if let Some(handle) = mirror {
            s.mirror_mut(handle)?;
        }
        if s.presenting.is_some() {
            return Err(Error::InvalidState);
        }
        let received_at = s.queued_at.take();
        let Some(frame) = s.frames.take(&s.gate, now)? else {
            return Ok(None);
        };
        let ticket = PresentationTicket {
            view,
            stamp: frame.stamp(),
            received_at: received_at.ok_or(Error::InvalidState)?,
            mirror: mirror.is_some(),
        };
        s.presenting = Some(ticket);
        Ok(Some(Presentation { ticket, frame }))
    }

    /// Only the trusted presenter may acknowledge actual display completion.
    /// Receiving, decoding or submitting a GPU command is not this acknowledgement.
    pub fn acknowledge_presentation(
        &mut self,
        ticket: PresentationTicket,
        now: u64,
    ) -> Result<(), Error> {
        self.poll(now)?;
        let s = self.surface_mut(ticket.view)?;
        if ticket.mirror || s.presenting != Some(ticket) {
            return Err(Error::WrongAcknowledgement);
        }
        s.presenting = None;
        s.gate.presented(ticket.stamp, now)
    }

    /// Confirms a mirror-only display without renewing primary input freshness.
    pub fn acknowledge_mirror(
        &mut self,
        handle: &MirrorHandle,
        ticket: PresentationTicket,
        now: u64,
    ) -> Result<(), Error> {
        self.poll(now)?;
        let s = self.surface_mut(handle.view)?;
        if !ticket.mirror || s.presenting != Some(ticket) || s.mirror_mut(handle).is_err() {
            return Err(Error::WrongAcknowledgement);
        }
        s.presenting = None;
        s.gate.can_view(now)
    }

    pub fn request_content_check(&mut self, view: ViewId, now: u64) -> Result<ContentCheck, Error> {
        self.poll(now)?;
        self.surface_mut(view)?.gate.request_content_check(now)
    }

    pub fn confirm_unchanged(
        &mut self,
        view: ViewId,
        check: ContentCheck,
        now: u64,
    ) -> Result<(), Error> {
        self.poll(now)?;
        self.surface_mut(view)?.gate.confirm_unchanged(check, now)
    }

    pub fn focus(&mut self, view: ViewId, now: u64) -> Result<InputTicket, Error> {
        self.poll(now)?;
        let Some(s) = self.surfaces.get(&view) else {
            self.stop_input();
            return Err(Error::StaleTarget);
        };
        let result = self.input.focus(&s.gate, s.placement, now);
        self.sync_input();
        if result.is_ok() {
            self.active = Some(view);
        }
        result
    }

    pub fn authorize_input(
        &mut self,
        view: ViewId,
        ticket: InputTicket,
        sequence: u64,
        now: u64,
    ) -> Result<SurfaceKey, Error> {
        self.poll(now)?;
        let s = self.surfaces.get(&view).ok_or(Error::StaleTarget)?;
        let result = self
            .input
            .authorize(ticket, &s.gate, s.placement, sequence, now);
        self.sync_input();
        result
    }

    pub fn pointer(
        &mut self,
        view: ViewId,
        ticket: InputTicket,
        position: Point,
        sequence: u64,
        now: u64,
    ) -> Result<MappedPoint, Error> {
        self.poll(now)?;
        let s = self.surfaces.get(&view).ok_or(Error::StaleTarget)?;
        let sample = PointerSample {
            placement: s.placement,
            geometry: s.geometry,
            position,
        };
        let result = self.input.pointer(ticket, &s.gate, sample, sequence, now);
        self.sync_input();
        result
    }

    pub fn blur(&mut self, now: u64) -> Result<Option<ReleaseRequest>, Error> {
        self.clock(now)?;
        Ok(self.stop_input())
    }

    pub fn pending_release(&self) -> Option<ReleaseRequest> {
        self.input.pending_release()
    }

    /// Native owner confirmation only; never call merely because a timer expired.
    pub fn acknowledge_release(&mut self, request: ReleaseRequest, now: u64) -> Result<(), Error> {
        self.clock(now)?;
        self.input.acknowledge_release(request)
    }

    /// An independent LOCAL decision after native parking capability verification.
    /// Defaults false and is cleared by re-consent, geometry change and any fault.
    pub fn allow_parking(&mut self, view: ViewId, allowed: bool, now: u64) -> Result<(), Error> {
        self.clock(now)?;
        let s = self.surface_mut(view)?;
        if allowed {
            s.gate.can_view(now)?;
        }
        s.parking_allowed = allowed;
        if !allowed {
            s.cancel_handoff();
        }
        Ok(())
    }

    pub fn begin_handoff(
        &mut self,
        view: ViewId,
        bounds: Rect,
        now: u64,
        timeout: u64,
    ) -> Result<HandoffToken, Error> {
        self.poll(now)?;
        if self.input.pending_release().is_some() {
            return Err(Error::ReleasePending);
        }
        let attempt = self.next_attempt.checked_add(1).ok_or(Error::Capacity)?;
        let s = self.surface_mut(view)?;
        if !s.parking_allowed {
            return Err(Error::PermissionDenied);
        }
        s.gate.can_control(now)?;
        if s.handoff
            .as_ref()
            .is_some_and(|(_, h)| h.state() != State::Local)
        {
            return Err(Error::InvalidState);
        }
        let h = Handoff::begin(s.gate.key(), attempt, bounds, now, timeout)?;
        let token = HandoffToken { view, attempt };
        s.handoff = Some((token, h));
        self.next_attempt = attempt;
        Ok(token)
    }

    pub fn destination_ready(&mut self, token: HandoffToken, now: u64) -> Result<(), Error> {
        self.poll(now)?;
        let s = self.surface_mut(token.view)?;
        if s.handoff.as_ref().map(|(t, _)| *t) != Some(token) {
            return Err(Error::WrongAcknowledgement);
        }
        let (_, h) = s.handoff.as_mut().ok_or(Error::InvalidState)?;
        h.destination_ready(&s.gate, token.attempt, now)
    }

    pub fn window_intent(&mut self, view: ViewId, now: u64) -> Result<Option<WindowIntent>, Error> {
        self.poll(now)?;
        let s = self.surface(view)?;
        Ok(s.handoff.as_ref().and_then(|(token, h)| {
            h.command().map(|command| WindowIntent {
                token: *token,
                command,
            })
        }))
    }

    pub fn acknowledge_hidden(&mut self, token: HandoffToken, now: u64) -> Result<(), Error> {
        self.poll(now)?;
        let s = self.surface_mut(token.view)?;
        if s.handoff.as_ref().map(|(t, _)| *t) != Some(token) {
            return Err(Error::WrongAcknowledgement);
        }
        let (_, h) = s.handoff.as_mut().ok_or(Error::InvalidState)?;
        h.acknowledge_hidden(&s.gate, token.attempt, now)
    }

    pub fn cancel_handoff(&mut self, token: HandoffToken, now: u64) -> Result<(), Error> {
        self.clock(now)?;
        self.surface_mut(token.view)?.transaction(token)?.cancel();
        Ok(())
    }

    /// Remains available after consent revocation or terminal connection closure.
    pub fn acknowledge_restored(&mut self, token: HandoffToken, now: u64) -> Result<(), Error> {
        self.clock(now)?;
        self.surface_mut(token.view)?
            .transaction(token)?
            .acknowledge_restored(token.attempt)
    }

    /// Terminal for this handle. A fault never silently restarts capture/control.
    pub fn close(&mut self, view: ViewId, fault: Fault, now: u64) -> Result<(), Error> {
        self.clock(now)?;
        let s = self.surface_mut(view)?;
        if s.connected {
            s.gate.disconnect(now)?;
            s.connected = false;
            s.fault = Some(fault);
        }
        s.mirror = None;
        s.clear_media();
        s.parking_allowed = false;
        s.cancel_handoff();
        self.stop_view_input(view);
        Ok(())
    }

    /// A transport/worker fault may affect several windows on one connection.
    /// Close every matching live view, not merely the currently selected panel.
    /// Unrelated authenticated bindings and their recovery obligations are retained.
    pub fn close_binding(
        &mut self,
        binding: Binding,
        fault: Fault,
        now: u64,
    ) -> Result<usize, Error> {
        self.clock(now)?;
        binding.validate()?;
        if binding.viewer != self.viewer {
            return Err(Error::InvalidIdentity);
        }
        let views: Vec<_> = self
            .surfaces
            .iter()
            .filter(|(_, s)| s.connected && s.gate.key().binding == binding)
            .map(|(view, _)| *view)
            .collect();
        let count = views.len();
        for view in views {
            self.close(view, fault, now)?;
        }
        Ok(count)
    }

    /// Closing a view cannot discard an outstanding native recovery obligation.
    pub fn retire(&mut self, view: ViewId, now: u64) -> Result<(), Error> {
        self.poll(now)?;
        let s = self.surface(view)?;
        if s.connected {
            return Err(Error::InvalidState);
        }
        if self
            .input
            .pending_release()
            .is_some_and(|r| r.target() == s.gate.key())
        {
            return Err(Error::ReleasePending);
        }
        if s.handoff
            .as_ref()
            .is_some_and(|(_, h)| h.state() != State::Local)
        {
            return Err(Error::InvalidState);
        }
        self.surfaces.remove(&view);
        Ok(())
    }

    /// Mandatory idle watchdog. No timeout ever acknowledges input/window cleanup.
    pub fn poll(&mut self, now: u64) -> Result<(), Error> {
        self.clock(now)?;
        for (view, s) in &mut self.surfaces {
            if s.gate.can_view(now).is_err() {
                s.clear_media();
                s.parking_allowed = false;
            }
            if s.queued_at
                .is_some_and(|received| now - received >= MAX_QUEUE_AGE_MS)
            {
                s.frames.clear();
                s.queued_at = None;
            }
            if s.presenting
                .is_some_and(|p| now - p.received_at >= MAX_QUEUE_AGE_MS)
            {
                s.presenting = None;
            }
            if let Some((_, h)) = &mut s.handoff {
                // Failure is represented by the retained restore intention/state.
                let _ = h.revalidate(&s.gate, now);
            }
            if self.active == Some(*view) && self.input.poll(&s.gate, now).is_err() {
                s.cancel_handoff();
                self.active = None;
            }
        }
        self.sync_input();
        Ok(())
    }

    /// Status only. Querying it does NOT run the watchdog or perform OS recovery.
    pub fn snapshot(&self, view: ViewId, now: u64) -> Result<Snapshot, Error> {
        if now < self.last_now {
            return Err(Error::InvalidTime);
        }
        let s = self.surface(view)?;
        Ok(Snapshot {
            connected: s.connected,
            viewing_allowed: s.gate.can_view(now).is_ok(),
            control_policy_ready: s.gate.can_control(now).is_ok(),
            input_active: self.active == Some(view),
            release_pending: self
                .input
                .pending_release()
                .is_some_and(|r| r.target() == s.gate.key()),
            frame_queued: s.frames.has_frame(),
            presentation_pending: s.presenting.is_some(),
            handoff: s.handoff.as_ref().map(|(_, h)| h.state()),
            fault: s.fault,
        })
    }
}
