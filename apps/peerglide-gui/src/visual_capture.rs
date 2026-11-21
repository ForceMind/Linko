//! Native frames from the owned Xvfb window of a CI-only test.
use anyhow::{Context, Result};
use eframe::{egui, icon_data::IconDataExt};
use std::{
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
    sync::{mpsc, Arc},
    time::{Duration, Instant},
};
use winit::platform::x11::EventLoopBuilderExtX11;

type Frame = Arc<egui::ColorImage>;

pub(super) fn output_dir() -> Result<PathBuf> {
    anyhow::ensure!(std::env::var("CI").as_deref() == Ok("true"));
    Ok(PathBuf::from(
        std::env::var_os("LINKO_UI_CAPTURE_DIR").context("capture directory required")?,
    ))
}

pub(super) fn options() -> eframe::NativeOptions {
    eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([920.0, 700.0]),
        event_loop_builder: Some(Box::new(|builder| {
            builder.with_any_thread(true);
        })),
        ..Default::default()
    }
}

pub(super) fn write_shot(path: &Path, image: &egui::ColorImage) -> Result<()> {
    anyhow::ensure!(image.size == [920, 700], "unexpected native capture size");
    let icon = egui::IconData {
        width: 920,
        height: 700,
        rgba: image
            .pixels
            .iter()
            .flat_map(|pixel| pixel.to_array())
            .collect(),
    };
    let png = icon.to_png_bytes().map_err(anyhow::Error::msg)?;
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?
        .write_all(&png)?;
    Ok(())
}

struct Capture<F> {
    draw: F,
    count: usize,
    frames: Vec<Frame>,
    pending: Option<mpsc::Receiver<Frame>>,
    report: Option<mpsc::Sender<Result<Vec<Frame>>>>,
    settled: Instant,
    deadline: Instant,
}

impl<F> Capture<F> {
    fn finish(&mut self, ctx: &egui::Context, result: Result<Vec<Frame>>) {
        if let Some(report) = self.report.take() {
            let _ = report.send(result);
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
}

impl<F: FnMut(usize, &mut egui::Ui) -> Result<()>> eframe::App for Capture<F> {
    fn ui(&mut self, ui: &mut egui::Ui, _: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if self.report.is_none() {
            return;
        }
        if Instant::now() >= self.deadline {
            self.finish(&ctx, Err(anyhow::anyhow!("native review frames timed out")));
            return;
        }
        if let Some(frame) = self.pending.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.pending = None;
            self.frames.push(frame);
            self.settled = Instant::now();
            if self.frames.len() == self.count {
                let frames = std::mem::take(&mut self.frames);
                self.finish(&ctx, Ok(frames));
                return;
            }
        }
        if let Err(error) = (self.draw)(self.frames.len(), ui) {
            self.finish(&ctx, Err(error));
            return;
        }
        if self.pending.is_none() && self.settled.elapsed() >= Duration::from_millis(320) {
            let (tx, rx) = mpsc::sync_channel(1);
            ctx.request_screenshot(move |image| {
                let _ = tx.send(image);
            });
            self.pending = Some(rx);
        }
        ctx.request_repaint_after(Duration::from_millis(50));
    }
}

pub(super) fn capture_views(
    names: &[&str],
    draw: impl FnMut(usize, &mut egui::Ui) -> Result<()> + 'static,
) -> Result<()> {
    let output = output_dir()?;
    let count = names.len();
    let (tx, rx) = mpsc::channel();
    eframe::run_native(
        "Linko / 屏跃",
        options(),
        Box::new(move |_| {
            Ok(Box::new(Capture {
                draw,
                count,
                frames: Vec::new(),
                pending: None,
                report: Some(tx),
                settled: Instant::now(),
                deadline: Instant::now() + Duration::from_secs(75),
            }))
        }),
    )
    .map_err(|error| anyhow::anyhow!("native review capture: {error}"))?;
    let frames = rx
        .try_recv()
        .context("native review capture incomplete")??;
    anyhow::ensure!(frames.len() == count);
    for (name, frame) in names.iter().zip(frames) {
        write_shot(&output.join(name), &frame)?;
    }
    Ok(())
}
