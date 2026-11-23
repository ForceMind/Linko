//! Synthetic, in-process integration tests. No screen is captured or controlled.
use linko_workspace_lab::coordinator::{Coordinator, Fault, SurfaceSpec, ViewId};
use linko_workspace_lab::frame_binding::{FrameMatch, FrameMatcher, MediaFrame, MediaTrack};
use linko_workspace_lab::geometry::{Camera, PixelSize, Point, Rect, Rotation, SourceGeometry};
use linko_workspace_lab::handoff::{Command, State};
use linko_workspace_lab::media::RawFrame;
use linko_workspace_lab::session::{ControlScope, SurfaceKind};
use linko_workspace_lab::{Binding, Error, SurfaceKey};

fn bounds() -> Rect {
    Rect {
        x: 0.0,
        y: 0.0,
        width: 100.0,
        height: 100.0,
    }
}

fn spec(seed: u8) -> SurfaceSpec {
    SurfaceSpec {
        key: SurfaceKey {
            binding: Binding {
                source: [seed; 32],
                viewer: [255; 32],
                nonce: [seed; 16],
            },
            object: 1,
            generation: 1,
        },
        kind: SurfaceKind::Window,
        geometry: SourceGeometry {
            pixels: PixelSize {
                width: 2,
                height: 2,
            },
            logical: bounds(),
            rotation: Rotation::None,
            revision: 1,
        },
        camera: Camera {
            origin: Point { x: 0.0, y: 0.0 },
            zoom: 1.0,
        },
        panel: bounds(),
    }
}

fn coordinator() -> Coordinator {
    Coordinator::new([255; 32], 4).unwrap()
}

fn raw(c: &Coordinator, v: ViewId, sequence: u64) -> RawFrame {
    RawFrame::new(
        c.frame_stamp(v, sequence).unwrap(),
        spec(1).geometry.pixels,
        vec![0; 16],
    )
    .unwrap()
}

fn present(c: &mut Coordinator, v: ViewId, sequence: u64, now: u64) {
    let frame = raw(c, v, sequence);
    c.receive(v, frame, now).unwrap();
    let p = c.take_for_present(v, now).unwrap().unwrap();
    c.acknowledge_presentation(p.ticket, now).unwrap();
}

fn live(c: &mut Coordinator, seed: u8) -> ViewId {
    let v = c.register(spec(seed), 0).unwrap();
    c.grant_view(v, 0, 10_000).unwrap();
    c.grant_control(v, ControlScope::WholeDesktop, 0, 10_000)
        .unwrap();
    present(c, v, 1, 0);
    v
}

#[test]
fn default_deny_and_independent_control_consent() {
    let mut c = coordinator();
    let v = c.register(spec(1), 0).unwrap();
    assert!(!c.snapshot(v, 0).unwrap().viewing_allowed);
    assert_eq!(c.focus(v, 0), Err(Error::PermissionDenied));
    c.grant_view(v, 0, 100).unwrap();
    present(&mut c, v, 1, 1);
    assert_eq!(c.focus(v, 1), Err(Error::PermissionDenied));
    c.grant_control(v, ControlScope::WholeDesktop, 1, 99)
        .unwrap();
    assert!(c.focus(v, 1).is_ok());
}

#[test]
fn window_capture_does_not_grant_window_only_input() {
    let mut c = coordinator();
    let v = c.register(spec(1), 0).unwrap();
    c.grant_view(v, 0, 100).unwrap();
    assert_eq!(
        c.grant_control(v, ControlScope::WindowOnly, 0, 100),
        Err(Error::PermissionDenied)
    );
}

#[test]
fn receiving_and_dequeuing_are_not_presentation() {
    let mut c = coordinator();
    let v = c.register(spec(1), 0).unwrap();
    c.grant_view(v, 0, 100).unwrap();
    c.grant_control(v, ControlScope::WholeDesktop, 0, 100)
        .unwrap();
    let frame = raw(&c, v, 1);
    c.receive(v, frame, 1).unwrap();
    assert_eq!(c.focus(v, 1), Err(Error::StaleFrame));
    let p = c.take_for_present(v, 2).unwrap().unwrap();
    assert_eq!(c.focus(v, 2), Err(Error::StaleFrame));
    assert!(matches!(c.take_for_present(v, 2), Err(Error::InvalidState)));
    c.acknowledge_presentation(p.ticket, 3).unwrap();
    assert!(c.focus(v, 3).is_ok());
    assert_eq!(
        c.acknowledge_presentation(p.ticket, 3),
        Err(Error::WrongAcknowledgement)
    );
}

