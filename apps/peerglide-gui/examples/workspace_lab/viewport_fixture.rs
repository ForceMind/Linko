//! Drives a single native decoder from the workspace's actual scroll viewport.
use std::io;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, Rect};
use linko_workspace_lab::coordinator::{Coordinator, ViewId};
use linko_workspace_lab::scheduling::{
    Allocation, Budget, Decision, DecoderReservation, DecoderReservations, PauseReason, Plan,
};
use linko_workspace_lab::session::FrameStamp;
use linko_workspace_lab::Error;

use super::native_fixture::{matches_pixels, prepare_draw, PendingDraw, Source, SourceEvent};
use super::{NativeTiles, Panel, Workspace, CANVAS};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const ORDER: [usize; 3] = [0, 1, 0];

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Starting,
    Receiving,
    Presenting,
    Displayed,
    Stopping,
}

struct Running {
    source: Source,
    reservation: DecoderReservation,
    slot: usize,
    scope: FrameStamp,
    state: State,
    draw: Option<PendingDraw>,
}

pub(super) struct ViewportFixture {
    launcher: PathBuf,
    codec: PathBuf,
    indices: [usize; 2],
    views: [ViewId; 2],
    started: Instant,
    reservations: DecoderReservations,
    running: Option<Running>,
    scroll_to: Option<usize>,
    readback: Option<Receiver<bool>>,
    displayed: usize,
    first_epoch: Option<u64>,
}

impl ViewportFixture {
    pub(super) fn new(
        launcher: PathBuf,
        codec: PathBuf,
        indices: [usize; 2],
        views: [ViewId; 2],
        started: Instant,
        budget: Budget,
    ) -> std::result::Result<Self, Error> {
        Ok(Self {
            launcher,
            codec,
            indices,
            views,
            started,
            reservations: DecoderReservations::new(budget)?,
            running: None,
            scroll_to: None,
            readback: None,
            displayed: 0,
            first_epoch: None,
        })
    }

    pub(super) fn step(
        &mut self,
        workspace: &mut Workspace,
        root: &mut egui::Ui,
        now: u64,
    ) -> Result<bool> {
        if self.started.elapsed() >= Duration::from_secs(60) {
            return Err(
                io::Error::new(io::ErrorKind::TimedOut, "workspace viewport timed out").into(),
            );
        }
        let context = root.ctx().clone();
        let preview = NativeTiles {
            indices: self.indices,
            textures: std::array::from_fn(|slot| {
                self.running
                    .as_ref()
                    .filter(|active| active.slot == slot)
                    .and_then(|active| active.draw.as_ref())
                    .map(|draw| &draw.texture)
            }),
        };
        let Some(paint) = workspace.show(root, now, Some(preview), self.scroll_to.take()) else {
            return Ok(false);
        };

        if let Some(active) = &mut self.running {
            let view = self.views[active.slot];
            let decision = paint
                .plan
                .surfaces
                .iter()
                .find(|surface| surface.key == active.scope.key)
                .map(|surface| surface.decision)
                .ok_or(Error::InvalidState)?;
            if active.state == State::Stopping {
                if active.source.poll_stopped()? {
                    self.reservations.release(&active.reservation)?;
                    self.running = None;
                    println!("Workspace viewport: decoder reaped before releasing its slot.");
                }
                return Ok(false);
            }
            let same_scope = workspace.coordinator.frame_stamp(view, 1)? == active.scope;
            let finishing = self.displayed == ORDER.len();
            if !matches!(decision, Decision::Stream(_)) || !same_scope || finishing {
                if active.state == State::Displayed
                    && same_scope
                    && !finishing
                    && decision != Decision::Pause(PauseReason::Offscreen)
                {
                    return Err(io::Error::other(
                        "shown panel lost its slot before going offscreen",
                    )
                    .into());
                }
                self.readback = None;
                active.stop(&mut workspace.coordinator, view, &context, now)?;
                return Ok(false);
            }
            if active.state == State::Presenting {
                if let Some(area) = paint.areas[active.slot] {
                    let draw = active.draw.as_ref().ok_or(Error::InvalidState)?;
                    if verified(&mut self.readback)? {
                        workspace
                            .coordinator
                            .acknowledge_presentation(draw.ticket, now)?;
                        if ORDER.get(self.displayed) != Some(&active.slot) {
                            return Err(io::Error::other(
                                "workspace displayed panels out of order",
                            )
                            .into());
                        }
                        if self.displayed == 0 {
                            self.first_epoch = Some(active.scope.view_epoch);
                        } else if self.displayed == 2
                            && self
                                .first_epoch
                                .is_none_or(|epoch| epoch >= active.scope.view_epoch)
                        {
                            return Err(io::Error::other(
                                "re-entered panel reused the old media epoch",
                            )
                            .into());
                        }
                        self.displayed += 1;
                        active.state = State::Displayed;
                        workspace.select(view);
                        self.scroll_to = ORDER.get(self.displayed).map(|&next| self.indices[next]);
                        println!(
                            "Workspace viewport: read back native panel {}.",
                            active.slot
                        );
                    } else if self.readback.is_none() {
                        self.readback = Some(request_readback(&context, area, draw.color));
                    }
                }
            } else if active.state == State::Displayed {
                active.source.require_running()?;
            } else {
                active.advance(&mut workspace.coordinator, view, &context, now)?;
            }
            return Ok(false);
        }

        if self.displayed == ORDER.len() {
            if verified(&mut self.readback)? {
                println!("Workspace viewport: A -> B -> A, slot released, final region cleared.");
                return Ok(true);
            }
            if let (None, Some(area)) = (&self.readback, paint.areas[0]) {
                self.readback = Some(request_readback(&context, area, CANVAS));
            }
        } else if let Some((slot, panel, allocation)) = self.next_panel(workspace, &paint.plan) {
            workspace.coordinator.view_gate(panel.view)?.can_view(now)?;
            let reservation = self.reservations.reserve(panel.key, allocation)?;
            let scope = workspace.coordinator.frame_stamp(panel.view, 1)?;
            let mut source = Source::start(
                &self.launcher,
                &self.codec,
                panel.view,
                ["0", "1"][slot],
                self.started,
                &context,
            )?;
            source.send_command(b'S')?;
            self.running = Some(Running {
                source,
                reservation,
                slot,
                scope,
                state: State::Starting,
                draw: None,
            });
        }
        Ok(false)
    }

