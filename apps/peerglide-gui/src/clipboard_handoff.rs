//! Clipboard handoff on the existing managed input and drag owners.
use super::{
    clipboard_drop, data_access, desktop_state, egui,
    file_drop_reservation::{next_request, NativeDelivery, Reservation, Status},
    native_drop_panel::HandoffRegion,
    App, Page, SessionMode,
};
use anyhow::{ensure, Context, Result};
use peerglide_platform::{display_layout::Layout, read_display_layout};
use peerglide_transport::{clipboard_worker::DragPayload, file_drop_link::ClipboardReceipt};
use std::{
    net::SocketAddr,
    time::{Duration, Instant},
};

const LAYOUT_POLL: Duration = Duration::from_millis(500);
const SOURCE_RELEASED: &str = "已将释放交给另一台电脑，请在目标应用确认复制。";
const COPIED: &str = "系统报告跨机复制拖放已完成。";
const CANCELLED: &str = "本次跨机复制拖放已取消。";
const UNKNOWN: &str = "跨机拖放结果未确认，请检查目标应用。";

pub(super) struct Prepared {
    pub(super) pin: data_access::Pin,
    pub(super) address: SocketAddr,
    pub(super) receipt: ClipboardReceipt,
    pub(super) payload: Option<DragPayload>,
}

impl Prepared {
    pub(super) fn current(&self, app: &App, ctx: &egui::Context) -> bool {
        self.pin.matches(app)
            && self.pin.matches_copy(&self.receipt)
            && clipboard_drop::eligible(app, ctx)
            && clipboard_drop::focused(ctx)
    }
}

enum Stage {
    Source,
    Waiting {
        payload: DragPayload,
        region: HandoffRegion,
    },
    #[cfg(target_os = "macos")]
    Dragging {
        drag: peerglide_platform::macos_native_drop::Drag,
        delivery: NativeDelivery,
        started: bool,
    },
    Finished,
}

pub(super) struct Handoff {
    pin: data_access::Pin,
    reservation: Reservation,
    stage: Stage,
    layout: Layout,
    edge: String,
    expires: Instant,
    layout_check: Instant,
    receiving: bool,
}

impl Handoff {
    pub(super) fn new(prepared: Prepared, app: &mut App, ctx: &egui::Context) -> Result<Self> {
        ensure!(prepared.current(app, ctx), "跨机拖放准备已失效");
        prepared.pin.recheck(app)?;
        ensure!(app.child.is_none(), "请先结束当前键鼠会话");
        let receiving = prepared.payload.is_some();
        ensure!(!receiving || app.config.allow, "请先启用本机的键鼠接收许可");
        let layout = read_display_layout().context("无法读取显示器布局")?;
        let (proof, expires) = prepared.receipt.into_local_worker()?;
        let request = next_request(app)?;
        let (reservation, stage) = match prepared.payload {
            Some(payload) => (
                Reservation::new(proof, request, prepared.address, expires),
                Stage::Waiting {
                    payload,
                    region: HandoffRegion::default(),
                },
            ),
            None => (
                Reservation::source_proof(proof, request, prepared.address, expires),
                Stage::Source,
            ),
        };
        Ok(Self {
            pin: prepared.pin,
            reservation,
            stage,
            layout,
            edge: app.displayed_edge().to_owned(),
            expires,
            layout_check: Instant::now() + LAYOUT_POLL,
            receiving,
        })
    }

    fn verify_layout(&self) -> Result<()> {
        let current = read_display_layout().context("无法重新读取本机显示器")?;
        ensure!(self.layout.same_configuration(&current), "显示器布局已变化");
        Ok(())
    }

