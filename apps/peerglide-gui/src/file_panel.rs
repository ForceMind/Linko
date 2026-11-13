//! Desktop selection and consent for one file job at a time.
use super::{
    data_access::{self, Pin},
    egui, transport, App,
};
use anyhow::{Context, Result};
use std::{io, path::PathBuf, sync::mpsc};
use transport::file_worker::{
    self, Decision, Event, FileOutcome, History, HistoryState, Job, Offer, Progress, Request,
};

const PORT: u16 = 24873;

enum Approval {
    Receive,
    Resume,
    Decline,
}

struct Active {
    pin: Pin,
    job: Job,
    offer: Option<Offer>,
    stopping: bool,
    #[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
    listened: Option<std::net::SocketAddr>,
}

enum HistoryQuery {
    Pending(mpsc::Receiver<io::Result<History>>),
    Complete(io::Result<History>),
}

struct HistorySearch {
    directory: PathBuf,
    query: HistoryQuery,
}

#[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
struct SourceHandoff {
    pin: Pin,
    edge: String,
    layout: peerglide_platform::display_layout::Layout,
    reservation: super::file_drop_reservation::Reservation,
}

#[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
impl SourceHandoff {
    fn new(
        app: &mut App,
        pin: Pin,
        target: std::net::SocketAddr,
        receipt: transport::file_drop_link::SentReceipt,
    ) -> Result<Self> {
        let layout = peerglide_platform::read_display_layout().context("无法读取本机显示器")?;
        let request = super::file_drop_reservation::next_request(app)?;
        let expires = std::time::Instant::now() + std::time::Duration::from_secs(60);
        Ok(Self {
            pin,
            edge: app.displayed_edge().to_owned(),
            layout,
            reservation: super::file_drop_reservation::Reservation::source(
                receipt, request, target, expires,
            ),
        })
    }

    fn check_layout(&self) -> Result<()> {
        let current = peerglide_platform::read_display_layout().context("无法读取本机显示器")?;
        anyhow::ensure!(self.layout.same_configuration(&current), "显示器布局已变化");
        Ok(())
    }
}

pub(super) struct Panel {
    active: Option<Active>,
    #[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
    export: super::native_drop_panel::Export,
    #[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
    sending_handoff: Option<SourceHandoff>,
    #[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
    edge: Option<super::file_edge_drop::Offer>,
    source: String,
    destination: String,
    target: String,
    bind: String,
    resume: String,
    prepared_id: Option<[u8; 16]>,
    progress: Option<Progress>,
    notice: String,
    history_directory: String,
    history: Option<HistorySearch>,
}

impl Default for Panel {
    fn default() -> Self {
        Self {
            active: None,
            #[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
            export: super::native_drop_panel::Export::default(),
            #[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
            sending_handoff: None,
            #[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
            edge: None,
            source: String::new(),
            destination: String::new(),
            target: String::new(),
            bind: format!("0.0.0.0:{PORT}"),
            resume: String::new(),
            prepared_id: None,
            progress: None,
            notice: String::new(),
            history_directory: String::new(),
            history: None,
        }
    }
}

impl Panel {
    pub(super) fn is_active(&self) -> bool {
        #[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
        if self.export.active() || self.sending_handoff.is_some() {
            return true;
        }
        self.active.is_some() || self.edge_active()
    }

    pub(super) fn stop(&mut self, _app: &mut App) {
        #[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
        {
            self.export.stop(_app);
            self.cancel_source_handoff(_app);
            self.edge = None;
        }
        if let Some(active) = self.active.as_mut().filter(|active| !active.stopping) {
            active.job.stop();
            active.offer = None;
            active.stopping = true;
            self.notice = "正在停止文件传输。".into();
        }
    }

    #[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
    pub(super) fn cancel_handoff(&mut self, app: &mut App) {
        self.cancel_source_handoff(app);
        if self.export.handoff_active() {
            self.export.stop(app);
        }
    }

    fn handoff_active(&self) -> bool {
        #[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
        return self.export.handoff_active() || self.sending_handoff.is_some();
        #[cfg(not(any(all(windows, target_env = "msvc"), target_os = "macos")))]
        false
    }

