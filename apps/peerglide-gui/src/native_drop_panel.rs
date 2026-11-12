//! Copy a verified local receive result through an explicit gesture in Linko.
use super::{
    data_access::Pin,
    egui,
    transport::file_transfer::{PreparedDropPath, ReceivedFile},
    App, SessionMode,
};
use anyhow::{Context, Result};
use std::{
    io,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    time::{Duration, Instant},
};

const PREPARED_FOR: Duration = Duration::from_secs(60);

struct Source {
    pin: Pin,
    file: Arc<ReceivedFile>,
}

struct Preparation {
    cancelled: Arc<AtomicBool>,
    result: mpsc::Receiver<Result<PreparedDropPath>>,
}

impl Drop for Preparation {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
    }
}

struct Prepared {
    path: PreparedDropPath,
    expires: Instant,
}

#[derive(Default)]
enum State {
    #[default]
    Idle,
    Preparing(Preparation),
    Ready(Prepared),
    #[cfg(target_os = "macos")]
    Dragging {
        drag: peerglide_platform::macos_native_drop::Drag,
        prepared: Prepared,
    },
}

#[derive(Default)]
pub(super) struct Export {
    source: Option<Source>,
    state: State,
    notice: Option<&'static str>,
}

pub(super) fn eligible(app: &App) -> bool {
    super::data_access::ready(app)
        && !app.window_hidden
        && !app.recovery_blocked
        && (app.child.is_none()
            || matches!(
                app.active_mode,
                Some(SessionMode::Communication | SessionMode::Probe)
            ))
        && app.pending_start != Some(SessionMode::Native)
}

impl Export {
    pub(super) fn active(&self) -> bool {
        !matches!(self.state, State::Idle)
    }

    pub(super) fn received(&mut self, pin: Pin, file: ReceivedFile) {
        self.state = State::Idle;
        self.notice = None;
        self.source = Some(Source {
            pin,
            file: Arc::new(file),
        });
    }

    pub(super) fn stop(&mut self) {
        if self.active() {
            self.state = State::Idle;
            self.notice = Some("本次复制准备已取消，接收文件仍在原目录。");
        }
    }

    pub(super) fn tick(&mut self, app: &App) {
        if self
            .source
            .as_ref()
            .is_some_and(|source| !source.pin.matches(app))
        {
            self.stop();
            self.source = None;
        }
        if !eligible(app) {
            self.stop();
            return;
        }
        match &self.state {
            State::Idle => (),
            State::Ready(prepared) if Instant::now() >= prepared.expires => {
                self.state = State::Idle;
                self.notice = Some("复制准备已过期，可以重新校验。");
            }
            State::Ready(_) => (),
            State::Preparing(preparation) => match preparation.result.try_recv() {
                Ok(Ok(path)) => {
                    self.state = State::Ready(Prepared {
                        path,
                        expires: Instant::now() + PREPARED_FOR,
                    });
                    self.notice = None;
                }
                Err(mpsc::TryRecvError::Empty) => (),
                Ok(Err(_)) | Err(mpsc::TryRecvError::Disconnected) => {
                    self.state = State::Idle;
                    self.notice = Some("文件无法完成重新校验，请检查接收目录。");
                }
            },
            #[cfg(target_os = "macos")]
            State::Dragging { drag, prepared } => {
                if let Some(outcome) = drag.outcome() {
                    use peerglide_platform::macos_native_drop::Outcome;
                    self.notice = Some(match outcome {
                        Outcome::Copied => "系统报告复制拖放已完成。",
                        Outcome::Cancelled => "系统报告拖放已取消。",
                        Outcome::Unknown => "拖放结果未确认，请检查目标应用。",
                    });
                    self.state = State::Idle;
                } else if Instant::now() >= prepared.expires {
                    drag.request_cancel();
                    self.state = State::Idle;
                    self.notice = Some("拖放已过期，请检查目标应用。");
                }
            }
        }
    }

