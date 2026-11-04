//! Explicit, window-local drawing only. Never installs hooks or dispatches actions.
use anyhow::{Context, Result};
use eframe::egui::{self, Color32, Event, PointerButton, Rect, Sense, Stroke as PaintStroke};
use peerglide_gestures::{
    starter_templates, Config, Match, Point, Recognizer, Rejection, Sample, Stroke,
};
use std::{
    io::Write,
    path::{Path, PathBuf},
    sync::LazyLock,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

static RECOGNIZER: LazyLock<Recognizer> = LazyLock::new(|| {
    Recognizer::new(Config::default(), starter_templates()).expect("built-in gesture templates")
});
const LABELS: [&str; 9] = [
    "未知手势 / 负样本",
    "向左",
    "向右",
    "向上",
    "向下",
    "先向下再向右",
    "先向下再向左",
    "先向上再向右",
    "先向上再向左",
];
fn rejection_label(reason: Rejection) -> &'static str {
    match reason {
        Rejection::SampleCount => "采样点数量不足或超出上限",
        Rejection::InvalidCoordinate => "坐标无效",
        Rejection::NonMonotonicTime => "采样时间顺序无效",
        Rejection::Duration => "绘制时间超出上限",
        Rejection::TooShort => "轨迹过短",
        Rejection::TooLong => "轨迹过长",
        Rejection::NoMatch => "没有匹配的手势",
        Rejection::Ambiguous => "结果不明确",
        Rejection::Cancelled => "已取消",
    }
}
struct Active {
    stroke: Stroke<'static>,
    time: f64,
    rect: Rect,
}
pub struct GestureLab {
    active: Option<Active>,
    samples: Vec<Sample>,
    result: Option<std::result::Result<Match, Rejection>>,
    completed: bool,
    saved: bool,
    expected: u32,
    export_dir: String,
    serial: u64,
    feedback: String,
    export_open: bool,
    inspection: Option<(Rect, Rect)>,
}
impl GestureLab {
    pub fn new(dir: &Path) -> Self {
        Self {
            active: None,
            samples: Vec::new(),
            result: None,
            completed: false,
            saved: false,
            expected: 0,
            export_dir: dir.join("gesture-samples").display().to_string(),
            serial: 0,
            feedback: String::new(),
            export_open: false,
            inspection: None,
        }
    }
    pub fn set_export_open(&mut self, open: bool) {
        self.export_open = open;
        self.inspection = None;
    }
    /// Widget geometry from the most recent frame, never pointer/input contents.
    pub fn inspection(&self) -> Option<(Rect, Rect)> {
        self.inspection
    }
    pub fn matched_id(&self) -> Option<u32> {
        self.result
            .as_ref()
            .and_then(|r| r.as_ref().ok())
            .map(|m| m.id.0)
    }
    pub fn cancel(&mut self) {
        if self.active.take().is_some() {
            self.result = Some(Err(Rejection::Cancelled));
            self.completed = false;
        }
    }
    fn begin(&mut self, rect: Rect, pos: egui::Pos2, time: f64) {
        self.samples.clear();
        self.result = None;
        self.completed = false;
        self.saved = false;
        self.feedback.clear();
        self.active = Some(Active {
            stroke: RECOGNIZER.stroke(),
            time,
            rect,
        });
        self.point(pos, time);
    }
    fn point(&mut self, pos: egui::Pos2, time: f64) {
        let Some(active) = &mut self.active else {
            return;
        };
        let elapsed = time - active.time;
        if !active.rect.contains(pos) || !elapsed.is_finite() || elapsed < 0. {
            self.cancel();
            return;
        }
        if elapsed >= Config::default().max_duration.as_secs_f64() {
            self.active = None;
            self.result = Some(Err(Rejection::Duration));
            self.completed = false;
            return;
        }
        let sample = Sample {
            point: Point::new(
                (pos.x - active.rect.min.x) as f64,
                (pos.y - active.rect.min.y) as f64,
            ),
            elapsed: Duration::from_secs_f64(elapsed),
        };
        match active.stroke.push(sample) {
            Ok(()) => self.samples.push(sample),
            Err(reason) => {
                self.active = None;
                self.result = Some(Err(reason));
                self.completed = false;
            }
        }
    }
    fn release(&mut self, pos: egui::Pos2, time: f64) {
        self.point(pos, time);
        if let Some(active) = self.active.take() {
            self.result = Some(
                active
                    .stroke
                    .finish(Duration::from_secs_f64(time - active.time)),
            );
            self.completed = true;
        }
    }
    fn export(&mut self) -> Result<PathBuf> {
        anyhow::ensure!(self.completed && !self.saved, "请先重新绘制一条完整样本");
        anyhow::ensure!(self.expected < LABELS.len() as u32, "样本标签无效");
        let directory = PathBuf::from(self.export_dir.trim());
        anyhow::ensure!(!directory.as_os_str().is_empty(), "请选择样本文件夹");
        let points: Vec<_> = self
            .samples
            .iter()
            .map(|s| {
                serde_json::json!({
            "x":s.point.x,"y":s.point.y,"elapsed_us":s.elapsed.as_micros() as u64})
            })
            .collect();
        let data = serde_json::to_vec_pretty(&serde_json::json!({
            "schema_version":1,"source":"egui_canvas","units":"logical_pixels",
            "expected_id":if self.expected==0 {None}else{Some(self.expected)},"samples":points
        }))?;
        self.serial = self.serial.checked_add(1).context("样本计数已达上限")?;
        let time = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
        std::fs::create_dir_all(&directory).context("创建样本文件夹失败")?;
        let path = directory.join(format!("sample-{time}-{}.json", self.serial));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .context("创建不覆盖现有文件的新样本失败")?;
        file.write_all(&data).context("写入标注样本失败")?;
        self.saved = true;
        Ok(path)
    }
    pub fn ui(&mut self, ui: &mut egui::Ui, worker_running: bool) -> Option<Rect> {
        self.inspection = None;
        ui.heading("录制与本窗口测试");
        ui.label("在画布内按住鼠标右键绘制手势，按 Esc 取消。");
        ui.weak("仅限此画布，不捕获全局输入、不执行动作。只有点击保存才会写入文件。");
        if worker_running {
            self.cancel();
            ui.label("设备会话运行期间暂停画布录制。请先停止设备会话，再进行窗口内测试。");
            return None;
        }
        let (rect, response) = ui.allocate_exact_size(
            egui::vec2(ui.available_width().max(1.), 260.),
            Sense::click_and_drag(),
        );
        ui.painter()
            .rect_filled(rect, 8, ui.visuals().extreme_bg_color);
        let (time, focused, escape, events) = ui.ctx().input(|i| {
            (
                i.time,
                i.focused,
                i.key_pressed(egui::Key::Escape),
                i.events.clone(),
            )
        });
        if !focused || escape || self.active.as_ref().is_some_and(|a| a.rect != rect) {
            self.cancel();
        }
        if focused && !escape {
            for event in events {
                match event {
                    Event::PointerButton {
                        pos,
                        button: PointerButton::Secondary,
                        pressed: true,
                        ..
                    } if rect.contains(pos) && response.hovered() => self.begin(rect, pos, time),
                    Event::PointerMoved(pos) => self.point(pos, time),
                    Event::PointerButton {
                        pos,
                        button: PointerButton::Secondary,
                        pressed: false,
                        ..
                    } => self.release(pos, time),
                    Event::PointerGone | Event::WindowFocused(false) => self.cancel(),
                    Event::PointerButton { .. } | Event::MouseWheel { .. }
                        if self.active.is_some() =>
                    {
                        self.cancel()
                    }
                    _ => (),
                }
            }
            // Expire even if no movement is delivered while the button is held.
            if self
                .active
                .as_ref()
                .is_some_and(|a| time - a.time >= Config::default().max_duration.as_secs_f64())
            {
                self.active = None;
                self.result = Some(Err(Rejection::Duration));
                self.completed = false;
            }
        }
        let painter = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
        for pair in self.samples.windows(2) {
            let position = |s: &Sample| rect.min + egui::vec2(s.point.x as f32, s.point.y as f32);
            painter.line_segment(
                [position(&pair[0]), position(&pair[1])],
                PaintStroke::new(2.5_f32, Color32::from_rgb(80, 175, 230)),
            );
        }
        if self.active.is_some() {
            ui.ctx().request_repaint();
        }
        match &self.result {
            Some(Ok(m)) => {
                ui.label(format!(
                    "识别结果：{}  ·  距离 {:.3}",
                    LABELS[m.id.0 as usize], m.distance
                ));
            }
            Some(Err(reason)) => {
                ui.label(format!("未识别 / 已取消：{}", rejection_label(*reason)));
            }
            None => {
                ui.label(if self.active.is_some() {
                    "正在内存中记录"
                } else {
                    "就绪"
                });
            }
        }
        ui.horizontal(|ui| {
            if ui.button("清空画布").clicked() {
                self.cancel();
                self.samples.clear();
                self.result = None;
                self.completed = false;
                self.saved = false;
            }
            ui.weak("绘制后到规则页选择轨迹、范围与动作；保存规则不会启用全局手势。");
        });
        let mut save_rect = None;
        let export = egui::CollapsingHeader::new("样本标注与导出 · 可选")
            .open(Some(self.export_open))
            .show(ui, |ui| {
                egui::ComboBox::from_id_salt("gesture_expected_label")
                    .selected_text(LABELS[self.expected as usize])
                    .show_ui(ui, |ui| {
                        for (id, label) in LABELS.iter().enumerate() {
                            ui.selectable_value(&mut self.expected, id as u32, *label);
                        }
                    });
                ui.weak(
                    "预期标签不影响识别结果。绘制仅保留在内存，只有点击保存才写入轨迹坐标和时间；不含屏幕图像、按键内容或应用标识，不上传。",
                );
                ui.horizontal(|ui| {
                    ui.label("样本文件夹");
                    ui.text_edit_singleline(&mut self.export_dir);
                });
                let save = ui.add_enabled(
                    self.completed && !self.saved,
                    egui::Button::new("保存标注样本"),
                );
                save_rect = ui.clip_rect().contains_rect(save.rect).then_some(save.rect);
                if save.clicked() {
                    self.feedback = match self.export() {
                        Ok(path) => format!("已保存：{}", path.display()),
                        Err(error) => format!("保存失败：{error:#}"),
                    };
                }
            });
        if export.header_response.clicked() {
            self.export_open = !self.export_open;
        }
        // Collapsing animations may still lay out a hidden body. Report only the
        // fully open body from this frame, never a collapsed or stale save target.
        if self.export_open && export.body_response.is_some() && ui.clip_rect().contains_rect(rect) {
            self.inspection = save_rect.map(|save| (rect, save));
        }
        ui.weak("窗口内轨迹不能证明原生鼠标采样率、全局手势或动作执行已经验收。");
        if !self.feedback.is_empty() {
            ui.label(&self.feedback);
        }
        Some(rect)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn right(lab: &mut GestureLab) {
        let rect = Rect::from_min_size(egui::pos2(0., 0.), egui::vec2(300., 260.));
        lab.begin(rect, egui::pos2(10., 10.), 0.);
        lab.point(egui::pos2(60., 10.), 0.1);
        lab.release(egui::pos2(110., 10.), 0.2);
    }
    fn frame(
        lab: &mut GestureLab,
        ctx: &egui::Context,
        time: f64,
        events: Vec<Event>,
        focused: bool,
        running: bool,
    ) -> Option<Rect> {
        let mut rect = None;
        let output = ctx.run_ui(
            egui::RawInput {
                time: Some(time),
                events,
                focused,
                screen_rect: Some(Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1000., 700.),
                )),
                ..Default::default()
            },
            |root| {
                egui::CentralPanel::default().show(root, |ui| {
                    rect = lab.ui(ui, running);
                });
            },
        );
        output.drop_without_applying_deltas();
        rect
    }
    fn button(pos: egui::Pos2, pressed: bool) -> Event {
        Event::PointerButton {
            pos,
            button: PointerButton::Secondary,
            pressed,
            modifiers: Default::default(),
        }
    }
    #[test]
    fn egui_pointer_frames_draw_and_recognize_without_a_worker_or_identity() {
        let mut lab = GestureLab::new(Path::new("unused"));
        let ctx = egui::Context::default();
        let rect = frame(&mut lab, &ctx, 0., vec![], true, false).unwrap();
        let start = rect.min + egui::vec2(10., 30.);
        let end = start + egui::vec2(100., 0.);
        frame(
            &mut lab,
            &ctx,
            0.1,
            vec![Event::PointerMoved(start), button(start, true)],
            true,
            false,
        );
        assert!(
            lab.active.is_some(),
            "canvas must own the secondary-button stroke"
        );
        frame(
            &mut lab,
            &ctx,
            0.2,
            vec![Event::PointerMoved(end)],
            true,
            false,
        );
        frame(&mut lab, &ctx, 0.3, vec![button(end, false)], true, false);
        assert!(lab.completed);
        assert_eq!(lab.result.as_ref().unwrap().as_ref().unwrap().id.0, 2);
        frame(
            &mut lab,
            &ctx,
            0.4,
            vec![Event::PointerMoved(start), button(start, true)],
            true,
            false,
        );
        frame(
            &mut lab,
            &ctx,
            0.5,
            vec![Event::WindowFocused(false)],
            false,
            false,
        );
        assert!(!lab.completed && lab.active.is_none());
        assert!(lab.export().is_err());
        assert!(frame(&mut lab, &ctx, 0.6, vec![], true, true).is_none());
    }
    #[test]
    fn idle_frames_expire_a_held_candidate_and_worker_start_cancels_it() {
        let mut lab = GestureLab::new(Path::new("unused"));
        let ctx = egui::Context::default();
        let rect = frame(&mut lab, &ctx, 0., vec![], true, false).unwrap();
        let pos = rect.min + egui::vec2(20., 30.);
        frame(
            &mut lab,
            &ctx,
            0.1,
            vec![Event::PointerMoved(pos), button(pos, true)],
            true,
            false,
        );
        frame(&mut lab, &ctx, 2.7, vec![], true, false);
        assert!(matches!(lab.result, Some(Err(Rejection::Duration))));
        assert!(!lab.completed);
        frame(&mut lab, &ctx, 2.8, vec![button(pos, false)], true, false);
        frame(&mut lab, &ctx, 2.9, vec![button(pos, true)], true, false);
        assert!(lab.active.is_some());
        frame(&mut lab, &ctx, 3., vec![], true, true);
        assert!(lab.active.is_none() && !lab.completed);
    }
    #[test]
    fn recognition_is_offline_and_label_does_not_bias_result() {
        let mut lab = GestureLab::new(Path::new("unused"));
        lab.expected = 1;
        right(&mut lab);
        assert_eq!(lab.result.as_ref().unwrap().as_ref().unwrap().id.0, 2);
        assert!(lab.completed && !lab.saved);
        right(&mut lab);
        lab.cancel(); // Completed result is preserved, not replayed.
        assert!(lab.completed);
    }
    #[test]
    fn cancellation_outside_bounds_and_late_release_never_export_a_prefix() {
        let mut lab = GestureLab::new(Path::new("unused"));
        let rect = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(300., 260.));
        for (pos, time) in [(egui::pos2(400., 10.), 0.2), (egui::pos2(110., 10.), 2.5)] {
            lab.begin(rect, egui::pos2(10., 10.), 0.);
            lab.point(egui::pos2(100., 10.), 0.1);
            lab.release(pos, time);
            assert!(!lab.completed);
            assert!(lab.export().is_err());
        }
        lab.begin(rect, egui::pos2(10., 10.), 0.);
        lab.cancel();
        lab.release(egui::pos2(110., 10.), 0.2);
        assert!(!lab.completed);
        assert!(lab.export().is_err());
    }
    #[test]
    fn inspection_reports_only_current_canvas_and_visible_save_widget() -> Result<()> {
        let fixture = super::super::tests::Fixture::new()?;
        let mut lab = GestureLab::new(&fixture.path);
        let ctx = egui::Context::default();
        ctx.style_mut(|style| style.animation_time = 0.);
        assert!(lab.inspection().is_none());
        frame(&mut lab, &ctx, 0., vec![], true, false);
        assert!(lab.inspection().is_none());
        lab.set_export_open(true);
        assert!(lab.inspection().is_none());
        let canvas = frame(&mut lab, &ctx, 0.1, vec![], true, false).unwrap();
        let (reported_canvas, save) = lab.inspection().unwrap();
        assert_eq!(reported_canvas, canvas);
        assert!(save.width() > 0. && save.height() > 0.);
        assert!(save.min.y >= canvas.max.y);
        assert!(!lab.completed); // Disabled Save still has real widget bounds.
        lab.set_export_open(false);
        assert!(lab.inspection().is_none());
        frame(&mut lab, &ctx, 0.2, vec![], true, false);
        assert!(lab.inspection().is_none());
        lab.set_export_open(true);
        frame(&mut lab, &ctx, 0.3, vec![], true, false);
        assert!(lab.inspection().is_some());
        assert!(frame(&mut lab, &ctx, 0.4, vec![], true, true).is_none());
        assert!(lab.inspection().is_none());
        assert_eq!(std::fs::read_dir(&fixture.path)?.count(), 0);
        Ok(())
    }
    #[test]
    fn rendering_a_completed_sample_never_exports_without_an_explicit_click() -> Result<()> {
        let fixture = super::super::tests::Fixture::new()?;
        let mut lab = GestureLab::new(&fixture.path);
        right(&mut lab);
        let ctx = egui::Context::default();
        for tick in 0..3 {
            assert!(frame(&mut lab, &ctx, f64::from(tick), vec![], true, false).is_some());
        }
        assert!(lab.completed && !lab.saved);
        assert!(!fixture.path.join("gesture-samples").exists());
        assert_eq!(std::fs::read_dir(&fixture.path)?.count(), 0);
        Ok(())
    }
    #[test]
    fn saving_is_explicit_labelled_and_never_duplicates_a_completed_sample() -> Result<()> {
        let marker = peerglide_transport::Identity::generate()?;
        let root = std::env::temp_dir().join(format!("peerglide-lab-{}", marker.fingerprint()));
        std::fs::create_dir(&root)?;
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(root.clone());
        let mut lab = GestureLab::new(&root);
        lab.expected = 2;
        right(&mut lab);
        assert!(!root.join("gesture-samples").exists());
        let path = lab.export()?;
        let data: serde_json::Value = serde_json::from_slice(&std::fs::read(&path)?)?;
        assert_eq!(data["source"], "egui_canvas");
        assert_eq!(data["expected_id"], 2);
        assert_eq!(data["samples"].as_array().unwrap().len(), 3);
        assert!(lab.export().is_err());
        right(&mut lab);
        lab.expected = 0;
        let second = lab.export()?;
        assert_ne!(path, second);
        assert!(
            serde_json::from_slice::<serde_json::Value>(&std::fs::read(second)?)?["expected_id"]
                .is_null()
        );
        Ok(())
    }
}
