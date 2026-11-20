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
use super::{clipboard_drop, clipboard_handoff};
#[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
use transport::clipboard_worker::DragPayload;

const PORT: u16 = 24872;

enum Decision {
    Import,
    Decline,
    #[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
    ImportForDrag,
    #[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
    PrepareDrop,
}

struct Active {
    pin: Pin,
    job: Job,
    pending: Option<Box<PendingClipboard>>,
    for_drop: bool,
    #[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
    drag_payload: Option<DragPayload>,
    #[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
    address: Option<std::net::SocketAddr>,
    #[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
    sending: bool,
    stopping: bool,
}

pub(super) struct Panel {
    clipboard: Option<NativeClipboard>,
    active: Option<Active>,
    mode: Mode,
    for_drop: bool,
    target: String,
    bind: String,
    notice: String,
    local_outcome: Option<TextOutcome>,
    #[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
    drag_offer: Option<clipboard_drop::Offer>,
    #[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
    prepared_handoff: Option<clipboard_handoff::Prepared>,
    #[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
    handoff: Option<clipboard_handoff::Handoff>,
}

impl Default for Panel {
    fn default() -> Self {
        Self {
            clipboard: None,
            active: None,
            mode: Mode::Text,
            for_drop: false,
            target: String::new(),
            bind: String::new(),
            notice: String::new(),
            local_outcome: None,
            #[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
            drag_offer: None,
            #[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
            prepared_handoff: None,
            #[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
            handoff: None,
        }
    }
}

impl Panel {
    pub(super) fn is_active(&self) -> bool {
        #[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
        if self.drag_offer.is_some() || self.prepared_handoff.is_some() || self.handoff.is_some() {
            return true;
        }
        self.active.is_some()
    }

    #[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
    pub(super) fn cancel_handoff(&mut self, app: &mut App) {
        if let Some(handoff) = self.handoff.take() {
            handoff.cancel(app);
            self.notice = "本次跨机复制拖放已取消。".into();
        }
    }