#[test]
fn presentation_deadline_starts_at_receive_not_dequeue() {
    let mut c = coordinator();
    let v = live(&mut c, 1);
    let frame = raw(&c, v, 2);
    c.receive(v, frame, 0).unwrap();
    let p = c.take_for_present(v, 1999).unwrap().unwrap();
    assert!(c.acknowledge_presentation(p.ticket, 2000).is_err());
    assert!(!c.snapshot(v, 2000).unwrap().control_policy_ready);
}

#[test]
fn raw_mailbox_replaces_only_older_frames() {
    let mut c = coordinator();
    let v = live(&mut c, 1);
    let f2 = raw(&c, v, 2);
    let duplicate = raw(&c, v, 2);
    let f3 = raw(&c, v, 3);
    assert_eq!(c.receive(v, f2, 1), Ok(false));
    assert_eq!(c.receive(v, duplicate, 1), Err(Error::InvalidSequence));
    assert_eq!(c.receive(v, f3, 1), Ok(true));
    let p = c.take_for_present(v, 1).unwrap().unwrap();
    assert_eq!(p.frame.sequence(), 3);
    c.acknowledge_presentation(p.ticket, 1).unwrap();
}

#[test]
fn reconsent_rejects_old_media_and_presentation_receipts() {
    let mut c = coordinator();
    let v = live(&mut c, 1);
    let old_frame = raw(&c, v, 3);
    let f2 = raw(&c, v, 2);
    c.receive(v, f2, 1).unwrap();
    let p = c.take_for_present(v, 1).unwrap().unwrap();
    c.grant_view(v, 2, 100).unwrap();
    assert!(c.acknowledge_presentation(p.ticket, 2).is_err());
    assert_eq!(c.receive(v, old_frame, 2), Err(Error::StaleTarget));
    present(&mut c, v, 1, 3);
    assert_eq!(c.focus(v, 3), Err(Error::PermissionDenied));
}

#[test]
fn frames_cannot_cross_surface_bindings() {
    let mut c = coordinator();
    let a = live(&mut c, 1);
    let b = live(&mut c, 2);
    let frame = raw(&c, a, 2);
    assert_eq!(c.receive(b, frame, 1), Err(Error::StaleTarget));
    assert!(!c.snapshot(b, 1).unwrap().frame_queued);
}

#[test]
fn switch_requires_acknowledged_release_not_a_timeout() {
    let mut c = coordinator();
    let a = live(&mut c, 1);
    let b = live(&mut c, 2);
    c.focus(a, 1).unwrap();
    assert_eq!(c.focus(b, 1), Err(Error::ReleasePending));
    let release = c.pending_release().unwrap();
    c.poll(5000).unwrap();
    assert_eq!(c.pending_release(), Some(release));
    assert_eq!(c.focus(b, 5000), Err(Error::ReleasePending));
    c.acknowledge_release(release, 5000).unwrap();
    present(&mut c, b, 2, 5000);
    assert!(c.focus(b, 5000).is_ok());
}

#[test]
fn late_old_input_does_not_stop_the_new_target() {
    let mut c = coordinator();
    let a = live(&mut c, 1);
    let b = live(&mut c, 2);
    let old = c.focus(a, 1).unwrap();
    assert_eq!(c.focus(b, 1), Err(Error::ReleasePending));
    c.acknowledge_release(c.pending_release().unwrap(), 1)
        .unwrap();
    let current = c.focus(b, 1).unwrap();
    assert_eq!(c.authorize_input(a, old, 1, 2), Err(Error::StaleTarget));
    assert!(c.authorize_input(b, current, 1, 2).is_ok());
    assert!(c.snapshot(b, 2).unwrap().input_active);
}

#[test]
fn repeated_focus_preserves_replay_watermark() {
    let mut c = coordinator();
    let v = live(&mut c, 1);
    let t = c.focus(v, 1).unwrap();
    c.authorize_input(v, t, 7, 1).unwrap();
    let same = c.focus(v, 1).unwrap();
    assert_eq!(t, same);
    assert_eq!(
        c.authorize_input(v, same, 7, 1),
        Err(Error::InvalidSequence)
    );
    assert!(c.authorize_input(v, same, 8, 1).is_ok());
}