    fn prepare(&mut self, app: &App) -> Result<()> {
        let source = self.source.as_ref().context("没有已接收文件")?;
        let (pin, _, _) = Pin::load(app)?;
        anyhow::ensure!(
            source.pin == pin && eligible(app),
            "当前状态无法开始本机复制拖放"
        );
        let file = source.file.clone();
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_stop = cancelled.clone();
        let wake = app.wake_ui.clone();
        let (result, receive) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("linko-prepare-copy-drop".into())
            .spawn(move || {
                let prepared = file.prepare_drop(|| {
                    if worker_stop.load(Ordering::Acquire) {
                        Err(io::ErrorKind::Interrupted.into())
                    } else {
                        Ok(())
                    }
                });
                let _ = result.send(prepared);
                wake();
            })?;
        self.state = State::Preparing(Preparation {
            cancelled,
            result: receive,
        });
        self.notice = None;
        Ok(())
    }

    fn start_drag(&mut self, app: &App) -> Result<()> {
        let State::Ready(prepared) = std::mem::take(&mut self.state) else {
            return Ok(());
        };
        anyhow::ensure!(
            Instant::now() < prepared.expires
                && eligible(app)
                && self
                    .source
                    .as_ref()
                    .is_some_and(|source| source.pin.matches(app)),
            "请重新准备复制拖放"
        );
        #[cfg(all(windows, target_env = "msvc"))]
        {
            use peerglide_platform::windows_native_drop::{drag_prepared_file, Outcome};
            self.notice = Some(match drag_prepared_file(prepared.path.path())? {
                Outcome::Copied => "系统报告复制拖放已完成。",
                Outcome::Cancelled => "系统报告拖放已取消。",
                Outcome::Unknown => "拖放结果未确认，请检查目标应用。",
            });
        }
        #[cfg(target_os = "macos")]
        {
            let drag = peerglide_platform::macos_native_drop::Drag::start_prepared_file(
                prepared.path.path(),
            )?;
            self.state = State::Dragging { drag, prepared };
        }
        Ok(())
    }

    pub(super) fn ui(&mut self, app: &App, ui: &mut egui::Ui) {
        let Some(source) = &self.source else {
            return;
        };
        ui.separator();
        ui.label(format!("已接收 {}", source.file.name()));
        if !eligible(app) {
            ui.label("停止键鼠会话后可以将已接收文件拖出复制。");
            return;
        }
        match &self.state {
            State::Idle => {
                if ui.button("准备复制拖放").clicked() && self.prepare(app).is_err() {
                    self.notice = Some("无法准备复制拖放，请检查配对和接收文件。");
                }
            }
            State::Preparing(_) => {
                ui.label("正在重新校验本机文件。");
                if ui.button("取消准备").clicked() {
                    self.stop();
                }
            }
            State::Ready(_) => {
                let target = ui.add(egui::Button::new("按住并拖出复制").sense(egui::Sense::drag()));
                let pressed = ui.input(|input| {
                    input.focused && input.pointer.button_pressed(egui::PointerButton::Primary)
                });
                if target.hovered() && pressed && self.start_drag(app).is_err() {
                    self.notice = Some("系统未开启拖放，请重新准备并从拖动按钮按下。");
                }
            }
            #[cfg(target_os = "macos")]
            State::Dragging { .. } => {
                ui.label("正在复制拖放。");
            }
        }
        if let Some(notice) = self.notice {
            ui.label(notice);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_export_requires_enrollment_and_a_visible_local_owner() -> Result<()> {
        let fixture = super::super::tests::Fixture::new()?;
        let mut app = App::load(fixture.path.clone());
        assert!(!eligible(&app));
        let peer = super::super::Identity::generate()?;
        app.config.fingerprint = Some(peer.fingerprint());
        assert!(eligible(&app));
        app.window_hidden = true;
        assert!(!eligible(&app));
        app.window_hidden = false;
        app.recovery_blocked = true;
        assert!(!eligible(&app));
        app.recovery_blocked = false;
        app.close_requested = true;
        assert!(!eligible(&app));
        drop(app);
        fixture.close()
    }
}
