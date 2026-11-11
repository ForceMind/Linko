//! One-shot text controls. Rendering and polling never access the system clipboard.
use super::{
    data_access::{self, Pin},
    egui, transport, App,
};
use anyhow::{Context, Result};
use peerglide_platform::clipboard::NativeClipboard;
use transport::clipboard_worker::{
    self, Classification, Event, Job, LocalText, PendingText, TextOutcome,
};

const PORT: u16 = 24872;

struct Active {
    pin: Pin,
    job: Job,
    pending: Option<PendingText>,
    stopping: bool,
}

pub(super) struct Panel {
    clipboard: Option<NativeClipboard>,
    active: Option<Active>,
    target: String,
    bind: String,
    notice: String,
    local_outcome: Option<TextOutcome>,
}

impl Default for Panel {
    fn default() -> Self {
        Self {
            clipboard: None,
            active: None,
            target: String::new(),
            bind: format!("0.0.0.0:{PORT}"),
            notice: String::new(),
            local_outcome: None,
        }
    }
}

impl Panel {
    pub(super) fn is_active(&self) -> bool {
        self.active.is_some()
    }

    pub(super) fn stop(&mut self) {
        if let Some(active) = self.active.as_mut().filter(|active| !active.stopping) {
            active.job.stop();
            active.pending = None;
            active.stopping = true;
            self.notice = "已请求停止本次剪贴板共享。".into();
        }
    }

    pub(super) fn tick(&mut self, app: &App) {
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
                Event::Pending(text) => {
                    active.pending = Some(text);
                    "已收到一条文本。请选择导入或拒绝。".into()
                }
                Event::Finished(outcome) => {
                    active.pending = None;
                    outcome_label(outcome).into()
                }
                Event::Stopped => "本次剪贴板共享已停止。".into(),
                Event::Failed(_) => {
                    active.pending = None;
                    "剪贴板共享未完成。请检查两端地址、端口、配对状态和等待时间。".into()
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
        let (pin, identity, peer) = Pin::load(app)?;
        let wake = app.wake_ui.clone();
        let job = if send {
            let target = data_access::send_address(&self.target, &app.config.address, PORT)?;
            let snapshot = self
                .clipboard()?
                .snapshot()
                .context("无法读取可共享的文本")?
                .context("当前剪贴板没有可共享的普通文本")?;
            clipboard_worker::send(
                target,
                identity,
                peer,
                LocalText {
                    text: snapshot.text,
                    classification: Classification::ShareableText,
                    origin: snapshot.origin,
                },
                wake,
            )
        } else {
            let bind =
                data_access::parse_address(&self.bind).context("请输入本机监听地址和 UDP 端口")?;
            let revision = self
                .clipboard()?
                .revision()
                .context("无法确认本机剪贴板当前版本")?;
            clipboard_worker::receive(bind, identity, peer, revision, wake)
        }
        .context("无法启动剪贴板后台任务")?;
        self.active = Some(Active {
            pin,
            job,
            pending: None,
            stopping: false,
        });
        self.notice = if send {
            "正在发送本次选定的文本。".into()
        } else {
            "正在准备接收一条文本。".into()
        };
        Ok(())
    }

    fn decide(&mut self, app: &App, import: bool) -> Result<()> {
        let active = self.active.as_mut().context("没有待处理的文本")?;
        let pending = active.pending.take().context("没有待处理的文本")?;
        let (pin, _, _) = Pin::load(app)?;
        anyhow::ensure!(pin == active.pin && !active.stopping, "配对状态已变化");
        let completion = if import {
            pending
                .import(self.clipboard.as_mut().context("系统剪贴板不可用")?)
                .context("本次导入授权已结束")?
        } else {
            pending.decline().context("本次导入授权已结束")?
        };
        self.local_outcome = Some(completion.outcome());
        self.notice = "正在向发送端报告处理结果。".into();
        active
            .job
            .complete(completion)
            .context("无法向发送端报告处理结果")
    }

    pub(super) fn ui(&mut self, app: &App, ui: &mut egui::Ui) {
        ui.label("先在接收端选择“等待接收”，再从另一台发送当前纯文本。");
        ui.weak("发送前请确认当前复制的内容。每次接收都需要本机确认导入。");
        ui.add_space(8.0);
        ui.add_enabled_ui(data_access::ready(app) && self.active.is_none(), |ui| {
            let hint = data_access::saved_target(&app.config.address, PORT)
                .map_or_else(|| format!("IP:{PORT}"), |address| address.to_string());
            ui.label("接收端地址");
            ui.add(egui::TextEdit::singleline(&mut self.target).hint_text(hint));
            if ui.button("发送当前纯文本").clicked() {
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
                    let import = ui.button("导入到本机剪贴板").clicked();
                    let decline = ui.button("拒绝").clicked();
                    if import || decline {
                        let result = self.decide(app, import);
                        if let Err(error) = result {
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

fn outcome_label(outcome: TextOutcome) -> &'static str {
    match outcome {
        TextOutcome::Submitted => "文本已提交给接收端的系统剪贴板。",
        TextOutcome::Changed => "接收端的剪贴板已变化，本次文本未导入。",
        TextOutcome::Declined => "接收端已拒绝导入。",
        TextOutcome::Failed => "接收端无法导入文本。",
        TextOutcome::Unknown => "系统未能确认导入结果，请在接收端检查剪贴板。",
    }
}
