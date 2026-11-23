//! Drives the native two-source codec fixture with actual eframe display receipts.
use std::io::{self, Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, Rect};
use linko_workspace_lab::codec_fixture::{nv12_rgba, NV12_BYTES};
use linko_workspace_lab::coordinator::{Coordinator, Fault, ViewId};
use linko_workspace_lab::media::MAX_QUEUE_AGE_MS;
use linko_workspace_lab::Error;

use super::native_fixture::{
    colors_match, matches_pixels, prepare_draw, PendingDraw, Received, FRAME_SIZE, PANELS,
};
use super::{elapsed_ms, NativePanelProbe, CANVAS};

const GENERATION: u64 = 0x0102_0a04_051a_0708;
const START: u8 = 1;
const STOPPED: u8 = 4;
const FRAME: u8 = 5;
const DISPLAYED: u8 = 8;
const PAIRS: u32 = 4;
const SURVIVOR: u32 = PAIRS + 1;
const COLORS: [[[u8; 3]; SURVIVOR as usize]; PANELS] = [
    [
        [190, 30, 70],
        [190, 100, 70],
        [190, 170, 70],
        [190, 30, 150],
        [0; 3],
    ],
    [
        [30, 70, 190],
        [30, 145, 190],
        [30, 220, 190],
        [95, 70, 190],
        [30, 140, 115],
    ],
];

type ProbeResult<T> = Result<T, Box<dyn std::error::Error>>;

enum Event {
    Frame { source: usize, frame: Received },
    Stopped(usize),
    End,
}

#[derive(Clone, Copy)]
enum Phase {
    Receiving,
    Drawing,
    Clearing,
    Exiting,
    Finished,
}

pub(super) struct LiveFixture {
    process: Child,
    views: [ViewId; PANELS],
    events: Receiver<io::Result<Event>>,
    round: u32,
    phase: Phase,
    stopped: [bool; PANELS],
    incoming: [Option<Received>; PANELS],
    draws: [Option<PendingDraw>; PANELS],
    readback: Option<Receiver<(u32, bool)>>,
    areas: Option<[Rect; PANELS]>,
    first_receipt: Option<u64>,
    deadline: Instant,
    ended: bool,
}

impl LiveFixture {
    pub(super) fn start(
        fixture: &Path,
        views: [ViewId; PANELS],
        clock: Instant,
        context: &egui::Context,
    ) -> io::Result<Self> {
        let mut process = Command::new(fixture)
            .arg("--dual-live-pixels")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        let mut pipe = process.stdout.take().expect("fixture stdout is piped");
        let (send, events) = mpsc::sync_channel(1);
        let context = context.clone();
        std::thread::spawn(move || loop {
            let event = read_event(&mut pipe, clock);
            let terminal = matches!(&event, Ok(Event::End) | Err(_));
            if send.send(event).is_err() {
                break;
            }
            context.request_repaint();
            if terminal {
                break;
            }
        });
        let mut fixture = Self {
            process,
            views,
            events,
            round: 1,
            phase: Phase::Receiving,
            stopped: [false; PANELS],
            incoming: [None, None],
            draws: [None, None],
            readback: None,
            areas: None,
            first_receipt: None,
            deadline: Instant::now() + Duration::from_secs(45),
            ended: false,
        };
        fixture.write(&record(START, GENERATION))?;
        Ok(fixture)
    }

    fn write(&mut self, data: &[u8]) -> io::Result<()> {
        self.process
            .stdin
            .as_mut()
            .expect("fixture stdin is piped")
            .write_all(data)
    }

    fn next_event(&self) -> io::Result<Option<Event>> {
        match self.events.try_recv() {
            Ok(event) => event.map(Some),
            Err(TryRecvError::Empty) => Ok(None),
            Err(error) => Err(io::Error::other(error)),
        }
    }