    pub(super) fn tick(&mut self, app: &mut App, _ctx: &egui::Context) {
        #[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
        if _ctx.viewport_id() == egui::ViewportId::ROOT {
            self.tick_edge(app, _ctx);
            self.tick_source_handoff(app, _ctx);
        }
        #[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
        self.export.tick(app, _ctx);
        if let Some(search) = self.history.as_mut() {
            search.poll();
        }
        if self
            .active
            .as_ref()
            .is_some_and(|active| !active.stopping && !active.pin.matches(app))
        {
            self.stop(app);
        }
        let Some(active) = self.active.as_mut() else {
            return;
        };
        let finished = active.job.is_finished();
        self.progress = active.job.progress();
        while let Some(event) = active.job.poll() {
            self.notice = match event {
                Event::Sent {
                    outcome,
                    handoff: _handoff,
                } => {
                    let notice = outcome_label(outcome, false);
                    #[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
                    if let Some((target, receipt)) = _handoff.filter(|_| {
                        !active.stopping && source_context(app, _ctx) && active.pin.matches(app)
                    }) {
                        match SourceHandoff::new(app, active.pin.clone(), target, receipt) {
                            Ok(handoff) => self.sending_handoff = Some(handoff),
                            Err(_) => {
                                self.notice = format!("{notice} 无法准备本次跨机拖动。");
                                continue;
                            }
                        }
                    }
                    notice.into()
                }
                Event::Received(result) => {
                    #[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
                    if !active.stopping && active.pin.matches(app) {
                        if let Some(file) = result.file {
                            self.export
                                .received(app, active.pin.clone(), file, active.listened);
                        }
                    }
                    let local = outcome_label(result.outcome, true);
                    if result.report_queued {
                        local.into()
                    } else {
                        format!("{local} 未能把回执送入发送队列。")
                    }
                }
                Event::Stopped => "文件任务已停止。已暂存的数据保留。".into(),
                _ if active.stopping => continue,
                Event::Prepared(id) => {
                    self.prepared_id = Some(id);
                    "文件准备完毕，正在连接接收端。".into()
                }
                Event::Listening(address) => {
                    #[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
                    {
                        active.listened = Some(address);
                    }
                    format!(
                        "正在等待已配对电脑。发送端需填写本机 IP 和 UDP 端口 {}。",
                        address.port()
                    )
                }
                Event::Offer(offer) => {
                    active.offer = Some(offer);
                    "收到文件请求，请选择接收、恢复或拒绝。".into()
                }
                Event::Failed => {
                    active.offer = None;
                    "传输结束，未收到成功确认。再次发送前请检查接收目录。".into()
                }
            };
        }
        if finished {
            if active.stopping && self.notice == "正在停止文件传输。" {
                self.notice = "文件任务已停止。请在接收端确认目录状态。".into();
            }
            self.active = None;
        }
    }

    fn start(&mut self, app: &mut App, sending: bool, prepare_handoff: bool) -> Result<()> {
        anyhow::ensure!(self.active.is_none(), "请先结束当前文件任务");
        anyhow::ensure!(!self.edge_active(), "请先关闭边沿发送");
        let request = if sending {
            Request::Send {
                target: data_access::send_address(&self.target, &app.config.address, PORT)?,
                source: absolute_path(&self.source).context("请填写源文件的绝对路径")?,
                resume_id: parse_id(&self.resume)?,
                prepare_handoff,
            }
        } else {
            Request::Receive {
                bind: data_access::parse_address(&self.bind)
                    .context("请填写本机监听地址和 UDP 端口")?,
                budget: file_worker::MAX_FILE_BYTES,
            }
        };
        self.start_job(app, request, None)
    }

    fn start_job(&mut self, app: &mut App, request: Request, expected: Option<&Pin>) -> Result<()> {
        anyhow::ensure!(self.active.is_none(), "请先结束当前文件任务");
        anyhow::ensure!(!self.handoff_active(), "请先撤回本次文件预约");
        let (pin, identity, peer) = Pin::load(app)?;
        anyhow::ensure!(
            expected.is_none_or(|expected| *expected == pin),
            "设备配对状态已变化"
        );
        let sending = matches!(request, Request::Send { .. });
        let job = file_worker::start(request, identity, peer, app.wake_ui.clone())
            .context("无法启动文件后台任务")?;
        #[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
        self.export.stop(app);
        self.active = Some(Active {
            pin,
            job,
            offer: None,
            stopping: false,
            #[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
            listened: None,
        });
        self.progress = None;
        if sending {
            self.prepared_id = None;
            self.notice = "正在读取文件并计算分块校验。".into();
        } else {
            self.notice = "正在准备接收文件请求。".into();
        }
        Ok(())
    }

