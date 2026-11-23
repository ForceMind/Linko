//! CI adapter for the existing native window codec probe.
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

use super::{elapsed_ms, CANVAS};

pub(super) const FRAME_SIZE: PixelSize = PixelSize {
    width: 128,
    height: 96,
};
pub(super) const ASPECT: f32 = 128.0 / 96.0;
const IMAGE_SIZE: [usize; 2] = [FRAME_SIZE.width as usize, FRAME_SIZE.height as usize];
const FRAME_BYTES: usize = IMAGE_SIZE[0] * IMAGE_SIZE[1] * 4;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

struct Received {
    pixels: Vec<u8>,
    first_seen: u64,
}

struct PendingDraw {
    texture: egui::TextureHandle,
    ticket: PresentationTicket,
    first_seen: u64,
    color: Color32,
}

enum Phase {
    Receiving(Receiver<io::Result<Received>>),
    Drawing(PendingDraw),
    Stopping(PendingDraw),
    Clearing,
    Finished,
}

pub(super) struct NativeFixture {
    process: Child,
    phase: Phase,
    readback: Option<Receiver<bool>>,
    drawn_area: Option<Rect>,
    deadline: Instant,
}

impl NativeFixture {
    pub(super) fn start(
        launcher: &Path,
        codec: &Path,
        clock: Instant,
        context: &egui::Context,
    ) -> io::Result<Self> {
        let mut process = Command::new(launcher)
            .arg("--export-native-window")
            .arg(codec)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        let mut pipe = process.stdout.take().expect("fixture stdout is piped");
        let (send, receive) = mpsc::sync_channel(1);
        let context = context.clone();
        std::thread::spawn(move || {
            let mut pixels = vec![0; FRAME_BYTES];
            let result = pipe.read_exact(&mut pixels).map(|()| Received {
                pixels,
                first_seen: elapsed_ms(clock),
            });
            let _ = send.send(result);
            context.request_repaint();
        });
        Ok(Self {
            process,
            phase: Phase::Receiving(receive),
            readback: None,
            drawn_area: None,
            deadline: Instant::now() + Duration::from_secs(45),
        })
    }

    pub(super) fn advance(
        &mut self,
        coordinator: &mut Coordinator,
        view: ViewId,
        context: &egui::Context,
        now: u64,
    ) -> Result<bool> {
        if Instant::now() >= self.deadline {
            let error = io::Error::new(io::ErrorKind::TimedOut, "native panel fixture timed out");
            return Err(error.into());
        }
        let status = self.process.try_wait()?;
        self.phase = match std::mem::replace(&mut self.phase, Phase::Finished) {
            Phase::Receiving(receive) => {
                if status.is_some() {
                    return Err(
                        io::Error::other("native window source stopped before frame").into(),
                    );
                }
                match receive.try_recv() {
                    Ok(frame) => {
                        Phase::Drawing(prepare_draw(coordinator, view, context, frame?, now)?)
                    }
                    Err(TryRecvError::Empty) => Phase::Receiving(receive),
                    Err(error) => return Err(error.into()),
                }
            }
            Phase::Drawing(draw) => {
                if status.is_some() || now - draw.first_seen >= MAX_QUEUE_AGE_MS {
                    return Err(
                        io::Error::other("native panel frame expired before readback").into(),
                    );
                }
                if self.readback_confirmed()? {
                    coordinator.acknowledge_presentation(draw.ticket, now)?;
                    self.process
                        .stdin
                        .as_mut()
                        .expect("fixture stdin is piped")
                        .write_all(&[1])?;
                    println!(
                        "Workspace panel: native decoded pixels confirmed by eframe readback."
                    );
                    Phase::Stopping(draw)
                } else {
                    Phase::Drawing(draw)
                }
            }
            Phase::Stopping(draw) => match status {
                None => Phase::Stopping(draw),
                Some(status) if status.success() => {
                    coordinator.close(view, Fault::SourceClosed, now)?;
                    Phase::Clearing
                }
                Some(_) => {
                    return Err(io::Error::other("native decoder did not stop cleanly").into())
                }
            },
            Phase::Clearing if self.readback_confirmed()? => {
                println!("Workspace panel: source reaped, texture released and readback cleared.");
                Phase::Finished
            }
            phase => phase,
        };
        Ok(matches!(self.phase, Phase::Finished))
    }

