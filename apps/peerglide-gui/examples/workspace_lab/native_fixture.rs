//! CI adapter for two independent native window codec probes.
use std::io::{self, Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, Rect};
use linko_workspace_lab::coordinator::{Coordinator, Fault, PresentationTicket, ViewId};
use linko_workspace_lab::frame_binding::{FrameMatcher, MediaFrame, MediaTrack};
use linko_workspace_lab::geometry::PixelSize;
use linko_workspace_lab::media::{RawFrame, MAX_QUEUE_AGE_MS};
use linko_workspace_lab::Error;

use super::{elapsed_ms, NativePanelProbe, CANVAS};

pub(super) const PANELS: usize = 2;
pub(super) const FRAME_SIZE: PixelSize = PixelSize {
    width: 128,
    height: 96,
};
pub(super) const ASPECT: f32 = 128.0 / 96.0;
const IMAGE_SIZE: [usize; 2] = [FRAME_SIZE.width as usize, FRAME_SIZE.height as usize];
const FRAME_BYTES: usize = IMAGE_SIZE[0] * IMAGE_SIZE[1] * 4;
const COLORS: [Color32; PANELS] = [
    Color32::from_rgb(190, 30, 70),
    Color32::from_rgb(30, 70, 190),
];

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

pub(super) struct Received {
    pub(super) pixels: Vec<u8>,
    pub(super) first_seen: u64,
    pub(super) sequence: u32,
}

enum SourceEvent {
    Ready,
    Frame(Received),
}

pub(super) struct PendingDraw {
    pub(super) texture: egui::TextureHandle,
    pub(super) ticket: PresentationTicket,
    pub(super) first_seen: u64,
    pub(super) color: Color32,
}

struct Source {
    process: Child,
    view: ViewId,
    events: Receiver<io::Result<SourceEvent>>,
    ready: bool,
    draw: Option<PendingDraw>,
}

impl Source {
    fn start(
        launcher: &Path,
        codec: &Path,
        view: ViewId,
        frame_index: &str,
        clock: Instant,
        context: &egui::Context,
    ) -> io::Result<Self> {
        let mut process = Command::new(launcher)
            .arg("--export-native-window")
            .arg(codec)
            .arg(frame_index)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        let mut pipe = process.stdout.take().expect("fixture stdout is piped");
        let (send, events) = mpsc::sync_channel(1);
        let context = context.clone();
        std::thread::spawn(move || {
            let mut ready = [0];
            let event = pipe.read_exact(&mut ready).and_then(|()| {
                if ready == *b"R" {
                    Ok(SourceEvent::Ready)
                } else {
                    Err(io::Error::other("invalid native source ready record"))
                }
            });
            let ready = event.is_ok();
            if send.send(event).is_err() {
                return;
            }
            context.request_repaint();
            if ready {
                let mut pixels = vec![0; FRAME_BYTES];
                let result = pipe.read_exact(&mut pixels).map(|()| {
                    SourceEvent::Frame(Received {
                        pixels,
                        first_seen: elapsed_ms(clock),
                        sequence: 1,
                    })
                });
                let _ = send.send(result);
                context.request_repaint();
            }
        });
        Ok(Self {
            process,
            view,
            events,
            ready: false,
            draw: None,
        })
    }

    fn next_event(&self) -> io::Result<Option<SourceEvent>> {
        match self.events.try_recv() {
            Ok(event) => event.map(Some),
            Err(TryRecvError::Empty) => Ok(None),
            Err(error) => Err(io::Error::other(error)),
        }
    }

    fn send_command(&mut self, command: u8) -> io::Result<()> {
        self.process
            .stdin
            .as_mut()
            .expect("fixture stdin is piped")
            .write_all(&[command])
    }

    fn require_running(&mut self) -> io::Result<()> {
        if let Some(status) = self.process.try_wait()? {
            return Err(io::Error::other(format!(
                "native window source exited early: {status}"
            )));
        }
        Ok(())
    }

    fn finish_stop(&mut self, coordinator: &mut Coordinator, now: u64) -> Result<bool> {
        let Some(status) = self.process.try_wait()? else {
            return Ok(false);
        };
        if !status.success() {
            return Err(
                io::Error::other(format!("native decoder failed to stop: {status}")).into(),
            );
        }
        coordinator.close(self.view, Fault::SourceClosed, now)?;
        self.draw = None;
        Ok(true)
    }

    fn fresh_draw(&self, now: u64) -> Option<&PendingDraw> {
        self.draw
            .as_ref()
            .filter(|draw| now - draw.first_seen < MAX_QUEUE_AGE_MS)
    }
}

