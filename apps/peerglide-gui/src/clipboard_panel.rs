//! One-shot clipboard controls. Rendering and polling never access the system clipboard.
use super::{
    data_access::{self, Pin},
    egui, transport, App,
};
use anyhow::{Context, Result};
use peerglide_platform::clipboard::NativeClipboard;
use transport::clipboard_worker::{
    self, Classification, Event, Job, LocalPayload, LocalText, Mode, PendingClipboard, TextOutcome,
};

#[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
use super::clipboard_drop;
#[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
use transport::clipboard_worker::DragPayload;

const PORT: u16 = 24872;

enum Decision {
    Import,
    Decline,
    #[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
    ImportForDrag,
}

struct Active {
    pin: Pin,
    job: Job,
    pending: Option<PendingClipboard>,
    #[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
    drag_payload: Option<DragPayload>,
    stopping: bool,
}

pub(super) struct Panel {
    clipboard: Option<NativeClipboard>,
    active: Option<Active>,
    mode: Mode,
    target: String,
    bind: String,
    notice: String,
    local_outcome: Option<TextOutcome>,
    #[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
    drag_offer: Option<clipboard_drop::Offer>,
}

impl Default for Panel {
    fn default() -> Self {
        Self {
            clipboard: None,
            active: None,
            mode: Mode::Text,
            target: String::new(),
            bind: format!("0.0.0.0:{PORT}"),
            notice: String::new(),
            local_outcome: None,
            #[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
            drag_offer: None,
        }
    }
}

impl Panel {
    pub(super) fn is_active(&self) -> bool {
        #[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
        if self.drag_offer.is_some() {
            return true;
        }
        self.active.is_some()
    }

    pub(super) fn stop(&mut self) {
        #[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
        if self.drag_offer.take().is_some() {
            self.notice = "已撤下本次复制拖放。".into();
        }
        if let Some(active) = self.active.as_mut().filter(|active| !active.stopping) {
            active.job.stop();
            active.pending = None;
            #[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
            {
                active.drag_payload = None;
            }
            active.stopping = true;
            self.notice = "已请求停止本次剪贴板共享。".into();
        }
    }

    pub(super) fn tick(&mut self, app: &App, _ctx: &egui::Context) {
        #[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
        if let Some(notice) = self.drag_offer.as_ref().and_then(|offer| offer.poll(app, _ctx)) {
            self.drag_offer = None;
            self.notice = notice.into();
        }
        if self
            .active
            .as_ref()
            .is_some_and(|active| !active.stopping && !active.pin.matches(app))
        {
            self.stop();
        }
        let Some(active) = self.active.as_mut() else {
            return;
        };
        #[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
        if !clipboard_drop::eligible(app, _ctx) || !clipboard_drop::focused(_ctx) {
            active.drag_payload = None;
        }
        let finished = active.job.is_finished();
        while let Some(event) = active.job.poll() {
            if active.stopping {
                continue;
            }
            self.notice = match event {
                Event::Listening(address) => format!(
                    "正在等待已配对电脑发送。请在发送端填写本机 IP 和 UDP 端口 {}。",
                    address.port()
                ),
                Event::PeerReceived => "另一台已收到文本，正在等对方决定是否导入。".into(),
                Event::WaitingForImageResult => "已提交图片，正在等待接收端回执。".into(),
                Event::Pending(pending) => {
                    let notice = pending.dimensions().map_or_else(
                        || "已收到一条文本。请选择导入或拒绝。".into(),
                        |(width, height)| {
                            format!("已收到 {width} × {height} PNG。请选择导入或拒绝。")
                        },
                    );
                    active.pending = Some(pending);
                    notice
                }
                Event::Finished(outcome) => {
                    active.pending = None;
                    #[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
                    if outcome == TextOutcome::Submitted {
                        self.drag_offer = active
                            .drag_payload
                            .take()
                            .map(|payload| clipboard_drop::Offer::new(active.pin.clone(), payload));
                    }
                    outcome_label(outcome).into()
                }
                Event::Stopped => "本次剪贴板共享已停止。".into(),
                Event::Failed(_) => {
                    active.pending = None;
                    "剪贴板共享未完成。请检查内容格式、两端类型、地址和等待时间。".into()
                }
            };
        }
        if finished {
            if active.stopping {
                self.notice = "本次剪贴板共享已停止。".into();
            }
            self.active = None;
        }
    }