    #[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
    fn edge_active(&self) -> bool {
        self.edge.is_some()
    }

    #[cfg(not(any(all(windows, target_env = "msvc"), target_os = "macos")))]
    fn edge_active(&self) -> bool {
        false
    }

    #[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
    fn arm_edge(&mut self, app: &App, ctx: &egui::Context) -> Result<()> {
        anyhow::ensure!(
            self.active.is_none() && self.edge.is_none() && !self.export.active(),
            "请先结束当前文件或拖放任务"
        );
        let (pin, _, _) = Pin::load(app)?;
        let target = data_access::send_address(&self.target, &app.config.address, PORT)?;
        self.edge = Some(super::file_edge_drop::Offer::arm(app, ctx, pin, target)?);
        self.notice = "把一个文件拖到屏幕边沿的接收条。".into();
        Ok(())
    }

    #[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
    fn tick_edge(&mut self, app: &mut App, ctx: &egui::Context) {
        use super::file_edge_drop::Event;
        let Some(offer) = self.edge.as_mut() else {
            return;
        };
        let now = std::time::Instant::now();
        let unavailable = ctx.input(|input| {
            input.viewport().close_requested() || input.viewport().minimized == Some(true)
        });
        let target = data_access::send_address(&self.target, &app.config.address, PORT);
        if unavailable || !offer.current(app, now) || target.ok() != Some(offer.target) {
            self.edge = None;
            self.notice = "边沿发送许可已结束。".into();
            return;
        }
        if let Err(error) = offer.check_layout_if_due(now) {
            self.edge = None;
            self.notice = error.to_string();
            return;
        }
        let Some(event) = offer.poll() else {
            return;
        };
        let offer = self.edge.take().expect("edge offer is present");
        self.notice = match event {
            Event::File(source) => {
                let result = offer.validate_drop(app, now).and_then(|()| {
                    self.start_job(
                        app,
                        Request::Send {
                            target: offer.target,
                            source,
                            resume_id: None,
                            prepare_handoff: false,
                        },
                        Some(&offer.pin),
                    )
                });
                match result {
                    Ok(()) => return,
                    Err(error) => error.to_string(),
                }
            }
            Event::Cancelled => "边沿发送已取消。".into(),
            Event::InvalidFile => "请重新开启边沿发送并拖入一个本机文件。".into(),
            Event::PositionUnavailable => "无法定位边沿接收条，请检查屏幕布局。".into(),
        };
    }

    #[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
    fn cancel_source_handoff(&mut self, app: &mut App) {
        if let Some(handoff) = self.sending_handoff.take() {
            handoff.reservation.cancel(app);
            self.notice = "本次跨机拖动已结束，另一台电脑保留已接收文件。".into();
        }
    }

    #[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
    fn tick_source_handoff(&mut self, app: &mut App, ctx: &egui::Context) {
        use super::file_drop_reservation::Status;
        let Some(handoff) = &mut self.sending_handoff else {
            return;
        };
        let valid = source_context(app, ctx)
            && handoff.pin.matches(app)
            && handoff.edge == app.displayed_edge();
        if !valid {
            self.cancel_source_handoff(app);
            return;
        }
        let now = std::time::Instant::now();
        if handoff.reservation.tick(app, &handoff.pin, now).is_err() {
            self.cancel_source_handoff(app);
        } else if matches!(handoff.reservation.status(), Status::SourceReleased) {
            self.sending_handoff = None;
            self.notice = "已将释放交给另一台电脑，请在目标应用确认文件。".into();
        }
    }

