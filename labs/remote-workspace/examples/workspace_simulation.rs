//! Deterministic OFFLINE fixtures; no actual computers, video, or input injection.
use std::io::Cursor;

use linko_workspace_lab::geometry::{Camera, PixelSize, Point, Rect};
use linko_workspace_lab::handoff::{Handoff, State};
use linko_workspace_lab::media::{read_raw_frame, LatestFrame, RawFrame};
use linko_workspace_lab::routing::{InputRouter, Placement};
use linko_workspace_lab::scheduling::{plan, Budget, ViewDemand};
use linko_workspace_lab::session::{ControlScope, SessionGate, SurfaceKind};
use linko_workspace_lab::{Binding, Error, SurfaceKey};

fn fixture(seed: u8) -> Result<SessionGate, Error> {
    let key = SurfaceKey {
        binding: Binding {
            source: [seed; 32],
            viewer: [255; 32],
            nonce: [seed; 16],
        },
        object: 1,
        generation: 1,
    };
    let mut gate = SessionGate::new(key, SurfaceKind::Window, 1, false)?;
    gate.grant_view(0, 10_000)?;
    gate.grant_control(ControlScope::WholeDesktop, 0, 10_000)?;
    let size = PixelSize {
        width: 2,
        height: 2,
    };
    let frame = RawFrame::new(gate.frame_stamp(1), size, vec![seed; 16])?;
    let frame = read_raw_frame(&mut Cursor::new(frame.encode()), &gate, size, 0)?;
    let mut mailbox = LatestFrame::new(&gate, size)?;
    mailbox.push(frame, &gate, 0)?;
    let displayed = mailbox.take(&gate, 0)?.ok_or(Error::StaleFrame)?;
    gate.presented(displayed.stamp(), 0)?;
    Ok(gate)
}

fn main() -> Result<(), Error> {
    let mut mac_fixture = fixture(1)?;
    let windows_fixture = fixture(2)?;
    let rect = Rect {
        x: 0.0,
        y: 0.0,
        width: 800.0,
        height: 600.0,
    };
    let placement = Placement {
        view_id: 1,
        revision: 1,
        camera: Camera {
            origin: Point { x: 0.0, y: 0.0 },
            zoom: 1.0,
        },
        panel: rect,
    };
    let mut router = InputRouter::default();
    let old_ticket = router.focus(&mac_fixture, placement, 0)?;
    router.authorize(old_ticket, &mac_fixture, placement, 1, 1)?;
    assert_eq!(
        router.focus(&windows_fixture, placement, 2),
        Err(Error::ReleasePending)
    );
    // Fixture acknowledgement only. Production requires verified native cleanup.
    router.acknowledge_release(router.pending_release().ok_or(Error::InvalidState)?)?;
    let new_ticket = router.focus(&windows_fixture, placement, 3)?;
    assert_eq!(
        router.authorize(old_ticket, &mac_fixture, placement, 2, 4),
        Err(Error::StaleTarget)
    );
    router.authorize(new_ticket, &windows_fixture, placement, 1, 5)?;
    let size = PixelSize {
        width: 1920,
        height: 1080,
    };
    let plan = plan(
        &[
            ViewDemand {
                gate: &mac_fixture,
                pixels: size,
                canvas_rect: rect,
                focused: false,
            },
            ViewDemand {
                gate: &windows_fixture,
                pixels: size,
                canvas_rect: rect,
                focused: true,
            },
        ],
        rect,
        Budget::default(),
        5,
    )?;
    assert_eq!(plan.usage.decoders, 2);
    let mut handoff = Handoff::begin(mac_fixture.key(), 1, rect, 5, 1000)?;
    handoff.destination_ready(&mac_fixture, 1, 6)?;
    handoff.acknowledge_hidden(&mac_fixture, 1, 7)?;
    mac_fixture.disconnect(8)?;
    assert_eq!(handoff.revalidate(&mac_fixture, 8), Err(Error::StaleTarget));
    assert_eq!(handoff.state(), State::RestoreRequested);
    handoff.acknowledge_restored(1)?;
    assert_eq!(handoff.state(), State::Local);
    println!("OFFLINE fixtures: two surfaces, bounded frame roundtrip, input isolation, cleanup barrier, admission budget, restore transaction passed.");
    println!(
        "No capture, network connection, native input, virtual monitor or mobile client was used."
    );
    Ok(())
}