    fn clipboard(&mut self) -> Result<&mut NativeClipboard> {
        if self.clipboard.is_none() {
            self.clipboard = Some(NativeClipboard::new().context("系统剪贴板不可用")?);
        }
        self.clipboard.as_mut().context("系统剪贴板不可用")
    }

    fn start(&mut self, app: &App, send: bool) -> Result<()> {
        anyhow::ensure!(self.active.is_none(), "请先结束当前共享");
        #[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
        {
            self.drag_offer = None;
        }
        let (pin, identity, peer) = Pin::load(app)?;
        let wake = app.wake_ui.clone();
        let job = if send {
            let target = data_access::send_address(&self.target, &app.config.address, PORT)?;
            let payload = self.local_payload()?;
            clipboard_worker::send(target, identity, peer, payload, wake)
        } else {
            let bind =
                data_access::parse_address(&self.bind).context("请输入本机监听地址和 UDP 端口")?;
            let revision = self
                .clipboard()?
                .revision()
                .context("无法确认本机剪贴板当前版本")?;
            clipboard_worker::receive(bind, identity, peer, revision, self.mode, wake)
        }
        .context("无法启动剪贴板后台任务")?;
        self.active = Some(Active {
            pin,
            job,
            pending: None,
            #[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
            drag_payload: None,
            stopping: false,
        });
        self.notice = if send {
            "正在准备发送当前剪贴板内容。".into()
        } else {
            "正在准备等待本次共享。".into()
        };
        Ok(())
    }

    fn local_payload(&mut self) -> Result<LocalPayload> {
        match self.mode {
            Mode::Text => {
                let snapshot = self
                    .clipboard()?
                    .snapshot()
                    .context("无法读取可共享的文本")?
                    .context("当前剪贴板没有可共享的普通文本")?;
                Ok(LocalPayload::Text(LocalText {
                    text: snapshot.text,
                    classification: Classification::ShareableText,
                    origin: snapshot.origin,
                }))
            }
            Mode::Image => {
                let snapshot = self
                    .clipboard()?
                    .image_snapshot()
                    .context("无法读取可共享的 PNG")?
                    .context("当前剪贴板没有可共享的 PNG")?;
                Ok(LocalPayload::Image {
                    png: snapshot.png,
                    origin: snapshot.origin,
                })
            }
        }
    }

    fn decide(&mut self, app: &App, decision: Decision) -> Result<()> {
        let active = self.active.as_mut().context("没有待处理的剪贴板内容")?;
        let pending = active.pending.take().context("没有待处理的剪贴板内容")?;
        let (pin, _, _) = Pin::load(app)?;
        anyhow::ensure!(pin == active.pin && !active.stopping, "配对状态已变化");
        let completion = match decision {
            Decision::Import => {
                pending.import(self.clipboard.as_mut().context("系统剪贴板不可用")?)
            }
            Decision::Decline => pending.decline(),
            #[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
            Decision::ImportForDrag => {
                let (completion, payload) = pending
                    .import_for_drag(self.clipboard.as_mut().context("系统剪贴板不可用")?)
                    .context("本次导入授权已结束")?;
                active.drag_payload = payload;
                Ok(completion)
            }
        }
        .context("本次导入授权已结束")?;
        self.local_outcome = Some(completion.outcome());
        self.notice = "正在向发送端报告处理结果。".into();
        active
            .job
            .complete(completion)
            .context("无法向发送端报告处理结果")
    }