    pub(super) fn tick(
        &mut self,
        app: &mut App,
        ctx: &egui::Context,
    ) -> Result<Option<&'static str>> {
        let now = Instant::now();
        #[cfg(target_os = "macos")]
        let dragging = matches!(self.stage, Stage::Dragging { .. });
        #[cfg(not(target_os = "macos"))]
        let dragging = false;
        ensure!(
            now < self.expires
                && self.pin.matches(app)
                && app.page == Page::Clipboard
                && self.edge == app.displayed_edge()
                && (!self.receiving || app.config.allow)
                && !app.routed_active
                && !app.window_hidden
                && !app.recovery_blocked
                && !app.input_terminal
                && app.stopping.is_none()
                && (app.child.is_none() || app.active_mode == Some(SessionMode::Native))
                && (dragging || clipboard_drop::focused(ctx))
                && ctx.input(|input| !input.viewport().close_requested()
                    && !input.viewport().minimized.unwrap_or(false)),
            "跨机拖放许可已失效"
        );
        if now >= self.layout_check {
            self.verify_layout()?;
            self.layout_check = now + LAYOUT_POLL;
        }
        #[cfg(target_os = "macos")]
        if let Stage::Dragging {
            drag,
            delivery,
            started,
        } = &mut self.stage
        {
            if !*started && drag.started() {
                ensure!(delivery.native_check(0), "无法确认原生拖放启动");
                *started = true;
            }
            use peerglide_platform::macos_native_drop::Outcome;
            return Ok(drag.outcome().map(|outcome| match outcome {
                Outcome::Copied => COPIED,
                Outcome::Cancelled => CANCELLED,
                Outcome::Unknown => UNKNOWN,
            }));
        }
        self.reservation.tick(app, &self.pin, now)?;
        if matches!(self.reservation.status(), Status::SourceReleased) {
            return Ok(Some(SOURCE_RELEASED));
        }
        if let (Stage::Waiting { region, .. }, Some(lease)) =
            (&self.stage, self.reservation.pending_lease())
        {
            if let Some(entry) = region.entry {
                self.verify_layout()?;
                self.reservation
                    .confirm_ready(app, &self.pin, lease, entry, now)?;
            }
        }
        Ok(None)
    }

    pub(super) fn ui(&mut self, app: &mut App, ui: &mut egui::Ui) -> Result<Option<&'static str>> {
        match &mut self.stage {
            Stage::Waiting { region, .. } => {
                if region.ui(app, ui, &self.layout, &self.reservation, "跨机内容拖放起点")?
                {
                    return self.begin_drag(app);
                }
            }
            #[cfg(target_os = "macos")]
            Stage::Dragging { .. } => {
                ui.label("正在接收跨机拖动。");
                return Ok(None);
            }
            Stage::Source => (),
            Stage::Finished => return Ok(Some(CANCELLED)),
        }
        match self.reservation.status() {
            Status::WaitingForWorker => {
                ui.label("请在两端分别开启键鼠协同。");
                if ui
                    .add_enabled(
                        app.native_start_available(),
                        egui::Button::new("开启键鼠协同"),
                    )
                    .clicked()
                {
                    app.workspace_action(desktop_state::Action::StartInput);
                }
            }
            Status::Press => {
                let drag =
                    ui.add(egui::Button::new("按住并拖向切换边沿").sense(egui::Sense::drag()));
                if drag.hovered()
                    && ui.input(|input| {
                        input.focused
                            && input.pointer.button_pressed(egui::PointerButton::Primary)
                            && input.pointer.primary_down()
                    })
                {
                    self.verify_layout()?;
                    self.reservation.press(app, &self.pin, Instant::now())?;
                }
            }
            Status::Confirming => {
                ui.label("等待键鼠会话确认。");
            }
            Status::Listening(_) | Status::Matched => {
                ui.label("请保持拖放起点可见，等待另一台电脑拖入。");
            }
            Status::SourceArmed => {
                ui.label("保持按住，拖向切换边沿。");
            }
            Status::SourceCommitted => {
                ui.label("移到目标应用后释放。");
            }
            Status::SourceReleased => return Ok(Some(SOURCE_RELEASED)),
        }
        Ok(None)
    }

    fn begin_drag(&mut self, app: &mut App) -> Result<Option<&'static str>> {
        self.pin.recheck(app)?;
        self.verify_layout()?;
        let remaining = self.expires.saturating_duration_since(Instant::now());
        let Stage::Waiting { payload, region } =
            std::mem::replace(&mut self.stage, Stage::Finished)
        else {
            anyhow::bail!("本次拖放已启动");
        };
        #[cfg(all(windows, target_env = "msvc"))]
        {
            use peerglide_platform::windows_native_drop::{
                drag_handoff_png, drag_handoff_text, Outcome,
            };
            let _ = region;
            let delivery = NativeDelivery::open(&self.reservation, app)?;
            let check = |phase| delivery.native_check(phase);
            let outcome = match payload {
                DragPayload::Text(text) => drag_handoff_text(&text, remaining, &check),
                DragPayload::Png(png) => drag_handoff_png(&png, remaining, &check),
            };
            delivery.finish(app);
            Ok(Some(match outcome? {
                Outcome::Copied => COPIED,
                Outcome::Cancelled => CANCELLED,
                Outcome::Unknown => UNKNOWN,
            }))
        }
        #[cfg(target_os = "macos")]
        {
            use peerglide_platform::macos_native_drop::Drag;
            let press = region.press.context("接收窗口未收到对应的鼠标按下")?;
            let delivery = NativeDelivery::open(&self.reservation, app)?;
            let check = delivery.content_gate();
            let drag = match payload {
                DragPayload::Text(text) => Drag::start_handoff_text(&text, remaining, press, check),
                DragPayload::Png(png) => Drag::start_handoff_png(&png, remaining, press, check),
            };
            let drag = match drag {
                Ok(drag) => drag,
                Err(error) => {
                    delivery.finish(app);
                    return Err(error);
                }
            };
            self.stage = Stage::Dragging {
                drag,
                delivery,
                started: false,
            };
            Ok(None)
        }
    }

    pub(super) fn cancel(self, app: &mut App) {
        #[cfg(target_os = "macos")]
        if let Stage::Dragging { drag, delivery, .. } = self.stage {
            drag.request_cancel();
            delivery.finish(app);
        }
        self.reservation.cancel(app);
    }
}