    #[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
    fn source_handoff_ui(&mut self, app: &mut App, ui: &mut egui::Ui) {
        use super::file_drop_reservation::Status;
        let Some(handoff) = &mut self.sending_handoff else {
            return;
        };
        if ui.button("取消跨机拖动").clicked() {
            self.cancel_source_handoff(app);
            return;
        }
        match handoff.reservation.status() {
            Status::WaitingForWorker => {
                ui.label("请让另一台电脑准备跨机接收，然后连接键鼠。");
                if ui
                    .add_enabled(
                        app.native_start_available(),
                        egui::Button::new("开启键鼠协同"),
                    )
                    .clicked()
                {
                    app.workspace_action(super::desktop_state::Action::StartInput);
                }
            }
            Status::Press => {
                let drag =
                    ui.add(egui::Button::new("按住并拖向切换边沿").sense(egui::Sense::drag()));
                let pressed = ui.input(|input| {
                    input.focused
                        && input.pointer.button_pressed(egui::PointerButton::Primary)
                        && input.pointer.primary_down()
                });
                if drag.hovered() && pressed {
                    let result = handoff.check_layout().and_then(|()| {
                        handoff
                            .reservation
                            .press(app, &handoff.pin, std::time::Instant::now())
                    });
                    if result.is_err() {
                        self.cancel_source_handoff(app);
                    }
                }
            }
            Status::Confirming => {
                ui.label("等待键鼠会话确认。");
            }
            Status::SourceArmed => {
                ui.label("保持按住，拖向切换边沿。");
            }
            Status::SourceCommitted => {
                ui.label("已交给另一台电脑，移动到目标后释放。");
            }
            Status::SourceReleased => (),
            Status::Listening(_) | Status::Matched => unreachable!("receive reservation in sender"),
        }
    }

    fn decide(&mut self, app: &App, choice: Approval) -> Result<()> {
        let active = self.active.as_mut().context("没有待处理的文件")?;
        let (pin, _, _) = Pin::load(app)?;
        anyhow::ensure!(active.pin == pin && !active.stopping, "设备配对状态已变化");
        let decision = match choice {
            Approval::Receive | Approval::Resume => Decision::Accept {
                directory: absolute_path(&self.destination).context("请填写接收目录的绝对路径")?,
                resume: matches!(choice, Approval::Resume),
            },
            Approval::Decline => Decision::Decline,
        };
        active.job.decide(decision).context("本次文件请求已结束")?;
        active.offer = None;
        self.notice = "正在处理本次文件请求。".into();
        Ok(())
    }

    pub(super) fn ui(&mut self, app: &mut App, ui: &mut egui::Ui) {
        let paired = data_access::ready(app);
        let ready = paired && !self.handoff_active();
        if !paired {
            ui.weak("请先完成设备配对并打开可写窗口。");
        }
        if ready && self.active.is_none() && !self.edge_active() {
            let dropped = ui.input(|input| {
                let files = &input.raw.dropped_files;
                if files.len() != 1 {
                    return None;
                }
                files[0].path().to_str().map(str::to_owned)
            });
            if let Some(path) = dropped {
                self.source = path;
            }
        }
        ui.add_enabled_ui(
            ready && self.active.is_none() && !self.edge_active(),
            |ui| {
                ui.strong("发送文件");
                ui.add(
                    egui::TextEdit::singleline(&mut self.source)
                        .hint_text("源文件绝对路径，可拖入单个本机文件"),
                );
                let hint = data_access::saved_target(&app.config.address, PORT)
                    .map_or_else(|| format!("IP:{PORT}"), |address| address.to_string());
                ui.label("接收端地址");
                ui.add(egui::TextEdit::singleline(&mut self.target).hint_text(hint));
                ui.add(
                    egui::TextEdit::singleline(&mut self.resume)
                        .hint_text("续传编号，首次发送留空"),
                );
                if ui.button("发送选定文件").clicked() {
                    let result = self.start(app, true, false);
                    if let Err(error) = result {
                        self.notice = error.to_string();
                    }
                }
                #[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
                if ui
                    .add_enabled(
                        source_context(app, ui.ctx()),
                        egui::Button::new("发送并准备跨机拖动"),
                    )
                    .clicked()
                {
                    if let Err(error) = self.start(app, true, true) {
                        self.notice = error.to_string();
                    }
                }
                #[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
                if ui
                    .add_enabled(!self.export.active(), egui::Button::new("开启一次边沿发送"))
                    .clicked()
                {
                    if let Err(error) = self.arm_edge(app, ui.ctx()) {
                        self.notice = error.to_string();
                    }
                }
                ui.add_space(8.0);
                ui.strong("接收文件");
                ui.label("本机监听地址");
                ui.text_edit_singleline(&mut self.bind);
                if ui.button("等待文件请求").clicked() {
                    let result = self.start(app, false, false);
                    if let Err(error) = result {
                        self.notice = error.to_string();
                    }
                }
            },
        );
        #[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
        if let Some(offer) = &self.edge {
            if ui.button("取消边沿发送").clicked() {
                self.edge = None;
                self.notice = "边沿发送已取消。".into();
            } else {
                offer.show(ui.ctx());
            }
        }
        #[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
        self.source_handoff_ui(app, ui);
        if let Some(id) = self.prepared_id {
            ui.horizontal_wrapped(|ui| {
                ui.label(format!("续传编号 {}", encode_id(id)));
                if ui.button("复制编号").clicked() {
                    ui.ctx().copy_text(encode_id(id));
                }
            });
        }
        if self.active.is_some() {
            self.active_ui(app, ui);
        }
        if let Some(progress) = self.progress {
            let ratio = if progress.total == 0 {
                1.0
            } else {
                progress.bytes as f32 / progress.total as f32
            };
            let label = if progress.verified_locally {
                "本机已校验"
            } else {
                "已排队发送或匹配续传"
            };
            ui.add(egui::ProgressBar::new(ratio).text(format!(
                "{label} {} / {} 字节",
                progress.bytes, progress.total
            )));
        }
        if !self.notice.is_empty() {
            ui.label(&self.notice);
        }
        #[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
        if self.active.is_none() && !self.edge_active() && self.sending_handoff.is_none() {
            self.export.ui(app, ui);
        }
        ui.collapsing("接收历史与配额", |ui| {
            self.history_ui(app, ui);
        });
    }