#[test]
fn old_release_ack_cannot_clear_a_new_cleanup_barrier() {
    let mut c = coordinator();
    let v = live(&mut c, 1);
    c.focus(v, 1).unwrap();
    let old = c.blur(1).unwrap().unwrap();
    c.acknowledge_release(old, 1).unwrap();
    c.focus(v, 1).unwrap();
    let current = c.blur(1).unwrap().unwrap();
    assert_ne!(old, current);
    assert_eq!(
        c.acknowledge_release(old, 1),
        Err(Error::WrongAcknowledgement)
    );
    assert_eq!(c.pending_release(), Some(current));
}

#[test]
fn idle_watchdog_stops_input_at_exact_freshness_boundary() {
    let mut c = coordinator();
    let v = live(&mut c, 1);
    c.focus(v, 0).unwrap();
    c.poll(1999).unwrap();
    assert!(c.snapshot(v, 1999).unwrap().input_active);
    c.poll(2000).unwrap();
    assert!(!c.snapshot(v, 2000).unwrap().input_active);
    assert!(c.pending_release().is_some());
}

#[test]
fn static_content_confirmation_is_one_shot_and_source_bound() {
    let mut c = coordinator();
    let v = live(&mut c, 1);
    c.focus(v, 0).unwrap();
    let check = c.request_content_check(v, 1500).unwrap();
    c.confirm_unchanged(v, check, 1600).unwrap();
    c.poll(3500).unwrap();
    assert!(c.snapshot(v, 3500).unwrap().input_active);
    assert_eq!(c.confirm_unchanged(v, check, 3501), Err(Error::StaleTarget));
    c.poll(3600).unwrap();
    assert!(c.pending_release().is_some());
}

#[test]
fn geometry_change_invalidates_media_control_and_input_tickets() {
    let mut c = coordinator();
    let v = live(&mut c, 1);
    let t = c.focus(v, 1).unwrap();
    let frame = raw(&c, v, 2);
    c.receive(v, frame, 1).unwrap();
    let p = c.take_for_present(v, 1).unwrap().unwrap();
    let mut geometry = spec(1).geometry;
    geometry.revision = 2;
    c.revise_geometry(v, geometry, 2).unwrap();
    assert!(c.acknowledge_presentation(p.ticket, 2).is_err());
    assert!(c.authorize_input(v, t, 1, 2).is_err());
    assert!(c.pending_release().is_some());
    assert!(!c.snapshot(v, 2).unwrap().control_policy_ready);
}

#[test]
fn invalid_geometry_is_rejected_without_losing_a_good_route() {
    let mut c = coordinator();
    let v = live(&mut c, 1);
    let t = c.focus(v, 1).unwrap();
    let mut geometry = spec(1).geometry;
    geometry.pixels.width = 0;
    geometry.revision = 2;
    assert_eq!(
        c.revise_geometry(v, geometry, 2),
        Err(Error::InvalidGeometry)
    );
    assert!(c.authorize_input(v, t, 1, 2).is_ok());
}

#[test]
fn camera_change_closes_old_pointer_mapping() {
    let mut c = coordinator();
    let v = live(&mut c, 1);
    let t = c.focus(v, 1).unwrap();
    let mut camera = spec(1).camera;
    camera.zoom = 2.0;
    c.relayout(v, camera, bounds(), 2).unwrap();
    assert!(c.pointer(v, t, Point { x: 10.0, y: 10.0 }, 1, 2).is_err());
    assert!(c.pending_release().is_some());
}

#[test]
fn pointer_uses_current_registry_geometry_and_rejects_outside_content() {
    let mut c = coordinator();
    let v = live(&mut c, 1);
    let t = c.focus(v, 1).unwrap();
    assert_eq!(
        c.pointer(v, t, Point { x: -1.0, y: 20.0 }, 1, 1),
        Err(Error::OutsideContent)
    );
    let p = c.pointer(v, t, Point { x: 50.0, y: 50.0 }, 1, 1).unwrap();
    assert_eq!(p.logical, Point { x: 50.0, y: 50.0 });
}

