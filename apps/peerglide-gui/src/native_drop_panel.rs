//! Copy a verified local receive result through an explicit gesture in Linko.
use super::{
    data_access::Pin,
    egui,
    transport::file_transfer::{drop_admission, PreparedDropPath, ReceivedFile},
    App, Page, SessionMode,
};
use anyhow::{ensure, Context, Result};
use drop_admission::{Admission, Content, Effect, NativeOutcome, Ticket};
use peerglide_platform::{display_layout::Layout, now_ms, read_display_layout};
use std::{
    io,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    time::{Duration, Instant},
};

const PREPARED_FOR: Duration = Duration::from_secs(60);
const LAYOUT_POLL: Duration = Duration::from_millis(500);

struct Source {
    pin: Pin,
    file: Arc<ReceivedFile>,
}

struct Scope {
    context: drop_admission::Context,
    layout: Layout,
}

impl Scope {
    fn verify_layout(&self) -> Result<()> {
        let current = read_display_layout().context("无法重新读取本机显示器")?;
        self.verify_snapshot(&current)
    }

    fn verify_snapshot(&self, current: &Layout) -> Result<()> {
        ensure!(
            self.layout.same_configuration(current),
            "显示器布局已变化"
        );
        Ok(())
    }
}

struct Preparation {
    _cancel: CancelPreparation,
    scope: Scope,
    result: mpsc::Receiver<Result<PreparedDropPath>>,
}

struct CancelPreparation(Arc<AtomicBool>);

impl Drop for CancelPreparation {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

struct Prepared {
    path: PreparedDropPath,
    attempt: Attempt,
}

struct Attempt {
    scope: Scope,
    ticket: Ticket,
    expires: Instant,
    next_layout_check: Instant,
}

impl Attempt {
    fn current(&mut self, now: Instant) -> bool {
        if now >= self.expires {
            return false;
        }
        if now < self.next_layout_check {
            return true;
        }
        self.next_layout_check = now + LAYOUT_POLL;
        self.scope.verify_layout().is_ok()
    }
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
        attempt: Attempt,
        _path: Option<PreparedDropPath>,
    },
}

