//! Verifies one live native decoder and one texture shared by two regions.
use std::io::{self, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, Rect};
use linko_workspace_lab::coordinator::{Coordinator, MirrorHandle, ViewId};
use linko_workspace_lab::media::MAX_QUEUE_AGE_MS;
use linko_workspace_lab::scheduling::{Budget, Decision, PauseReason};
use linko_workspace_lab::Error;

use super::live_fixture::{read_event, record, Event, DISPLAYED, GENERATION, START};
use super::native_fixture::{colors_match, matches_pixels, prepare_draw, PendingDraw};
use super::{canvas_rect, render_native_preview, CANVAS, ORIGIN};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const STOP: u8 = 3;
const COLORS: [Color32; 3] = [
    Color32::from_rgb(190, 30, 70),
    Color32::from_rgb(190, 100, 70),
    Color32::from_rgb(190, 170, 70),
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Receiving,
    Drawing,
    Revoked,
}

pub(super) struct MirrorFixture {
    worker: Child,
    events: Receiver<io::Result<Event>>,
    view: ViewId,
    mirror: Option<MirrorHandle>,
    phase: Phase,
    sequence: u32,
    draw: Option<PendingDraw>,
    texture_id: Option<egui::TextureId>,
    areas: [Rect; 2],
    readback: Option<Receiver<bool>>,
    deadline: Instant,
    stopped: bool,
    ended: bool,
    cleared: bool,
}

impl MirrorFixture {
    pub(super) fn start(
        binary: &Path,
        coordinator: &mut Coordinator,
        view: ViewId,
        clock: Instant,
        context: &egui::Context,
    ) -> anyhow::Result<Self> {
        let initial = Rect::from_min_size(egui::pos2(456.0, 0.0), egui::vec2(400.0, 200.0));
        let mirror = coordinator.attach_mirror(view, canvas_rect(initial), 0)?;
        let mut worker = Command::new(binary)
            .arg("--mirror-live-pixels")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        let mut pipe = worker.stdout.take().expect("mirror stdout is piped");
        let (send, events) = mpsc::sync_channel(1);
        let wake = context.clone();
        std::thread::spawn(move || loop {
            let event = read_event(&mut pipe, clock);
            let terminal = matches!(&event, Ok(Event::End) | Err(_));
            if send.send(event).is_err() {
                break;
            }
            wake.request_repaint();
            if terminal {
                break;
            }
        });
        let mut fixture = Self {
            worker,
            events,
            view,
            mirror: Some(mirror),
            phase: Phase::Receiving,
            sequence: 1,
            draw: None,
            texture_id: None,
            areas: [Rect::NOTHING; 2],
            readback: None,
            deadline: Instant::now() + Duration::from_secs(45),
            stopped: false,
            ended: false,
            cleared: false,
        };
        fixture.write(&record(START, GENERATION))?;
        Ok(fixture)
    }

    pub(super) fn step(
        &mut self,
        coordinator: &mut Coordinator,
        root: &mut egui::Ui,
        now: u64,
    ) -> Result<bool> {
        if Instant::now() >= self.deadline {
            return Err(
                io::Error::new(io::ErrorKind::TimedOut, "workspace mirror timed out").into(),
            );
        }
        coordinator.poll(now)?;
        let context = root.ctx().clone();
        match self.phase {
            Phase::Receiving => self.receive_frame(coordinator, &context, now)?,
            Phase::Drawing => {
                if self.confirm_readback()? {
                    self.complete_frame(coordinator, &context, now)?;
                }
            }
            Phase::Revoked => {
                if self.confirm_readback()? {
                    self.cleared = true;
                }
                if self.poll_stop()? && self.cleared {
                    println!("Workspace mirror: both regions clear, one native worker reaped.");
                    return Ok(true);
                }
            }
        }

        let viewport = root.max_rect();
        self.areas = self.paint(root, now);
        if self.areas[0].intersects(self.areas[1])
            || self.areas.iter().any(|&area| !viewport.contains_rect(area))
        {
            return Err(io::Error::other("mirror regions must be separate and visible").into());
        }
        coordinator.relayout(self.view, ORIGIN, canvas_rect(self.areas[0]), now)?;
        if let Some(mirror) = &self.mirror {
            coordinator.relayout_mirror(mirror, canvas_rect(self.areas[1]), now)?;
        }
        let plan = coordinator.plan_viewport(
            canvas_rect(viewport),
            Some(self.view),
            Budget {
                decoders: 1,
                ..Budget::default()
            },
            now,
        )?;
        let planned = plan.surfaces.first().ok_or(Error::InvalidState)?;
        let expected = if self.phase == Phase::Revoked {
            plan.usage.decoders == 0
                && planned.decision == Decision::Pause(PauseReason::Unauthorized)
        } else {
            plan.usage.decoders == 1 && matches!(planned.decision, Decision::Stream(_))
        };
        if plan.surfaces.len() != 1 || !expected {
            return Err(io::Error::other("unexpected mirror decoder allocation").into());
        }
        if self.readback.is_none() && !self.cleared {
            match self.phase {
                Phase::Receiving => {}
                Phase::Drawing => {
                    let draw = self.draw.as_ref().ok_or(Error::InvalidState)?;
                    if now - draw.first_seen >= MAX_QUEUE_AGE_MS {
                        return Err(Error::StaleFrame.into());
                    }
                    let colors = [
                        draw.color,
                        if self.mirror.is_some() {
                            draw.color
                        } else {
                            CANVAS
                        },
                    ];
                    self.request_readback(&context, colors);
                }
                Phase::Revoked => self.request_readback(&context, [CANVAS; 2]),
            }
        }
        Ok(false)
    }