#[test]
fn control_revoke_retains_view_but_releases_input() {
    let mut c = coordinator();
    let v = live(&mut c, 1);
    c.focus(v, 1).unwrap();
    c.revoke_control(v, 2).unwrap();
    let s = c.snapshot(v, 2).unwrap();
    assert!(s.viewing_allowed);
    assert!(!s.control_policy_ready);
    assert!(s.release_pending);
}

#[test]
fn view_expiry_clears_queued_and_inflight_frames() {
    let mut c = coordinator();
    let v = c.register(spec(1), 0).unwrap();
    c.grant_view(v, 0, 10).unwrap();
    let frame = raw(&c, v, 1);
    c.receive(v, frame, 1).unwrap();
    let p = c.take_for_present(v, 1).unwrap().unwrap();
    let next = raw(&c, v, 2);
    c.receive(v, next, 2).unwrap();
    c.poll(10).unwrap();
    let s = c.snapshot(v, 10).unwrap();
    assert!(!s.frame_queued && !s.presentation_pending && !s.viewing_allowed);
    assert!(c.acknowledge_presentation(p.ticket, 10).is_err());
}

#[test]
fn all_fault_categories_are_terminal_and_preserve_release_obligations() {
    for fault in [
        Fault::ConnectionLost,
        Fault::MediaWorkerExited,
        Fault::SourceClosed,
        Fault::PermissionLost,
        Fault::LocalTakeover,
        Fault::ScreenLocked,
    ] {
        let mut c = coordinator();
        let v = live(&mut c, 1);
        c.focus(v, 1).unwrap();
        let frame = raw(&c, v, 2);
        c.receive(v, frame, 1).unwrap();
        c.close(v, fault, 2).unwrap();
        let s = c.snapshot(v, 2).unwrap();
        assert!(!s.connected && !s.viewing_allowed && !s.frame_queued);
        assert!(s.release_pending);
        assert_eq!(s.fault, Some(fault));
        assert_eq!(c.grant_view(v, 3, 100), Err(Error::Disconnected));
        assert_eq!(c.retire(v, 3), Err(Error::ReleasePending));
    }
}

#[test]
fn default_mirror_mode_never_requests_source_parking() {
    let mut c = coordinator();
    let v = live(&mut c, 1);
    assert_eq!(
        c.begin_handoff(v, bounds(), 1, 100),
        Err(Error::PermissionDenied)
    );
    assert_eq!(c.window_intent(v, 1).unwrap(), None);
    c.allow_parking(v, true, 1).unwrap();
    let token = c.begin_handoff(v, bounds(), 1, 100).unwrap();
    assert_eq!(c.window_intent(v, 1).unwrap(), None);
    c.destination_ready(token, 2).unwrap();
    assert_eq!(
        c.window_intent(v, 2).unwrap().unwrap().command,
        Command::HideSource
    );
}

#[test]
fn disconnect_after_parking_requires_both_native_cleanup_confirmations() {
    let mut c = coordinator();
    let v = live(&mut c, 1);
    c.focus(v, 1).unwrap();
    c.allow_parking(v, true, 1).unwrap();
    let token = c.begin_handoff(v, bounds(), 1, 100).unwrap();
    c.destination_ready(token, 1).unwrap();
    c.acknowledge_hidden(token, 1).unwrap();
    c.close(v, Fault::ConnectionLost, 2).unwrap();
    let intent = c.window_intent(v, 1000).unwrap().unwrap();
    assert_eq!(intent.command, Command::RestoreSource(bounds()));
    assert_eq!(c.retire(v, 1000), Err(Error::ReleasePending));
    c.acknowledge_release(c.pending_release().unwrap(), 1000)
        .unwrap();
    assert_eq!(c.retire(v, 1000), Err(Error::InvalidState));
    c.acknowledge_restored(token, 1000).unwrap();
    c.retire(v, 1000).unwrap();
    assert_eq!(c.snapshot(v, 1000), Err(Error::StaleTarget));
}

#[test]
fn preparing_timeout_does_not_claim_a_hide_occurred() {
    let mut c = coordinator();
    let v = live(&mut c, 1);
    c.allow_parking(v, true, 0).unwrap();
    let token = c.begin_handoff(v, bounds(), 0, 10).unwrap();
    c.poll(10).unwrap();
    assert_eq!(c.snapshot(v, 10).unwrap().handoff, Some(State::Local));
    assert_eq!(c.window_intent(v, 10).unwrap(), None);
    assert!(c.destination_ready(token, 10).is_err());
}