    pub(super) fn texture(&self, now: u64) -> Option<&egui::TextureHandle> {
        match &self.phase {
            Phase::Drawing(draw) | Phase::Stopping(draw)
                if now - draw.first_seen < MAX_QUEUE_AGE_MS =>
            {
                Some(&draw.texture)
            }
            _ => None,
        }
    }

    pub(super) fn verify_paint(&mut self, context: &egui::Context, area: Rect) -> io::Result<()> {
        if self.readback.is_some() {
            return Ok(());
        }
        let expected = match &self.phase {
            Phase::Drawing(draw) => {
                self.drawn_area = Some(area);
                draw.color
            }
            Phase::Clearing if self.drawn_area == Some(area) => CANVAS,
            Phase::Clearing => return Err(io::Error::other("panel moved before clear readback")),
            _ => return Ok(()),
        };
        let scale = context.pixels_per_point();
        let (send, receive) = mpsc::sync_channel(1);
        let callback_context = context.clone();
        context.request_screenshot(move |image| {
            let _ = send.send(matches_pixels(&image, area, scale, expected));
            callback_context.request_repaint();
        });
        self.readback = Some(receive);
        Ok(())
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
            Ok(false) => Err(io::Error::other("workspace panel readback did not match")),
            Err(TryRecvError::Empty) => Ok(false),
            Err(error) => Err(io::Error::other(error)),
        }
    }
}

impl Drop for NativeFixture {
    fn drop(&mut self) {
        if self.process.try_wait().ok().flatten().is_none() {
            let _ = self.process.kill();
            let _ = self.process.wait();
        }
    }
}

fn prepare_draw(
    coordinator: &mut Coordinator,
    view: ViewId,
    context: &egui::Context,
    frame: Received,
    now: u64,
) -> Result<PendingDraw> {
    let first_seen = frame.first_seen.min(now);
    let frame_stamp = coordinator.frame_stamp(view, 1)?;
    let raw = RawFrame::new(frame_stamp, FRAME_SIZE, frame.pixels)?;
    let media = MediaFrame {
        track: MediaTrack {
            generation: 1,
            track: 1,
            ssrc: 1,
        },
        timestamp: 1,
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
    let rgba = presentation.frame.pixels();
    let center = (IMAGE_SIZE[1] / 2 * IMAGE_SIZE[0] + IMAGE_SIZE[0] / 2) * 4;
    Ok(PendingDraw {
        texture: context.load_texture(
            "native-window",
            egui::ColorImage::from_rgba_unmultiplied(IMAGE_SIZE, rgba),
            egui::TextureOptions::NEAREST,
        ),
        ticket: presentation.ticket,
        first_seen,
        color: Color32::from_rgb(rgba[center], rgba[center + 1], rgba[center + 2]),
    })
}

fn matches_pixels(image: &egui::ColorImage, area: Rect, scale: f32, expected: Color32) -> bool {
    let [width, height] = image.size;
    for vertical in [0.25, 0.5, 0.75] {
        for horizontal in [0.25, 0.5, 0.75] {
            let x = ((area.min.x + area.width() * horizontal) * scale).floor() as usize;
            let y = ((area.min.y + area.height() * vertical) * scale).floor() as usize;
            if x >= width || y >= height {
                return false;
            }
            let actual = image.pixels[y * width + x];
            if actual.r().abs_diff(expected.r()) > 18
                || actual.g().abs_diff(expected.g()) > 18
                || actual.b().abs_diff(expected.b()) > 18
            {
                return false;
            }
        }
    }
    true
}