    fn receive_frame(
        &mut self,
        coordinator: &mut Coordinator,
        context: &egui::Context,
        now: u64,
    ) -> Result<()> {
        let Some(event) = self.next_event()? else {
            return Ok(());
        };
        let Event::Frame { source: 0, frame } = event else {
            return Err(io::Error::other("expected one live mirror source").into());
        };
        if frame.sequence != self.sequence {
            return Err(Error::InvalidSequence.into());
        }
        let texture = self.draw.take().map(|draw| draw.texture);
        let draw = prepare_draw(coordinator, self.view, context, frame, now, texture)?;
        if !colors_match(draw.color, COLORS[self.sequence as usize - 1], 24) {
            return Err(
                io::Error::other("live mirror color did not match the source revision").into(),
            );
        }
        let id = draw.texture.id();
        if self.texture_id.replace(id).is_some_and(|old| old != id) {
            return Err(io::Error::other("mirror created a second video texture").into());
        }
        self.draw = Some(draw);
        self.phase = Phase::Drawing;
        Ok(())
    }

    fn complete_frame(
        &mut self,
        coordinator: &mut Coordinator,
        context: &egui::Context,
        now: u64,
    ) -> Result<()> {
        let draw = self.draw.as_ref().ok_or(Error::InvalidState)?;
        coordinator.acknowledge_presentation(draw.ticket, now)?;
        match self.sequence {
            1 => {
                let mirror = self.mirror.take().ok_or(Error::InvalidState)?;
                coordinator.detach_mirror(&mirror, now)?;
                println!("Workspace mirror: frame 1 shared by both regions; mirror detached.");
            }
            2 => {
                self.mirror =
                    Some(coordinator.attach_mirror(self.view, canvas_rect(self.areas[1]), now)?);
                println!("Workspace mirror: frame 2 updated primary only; mirror attached again.");
            }
            3 => {
                coordinator.revoke_view(self.view, now)?;
                self.draw = None;
                let id = self.texture_id.take().ok_or(Error::InvalidState)?;
                if context.tex_manager().read().meta(id).is_some() {
                    return Err(io::Error::other("revoked mirror texture was retained").into());
                }
                self.send_receipt(STOP)?;
                self.phase = Phase::Revoked;
                println!("Workspace mirror: frame 3 shared, viewing revoked and texture released.");
                return Ok(());
            }
            _ => return Err(Error::InvalidSequence.into()),
        }
        self.send_receipt(DISPLAYED)?;
        self.sequence += 1;
        self.phase = Phase::Receiving;
        Ok(())
    }

    fn paint(&self, root: &mut egui::Ui, now: u64) -> [Rect; 2] {
        let texture = self
            .draw
            .as_ref()
            .filter(|draw| now - draw.first_seen < MAX_QUEUE_AGE_MS)
            .map(|draw| &draw.texture);
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(Color32::WHITE).inner_margin(24))
            .show(root, |ui| {
                ui.heading("Workspace");
                ui.columns(2, |columns| {
                    columns[0].label("Source");
                    columns[1].label("Mirror");
                    [
                        render_native_preview(&mut columns[0], texture, 160.0),
                        render_native_preview(
                            &mut columns[1],
                            texture.filter(|_| self.mirror.is_some()),
                            160.0,
                        ),
                    ]
                })
            })
            .inner
    }

    fn request_readback(&mut self, context: &egui::Context, colors: [Color32; 2]) {
        let areas = self.areas;
        let scale = context.pixels_per_point();
        let (send, receive) = mpsc::sync_channel(1);
        let wake = context.clone();
        context.request_screenshot(move |image| {
            let matched = areas
                .into_iter()
                .zip(colors)
                .all(|(area, color)| matches_pixels(&image, area, scale, color));
            let _ = send.send(matched);
            wake.request_repaint();
        });
        self.readback = Some(receive);
    }

    fn confirm_readback(&mut self) -> io::Result<bool> {
        let Some(receipt) = &self.readback else {
            return Ok(false);
        };
        match receipt.try_recv() {
            Ok(true) => {
                self.readback = None;
                Ok(true)
            }
            Ok(false) => Err(io::Error::other(format!(
                "workspace mirror pixels did not match for frame {}",
                self.sequence
            ))),
            Err(TryRecvError::Empty) => Ok(false),
            Err(error) => Err(io::Error::other(error)),
        }
    }

    fn next_event(&self) -> io::Result<Option<Event>> {
        match self.events.try_recv() {
            Ok(event) => event.map(Some),
            Err(TryRecvError::Empty) => Ok(None),
            Err(error) => Err(io::Error::other(error)),
        }
    }

    fn poll_stop(&mut self) -> io::Result<bool> {
        if !self.ended {
            match self.next_event()? {
                Some(Event::Stopped(0)) if !self.stopped => self.stopped = true,
                Some(Event::End) if self.stopped => self.ended = true,
                Some(_) => return Err(io::Error::other("unexpected native mirror stop record")),
                None => {}
            }
        }
        let Some(status) = self.worker.try_wait()? else {
            return Ok(false);
        };
        if !status.success() {
            return Err(io::Error::other(format!(
                "native mirror worker failed: {status}"
            )));
        }
        Ok(self.ended)
    }

    fn send_receipt(&mut self, command: u8) -> io::Result<()> {
        let mut bytes = [0; 20];
        bytes[..16].copy_from_slice(&record(command, GENERATION));
        bytes[16..].copy_from_slice(&self.sequence.to_be_bytes());
        self.write(&bytes)
    }

    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.worker
            .stdin
            .as_mut()
            .expect("mirror stdin is piped")
            .write_all(bytes)
    }
}

impl Drop for MirrorFixture {
    fn drop(&mut self) {
        if self.worker.try_wait().ok().flatten().is_none() {
            let _ = self.worker.kill();
            let _ = self.worker.wait();
        }
    }
}