#[test]
fn hide_request_timeout_retains_restore_and_rejects_late_hidden_ack() {
    let mut c = coordinator();
    let v = live(&mut c, 1);
    c.allow_parking(v, true, 0).unwrap();
    let token = c.begin_handoff(v, bounds(), 0, 10).unwrap();
    c.destination_ready(token, 1).unwrap();
    c.poll(10).unwrap();
    assert_eq!(
        c.snapshot(v, 10).unwrap().handoff,
        Some(State::RestoreRequested)
    );
    assert!(c.acknowledge_hidden(token, 10).is_err());
    c.poll(5000).unwrap();
    assert_eq!(
        c.snapshot(v, 5000).unwrap().handoff,
        Some(State::RestoreRequested)
    );
    c.acknowledge_restored(token, 5000).unwrap();
    assert_eq!(c.snapshot(v, 5000).unwrap().handoff, Some(State::Local));
}

#[test]
fn old_handoff_token_cannot_acknowledge_a_new_attempt() {
    let mut c = coordinator();
    let v = live(&mut c, 1);
    c.allow_parking(v, true, 0).unwrap();
    let old = c.begin_handoff(v, bounds(), 0, 100).unwrap();
    c.cancel_handoff(old, 1).unwrap();
    let new = c.begin_handoff(v, bounds(), 1, 100).unwrap();
    assert_ne!(old, new);
    assert_eq!(
        c.destination_ready(old, 1),
        Err(Error::WrongAcknowledgement)
    );
    c.destination_ready(new, 1).unwrap();
    assert_eq!(
        c.acknowledge_hidden(old, 1),
        Err(Error::WrongAcknowledgement)
    );
    c.acknowledge_hidden(new, 1).unwrap();
}

#[test]
fn reconsent_cancels_pending_hide_and_resets_parking_permission() {
    let mut c = coordinator();
    let v = live(&mut c, 1);
    c.allow_parking(v, true, 0).unwrap();
    let token = c.begin_handoff(v, bounds(), 0, 100).unwrap();
    c.destination_ready(token, 1).unwrap();
    c.grant_view(v, 2, 100).unwrap();
    assert_eq!(
        c.snapshot(v, 2).unwrap().handoff,
        Some(State::RestoreRequested)
    );
    assert!(c.acknowledge_hidden(token, 2).is_err());
    c.acknowledge_restored(token, 2).unwrap();
    c.grant_control(v, ControlScope::WholeDesktop, 2, 100)
        .unwrap();
    present(&mut c, v, 1, 2);
    assert_eq!(
        c.begin_handoff(v, bounds(), 2, 10),
        Err(Error::PermissionDenied)
    );
}

#[test]
fn registration_is_bounded_and_rejects_other_viewer_identities() {
    assert!(Coordinator::new([0; 32], 1).is_err());
    assert!(Coordinator::new([255; 32], 0).is_err());
    assert!(Coordinator::new([255; 32], 17).is_err());
    let mut c = Coordinator::new([255; 32], 1).unwrap();
    let mut wrong = spec(1);
    wrong.key.binding.viewer = [254; 32];
    assert_eq!(c.register(wrong, 0), Err(Error::InvalidIdentity));
    let v = c.register(spec(1), 0).unwrap();
    assert_eq!(c.register(spec(2), 0), Err(Error::Capacity));
    c.close(v, Fault::SourceClosed, 1).unwrap();
    c.retire(v, 1).unwrap();
    let new = c.register(spec(2), 1).unwrap();
    assert_ne!(v, new);
}

#[test]
fn callbacks_for_retired_views_cannot_target_replacements() {
    let mut c = coordinator();
    let v = live(&mut c, 1);
    let frame = raw(&c, v, 2);
    c.receive(v, frame, 1).unwrap();
    let old = c.take_for_present(v, 1).unwrap().unwrap();
    c.close(v, Fault::MediaWorkerExited, 2).unwrap();
    c.retire(v, 2).unwrap();
    let mut replacement = spec(1);
    replacement.key.binding.nonce = [2; 16];
    let new = c.register(replacement, 2).unwrap();
    c.grant_view(new, 2, 100).unwrap();
    assert_eq!(
        c.acknowledge_presentation(old.ticket, 2),
        Err(Error::StaleTarget)
    );
    assert!(!c.snapshot(new, 2).unwrap().presentation_pending);
    assert!(!c.snapshot(new, 2).unwrap().control_policy_ready);
}