#[derive(Default)]
pub(super) struct Export {
    source: Option<Source>,
    state: State,
    admission: Admission,
    gesture: u64,
    unknown: Option<Ticket>,
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
        self.stop();
        self.notice = None;
        self.source = pin.authenticates(file.drop_origin()).then(|| Source {
            pin,
            file: Arc::new(file),
        });
    }

    pub(super) fn stop(&mut self) {
        let ticket = match std::mem::take(&mut self.state) {
            State::Idle => return,
            State::Preparing(_) => None,
            State::Ready(prepared) => Some(prepared.attempt.ticket),
            #[cfg(target_os = "macos")]
            State::Dragging { attempt, .. } => Some(attempt.ticket),
        };
        self.notice = Some(if ticket.is_some_and(|ticket| self.cancel(ticket)) {
            "拖放已停止。如已放下文件，请检查目标应用。"
        } else {
            "本次复制准备已取消，接收文件仍在原目录。"
        });
    }

    fn cancel(&mut self, ticket: Ticket) -> bool {
        let unknown = self.admission.cancel(ticket) == Ok(drop_admission::State::Unknown);
        if unknown {
            self.unknown = Some(ticket);
        }
        unknown
    }

    pub(super) fn tick(&mut self, app: &App, ctx: &egui::Context) {
        if self
            .source
            .as_ref()
            .is_some_and(|source| !source.pin.matches(app))
        {
            self.stop();
            self.source = None;
        }
        if !eligible(app) || app.page != Page::Transfers {
            self.stop();
            return;
        }
        if matches!(self.state, State::Preparing(_) | State::Ready(_))
            && !ctx.input(|input| input.focused)
        {
            self.stop();
            return;
        }
        self.state = match std::mem::take(&mut self.state) {
            State::Idle => State::Idle,
            State::Ready(mut prepared) => {
                if prepared.attempt.current(Instant::now()) {
                    State::Ready(prepared)
                } else {
                    self.cancel(prepared.attempt.ticket);
                    self.notice = Some("布局已变化或复制准备已过期，请重新校验。");
                    State::Idle
                }
            }
            State::Preparing(preparation) => match preparation.result.try_recv() {
                Ok(Ok(path)) => match self.prepare_ticket(preparation.scope) {
                    Ok(attempt) => {
                        self.notice = None;
                        State::Ready(Prepared { path, attempt })
                    }
                    Err(_) => {
                        self.notice = Some("当前布局或本次拖放已失效，请重新准备。");
                        State::Idle
                    }
                },
                Err(mpsc::TryRecvError::Empty) => State::Preparing(preparation),
                Ok(Err(_)) | Err(mpsc::TryRecvError::Disconnected) => {
                    self.notice = Some("文件无法完成重新校验，请检查接收目录。");
                    State::Idle
                }
            },
            #[cfg(target_os = "macos")]
            State::Dragging {
                drag,
                mut attempt,
                _path,
            } => {
                if let Some(outcome) = drag.outcome() {
                    use peerglide_platform::macos_native_drop::Outcome;
                    let outcome = match outcome {
                        Outcome::Copied => NativeOutcome::Copied,
                        Outcome::Cancelled => NativeOutcome::Cancelled,
                        Outcome::Unknown => NativeOutcome::Unknown,
                    };
                    self.complete(app, &attempt, outcome);
                    State::Idle
                } else if !attempt.current(Instant::now()) {
                    drag.request_cancel();
                    self.cancel(attempt.ticket);
                    self.notice = Some("拖放已失效，请检查目标应用。");
                    State::Idle
                } else {
                    State::Dragging {
                        drag,
                        attempt,
                        _path,
                    }
                }
            }
        };
    }

    fn prepare(&mut self, app: &App) -> Result<()> {
        let source = self.source.as_ref().context("没有已接收文件")?;
        source.pin.recheck(app)?;
        ensure!(
            eligible(app) && app.page == Page::Transfers,
            "当前状态无法开始本机复制拖放"
        );
        let file = source.file.clone();
        let layout = read_display_layout().context("无法读取本机显示器")?;
        self.gesture = self.gesture.checked_add(1).context("拖放次数已达上限")?;
        let scope = Scope {
            context: drop_admission::Context {
                connection: file.drop_origin().connection(),
                layout_epoch: layout.local_epoch(),
                gesture_generation: self.gesture,
                focused: true,
            },
            layout,
        };
        if let Some(ticket) = self.unknown.take() {
            self.admission
                .acknowledge_unknown(ticket)
                .map_err(admission_error)?;
        }
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
            _cancel: CancelPreparation(cancelled),
            scope,
            result: receive,
        });
        self.notice = None;
        Ok(())
    }

    fn prepare_ticket(&mut self, scope: Scope) -> Result<Attempt> {
        scope.verify_layout()?;
        let source = self.source.as_ref().context("没有已接收文件")?;
        let origin = source.file.drop_origin();
        ensure!(
            scope.context.connection == origin.connection(),
            "接收连接已变化"
        );
        let now = now_ms();
        let ticket = self
            .admission
            .begin(scope.context, origin.transfer(), Content::File, now, true)
            .map_err(admission_error)?;
        self.admission
            .verified(ticket, scope.context, now, true)
            .map_err(admission_error)?;
        let now = Instant::now();
        Ok(Attempt {
            scope,
            ticket,
            expires: now + PREPARED_FOR,
            next_layout_check: now + LAYOUT_POLL,
        })
    }

    fn start_drag(&mut self, app: &App, copy: bool) {
        let State::Ready(prepared) = std::mem::take(&mut self.state) else {
            return;
        };
        let ticket = prepared.attempt.ticket;
        if self.submit(app, prepared, copy).is_err() {
            self.notice = Some(if self.cancel(ticket) {
                "拖放结果未确认，请检查目标应用。"
            } else {
                "复制准备已失效，请重新校验后拖放。"
            });
        }
    }

    fn submit(&mut self, app: &App, prepared: Prepared, copy: bool) -> Result<()> {
        let Prepared { path, attempt } = prepared;
        let source = self.source.as_ref().context("没有已接收文件")?;
        source.pin.recheck(app)?;
        attempt.scope.verify_layout()?;
        let remaining = attempt.expires.saturating_duration_since(Instant::now());
        ensure!(
            !remaining.is_zero() && eligible(app) && app.page == Page::Transfers,
            "请重新准备复制拖放"
        );
        self.admission
            .submit(
                attempt.ticket,
                attempt.scope.context,
                now_ms(),
                true,
                Effect::Copy,
            )
            .map_err(admission_error)?;
        #[cfg(all(windows, target_env = "msvc"))]
        {
            use peerglide_platform::windows_native_drop::{
                drag_prepared_file, drag_verified_file, Outcome,
            };
            let result = if copy {
                let mut reader = path.into_verified_reader();
                drag_verified_file(
                    source.file.name(),
                    reader.size(),
                    remaining,
                    move |offset, output| reader.read_at(offset, output),
                )?
            } else {
                drag_prepared_file(path.path(), remaining)?
            };
            let outcome = match result {
                Outcome::Copied => NativeOutcome::Copied,
                Outcome::Cancelled => NativeOutcome::Cancelled,
                Outcome::Unknown => NativeOutcome::Unknown,
            };
            self.complete(app, &attempt, outcome);
        }
        #[cfg(target_os = "macos")]
        {
            use peerglide_platform::macos_native_drop::Drag;
            let (drag, retained) = if copy {
                let write = move |destination: &std::path::Path, allowed: &dyn Fn() -> bool| {
                    path.copy_to_drop_destination(destination, || {
                        if allowed() {
                            Ok(())
                        } else {
                            Err(io::ErrorKind::Interrupted.into())
                        }
                    })
                };
                let drag = Drag::start_file_promise(source.file.name(), remaining, write)?;
                (drag, None)
            } else {
                let drag = Drag::start_prepared_file(path.path(), remaining)?;
                (drag, Some(path))
            };
            self.state = State::Dragging {
                drag,
                attempt,
                _path: retained,
            };
        }
        Ok(())
    }

    fn complete(&mut self, app: &App, attempt: &Attempt, outcome: NativeOutcome) {
        let current = eligible(app)
            && app.page == Page::Transfers
            && self.source.as_ref().is_some_and(|source| {
                source.file.drop_origin().connection() == attempt.scope.context.connection
                    && source.pin.recheck(app).is_ok()
            })
            && attempt.scope.verify_layout().is_ok();
        let reported = self.admission.complete(
            attempt.ticket,
            attempt.scope.context,
            now_ms(),
            current,
            outcome,
        );
        let outcome = if reported.is_err() {
            self.cancel(attempt.ticket);
            NativeOutcome::Unknown
        } else {
            if outcome == NativeOutcome::Unknown {
                self.unknown = Some(attempt.ticket);
            }
            outcome
        };
        self.notice = Some(match outcome {
            NativeOutcome::Copied => "系统报告复制拖放已完成。",
            NativeOutcome::Cancelled => "系统报告拖放已取消。",
            NativeOutcome::Unknown => "拖放结果未确认，请检查目标应用。",
        });
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
                let label = if self.unknown.is_some() {
                    "核对目标应用后重新准备"
                } else {
                    "准备复制拖放"
                };
                if ui.button(label).clicked() && self.prepare(app).is_err() {
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
                let copy = drag_pressed(ui, "按住并拖出文件副本");
                let reference = drag_pressed(ui, "按住并拖出文件引用");
                if copy || reference {
                    self.start_drag(app, copy);
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

fn admission_error(error: drop_admission::Error) -> anyhow::Error {
    anyhow::anyhow!("文件拖放票据已失效 ({error:?})")
}

fn drag_pressed(ui: &mut egui::Ui, label: &str) -> bool {
    let target = ui.add(egui::Button::new(label).sense(egui::Sense::drag()));
    target.hovered()
        && ui.input(|input| {
            input.focused && input.pointer.button_pressed(egui::PointerButton::Primary)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use peerglide_platform::display_layout::{Bounds, Display, DisplayId, Rotation};

    #[test]
    fn fresh_snapshot_must_match_prepared_display_geometry() -> Result<()> {
        let mut screen = Display {
            id: DisplayId(1),
            bounds: Bounds {
                left: 0.0,
                top: 0.0,
                right: 1920.0,
                bottom: 1080.0,
            },
            scale: 1.0,
            rotation: Rotation::Upright,
        };
        let layout = Layout::new(screen.id, vec![screen])?;
        let scope = Scope {
            context: drop_admission::Context {
                connection: [1; 32],
                layout_epoch: layout.local_epoch(),
                gesture_generation: 1,
                focused: true,
            },
            layout,
        };
        scope.verify_snapshot(&Layout::new(screen.id, vec![screen])?)?;
        screen.scale = 2.0;
        assert!(scope.verify_snapshot(&Layout::new(screen.id, vec![screen])?).is_err());
        Ok(())
    }

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