    fn receive(
        &mut self,
        coordinator: &mut Coordinator,
        context: &egui::Context,
        now: u64,
    ) -> ProbeResult<()> {
        let Some(event) = self.next_event()? else {
            return Ok(());
        };
        match event {
            Event::Stopped(source)
                if (source == 0 && self.round == SURVIVOR && !self.stopped[0])
                    || (source == 1
                        && self.round == SURVIVOR + 1
                        && self.stopped[0]
                        && !self.stopped[1]) =>
            {
                coordinator.close(self.views[source], Fault::SourceClosed, now)?;
                self.stopped[source] = true;
                self.draws[source] = None;
                if source == 1 {
                    self.phase = Phase::Clearing;
                }
            }
            Event::Frame { source, frame }
                if frame.sequence == self.round
                    && self.incoming[source].is_none()
                    && (self.round <= PAIRS
                        || (self.round == SURVIVOR && self.stopped[0] && source == 1)) =>
            {
                self.incoming[source] = Some(frame);
                if self.incoming[1].is_some() && (self.stopped[0] || self.incoming[0].is_some()) {
                    self.install_round(coordinator, context, now)?;
                }
            }
            _ => return Err(io::Error::other("unexpected native frame or stop order").into()),
        }
        Ok(())
    }

    fn install_round(
        &mut self,
        coordinator: &mut Coordinator,
        context: &egui::Context,
        now: u64,
    ) -> ProbeResult<()> {
        for (source, incoming) in self.incoming.iter_mut().enumerate() {
            if let Some(frame) = incoming.take() {
                let [r, g, b] = COLORS[source][(self.round - 1) as usize];
                let old_texture = self.draws[source].take().map(|draw| draw.texture);
                let draw = prepare_draw(
                    coordinator,
                    self.views[source],
                    context,
                    frame,
                    now,
                    old_texture,
                )?;
                if !colors_match(draw.color, Color32::from_rgb(r, g, b), 24) {
                    return Err(io::Error::other("unexpected native frame color").into());
                }
                self.draws[source] = Some(draw);
            }
        }
        self.phase = Phase::Drawing;
        Ok(())
    }

    fn confirm_draws(&mut self, coordinator: &mut Coordinator, now: u64) -> ProbeResult<()> {
        let first = *self.first_receipt.get_or_insert(now);
        if self.round == PAIRS && now - first <= MAX_QUEUE_AGE_MS {
            return Err(io::Error::other("dual display did not cover the frame lifetime").into());
        }
        for draw in self.draws.iter().flatten() {
            if now.saturating_sub(draw.first_seen) >= MAX_QUEUE_AGE_MS {
                return Err(Error::StaleFrame.into());
            }
        }
        for draw in self.draws.iter().flatten() {
            coordinator.acknowledge_presentation(draw.ticket, now)?;
        }
        for (source, stopped) in self.stopped.into_iter().enumerate() {
            if !stopped {
                self.write(&record(DISPLAYED, GENERATION + source as u64))?;
                self.write(&self.round.to_be_bytes())?;
            }
        }
        println!(
            "Live panels: round {} verified in eframe after {}ms{}.",
            self.round,
            now - first,
            if self.round == SURVIVOR {
                ", first region cleared"
            } else {
                ""
            }
        );
        self.round += 1;
        self.phase = Phase::Receiving;
        Ok(())
    }

    fn readback_confirmed(&mut self) -> io::Result<bool> {
        let Some(readback) = &self.readback else {
            return Ok(false);
        };
        match readback.try_recv() {
            Ok((round, true)) if round == self.round => {
                self.readback = None;
                Ok(true)
            }
            Ok(_) => Err(io::Error::other(
                "live panel readback did not match the round",
            )),
            Err(TryRecvError::Empty) => Ok(false),
            Err(error) => Err(io::Error::other(error)),
        }
    }
}