#[test]
fn backwards_clock_stops_input_without_forging_cleanup() {
    let mut c = coordinator();
    let v = live(&mut c, 1);
    let t = c.focus(v, 10).unwrap();
    assert_eq!(c.authorize_input(v, t, 1, 9), Err(Error::InvalidTime));
    assert!(c.pending_release().is_some());
    assert!(!c.snapshot(v, 10).unwrap().input_active);
}

#[test]
fn one_hundred_consent_cycles_leave_no_model_frames_or_active_route() {
    let mut c = coordinator();
    let v = c.register(spec(1), 0).unwrap();
    for cycle in 0..100 {
        let now = cycle * 3;
        c.grant_view(v, now, 100).unwrap();
        c.grant_control(v, ControlScope::WholeDesktop, now, 100)
            .unwrap();
        present(&mut c, v, 1, now);
        c.focus(v, now).unwrap();
        let release = c.blur(now + 1).unwrap().unwrap();
        c.acknowledge_release(release, now + 1).unwrap();
        c.revoke_view(v, now + 2).unwrap();
        let s = c.snapshot(v, now + 2).unwrap();
        assert!(!s.frame_queued && !s.presentation_pending && !s.input_active);
        assert!(!s.release_pending && !s.viewing_allowed);
    }
}

#[test]
fn connection_fault_closes_all_and_only_matching_views() {
    let mut c = coordinator();
    let a = live(&mut c, 1);
    let mut second = spec(1);
    second.key.object = 2;
    let other_window = c.register(second, 0).unwrap();
    c.grant_view(other_window, 0, 100).unwrap();
    let b = live(&mut c, 2);
    c.focus(a, 1).unwrap();
    let binding = spec(1).key.binding;
    assert_eq!(c.close_binding(binding, Fault::ConnectionLost, 2), Ok(2));
    assert!(!c.snapshot(a, 2).unwrap().connected);
    assert!(!c.snapshot(other_window, 2).unwrap().connected);
    assert!(c.snapshot(b, 2).unwrap().viewing_allowed);
    assert!(c.pending_release().is_some());
    assert_eq!(c.close_binding(binding, Fault::ConnectionLost, 2), Ok(0));
}

#[test]
fn connection_fault_does_not_interrupt_unrelated_active_input() {
    let mut c = coordinator();
    let a = live(&mut c, 1);
    let b = live(&mut c, 2);
    let t = c.focus(b, 1).unwrap();
    c.close_binding(spec(1).key.binding, Fault::MediaWorkerExited, 2)
        .unwrap();
    assert!(!c.snapshot(a, 2).unwrap().connected);
    assert!(c.snapshot(b, 2).unwrap().input_active);
    assert!(c.pending_release().is_none());
    assert!(c.authorize_input(b, t, 1, 2).is_ok());
}

#[test]
fn idle_watchdog_discards_aged_queued_pixels() {
    let mut c = coordinator();
    let v = live(&mut c, 1);
    let frame = raw(&c, v, 2);
    c.receive(v, frame, 1).unwrap();
    c.poll(2000).unwrap();
    assert!(c.snapshot(v, 2000).unwrap().frame_queued);
    c.poll(2001).unwrap();
    assert!(!c.snapshot(v, 2001).unwrap().frame_queued);
    assert!(c.take_for_present(v, 2001).unwrap().is_none());
}

#[test]
fn losing_focus_cancels_source_parking_without_claiming_recovery() {
    for switch in [false, true] {
        let mut c = coordinator();
        let a = live(&mut c, 1);
        let b = live(&mut c, 2);
        c.focus(a, 0).unwrap();
        c.allow_parking(a, true, 0).unwrap();
        let token = c.begin_handoff(a, bounds(), 0, 100).unwrap();
        c.destination_ready(token, 0).unwrap();
        c.acknowledge_hidden(token, 0).unwrap();
        if switch {
            assert_eq!(c.focus(b, 1), Err(Error::ReleasePending));
        } else {
            c.blur(1).unwrap();
        }
        assert_eq!(
            c.snapshot(a, 1).unwrap().handoff,
            Some(State::RestoreRequested)
        );
        assert_eq!(
            c.window_intent(a, 1).unwrap().unwrap().command,
            Command::RestoreSource(bounds())
        );
        assert!(c.pending_release().is_some());
    }
}