    fn next_panel(&self, workspace: &Workspace, plan: &Plan) -> Option<(usize, Panel, Allocation)> {
        plan.surfaces.iter().find_map(|surface| {
            let Decision::Stream(allocation) = surface.decision else {
                return None;
            };
            self.indices.iter().enumerate().find_map(|(slot, &index)| {
                let panel = workspace.panels[index];
                (panel.key == surface.key).then_some((slot, panel, allocation))
            })
        })
    }
}

impl Running {
    fn advance(
        &mut self,
        coordinator: &mut Coordinator,
        view: ViewId,
        context: &egui::Context,
        now: u64,
    ) -> Result<()> {
        self.source.require_running()?;
        let Some(event) = self.source.next_event()? else {
            return Ok(());
        };
        match (self.state, event) {
            (State::Starting, SourceEvent::Ready) => {
                self.source.send_command(1)?;
                self.state = State::Receiving;
            }
            (State::Receiving, SourceEvent::Frame(frame)) => {
                let draw = prepare_draw(coordinator, view, context, frame, now, None)?;
                self.draw = Some(draw);
                self.state = State::Presenting;
            }
            _ => return Err(io::Error::other("unexpected viewport worker event").into()),
        }
        Ok(())
    }

    fn stop(
        &mut self,
        coordinator: &mut Coordinator,
        view: ViewId,
        context: &egui::Context,
        now: u64,
    ) -> Result<()> {
        if let Some(draw) = self.draw.take() {
            let id = draw.texture.id();
            drop(draw);
            if context.tex_manager().read().meta(id).is_some() {
                return Err(io::Error::other("offscreen media texture was kept").into());
            }
        }
        coordinator.invalidate_media(view, now)?;
        self.source.send_command(2)?;
        self.state = State::Stopping;
        Ok(())
    }
}

fn verified(readback: &mut Option<Receiver<bool>>) -> io::Result<bool> {
    let Some(receiver) = readback else {
        return Ok(false);
    };
    match receiver.try_recv() {
        Ok(true) => {
            *readback = None;
            Ok(true)
        }
        Ok(false) => Err(io::Error::other("viewport readback did not match")),
        Err(TryRecvError::Empty) => Ok(false),
        Err(error) => Err(io::Error::other(error)),
    }
}

fn request_readback(context: &egui::Context, area: Rect, color: Color32) -> Receiver<bool> {
    let scale = context.pixels_per_point();
    let (send, receive) = mpsc::sync_channel(1);
    let context_to_wake = context.clone();
    context.request_screenshot(move |image| {
        let _ = send.send(matches_pixels(&image, area, scale, color));
        context_to_wake.request_repaint();
    });
    receive
}