    fn history_ui(&mut self, app: &App, ui: &mut egui::Ui) {
        let pending = self
            .history
            .as_ref()
            .is_some_and(|search| matches!(search.query, HistoryQuery::Pending(_)));
        let enabled = !pending
            && !app.resource_check_mode
            && !app.quit_check_mode
            && !app.close_requested
            && !app.exit_confirmed;
        ui.add_enabled_ui(enabled, |ui| {
            ui.label("接收目录");
            ui.add(
                egui::TextEdit::singleline(&mut self.history_directory)
                    .hint_text(&self.destination),
            );
            if ui.button("查询接收历史").clicked() {
                let result = self.start_history(app);
                if let Err(error) = result {
                    self.notice = error.to_string();
                }
            }
        });
        if let Some(search) = &self.history {
            ui.weak(search.directory.display().to_string());
            match &search.query {
                HistoryQuery::Pending(_) => {
                    ui.label("正在读取接收账本。");
                }
                HistoryQuery::Complete(Ok(history)) => render_history(ui, history),
                HistoryQuery::Complete(Err(_)) => {
                    ui.label("无法读取接收账本。请检查目录是否存在或正在接收文件。");
                }
            }
        }
    }

    fn start_history(&mut self, app: &App) -> Result<()> {
        let text = if self.history_directory.is_empty() {
            &self.destination
        } else {
            &self.history_directory
        };
        let directory = absolute_path(text).context("请填写接收目录的绝对路径")?;
        let events = file_worker::inspect_history(directory.clone(), app.wake_ui.clone())
            .context("无法查询接收历史")?;
        self.history = Some(HistorySearch {
            directory,
            query: HistoryQuery::Pending(events),
        });
        Ok(())
    }