#[test]
fn pending_release_blocks_new_window_parking() {
    let mut c = coordinator();
    let v = live(&mut c, 1);
    c.focus(v, 0).unwrap();
    let release = c.blur(1).unwrap().unwrap();
    c.allow_parking(v, true, 1).unwrap();
    assert_eq!(
        c.begin_handoff(v, bounds(), 1, 100),
        Err(Error::ReleasePending)
    );
    c.acknowledge_release(release, 1).unwrap();
    assert!(c.begin_handoff(v, bounds(), 1, 100).is_ok());
}

#[test]
fn duplicate_and_oversized_registration_do_not_consume_slots() {
    let mut c = Coordinator::new([255; 32], 2).unwrap();
    c.register(spec(1), 0).unwrap();
    assert_eq!(c.register(spec(1), 0), Err(Error::InvalidState));
    let mut huge = spec(2);
    huge.geometry.pixels = PixelSize {
        width: 16_384,
        height: 16_384,
    };
    assert_eq!(c.register(huge, 0), Err(Error::Capacity));
    assert!(c.register(spec(2), 0).is_ok());
}

const TEST_MEDIA: MediaTrack = MediaTrack {
    generation: 1,
    track: 1,
    ssrc: 1,
};

fn matched(
    coordinator: &Coordinator,
    view: ViewId,
    matcher: &mut FrameMatcher,
    sequence: u64,
    first_seen: u64,
    matched_at: u64,
) -> FrameMatch {
    let gate = coordinator.view_gate(view).unwrap();
    let frame = MediaFrame {
        track: TEST_MEDIA,
        timestamp: sequence as u32,
    };
    assert_eq!(matcher.decoded(gate, frame, first_seen), Ok(None));
    matcher
        .describe(gate, frame, gate.frame_stamp(sequence), matched_at)
        .unwrap()
        .unwrap()
}

#[test]
fn matched_frame_keeps_first_arrival_deadline_before_presentation() {
    let mut c = coordinator();
    let v = c.register(spec(1), 0).unwrap();
    c.grant_view(v, 0, 10_000).unwrap();
    let mut matcher = FrameMatcher::new(c.view_gate(v).unwrap(), TEST_MEDIA, 0, 0).unwrap();

    let pair = matched(&c, v, &mut matcher, 1, 10, 1800);
    let frame = raw(&c, v, 1);
    assert_eq!(c.receive_matched(v, frame, pair, 1999), Ok(false));
    assert!(c.take_for_present(v, 2010).unwrap().is_none());
}

#[test]
fn expired_presentation_ticket_cannot_refresh_source_but_fresh_one_can() {
    let mut c = coordinator();
    let v = c.register(spec(1), 0).unwrap();
    c.grant_view(v, 0, 10_000).unwrap();
    c.grant_control(v, ControlScope::WholeDesktop, 0, 10_000)
        .unwrap();
    let mut matcher = FrameMatcher::new(c.view_gate(v).unwrap(), TEST_MEDIA, 0, 0).unwrap();

    let pair = matched(&c, v, &mut matcher, 1, 10, 1800);
    let frame = raw(&c, v, 1);
    c.receive_matched(v, frame, pair, 1999).unwrap();
    let old = c.take_for_present(v, 2000).unwrap().unwrap();
    assert_eq!(
        c.acknowledge_presentation(old.ticket, 2010),
        Err(Error::WrongAcknowledgement)
    );
    assert_eq!(c.focus(v, 2010), Err(Error::StaleFrame));

    let mismatched = matched(&c, v, &mut matcher, 2, 2020, 2021);
    let frame = raw(&c, v, 3);
    assert_eq!(
        c.receive_matched(v, frame, mismatched, 2021),
        Err(Error::InvalidFrame)
    );
    let current = matched(&c, v, &mut matcher, 3, 2022, 2023);
    let frame = raw(&c, v, 3);
    c.receive_matched(v, frame, current, 2024).unwrap();
    let p = c.take_for_present(v, 2025).unwrap().unwrap();
    assert_eq!(c.focus(v, 2025), Err(Error::StaleFrame));
    c.acknowledge_presentation(p.ticket, 2026).unwrap();
    assert!(c.focus(v, 2026).is_ok());
}