    pub(super) fn ui(&mut self, app: &App, ui: &mut egui::Ui) {
        ui.add_enabled_ui(self.active.is_none(), |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.selectable_value(&mut self.mode, Mode::Text, "纯文本");
                ui.selectable_value(&mut self.mode, Mode::Image, "PNG 图片");
            });
        });
        ui.label("两端先选同一种内容，接收端再开启等待。");
        ui.weak("发送前请确认当前复制的内容。导入需要在接收端再次确认。");
        let send_label = match self.mode {
            Mode::Text => "发送当前纯文本",
            Mode::Image => "发送当前 PNG",
        };
        ui.add_space(8.0);
        ui.add_enabled_ui(data_access::ready(app) && self.active.is_none(), |ui| {
            let hint = data_access::saved_target(&app.config.address, PORT)
                .map_or_else(|| format!("IP:{PORT}"), |address| address.to_string());
            ui.label("接收端地址");
            ui.add(egui::TextEdit::singleline(&mut self.target).hint_text(hint));
            if ui.button(send_label).clicked() {
                self.local_outcome = None;
                if let Err(error) = self.start(app, true) {
                    self.notice = error.to_string();
                }
            }
            ui.add_space(8.0);
            ui.label("本机监听地址");
            ui.text_edit_singleline(&mut self.bind);
            if ui.button("等待接收").clicked() {
                self.local_outcome = None;
                if let Err(error) = self.start(app, false) {
                    self.notice = error.to_string();
                }
            }
        });
        if !data_access::ready(app) {
            ui.weak("请先完成设备配对并打开可写窗口。");
        }
        if let Some(active) = &self.active {
            let pending = active.pending.is_some() && !active.stopping;
            let can_stop = !active.stopping;
            ui.horizontal_wrapped(|ui| {
                if pending {
                    let decision = pending_decision(app, ui);
                    if let Some(decision) = decision {
                        if let Err(error) = self.decide(app, decision) {
                            self.stop();
                            self.notice = error.to_string();
                        }
                    }
                }
                if ui
                    .add_enabled(can_stop, egui::Button::new("停止本次共享"))
                    .clicked()
                {
                    self.stop();
                }
            });
        }
        #[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
        if let Some(offer) = self.drag_offer.as_mut() {
            match offer.ui(app, ui) {
                Ok(None) => (),
                Ok(Some(notice)) => {
                    self.drag_offer = None;
                    self.notice = notice.into();
                }
                Err(_) => {
                    self.drag_offer = None;
                    self.notice = "系统未能开始复制拖放，请检查目标应用。".into();
                }
            }
        }
        if let Some(label) = self
            .local_outcome
            .map(outcome_label)
            .filter(|label| self.notice != *label)
        {
            ui.label(label);
        }
        if !self.notice.is_empty() {
            ui.label(&self.notice);
        }
    }
}

fn pending_decision(_app: &App, ui: &mut egui::Ui) -> Option<Decision> {
    let import = ui.button("导入到本机剪贴板").clicked();
    #[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
    let prepare_drag = ui
        .add_enabled(
            clipboard_drop::eligible(_app, ui.ctx()) && clipboard_drop::focused(ui.ctx()),
            egui::Button::new("导入并准备拖放"),
        )
        .on_hover_text("导入到本机剪贴板，并准备一次本机复制拖放。")
        .clicked();
    let decline = ui.button("拒绝").clicked();
    if import {
        return Some(Decision::Import);
    }
    #[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
    if prepare_drag {
        return Some(Decision::ImportForDrag);
    }
    decline.then_some(Decision::Decline)
}

fn outcome_label(outcome: TextOutcome) -> &'static str {
    match outcome {
        TextOutcome::Submitted => "已提交给接收端的系统剪贴板。",
        TextOutcome::Changed => "接收端的剪贴板已变化，本次内容未导入。",
        TextOutcome::Declined => "接收端已拒绝导入。",
        TextOutcome::Failed => "接收端无法导入。",
        TextOutcome::Unknown => "系统未能确认导入结果，请在接收端检查剪贴板。",
    }
}