    pub(super) fn stop(&mut self) {
        #[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
        if self.prepared_handoff.take().is_some() {
            self.notice = "已撤下本次复制拖放。".into();
        }
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

    pub(super) fn tick(&mut self, app: &mut App, _ctx: &egui::Context) {
        #[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
        {
            if let Some(handoff) = self.handoff.as_mut() {
                let result = handoff.tick(app, _ctx);
                self.finish_handoff(app, result);
            }
            if self
                .prepared_handoff
                .as_ref()
                .is_some_and(|ready| !ready.current(app, _ctx))
            {
                self.prepared_handoff = None;
                self.notice = "本次复制拖放准备已失效。".into();
            }
        }
        #[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
        if let Some(notice) = self
            .drag_offer
            .as_ref()
            .and_then(|offer| offer.poll(app, _ctx))
        {
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
        #[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
        if self.active.as_ref().is_some_and(|active| active.for_drop)
            && (!clipboard_drop::eligible(app, _ctx) || !clipboard_drop::focused(_ctx))
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
                Event::PeerReceived if active.for_drop => {
                    "另一台已收到文本，正在等对方决定是否准备拖放。".into()
                }
                Event::PeerReceived => "另一台已收到文本，正在等对方决定是否导入。".into(),
                Event::WaitingForImageResult => "已提交图片，正在等待接收端回执。".into(),
                Event::Pending(pending) => {
                    let decision = if active.for_drop {
                        "请选择准备拖放或拒绝。"
                    } else {
                        "请选择导入或拒绝。"
                    };
                    let notice = pending.dimensions().map_or_else(
                        || format!("已收到一条文本。{decision}"),
                        |(width, height)| format!("已收到 {width} × {height} PNG。{decision}"),
                    );
                    active.pending = Some(pending);
                    notice
                }
                Event::Finished {
                    outcome,
                    copy_receipt,
                    receive_bind: _receive_bind,
                } => {
                    active.pending = None;
                    if active.for_drop
                        && outcome == TextOutcome::Submitted
                        && !copy_receipt
                            .as_ref()
                            .is_some_and(|receipt| active.pin.matches_copy(receipt))
                    {
                        self.stop();
                        self.notice = "本次拖放的配对回执已失效。".into();
                        return;
                    }
                    #[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
                    if outcome == TextOutcome::Submitted {
                        let payload = active.drag_payload.take();
                        if let Some(receipt) = copy_receipt {
                            if !active.sending && payload.is_none() {
                                self.notice = "本次复制拖放准备已失效。".into();
                                return;
                            }
                            let Some(address) = _receive_bind.or(active.address) else {
                                self.notice = "本次复制拖放缺少已确认的地址。".into();
                                return;
                            };
                            self.prepared_handoff = Some(clipboard_handoff::Prepared {
                                pin: active.pin.clone(),
                                address,
                                receipt,
                                payload,
                            });
                        } else if let Some(payload) = payload {
                            self.drag_offer =
                                Some(clipboard_drop::Offer::new(active.pin.clone(), payload));
                        }
                    }
                    outcome_label(outcome, active.for_drop).into()
                }
                Event::Disconnected => {
                    active.pending = None;
                    "剪贴板连接已关闭，本次共享已结束。".into()
                }
                Event::Stopped => "本次剪贴板共享已停止。".into(),
                Event::Failed(_) => {
                    active.pending = None;
                    "共享未完成。请检查两端内容类型、用途、地址和等待时间。".into()
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
        let for_drop = self.for_drop;
        let (pin, identity, peer) = Pin::load(app)?;
        let wake = app.wake_ui.clone();
        let (job, _address) = if send {
            let address =
                data_access::send_address(&self.target, data_access::default_target(app, PORT))?;
            let payload = self.local_payload()?;
            (
                clipboard_worker::send(address, identity, peer, payload, for_drop, wake),
                Some(address),
            )
        } else {
            let bind = data_access::receive_bind(&self.bind, PORT)?;
            let revision = if for_drop {
                None
            } else {
                Some(
                    self.clipboard()?
                        .revision()
                        .context("无法确认本机剪贴板当前版本")?,
                )
            };
            (
                clipboard_worker::receive(bind, identity, peer, revision, self.mode, wake),
                None,
            )
        };
        let job = job.context("无法启动剪贴板后台任务")?;
        self.active = Some(Active {
            pin,
            job,
            pending: None,
            for_drop,
            #[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
            drag_payload: None,
            #[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
            address: _address,
            #[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
            sending: send,
            stopping: false,
        });
        self.notice = match (send, for_drop) {
            (true, true) => "正在发送当前剪贴板内容供另一台电脑拖放。",
            (true, false) => "正在准备发送当前剪贴板内容。",
            (false, _) => "正在准备等待本次共享。",
        }
        .into();
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
            Decision::PrepareDrop => {
                let (completion, payload) = pending.prepare_drop()?;
                active.drag_payload = Some(payload);
                Ok(completion)
            }
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
        if !active.for_drop {
            self.local_outcome = Some(completion.outcome());
        }
        self.notice = "正在向发送端报告处理结果。".into();
        active
            .job
            .complete(completion)
            .context("无法向发送端报告处理结果")
    }

    pub(super) fn ui(&mut self, app: &mut App, ui: &mut egui::Ui) {
        #[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
        if self.handoff_ui(app, ui) {
            return;
        }
        super::visual_assets::handoff_art(ui, super::icons::Icon::Log);
        ui.add_enabled_ui(self.active.is_none(), |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.selectable_value(&mut self.mode, Mode::Text, "纯文本");
                ui.selectable_value(&mut self.mode, Mode::Image, "PNG 图片");
            });
            #[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
            ui.checkbox(&mut self.for_drop, "接收端准备拖放");
        });
        ui.small("两端选择相同类型与用途，接收端先等待。");
        if self.for_drop {
            ui.weak("接收端确认后可在本机拖放，系统剪贴板不会被写入。");
        } else {
            ui.weak("发送当前内容，导入需接收端确认。");
        }
        let send_label = match self.mode {
            Mode::Text => "发送当前纯文本",
            Mode::Image => "发送当前 PNG",
        };
        ui.add_space(8.0);
        #[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
        let drop_available =
            clipboard_drop::eligible(app, ui.ctx()) && clipboard_drop::focused(ui.ctx());
        #[cfg(not(any(all(windows, target_env = "msvc"), target_os = "macos")))]
        let drop_available = false;
        let ready =
            data_access::ready(app) && self.active.is_none() && (!self.for_drop || drop_available);
        ui.add_enabled_ui(ready, |ui| {
            let hint = data_access::default_target(app, PORT)
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
            ui.add(
                egui::TextEdit::singleline(&mut self.bind)
                    .hint_text(format!("留空自动监听 IPv4 / IPv6，端口 {PORT}")),
            );
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
            let for_drop = active.for_drop;
            ui.horizontal_wrapped(|ui| {
                if pending {
                    let decision = pending_decision(app, ui, for_drop);
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
            .map(|outcome| outcome_label(outcome, false))
            .filter(|label| self.notice != *label)
        {
            ui.label(label);
        }
        if !self.notice.is_empty() {
            ui.label(&self.notice);
        }
    }
}

#[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
impl Panel {
    fn finish_handoff(&mut self, app: &mut App, result: Result<Option<&'static str>>) {
        let notice = match result {
            Ok(Some(notice)) => Some(notice.to_owned()),
            Ok(None) => None,
            Err(error) => Some(error.to_string()),
        };
        if let Some(notice) = notice {
            self.cancel_handoff(app);
            self.notice = notice;
        }
    }

    fn handoff_ui(&mut self, app: &mut App, ui: &mut egui::Ui) -> bool {
        if self.handoff.is_some() {
            if ui.button("取消跨机拖动").clicked() {
                self.cancel_handoff(app);
                return true;
            }
            if let Some(handoff) = self.handoff.as_mut() {
                let result = handoff.ui(app, ui);
                self.finish_handoff(app, result);
            }
            return true;
        }
        let Some(ready) = &self.prepared_handoff else {
            return false;
        };
        if self.active.is_some() {
            ui.label("正在结束本次数据连接。");
            return true;
        }
        let receiving = ready.payload.is_some();
        let label = if receiving {
            "准备跨机接收"
        } else {
            "准备跨机拖出"
        };
        let can_start = app.child.is_none() && (!receiving || app.config.allow);
        if ui
            .add_enabled(can_start, egui::Button::new(label))
            .clicked()
        {
            let prepared = self.prepared_handoff.take().expect("current preparation");
            match clipboard_handoff::Handoff::new(prepared, app, ui.ctx()) {
                Ok(handoff) => self.handoff = Some(handoff),
                Err(error) => self.notice = error.to_string(),
            }
        } else if receiving && ui.button("准备本机拖放").clicked() {
            if let Some(prepared) = self.prepared_handoff.take() {
                self.drag_offer = prepared
                    .payload
                    .map(|payload| clipboard_drop::Offer::new(prepared.pin, payload));
            }
        } else if ui.button("取消准备").clicked() {
            self.prepared_handoff = None;
            self.notice = "已撤下本次复制拖放。".into();
        }
        if !can_start {
            ui.label("请先结束当前会话。接收端还需在设置中允许键鼠接收。");
        }
        if !self.notice.is_empty() {
            ui.label(&self.notice);
        }
        true
    }
}

fn pending_decision(_app: &App, ui: &mut egui::Ui, for_drop: bool) -> Option<Decision> {
    if for_drop {
        #[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
        if ui
            .add_enabled(
                clipboard_drop::eligible(_app, ui.ctx()) && clipboard_drop::focused(ui.ctx()),
                egui::Button::new("同意准备拖放"),
            )
            .clicked()
        {
            return Some(Decision::PrepareDrop);
        }
        return ui.button("拒绝").clicked().then_some(Decision::Decline);
    }
    let import = ui
        .add(super::desktop_style::primary("导入到本机剪贴板"))
        .clicked();
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

fn outcome_label(outcome: TextOutcome, for_drop: bool) -> &'static str {
    if for_drop {
        return match outcome {
            TextOutcome::Submitted => "接收端已同意准备本机拖放。",
            TextOutcome::Declined => "接收端已拒绝准备拖放。",
            TextOutcome::Changed | TextOutcome::Failed | TextOutcome::Unknown => {
                "接收端未确认准备拖放。"
            }
        };
    }
    match outcome {
        TextOutcome::Submitted => "已提交给接收端的系统剪贴板。",
        TextOutcome::Changed => "接收端的剪贴板已变化，本次内容未导入。",
        TextOutcome::Declined => "接收端已拒绝导入。",
        TextOutcome::Failed => "接收端无法导入。",
        TextOutcome::Unknown => "系统未能确认导入结果，请在接收端检查剪贴板。",
    }
}