impl NativePanelProbe for LiveFixture {
    fn advance(
        &mut self,
        coordinator: &mut Coordinator,
        context: &egui::Context,
        now: u64,
    ) -> ProbeResult<bool> {
        if Instant::now() >= self.deadline {
            return Err(
                io::Error::new(io::ErrorKind::TimedOut, "live panel fixture timed out").into(),
            );
        }
        match self.phase {
            Phase::Receiving => self.receive(coordinator, context, now)?,
            Phase::Drawing => {
                if self.readback_confirmed()? {
                    self.confirm_draws(coordinator, now)?;
                }
            }
            Phase::Clearing => {
                if self.readback_confirmed()? {
                    self.phase = Phase::Exiting;
                }
            }
            Phase::Exiting => {
                if !self.ended {
                    match self.next_event()? {
                        Some(Event::End) => self.ended = true,
                        None => return Ok(false),
                        _ => {
                            return Err(io::Error::other(
                                "unexpected frames after both sources closed",
                            )
                            .into());
                        }
                    }
                }
                if let Some(status) = self.process.try_wait()? {
                    if !status.success() {
                        return Err(io::Error::other(format!(
                            "native live source exited: {status}"
                        ))
                        .into());
                    }
                    println!("Live panels: both regions cleared and native fixture reaped.");
                    self.phase = Phase::Finished;
                    return Ok(true);
                }
            }
            Phase::Finished => return Ok(true),
        }
        Ok(false)
    }

    fn textures(&self, now: u64) -> [Option<&egui::TextureHandle>; PANELS] {
        self.draws.each_ref().map(|draw| {
            draw.as_ref()
                .filter(|draw| now.saturating_sub(draw.first_seen) < MAX_QUEUE_AGE_MS)
                .map(|draw| &draw.texture)
        })
    }

    fn verify_paint(&mut self, context: &egui::Context, areas: [Rect; PANELS]) -> io::Result<()> {
        if self.readback.is_some() || !matches!(self.phase, Phase::Drawing | Phase::Clearing) {
            return Ok(());
        }
        if *self.areas.get_or_insert(areas) != areas {
            return Err(io::Error::other("live panels moved before readback"));
        }
        let expected = self
            .draws
            .each_ref()
            .map(|draw| draw.as_ref().map_or(CANVAS, |draw| draw.color));
        let round = self.round;
        let (send, receive) = mpsc::sync_channel(1);
        let scale = context.pixels_per_point();
        let repaint = context.clone();
        context.request_screenshot(move |image| {
            let valid = areas
                .into_iter()
                .zip(expected)
                .all(|(area, color)| matches_pixels(&image, area, scale, color));
            let _ = send.send((round, valid));
            repaint.request_repaint();
        });
        self.readback = Some(receive);
        Ok(())
    }
}

impl Drop for LiveFixture {
    fn drop(&mut self) {
        if self.process.try_wait().ok().flatten().is_none() {
            let _ = self.process.kill();
            let _ = self.process.wait();
        }
    }
}

fn record(kind: u8, generation: u64) -> [u8; 16] {
    let mut record = *b"LRW4\x01\0\0\0\0\0\0\0\0\0\0\0";
    record[5] = kind;
    record[8..].copy_from_slice(&generation.to_be_bytes());
    record
}

fn read_event(pipe: &mut impl Read, clock: Instant) -> io::Result<Event> {
    let mut header = [0; 16];
    match pipe.read_exact(&mut header[..1]) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(Event::End),
        Err(error) => return Err(error),
    }
    let first_seen = elapsed_ms(clock);
    pipe.read_exact(&mut header[1..])?;
    let kind = header[5];
    let source = (0..PANELS)
        .find(|&index| header == record(kind, GENERATION + index as u64))
        .ok_or_else(|| io::Error::other("invalid live fixture header"))?;
    match kind {
        STOPPED => Ok(Event::Stopped(source)),
        FRAME => {
            let mut metadata = [0; 12];
            pipe.read_exact(&mut metadata)?;
            let sequence = u32::from_be_bytes(metadata[..4].try_into().unwrap());
            if metadata[4..6] != (FRAME_SIZE.width as u16).to_be_bytes()
                || metadata[6..8] != (FRAME_SIZE.height as u16).to_be_bytes()
                || metadata[8..] != (NV12_BYTES as u32).to_be_bytes()
            {
                return Err(io::Error::other("invalid live fixture frame size"));
            }
            let mut bytes = [0; NV12_BYTES];
            pipe.read_exact(&mut bytes)?;
            Ok(Event::Frame {
                source,
                frame: Received {
                    pixels: nv12_rgba(&bytes),
                    first_seen,
                    sequence,
                },
            })
        }
        _ => Err(io::Error::other("invalid live fixture message kind")),
    }
}