    fn active_ui(&mut self, app: &mut App, ui: &mut egui::Ui) {
        let Some(active) = self.active.as_ref() else {
            return;
        };
        let stopping = active.stopping;
        if let Some(offer) = &active.offer {
            ui.separator();
            ui.label(format!("{} · {} 字节", offer.name, offer.size));
            ui.label(format!("续传编号 {}", encode_id(offer.id)));
            ui.weak("每个接收目录累计配额 8 GiB。已存在的文件会保留。");
            ui.add(
                egui::TextEdit::singleline(&mut self.destination)
                    .hint_text("已有接收目录的绝对路径"),
            );
            ui.horizontal_wrapped(|ui| {
                for (label, approval) in [
                    ("接收文件", Approval::Receive),
                    ("恢复已暂存文件", Approval::Resume),
                    ("拒绝", Approval::Decline),
                ] {
                    if ui.button(label).clicked() {
                        let result = self.decide(app, approval);
                        if let Err(error) = result {
                            self.notice = error.to_string();
                        }
                    }
                }
            });
        }
        if ui
            .add_enabled(!stopping, egui::Button::new("停止文件任务"))
            .clicked()
        {
            self.stop(app);
        }
    }
}

impl HistorySearch {
    fn poll(&mut self) {
        let HistoryQuery::Pending(events) = &self.query else {
            return;
        };
        let result = match events.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => {
                Err(io::Error::other("receive history worker ended"))
            }
        };
        self.query = HistoryQuery::Complete(result);
    }
}

#[cfg(any(all(windows, target_env = "msvc"), target_os = "macos"))]
fn source_context(app: &App, ctx: &egui::Context) -> bool {
    data_access::ready(app)
        && app.page == super::Page::Transfers
        && !app.routed_active
        && (app.child.is_none() || app.active_mode == Some(super::SessionMode::Native))
        && !app.window_hidden
        && !app.recovery_blocked
        && !app.input_terminal
        && app.stopping.is_none()
        && ctx.input(|input| {
            input.focused
                && !input.viewport().close_requested()
                && !input.viewport().minimized.unwrap_or(false)
        })
}

fn render_history(ui: &mut egui::Ui, history: &History) {
    ui.label(format!(
        "配额已预留 {} / {} 字节",
        history.inventory.reserved_bytes,
        file_worker::MAX_FILE_BYTES,
    ));
    if history.entries.is_empty() {
        ui.label("此目录暂无接收记录。");
    }
    for entry in &history.entries {
        let name = entry.name.as_deref().unwrap_or("名称未记录");
        let state = match entry.state {
            HistoryState::Reserved => "已预留",
            HistoryState::Staged => "保有暂存",
            HistoryState::PublicationUnconfirmed => "发布结果未确认",
        };
        ui.label(format!("{name} · {} 字节 · {state}", entry.size));
        ui.weak(format!("续传编号 {}", encode_id(entry.id)));
    }
}

fn absolute_path(text: &str) -> Result<PathBuf> {
    let path = PathBuf::from(text);
    anyhow::ensure!(path.is_absolute(), "请输入绝对路径");
    Ok(path)
}

fn encode_id(id: [u8; 16]) -> String {
    use std::fmt::Write as _;
    let mut text = String::with_capacity(32);
    for byte in id {
        let _ = write!(text, "{byte:02x}");
    }
    text
}

fn parse_id(text: &str) -> Result<Option<[u8; 16]>> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    anyhow::ensure!(
        text.len() == 32 && text.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "续传编号须为 32 位十六进制字符"
    );
    let mut id = [0; 16];
    for (index, byte) in id.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16)?;
    }
    anyhow::ensure!(id != [0; 16], "续传编号无效");
    Ok(Some(id))
}

fn outcome_label(outcome: FileOutcome, receiving: bool) -> &'static str {
    match outcome {
        FileOutcome::Committed if receiving => "本机已校验并保存文件。",
        FileOutcome::Committed => "接收端已确认保存文件。",
        FileOutcome::Declined => "接收端已拒绝本次文件。",
        FileOutcome::Failed => "接收端未能接收或校验本次文件。",
        FileOutcome::Unknown => "文件保存结果未确认，请检查接收目录。",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resume_id_round_trips_and_rejects_unbounded_or_malformed_input() -> Result<()> {
        let id = [127; 16];
        assert_eq!(parse_id(&encode_id(id))?, Some(id));
        assert_eq!(parse_id(&encode_id(id).to_uppercase())?, Some(id));
        assert_eq!(parse_id(" ")?, None);
        assert!(parse_id(&encode_id([0; 16])).is_err());
        assert!(parse_id("屏跃").is_err());
        assert!(parse_id(&"a".repeat(1024)).is_err());
        assert!(absolute_path("relative.bin").is_err());
        Ok(())
    }
}