impl Drop for Source {
    fn drop(&mut self) {
        if self.process.try_wait().ok().flatten().is_none() {
            let _ = self.process.kill();
            let _ = self.process.wait();
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Starting,
    Receiving,
    Drawing,
    StoppingFirst,
    FirstCleared,
    StoppingSecond,
    BothCleared,
    Finished,
}

pub(super) struct NativeFixture {
    sources: [Source; PANELS],
    phase: Phase,
    second_started: bool,
    readback: Option<Receiver<bool>>,
    drawn_areas: Option<[Rect; PANELS]>,
    deadline: Instant,
}

impl NativeFixture {
    pub(super) fn start(
        launcher: &Path,
        codec: &Path,
        views: [ViewId; PANELS],
        clock: Instant,
        context: &egui::Context,
    ) -> io::Result<Self> {
        let mut sources = [
            Source::start(launcher, codec, views[0], "0", clock, context)?,
            Source::start(launcher, codec, views[1], "1", clock, context)?,
        ];
        sources[0].send_command(b'S')?;
        let second_started = !cfg!(target_os = "macos");
        if second_started {
            sources[1].send_command(b'S')?;
        }
        Ok(Self {
            sources,
            phase: Phase::Starting,
            second_started,
            readback: None,
            drawn_areas: None,
            deadline: Instant::now() + Duration::from_secs(45),
        })
    }

}

impl NativePanelProbe for NativeFixture {
    fn advance(
        &mut self,
        coordinator: &mut Coordinator,
        context: &egui::Context,
        now: u64,
    ) -> Result<bool> {
        if Instant::now() >= self.deadline {
            let error = io::Error::new(io::ErrorKind::TimedOut, "native panel fixture timed out");
            return Err(error.into());
        }
        match self.phase {
            Phase::Starting => {
                for source in &mut self.sources {
                    source.require_running()?;
                    if let Some(event) = source.next_event()? {
                        if source.ready || !matches!(event, SourceEvent::Ready) {
                            return Err(io::Error::other("unexpected native source event").into());
                        }
                        source.ready = true;
                    }
                }
                // Each ScreenCaptureKit fixture closes capture before READY.
                // Let that happen before the other AppKit fixture starts.
                if !self.second_started && self.sources[0].ready {
                    self.sources[1].send_command(b'S')?;
                    self.second_started = true;
                }
                if self.sources.iter().all(|source| source.ready) {
                    for source in &mut self.sources {
                        source.send_command(1)?;
                    }
                    self.phase = Phase::Receiving;
                }
            }
            Phase::Receiving => {
                for (source, expected) in self.sources.iter_mut().zip(COLORS) {
                    source.require_running()?;
                    if source.draw.is_none() {
                        match source.next_event()? {
                            Some(SourceEvent::Frame(frame)) => {
                                let draw = prepare_draw(
                                    coordinator,
                                    source.view,
                                    context,
                                    frame,
                                    now,
                                    None,
                                )?;
                                if !colors_match(draw.color, expected, 24) {
                                    return Err(io::Error::other(
                                        "decoded source color did not match panel",
                                    )
                                    .into());
                                }
                                source.draw = Some(draw);
                            }
                            Some(SourceEvent::Ready) => {
                                return Err(io::Error::other(
                                    "duplicate native source ready record",
                                )
                                .into());
                            }
                            None => {}
                        }
                    }
                }
                if self.sources.iter().all(|source| source.draw.is_some()) {
                    self.phase = Phase::Drawing;
                }
            }
            Phase::Drawing => {
                for source in &mut self.sources {
                    source.require_running()?;
                    source.fresh_draw(now).ok_or(Error::StaleFrame)?;
                }
                if self.readback_confirmed()? {
                    for source in &self.sources {
                        let draw = source.fresh_draw(now).ok_or(Error::StaleFrame)?;
                        coordinator.acknowledge_presentation(draw.ticket, now)?;
                    }
                    println!(
                        "Workspace panels: two native frames confirmed in one eframe readback."
                    );
                    self.sources[0].send_command(2)?;
                    self.phase = Phase::StoppingFirst;
                }
            }
            Phase::StoppingFirst => {
                self.sources[1].require_running()?;
                self.sources[1].fresh_draw(now).ok_or(Error::StaleFrame)?;
                if self.sources[0].finish_stop(coordinator, now)? {
                    self.phase = Phase::FirstCleared;
                }
            }
            Phase::FirstCleared => {
                self.sources[1].require_running()?;
                self.sources[1].fresh_draw(now).ok_or(Error::StaleFrame)?;
                if self.readback_confirmed()? {
                    println!("Workspace panels: first source cleared, second still visible.");
                    self.sources[1].send_command(2)?;
                    self.phase = Phase::StoppingSecond;
                }
            }
            Phase::StoppingSecond => {
                if self.sources[1].finish_stop(coordinator, now)? {
                    self.phase = Phase::BothCleared;
                }
            }
            Phase::BothCleared => {
                if self.readback_confirmed()? {
                    println!("Workspace panels: both sources reaped and both regions cleared.");
                    self.phase = Phase::Finished;
                }
            }
            Phase::Finished => {}
        }
        Ok(self.phase == Phase::Finished)
    }

    fn textures(&self, now: u64) -> [Option<&egui::TextureHandle>; PANELS] {
        if matches!(self.phase, Phase::Starting | Phase::Receiving) {
            return [None; PANELS];
        }
        std::array::from_fn(|index| {
            self.sources[index]
                .fresh_draw(now)
                .map(|draw| &draw.texture)
        })
    }

    fn verify_paint(
        &mut self,
        context: &egui::Context,
        areas: [Rect; PANELS],
    ) -> io::Result<()> {
        if self.readback.is_some() {
            return Ok(());
        }
        let expected = match self.phase {
            Phase::Drawing => {
                self.drawn_areas = Some(areas);
                [self.source_color(0)?, self.source_color(1)?]
            }
            Phase::FirstCleared if self.drawn_areas == Some(areas) => {
                [CANVAS, self.source_color(1)?]
            }
            Phase::BothCleared if self.drawn_areas == Some(areas) => [CANVAS; PANELS],
            Phase::FirstCleared | Phase::BothCleared => {
                return Err(io::Error::other("panels moved before clear readback"))
            }
            _ => return Ok(()),
        };
        let scale = context.pixels_per_point();
        let (send, receive) = mpsc::sync_channel(1);
        let callback_context = context.clone();
        context.request_screenshot(move |image| {
            let pass = areas
                .into_iter()
                .zip(expected)
                .all(|(area, color)| matches_pixels(&image, area, scale, color));
            let _ = send.send(pass);
            callback_context.request_repaint();
        });
        self.readback = Some(receive);
        Ok(())
    }

}

impl NativeFixture {
    fn source_color(&self, index: usize) -> io::Result<Color32> {
        self.sources[index]
            .draw
            .as_ref()
            .map(|draw| draw.color)
            .ok_or_else(|| io::Error::other("native panel has no decoded frame"))
    }

    fn readback_confirmed(&mut self) -> io::Result<bool> {
        let Some(readback) = &self.readback else {
            return Ok(false);
        };
        match readback.try_recv() {
            Ok(true) => {
                self.readback = None;
                Ok(true)
            }
            Ok(false) => Err(io::Error::other(format!(
                "workspace panel readback did not match in {:?}",
                self.phase
            ))),
            Err(TryRecvError::Empty) => Ok(false),
            Err(error) => Err(io::Error::other(error)),
        }
    }
}

pub(super) fn prepare_draw(
    coordinator: &mut Coordinator,
    view: ViewId,
    context: &egui::Context,
    frame: Received,
    now: u64,
    texture: Option<egui::TextureHandle>,
) -> Result<PendingDraw> {
    let first_seen = frame.first_seen.min(now);
    let frame_stamp = coordinator.frame_stamp(view, u64::from(frame.sequence))?;
    let raw = RawFrame::new(frame_stamp, FRAME_SIZE, frame.pixels)?;
    let media = MediaFrame {
        track: MediaTrack {
            generation: 1,
            track: 1,
            ssrc: 1,
        },
        timestamp: frame.sequence,
    };
    let gate = coordinator.view_gate(view)?;
    let mut matcher = FrameMatcher::new(gate, media.track, 0, first_seen)?;
    if matcher.decoded(gate, media, first_seen)?.is_some() {
        return Err(Error::InvalidState.into());
    }
    let matched = matcher
        .describe(gate, media, frame_stamp, first_seen)?
        .ok_or(Error::StaleFrame)?;
    coordinator.receive_matched(view, raw, matched, now)?;
    let presentation = coordinator
        .take_for_present(view, now)?
        .ok_or(Error::StaleFrame)?;
    let pixels = presentation.frame.pixels();
    let center = (IMAGE_SIZE[1] / 2 * IMAGE_SIZE[0] + IMAGE_SIZE[0] / 2) * 4;
    let image = egui::ColorImage::from_rgba_unmultiplied(IMAGE_SIZE, pixels);
    let options = egui::TextureOptions::NEAREST;
    let texture = if let Some(mut texture) = texture {
        texture.set(image, options);
        texture
    } else {
        context.load_texture("native-window", image, options)
    };
    Ok(PendingDraw {
        texture,
        ticket: presentation.ticket,
        first_seen,
        color: Color32::from_rgb(pixels[center], pixels[center + 1], pixels[center + 2]),
    })
}

pub(super) fn matches_pixels(
    image: &egui::ColorImage,
    area: Rect,
    scale: f32,
    expected: Color32,
) -> bool {
    let [width, height] = image.size;
    for vertical in [0.25, 0.5, 0.75] {
        for horizontal in [0.25, 0.5, 0.75] {
            let x = ((area.min.x + area.width() * horizontal) * scale).floor() as usize;
            let y = ((area.min.y + area.height() * vertical) * scale).floor() as usize;
            if x >= width || y >= height {
                return false;
            }
            let actual = image.pixels[y * width + x];
            if !colors_match(actual, expected, 18) {
                return false;
            }
        }
    }
    true
}

pub(super) fn colors_match(actual: Color32, expected: Color32, tolerance: u8) -> bool {
    actual.r().abs_diff(expected.r()) <= tolerance
        && actual.g().abs_diff(expected.g()) <= tolerance
        && actual.b().abs_diff(expected.b()) <= tolerance
}
