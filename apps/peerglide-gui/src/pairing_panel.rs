//! Chinese pairing flow; only real owned-child barriers reach enrollment.
use super::{
    config_store, enrollment_store, pairing_receipt,
    pairing_supervisor::{Mode, Phase, Supervisor},
    App, Config, Identity,
};
use anyhow::{Context, Result};
use eframe::egui;
use peerglide_transport as transport;
use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
    sync::Arc,
};

use crate::desktop_style::{self, tokens, Tone};
use crate::icons::{self, Icon};
use crate::motion;
use peerglide_protocol::pairing_worker::{Failure, StopReason};

/// Digits in a pairing code; the worker accepts exactly eight ASCII digits.
const CODE_LEN: usize = 8;
/// Cells before the wider gap that splits the code into two groups.
const HALF: usize = 4;
const CODE_LABEL: &str = "八位数字配对码";
const CODE_CELLS_ID: &str = "pairing_code_cells";
const CELL_W: f32 = 34.0;
const CELL_H: f32 = 42.0;
const CELL_GAP: f32 = 6.0;
const GROUP_GAP: f32 = 16.0;
const CARD_H: f32 = 78.0;
const CARD_MIN_W: f32 = 140.0;
/// Pairing stages shown as dots. Each name stays visible beneath its dot.
const STAGES: [&str; 5] = ["准备", "连接", "核验", "记住", "回执"];
const DOTS_H: f32 = 36.0;
const DOT_TOP: f32 = 8.0;
const LABEL_TOP: f32 = 18.0;
const DOT_RADIUS: f32 = 4.5;

#[derive(Clone)]
struct Origin {
    generation: u64,
    config: Config,
    raw: Option<Vec<u8>>,
    peer: Option<Vec<u8>>,
    local: Vec<u8>,
    next: Config,
}
fn optional_peer(dir: &Path) -> Result<Option<Vec<u8>>> {
    let path = dir.join("trusted-peer.crt.der");
    match std::fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
        Ok(_) => Ok(Some(transport::read_certificate(&path)?)),
    }
}
fn parse_address(text: &str) -> Result<SocketAddr> {
    let text = text.trim();
    anyhow::ensure!(!text.is_empty(), "请填写另一台电脑的 IPv4 地址");
    let address = text
        .parse::<SocketAddr>()
        .or_else(|_| text.parse::<IpAddr>().map(|ip| SocketAddr::new(ip, 24871)))
        .map_err(|_| anyhow::anyhow!("地址格式不正确，请填写 IPv4 或 IPv4:端口"))?;
    anyhow::ensure!(
        address.is_ipv4()
            && !address.ip().is_unspecified()
            && !address.ip().is_multicast()
            && address.ip() != IpAddr::V4(Ipv4Addr::BROADCAST)
            && address.port() != 0,
        "请使用有效的 IPv4 单播地址和端口"
    );
    Ok(address)
}
impl Origin {
    fn capture(app: &App, next: Config) -> Result<Self> {
        let local = Identity::load(&app.identity_dir())?.certificate;
        let peer = optional_peer(&app.dir)?;
        anyhow::ensure!(
            peer.as_deref().map(transport::fingerprint) == app.config.fingerprint,
            "旧设备资料不一致，请先核验原文件"
        );
        Ok(Self {
            generation: 0,
            config: app.config.clone(),
            raw: config_store::read(&app.dir.join("settings.json"))?,
            peer,
            local,
            next,
        })
    }
    fn check(&self, dir: &Path, expected: &Config) -> Result<()> {
        anyhow::ensure!(
            config_store::read(&dir.join("settings.json"))? == self.raw,
            "配对期间设置已变化，原文件保留"
        );
        anyhow::ensure!(optional_peer(dir)? == self.peer, "配对期间旧设备证书已变化");
        anyhow::ensure!(
            Identity::load(&dir.join("identity"))?.certificate == self.local,
            "配对期间本机身份已变化"
        );
        anyhow::ensure!(*expected == self.config, "配对期间设备设置已变化");
        Ok(())
    }
}

#[derive(Default)]
pub(super) struct Panel {
    pub(super) open: bool,
    join_entry: bool,
    supervisor: Supervisor,
    origin: Option<Origin>,
    name: String,
    address: String,
    code: String,
    message: String,
    discovery: super::pairing_discovery::Panel,
    feedback: Feedback,
}
impl Drop for Panel {
    fn drop(&mut self) {
        peerglide_protocol::pairing_worker::clear_code(&mut self.code);
    }
}
impl Panel {
    pub(super) fn open(&mut self, name: &str, address: &str) {
        if !self.needs_poll() {
            if let Err(error) = self.supervisor.reset_finished() {
                self.message = format!("无法开始新配对：{error:#}");
                return;
            }
            self.origin = None;
            self.feedback = Feedback::default();
        }
        self.open = true;
        self.join_entry = false;
        self.name = name.into();
        self.address = address
            .parse::<SocketAddr>()
            .map(|mut address| {
                address.set_port(24871);
                address.to_string()
            })
            .unwrap_or_default();
        self.message.clear();
    }
    fn select_entry(&mut self, join: bool) {
        if self.join_entry != join {
            self.discovery.stop();
            peerglide_protocol::pairing_worker::clear_code(&mut self.code);
            self.message.clear();
            self.feedback.code_error = false;
            self.join_entry = join;
        }
    }
    pub(super) fn needs_poll(&self) -> bool {
        self.supervisor.needs_poll() || self.discovery.needs_poll()
    }
    pub(super) fn blocks_actions(&self) -> bool {
        self.open || self.needs_poll()
    }
    fn discovery_offer(&self) -> Option<super::pairing_discovery::Offer> {
        let (address, attempts_remaining) = self.supervisor.offer_hint_state()?;
        Some(super::pairing_discovery::Offer {
            generation: self.supervisor.generation(),
            attempts_remaining,
            port: address.port(),
        })
    }
    pub(super) fn cancel(&mut self) {
        self.supervisor.cancel();
        self.discovery.stop();
        self.origin = None;
        peerglide_protocol::pairing_worker::clear_code(&mut self.code);
    }
    fn worker() -> Result<PathBuf> {
        Ok(std::env::current_exe()?.with_file_name(if cfg!(windows) {
            "peerglide.exe"
        } else {
            "peerglide"
        }))
    }
    fn start(
        &mut self,
        app: &App,
        ctx: &egui::Context,
        mode: Mode,
        connect: Option<SocketAddr>,
        worker: &Path,
    ) -> Result<()> {
        anyhow::ensure!(
            app.instance_lock.is_some() && app.config_store.is_some() && !app.enrollment_blocked,
            "设备资料只读，请先恢复原文件"
        );
        app.validate_pairing_receipt()?;
        anyhow::ensure!(
            !app.quit_check_mode
                && !app.resource_check_mode
                && !app.recording_mode
                && !app.close_requested,
            "当前窗口不能开始配对"
        );
        anyhow::ensure!(
            app.child.is_none() && !app.recovery_blocked && app.pending_start.is_none(),
            "请先停止会话并确认本机输入已恢复"
        );
        anyhow::ensure!(!self.supervisor.needs_poll(), "请等待上次配对进程结束");
        anyhow::ensure!(self.name.chars().count() <= 80, "电脑名称过长");
        let mut next = app.config.clone();
        next.peer_name = if self.name.trim().is_empty() {
            "另一台电脑".into()
        } else {
            self.name.clone()
        };
        next.allow = false;
        next.native = false;
        next.reconnect = false;
        next.listen = connect.is_none();
        next.address = connect
            .map(|mut address| {
                address.set_port(24870);
                address.to_string()
            })
            .unwrap_or_default();
        let mut origin = Origin::capture(app, next)?;
        self.discovery.stop();
        let repaint = ctx.clone();
        self.supervisor.start(
            worker,
            &app.identity_dir(),
            mode,
            Arc::new(move || repaint.request_repaint()),
        )?;
        origin.generation = self.supervisor.generation();
        self.origin = Some(origin);
        self.message.clear();
        Ok(())
    }
    fn approve(&mut self, app: &App) -> Result<()> {
        let origin = self.origin.as_ref().context("本次配对已失效")?;
        origin.check(&app.dir, &app.config)?;
        anyhow::ensure!(
            origin.generation == self.supervisor.generation(),
            "配对目标已变化"
        );
        self.supervisor.approve()
    }
    pub(super) fn tick(&mut self, app: &mut App, _ctx: &egui::Context) {
        if app.close_requested {
            self.cancel();
        }
        self.supervisor.poll();
        let offer = self.discovery_offer();
        self.discovery.poll(offer);
        if !self.open && self.discovery.needs_poll() {
            self.discovery.stop();
        }
        if let Some(origin) = &self.origin {
            if origin.config != app.config
                && self.supervisor.phase() != Phase::AwaitingPeerSaved
                && self.supervisor.phase() != Phase::ReceiptReady
                && self.supervisor.phase() != Phase::Finishing
                && self.supervisor.phase() != Phase::Completed
            {
                self.cancel();
                self.message = "设置已变化，旧配对已停止。".into();
            }
        }
        if let Some(claim) = self.supervisor.take_save_request() {
            let result = (|| -> Result<()> {
                let origin = self.origin.as_ref().context("配对已取消")?;
                anyhow::ensure!(
                    claim.generation() == origin.generation
                        && self.supervisor.generation() == origin.generation,
                    "旧配对结果已失效"
                );
                anyhow::ensure!(claim.local_certificate() == origin.local, "本机身份已变化");
                let mut next = origin.next.clone();
                let context = claim.context();
                next.fingerprint = Some(transport::fingerprint(claim.certificate()));
                next.pairing_receipt = Some(pairing_receipt::Record::pending(
                    context,
                    &transport::fingerprint(&origin.local),
                    next.fingerprint.as_deref().unwrap(),
                )?);
                let dir = app.dir.clone();
                let current = app.config.clone();
                let store = app.config_store.as_mut().context("设置只读")?;
                enrollment_store::commit_guarded(store, &next, claim.certificate(), || {
                    origin.check(&dir, &current)?;
                    anyhow::ensure!(
                        claim.local_certificate() == origin.local,
                        "保存前本机身份已变化"
                    );
                    claim.claim()
                })?;
                app.config = next;
                if let Some(origin) = self.origin.as_mut() {
                    origin.config = app.config.clone();
                }
                self.supervisor.local_saved(context)?;
                Ok(())
            })();
            if let Err(error) = result {
                app.enrollment_blocked =
                    enrollment_store::ensure_idle(&app.dir.join("settings.json")).is_err();
                // A write error can follow durable intent; keep files and require
                // recovery, never restore the former trust or fabricate Saved.
                if !app.pairing_pending() {
                    app.config_store = None;
                }
                self.cancel();
                self.message = format!("保存结果需要核验：{error:#}。不要把取消当作两端都未保存。");
            }
        }
        if let Some(claim) = self.supervisor.take_receipt_request() {
            let result = (|| -> Result<()> {
                let origin = self.origin.as_ref().context("配对已取消")?;
                anyhow::ensure!(
                    claim.generation() == origin.generation
                        && self.supervisor.generation() == origin.generation,
                    "旧收据已失效"
                );
                let mut next = app.config.clone();
                let receipt = next.pairing_receipt.as_mut().context("缺少本机保存记录")?;
                anyhow::ensure!(
                    receipt.is_pending() && receipt.context() == claim.context(),
                    "收据不属于本次待确认登记"
                );
                receipt.mark_confirmed()?;
                let pending = app.config.clone();
                let dir = app.dir.clone();
                let store = app.config_store.as_mut().context("设置只读，收据待核验")?;
                store.save_guarded(&next, || {
                    anyhow::ensure!(
                        Identity::load(&dir.join("identity"))?.certificate
                            == claim.local_certificate(),
                        "本机身份已变化"
                    );
                    let peer = transport::read_certificate(&dir.join("trusted-peer.crt.der"))?;
                    anyhow::ensure!(
                        peer == claim.certificate()
                            && pending.fingerprint.as_deref()
                                == Some(transport::fingerprint(&peer).as_str()),
                        "设备已撤销或替换，旧收据无效"
                    );
                    let disk: Config = config_store::decode(
                        &config_store::read(&dir.join("settings.json"))?
                            .context("缺少待确认登记")?,
                    )?;
                    anyhow::ensure!(disk == pending, "待确认登记已变化");
                    claim.claim()
                })?;
                app.config = next;
                if let Some(origin) = self.origin.as_mut() {
                    origin.config = app.config.clone();
                }
                // Notification loss cannot downgrade the genuine stored receipt.
                self.supervisor.receipt_stored(claim.context())?;
                Ok(())
            })();
            if let Err(error) = result {
                self.cancel();
                self.message = format!("本机记录保留，完成通知待核验：{error:#}");
            }
        }
    }
    pub(super) fn ui(&mut self, app: &mut App, ctx: &egui::Context) {
        if !self.open {
            return;
        }
        let mut open = true;
        let now = ctx.input(|input| input.time);
        let phase = self.supervisor.phase();
        self.feedback.observe(phase, self.join_entry, now);
        // M10: the fixed-size window glides between content heights instead of jumping.
        let target = self.target_height(app);
        let height_id = egui::Id::unique("pairing_window_height");
        let requested_height = motion::dialog_height(ctx, height_id, target);
        let height = requested_height.min((ctx.viewport_rect().height() - 48.0).max(1.0));
        egui::Window::new("添加电脑").open(&mut open).collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER,egui::Vec2::ZERO).fixed_size(egui::vec2(510.0,height)).vscroll(true).show(ctx,|ui|{
            ui.label(desktop_style::semibold("把另一台电脑加入工作区",21.0));
            ui.label("两台电脑连到同一 Wi-Fi 或局域网。任选一台显示配对码，另一台输入；双方确认后会记住彼此。键鼠与数据权限仍单独选择。");
            ui.add_space(12.0);
            if !self.supervisor.needs_poll() && !matches!(self.supervisor.phase(),Phase::Completed) {
                if let Some(join)=entry_cards(ui,self.join_entry){self.select_entry(join);}
                ui.add_space(10.0);
                if app.local_fingerprint().is_none() {
                    ui.label("首次使用，先为这台电脑准备独立安全身份。不会因此开始协同。");
                    if ui.add_enabled(app.instance_lock.is_some()&&app.config_store.is_some()&&!app.enrollment_blocked&&!app.quit_check_mode&&!app.recording_mode,egui::Button::new("准备这台电脑")).clicked(){
                        let result=Identity::create(&app.identity_dir());
                        self.message=match result{Ok(_)=>"本机身份已创建。".into(),Err(error)=>format!("身份未创建，原文件保留：{error:#}")};
                    }
                }else{
                    ui.label("如何称呼另一台电脑");ui.text_edit_singleline(&mut self.name);
                    ui.add_space(10.0);
                    if !self.join_entry {
                    ui.label("点击显示后，在另一台屏跃中选择“我来输入配对码”。");
                    if ui.add(desktop_style::primary("显示我的配对码")).clicked(){
                        let result=Self::worker().and_then(|worker|self.start(app,ctx,Mode::Offer(SocketAddr::from((Ipv4Addr::UNSPECIFIED,24871))),None,&worker));
                        if let Err(error)=result{self.message=format!("尚未开始：{error:#}");}
                    }
                    }else{
                    ui.strong("找到另一台电脑，输入它显示的短码");
                    if let Some(address)=self.discovery.ui(ui,app.local_fingerprint(),None,true){self.select_address(address);}
                    ui.add(egui::TextEdit::singleline(&mut self.address).hint_text("另一台的 IPv4 或 IPv4:24871"));
                    ui.label(CODE_LABEL);
                    let offset=self.feedback.shake_offset(ui.ctx());
                    let (_,edited)=code_cells(ui,&mut self.code,offset,self.feedback.code_error);
                    if edited{self.feedback.code_error=false;}
                    if ui.add(desktop_style::primary("开始核验这台电脑")).clicked(){
                        // Only a malformed address or code shakes the cells (M8); other refusals keep still.
                        let input=(||->Result<(SocketAddr,Mode)>{
                            let address=parse_address(&self.address)?;
                            let mode=Mode::connect(address,std::mem::take(&mut self.code))?;
                            Ok((address,mode))
                        })();
                        let result=match input{
                            Err(error)=>{self.feedback.reject(now);Err(error)}
                            Ok((address,mode))=>Self::worker().and_then(|worker|self.start(app,ctx,mode,Some(address),&worker)),
                        };
                        if let Err(error)=result{self.message=format!("尚未开始：{error:#}");}
                    }
                    }
                    ui.add_space(12.0);
                    if ui.button("高级：独立核对公开连接信息…").clicked(){self.open=false;app.open_advanced_setup();}
                }
            }
            if let Some((code,address))=self.supervisor.displayed_code(){
                if is_cells_code(code){shown_code(ui,code);}else{ui.heading(code);}
                warning_line(ui,"请在另一台输入这个短码；不要发到公开群聊。");
                for interface in if_addrs::get_if_addrs().unwrap_or_default(){
                    if let IpAddr::V4(ip)=interface.ip(){if !ip.is_loopback()&&!ip.is_unspecified()&&(ip.is_private()||ip.is_link_local()){ui.label(format!("本机地址：{ip}:{}",address.port()));}}
                }
                if let Some(offer)=self.discovery_offer(){
                    let own=self.supervisor.local_certificate_snapshot().map(transport::fingerprint);
                    self.discovery.ui(ui,own,Some(offer),false);
                }
            }
            let current=self.supervisor.phase();
            // Dots add a shape for progress; the phase sentence below always stays.
            if app.local_fingerprint().is_some(){stage_dots(ui,self.feedback.stage,stage_of(current).is_none());}
            let glyph=self.feedback.glyph(ui.ctx(),self.supervisor.needs_poll());
            status_row(ui,glyph,phase_text(current));
            if current==Phase::Verified{
                ui.label("两台电脑都要点击一次。记住设备后，本机接收、键鼠和自动重连仍关闭。");
                if app.config.fingerprint.is_some(){ui.weak("若身份不同，这会替换原设备登记。");}
                if ui.add(desktop_style::primary("记住这台设备")).clicked(){if let Err(error)=self.approve(app){self.cancel();self.message=format!("未确认：{error:#}");}}
            }
            if app.pairing_pending(){ui.colored_label(egui::Color32::from_rgb(185,110,25),"本机保存了待回执登记，暂不能连接。断线或退出后请重新核验或撤销，不会恢复旧授权。");}
            if app.pairing_pending() && !self.needs_poll() && ui.button("撤销待确认登记").clicked(){
                self.cancel();let result=app.revoke();self.message=match result{Ok(())=>"待确认登记已撤销，本机身份保留。".into(),Err(error)=>format!("撤销未确认：{error:#}")};
            }
            if !self.message.is_empty(){ui.separator();ui.label(&self.message);}
            ui.add_space(8.0);
            ui.horizontal(|ui|{
                if self.supervisor.phase()==Phase::Completed&&ui.add(desktop_style::primary("下一步：摆好屏幕位置")).clicked(){
                    app.page=super::Page::Devices;app.device_tab=super::DeviceTab::Layout;self.open=false;
                }
                if self.supervisor.needs_poll()&&ui.button("停止核验").clicked(){self.cancel();}
                if ui.button("关闭").clicked(){self.open=false;self.cancel();}
            });
        });
        if !open {
            self.open = false;
            self.cancel();
        }
    }
    fn select_address(&mut self, address: SocketAddr) {
        self.address = address.to_string();
        peerglide_protocol::pairing_worker::clear_code(&mut self.code);
        self.message = "已填写地址，请输入这台电脑当前显示的配对码；发现结果不代表已核验。".into();
    }
    /// Window height for the current content; M10 glides between these values.
    fn target_height(&self, app: &App) -> f32 {
        if app.local_fingerprint().is_none() {
            return 520.0;
        }
        if self.supervisor.displayed_code().is_some() {
            return 600.0;
        }
        if self.join_entry {
            return 620.0;
        }
        540.0
    }
}

/// Result shown by the M9 stroke beside the phase sentence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Outcome {
    Success,
    Failure,
}

/// Glyph beside the phase sentence: nothing, the discrete spinner, or a check or cross with
/// its drawing progress.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Glyph {
    Blank,
    Spinner,
    Check(f32),
    Cross(f32),
}

/// State of one stage dot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Dot {
    Done,
    Current,
    Halted,
    Pending,
}

/// Presentation memory of the pairing window: the last observed phase, the last stage it
/// reached and the one-shot M8 and M9 animations. Nothing here feeds pairing decisions.
#[derive(Default)]
struct Feedback {
    phase: Option<Phase>,
    stage: usize,
    outcome: Option<(Outcome, motion::OneShot)>,
    shake: Option<motion::OneShot>,
    code_error: bool,
}

impl Feedback {
    /// Records the supervisor phase once per frame. A change into a final phase starts one
    /// check or cross drawing, and a rejected code on the entering side shakes once. The
    /// first observation only records, so reopening the window never replays old feedback.
    fn observe(&mut self, phase: Phase, join: bool, now: f64) {
        if let Some(stage) = stage_of(phase) {
            self.stage = stage;
        }
        let Some(previous) = self.phase.replace(phase) else {
            let finished = motion::OneShot::finished();
            self.outcome = outcome_of(phase).map(|outcome| (outcome, finished));
            return;
        };
        if previous == phase {
            return;
        }
        let shot = motion::OneShot::start(now, tokens::STROKE_DRAW_SECS);
        self.outcome = outcome_of(phase).map(|outcome| (outcome, shot));
        if join && phase == Phase::Failed(Failure::Authentication) {
            self.reject(now);
        }
    }

    /// A rejected code or address: shake the code cells once and mark them until edited.
    fn reject(&mut self, now: f64) {
        self.shake = Some(motion::OneShot::start(now, tokens::SHAKE_SECS));
        self.code_error = true;
    }

    /// Horizontal M8 offset. Asks for frames only while the shake runs.
    fn shake_offset(&self, ctx: &egui::Context) -> f32 {
        match &self.shake {
            Some(shake) => motion::shake_offset(shake.tick(ctx)),
            None => 0.0,
        }
    }

    /// The final check or cross wins over the spinner; the spinner shows while the pairing
    /// child is polled, which is when the existing 100 ms activity beat repaints.
    fn glyph(&self, ctx: &egui::Context, waiting: bool) -> Glyph {
        match &self.outcome {
            Some((Outcome::Success, shot)) => Glyph::Check(shot.tick(ctx)),
            Some((Outcome::Failure, shot)) => Glyph::Cross(shot.tick(ctx)),
            None if waiting => Glyph::Spinner,
            None => Glyph::Blank,
        }
    }
}

/// Stage index of a phase; `STAGES.len()` once complete. Stop and failure phases have none,
/// so the dots keep the last stage reached and mark it halted.
fn stage_of(phase: Phase) -> Option<usize> {
    match phase {
        Phase::Idle | Phase::Starting => Some(0),
        Phase::Listening => Some(1),
        Phase::Verifying => Some(2),
        Phase::Verified | Phase::WaitingPeer | Phase::SaveReady => Some(3),
        Phase::AwaitingPeerSaved | Phase::ReceiptReady | Phase::Finishing => Some(4),
        Phase::Completed => Some(STAGES.len()),
        Phase::Stopping | Phase::Stopped(_) | Phase::Failed(_) => None,
    }
}

/// Final result of a phase. A stop the user asked for is not a failure.
fn outcome_of(phase: Phase) -> Option<Outcome> {
    match phase {
        Phase::Completed => Some(Outcome::Success),
        Phase::Failed(_) => Some(Outcome::Failure),
        Phase::Stopped(reason) if reason != StopReason::Cancelled => Some(Outcome::Failure),
        _ => None,
    }
}

/// Sentence for each phase. Stop, failure, save and receipt states keep full sentences.
fn phase_text(phase: Phase) -> &'static str {
    match phase {
        Phase::Idle => "尚未开始",
        Phase::Starting => "正在准备核验",
        Phase::Listening => "等待另一台电脑",
        Phase::Verifying => "正在核验身份",
        Phase::Verified => "身份已核验，尚未记住设备",
        Phase::WaitingPeer => "等待两边分别确认记住设备",
        Phase::SaveReady => "正在检查并保存本机登记",
        Phase::AwaitingPeerSaved => "本机已记住，等待另一台保存回执",
        Phase::ReceiptReady => "正在保存完成回执",
        Phase::Finishing => "本机回执已保存，正在结束核验",
        Phase::Completed => "本机已记住，并已收到另一台的保存回执",
        Phase::Stopping => "正在停止核验",
        Phase::Stopped(_) => "核验已停止；已写入的记录会保留",
        Phase::Failed(_) => "核验未完成，请检查两端状态",
    }
}

fn dot_state(index: usize, stage: usize, halted: bool) -> Dot {
    if index < stage {
        Dot::Done
    } else if index > stage {
        Dot::Pending
    } else if halted {
        Dot::Halted
    } else {
        Dot::Current
    }
}

/// ASCII digit for an ASCII or full-width digit; anything else is dropped.
fn code_digit(c: char) -> Option<char> {
    if c.is_ascii_digit() {
        return Some(c);
    }
    let offset = u32::from(c).checked_sub(u32::from('０'))?;
    if offset < 10 {
        return char::from_digit(offset, 10);
    }
    None
}

/// Reserves the whole code up front, so typing never reallocates and strands digits.
fn reserve_code(code: &mut String) {
    if code.capacity() < CODE_LEN {
        code.reserve_exact(CODE_LEN.saturating_sub(code.len()));
    }
}

/// Appends the digits of `text` until the code is full. Returns true when it changed.
fn push_digits(code: &mut String, text: &str) -> bool {
    let before = code.len();
    for digit in text.chars().filter_map(code_digit) {
        if code.len() >= CODE_LEN {
            break;
        }
        code.push(digit);
    }
    code.len() != before
}

/// A paste holding a whole code replaces the buffer with its first eight digits; a shorter
/// paste appends like typing.
fn paste_code(code: &mut String, text: &str) -> bool {
    let digits = text.chars().filter_map(code_digit).count();
    if digits < CODE_LEN {
        return push_digits(code, text);
    }
    peerglide_protocol::pairing_worker::clear_code(code);
    push_digits(code, text);
    true
}

/// Applies typed text, IME commits, pastes and backspace presses to the code buffer.
fn apply_code_events(code: &mut String, events: &[egui::Event]) -> bool {
    use egui::{Event, ImeEvent, Key};
    reserve_code(code);
    let mut changed = false;
    for event in events {
        match event {
            Event::Text(text) | Event::Ime(ImeEvent::Commit(text)) => {
                changed |= push_digits(code, text);
            }
            Event::Paste(text) => changed |= paste_code(code, text),
            Event::Key {
                key: Key::Backspace,
                pressed: true,
                ..
            } => changed |= code.pop().is_some(),
            _ => {}
        }
    }
    changed
}

fn is_cells_code(code: &str) -> bool {
    code.len() == CODE_LEN && code.bytes().all(|byte| byte.is_ascii_digit())
}

fn cells_width() -> f32 {
    let cells = CODE_LEN as f32;
    cells * CELL_W + (cells - 1.0) * CELL_GAP + GROUP_GAP
}

fn cell_rect(row: egui::Rect, index: usize) -> egui::Rect {
    let group = if index >= HALF { GROUP_GAP } else { 0.0 };
    let x = row.left() + index as f32 * (CELL_W + CELL_GAP) + group;
    egui::Rect::from_min_size(egui::pos2(x, row.top()), egui::vec2(CELL_W, CELL_H))
}

/// Eight code cells in two groups of four. A click focuses them; digits, IME commits,
/// pastes and backspace then edit `code` directly, with no blinking caret. `offset` moves
/// only the painted row for the M8 shake, never the click target. Returns whether `code`
/// changed this frame.
fn code_cells(
    ui: &mut egui::Ui,
    code: &mut String,
    offset: f32,
    error: bool,
) -> (egui::Response, bool) {
    let size = egui::vec2(cells_width(), CELL_H);
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let id = egui::Id::unique(CODE_CELLS_ID);
    let response = ui.interact(rect, id, egui::Sense::click());
    if response.clicked() {
        response.request_focus();
    }
    let focused = response.has_focus();
    let changed = focused && ui.input(|input| apply_code_events(code, &input.events));
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, CODE_LABEL));
    if ui.is_rect_visible(rect) {
        let row = rect.translate(egui::vec2(offset, 0.0));
        let caret = focused.then_some(code.len());
        paint_cells(ui, row, code.as_bytes(), caret, error);
    }
    (response, changed)
}

/// The code this computer offers, in the same cells, read only.
fn shown_code(ui: &mut egui::Ui, code: &str) {
    let size = egui::vec2(cells_width(), CELL_H);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::hover());
    let label = format!("本次配对码 {code}");
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, &label));
    if ui.is_rect_visible(rect) {
        paint_cells(ui, rect, code.as_bytes(), None, false);
    }
}

/// Cells and their digits; `caret_cell` outlines the next cell to fill while focused.
fn paint_cells(
    ui: &egui::Ui,
    row: egui::Rect,
    digits: &[u8],
    caret_cell: Option<usize>,
    has_error: bool,
) {
    let dark = ui.visuals().dark_mode;
    let accent = tokens::accent_color(dark);
    let danger = desktop_style::status_colors(dark, Tone::Danger).1;
    let line = tokens::line_color(dark);
    let edge = if has_error { danger } else { line };
    let ink = ui.visuals().text_color();
    let fill = ui.visuals().extreme_bg_color;
    let radius = f32::from(tokens::RADIUS_CONTROL);
    let font = egui::FontId::monospace(20.0);
    let painter = ui.painter();
    for index in 0..CODE_LEN {
        let cell = cell_rect(row, index);
        painter.rect_filled(cell, radius, fill);
        let active = caret_cell.map(|caret| caret.min(CODE_LEN - 1)) == Some(index);
        let stroke = if active {
            egui::Stroke::new(2.0, accent)
        } else {
            egui::Stroke::new(1.0, edge)
        };
        painter.rect_stroke(cell, radius, stroke, egui::StrokeKind::Inside);
        let Some(&digit) = digits.get(index) else {
            continue;
        };
        let galley = painter.layout_no_wrap(char::from(digit).to_string(), font.clone(), ink);
        let at = cell.center() - galley.size() / 2.0;
        painter.galley(at, galley, ink);
    }
}

/// One of the two entry paths, painted as a large card.
struct EntryCard {
    icon: Icon,
    title: &'static str,
    detail: &'static str,
}

const SHOW_CARD: EntryCard = EntryCard {
    icon: Icon::Device,
    title: "我来显示配对码",
    detail: "本机生成短码",
};

const ENTER_CARD: EntryCard = EntryCard {
    icon: Icon::Keyboard,
    title: "我来输入配对码",
    detail: "输入另一台的短码",
};

/// The two entry paths side by side. Returns `Some(join)` when a card is clicked.
fn entry_cards(ui: &mut egui::Ui, join: bool) -> Option<bool> {
    let gap = tokens::SPACE_MD;
    let width = ((ui.available_width() - gap) / 2.0).max(CARD_MIN_W);
    let mut chosen = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        for (value, card) in [(false, SHOW_CARD), (true, ENTER_CARD)] {
            if entry_card(ui, &card, value == join, width).clicked() {
                chosen = Some(value);
            }
        }
    });
    chosen
}

/// Card with hover (M1), selection ring (M2) and a focus ring for keyboard users.
fn entry_card(ui: &mut egui::Ui, card: &EntryCard, on: bool, width: f32) -> egui::Response {
    let size = egui::vec2(width, CARD_H);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    let enabled = ui.is_enabled();
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, card.title));
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let ctx = ui.ctx();
    let hover = motion::hover_mix(ctx, response.id.with("hover"), response.hovered());
    let select = motion::select_mix(ctx, response.id.with("select"), on);
    let dark = ui.visuals().dark_mode;
    let accent = tokens::accent_color(dark);
    let soft = desktop_style::selection_colors(ui).0;
    let radius = f32::from(tokens::RADIUS_CARD);
    let inside = egui::StrokeKind::Inside;
    let painter = ui.painter();
    painter.rect_filled(rect, radius, tokens::surface_color(dark));
    painter.rect_filled(rect, radius, tokens::faded(soft, hover.max(select)));
    let edge = egui::Stroke::new(1.0, tokens::line_color(dark));
    painter.rect_stroke(rect, radius, edge, inside);
    let ring = egui::Stroke::new(2.0, tokens::faded(accent, select));
    painter.rect_stroke(rect, radius, ring, inside);
    if response.has_focus() {
        let focus = egui::Stroke::new(1.5, accent);
        painter.rect_stroke(rect.expand(3.0), radius + 3.0, focus, inside);
    }
    let text = ui.visuals().text_color();
    let ink = if on { accent } else { text };
    let icon_center = egui::pos2(rect.left() + 30.0, rect.center().y);
    let icon_rect = egui::Rect::from_center_size(icon_center, egui::Vec2::splat(28.0));
    card.icon.paint(painter, icon_rect, ink);
    let title_font = egui::FontId::proportional(15.0);
    let title = painter.layout_no_wrap(card.title.to_owned(), title_font, ink);
    let muted = tokens::muted_color(dark);
    let detail_font = egui::FontId::proportional(12.0);
    let detail = painter.layout_no_wrap(card.detail.to_owned(), detail_font, muted);
    let x = rect.left() + 56.0;
    let title_top = rect.center().y - title.size().y;
    painter.galley(egui::pos2(x, title_top), title, ink);
    painter.galley(egui::pos2(x, rect.center().y + 2.0), detail, muted);
    response
}

fn dot_center(rect: egui::Rect, index: usize) -> egui::Pos2 {
    let step = rect.width() / STAGES.len() as f32;
    let x = rect.left() + step * (index as f32 + 0.5);
    egui::pos2(x, rect.top() + DOT_TOP)
}

/// Stage dots: done stages filled, the current one ringed and a halted one in the warning
/// tone. Every dot keeps its name beneath, so color is never the only cue.
fn stage_dots(ui: &mut egui::Ui, stage: usize, halted: bool) {
    let size = egui::vec2(ui.available_width(), DOTS_H);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::hover());
    let current = STAGES.get(stage).copied().unwrap_or("完成");
    let stage_label = format!("配对阶段：{current}");
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::Role::ProgressIndicator, true, &stage_label)
    });
    if !ui.is_rect_visible(rect) {
        return;
    }
    let dark = ui.visuals().dark_mode;
    let accent = tokens::accent_color(dark);
    let line = tokens::line_color(dark);
    let muted = tokens::muted_color(dark);
    let text = ui.visuals().text_color();
    let warning = desktop_style::status_colors(dark, Tone::Warning).1;
    let fill = ui.visuals().window_fill;
    let painter = ui.painter();
    for index in 1..STAGES.len() {
        let color = if index <= stage { accent } else { line };
        let span = [dot_center(rect, index - 1), dot_center(rect, index)];
        painter.line_segment(span, egui::Stroke::new(1.5, color));
    }
    let font = egui::FontId::proportional(11.0);
    for (index, name) in STAGES.into_iter().enumerate() {
        let center = dot_center(rect, index);
        let state = dot_state(index, stage, halted);
        let color = match state {
            Dot::Done | Dot::Current => accent,
            Dot::Halted => warning,
            Dot::Pending => line,
        };
        painter.circle_filled(center, DOT_RADIUS, fill);
        if state == Dot::Pending {
            painter.circle_stroke(center, DOT_RADIUS, egui::Stroke::new(1.5, color));
        } else {
            painter.circle_filled(center, DOT_RADIUS, color);
        }
        if matches!(state, Dot::Current | Dot::Halted) {
            painter.circle_stroke(center, DOT_RADIUS + 3.0, egui::Stroke::new(1.5, color));
        }
        let ink = if state == Dot::Pending { muted } else { text };
        let galley = painter.layout_no_wrap(name.to_owned(), font.clone(), ink);
        let at = egui::pos2(center.x - galley.size().x / 2.0, rect.top() + LABEL_TOP);
        painter.galley(at, galley, ink);
    }
}

/// Spinner, check or cross beside the phase sentence, which always stays visible.
fn status_row(ui: &mut egui::Ui, glyph: Glyph, text: &str) {
    ui.horizontal(|ui| {
        let size = egui::Vec2::splat(tokens::ICON_MEDIUM);
        let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
        paint_glyph(ui, rect, glyph);
        ui.label(text);
    });
}

/// Paints a glyph. The spinner reads time only and never schedules a repaint (M11).
fn paint_glyph(ui: &egui::Ui, rect: egui::Rect, glyph: Glyph) {
    let dark = ui.visuals().dark_mode;
    let painter = ui.painter();
    match glyph {
        Glyph::Blank => {}
        Glyph::Spinner => {
            let now = ui.input(|input| input.time);
            let ink = tokens::accent_color(dark);
            icons::paint_spinner(painter, rect, ink, motion::spinner_head_at(now));
        }
        Glyph::Check(progress) => {
            let ink = desktop_style::status_colors(dark, Tone::Good).1;
            icons::paint_check_progress(painter, rect, ink, progress);
        }
        Glyph::Cross(progress) => {
            let ink = desktop_style::status_colors(dark, Tone::Danger).1;
            icons::paint_cross_progress(painter, rect, ink, progress);
        }
    }
}

/// A caution sentence led by the warning icon; the sentence carries the meaning.
fn warning_line(ui: &mut egui::Ui, text: &str) {
    ui.horizontal(|ui| {
        let size = egui::Vec2::splat(tokens::ICON_SMALL);
        let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
        let dark = ui.visuals().dark_mode;
        let ink = desktop_style::status_colors(dark, Tone::Warning).1;
        Icon::Warning.paint(ui.painter(), rect, ink);
        ui.label(text);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    fn key(key: egui::Key, pressed: bool) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }
    }
    fn text(text: &str) -> egui::Event {
        egui::Event::Text(text.into())
    }
    fn paste(text: &str) -> egui::Event {
        egui::Event::Paste(text.into())
    }
    // Every code below is a synthetic UI buffer, never authentication.
    #[test]
    fn code_events_keep_eight_digits_and_handle_backspace_and_paste_edges() {
        let mut code = String::new();
        assert!(apply_code_events(&mut code, &[text("1a2 3"), text("４")]));
        assert_eq!(code, "1234");
        assert!(!apply_code_events(&mut code, &[text("-x"), paste("abc")]));
        assert!(!apply_code_events(
            &mut code,
            &[key(egui::Key::Backspace, false)]
        ));
        assert_eq!(code, "1234");
        assert!(apply_code_events(
            &mut code,
            &[key(egui::Key::Backspace, true)]
        ));
        assert_eq!(code, "123");
        let commit = egui::Event::Ime(egui::ImeEvent::Commit("4".into()));
        assert!(apply_code_events(&mut code, &[commit, paste("56")]));
        assert_eq!(code, "12345");
        // A short paste appends and stops at eight digits.
        assert!(apply_code_events(&mut code, &[paste("6789")]));
        assert_eq!(code, "12345678");
        assert!(!apply_code_events(&mut code, &[text("9")]));
        assert_eq!(code, "12345678");
        // A whole code replaces the buffer; extra digits after the eighth are dropped.
        assert!(apply_code_events(&mut code, &[paste(" 8765-4321 ")]));
        assert_eq!(code, "87654321");
        assert!(apply_code_events(&mut code, &[paste("123456789")]));
        assert_eq!(code, "12345678");
        let clear = vec![key(egui::Key::Backspace, true); CODE_LEN + 2];
        assert!(apply_code_events(&mut code, &clear));
        assert!(code.is_empty());
        assert!(!apply_code_events(
            &mut code,
            &[key(egui::Key::Backspace, true)]
        ));
        assert!(code.capacity() >= CODE_LEN);
        assert!(is_cells_code("01234567") && !is_cells_code("0123456"));
        assert!(!is_cells_code("0123456a") && !is_cells_code("０１２３４５６７"));
    }
    fn cells_pass(
        ctx: &egui::Context,
        code: &mut String,
        cell_events: Vec<egui::Event>,
    ) -> egui::Rect {
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(480.0, 200.0));
        let input = egui::RawInput {
            screen_rect: Some(screen),
            events: cell_events,
            ..Default::default()
        };
        let mut rect = egui::Rect::NOTHING;
        let output = ctx.run_ui(input, |ui| rect = code_cells(ui, code, 0.0, false).0.rect);
        output.drop_without_applying_deltas();
        rect
    }
    #[test]
    fn code_cells_take_input_only_after_focus() {
        let ctx = crate::tests::ui_context();
        let mut code = String::new();
        let rect = cells_pass(&ctx, &mut code, vec![text("12")]);
        assert!(code.is_empty(), "unfocused cells must ignore typing");
        assert!((rect.width() - cells_width()).abs() < 0.5);
        let at = rect.center();
        for pressed in [true, false] {
            let click = vec![
                egui::Event::PointerMoved(at),
                egui::Event::PointerButton {
                    pos: at,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ];
            cells_pass(&ctx, &mut code, click);
        }
        cells_pass(&ctx, &mut code, vec![text("12")]);
        assert_eq!(code, "12");
        cells_pass(&ctx, &mut code, vec![key(egui::Key::Backspace, true)]);
        assert_eq!(code, "1");
        cells_pass(&ctx, &mut code, vec![paste("1234 5678")]);
        assert_eq!(code, "12345678");
        cells_pass(&ctx, &mut code, vec![text("9")]);
        assert_eq!(code, "12345678");
    }
    #[test]
    fn rejected_code_shakes_once_and_then_stops_repainting() {
        let rejected = Phase::Failed(Failure::Authentication);
        let mut feedback = Feedback::default();
        feedback.observe(Phase::Idle, true, 0.0);
        feedback.observe(Phase::Verifying, true, 0.1);
        assert!(feedback.shake.is_none() && !feedback.code_error);
        feedback.observe(rejected, true, 0.2);
        let first = feedback.shake;
        assert!(first.is_some() && feedback.code_error);
        assert!(matches!(feedback.outcome, Some((Outcome::Failure, _))));
        for step in 1..30 {
            feedback.observe(rejected, true, 0.2 + f64::from(step) * 0.05);
        }
        assert_eq!(
            feedback.shake, first,
            "a lasting failure must not shake again"
        );
        let ctx = egui::Context::default();
        let requests = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&requests);
        ctx.set_request_repaint_callback(move |_| {
            observed.fetch_add(1, Ordering::SeqCst);
        });
        let mut offsets = Vec::new();
        let mut time = 0.2;
        let mut settled = None;
        while time < 1.4 {
            let input = egui::RawInput {
                time: Some(time),
                ..Default::default()
            };
            let output = ctx.run_ui(input, |ui| offsets.push(feedback.shake_offset(ui.ctx())));
            output.drop_without_applying_deltas();
            if settled.is_none() && time > 0.8 {
                settled = Some(requests.load(Ordering::SeqCst));
            }
            time += 1.0 / 60.0;
        }
        // Sensitivity: the shake moved and asked for frames while it ran.
        assert!(offsets.iter().any(|offset| offset.abs() > 1.0));
        assert!(settled.is_some_and(|count| count > 0));
        assert_eq!(offsets.last().copied(), Some(0.0));
        assert_eq!(Some(requests.load(Ordering::SeqCst)), settled);
        // The offering side sees the same failure as a cross, never a shake.
        let mut offering = Feedback::default();
        offering.observe(Phase::Listening, false, 0.0);
        offering.observe(rejected, false, 0.1);
        assert!(offering.shake.is_none() && !offering.code_error);
        assert!(matches!(offering.outcome, Some((Outcome::Failure, _))));
    }
    #[test]
    fn stage_dots_and_result_marks_follow_the_supervisor_phase() {
        let expected = [
            (Phase::Idle, Some(0)),
            (Phase::Starting, Some(0)),
            (Phase::Listening, Some(1)),
            (Phase::Verifying, Some(2)),
            (Phase::Verified, Some(3)),
            (Phase::WaitingPeer, Some(3)),
            (Phase::SaveReady, Some(3)),
            (Phase::AwaitingPeerSaved, Some(4)),
            (Phase::ReceiptReady, Some(4)),
            (Phase::Finishing, Some(4)),
            (Phase::Completed, Some(STAGES.len())),
            (Phase::Stopping, None),
            (Phase::Stopped(StopReason::Expired), None),
            (Phase::Failed(Failure::Protocol), None),
        ];
        for (phase, stage) in expected {
            assert_eq!(stage_of(phase), stage, "{}", phase_text(phase));
            assert!(!phase_text(phase).is_empty());
        }
        assert_eq!(dot_state(0, 2, false), Dot::Done);
        assert_eq!(dot_state(2, 2, false), Dot::Current);
        assert_eq!(dot_state(2, 2, true), Dot::Halted);
        assert_eq!(dot_state(3, 2, true), Dot::Pending);
        for index in 0..STAGES.len() {
            assert_eq!(dot_state(index, STAGES.len(), false), Dot::Done);
        }
        // A failure keeps the stage it reached and marks it halted.
        let mut feedback = Feedback::default();
        feedback.observe(Phase::Starting, true, 0.0);
        feedback.observe(Phase::Verifying, true, 0.1);
        feedback.observe(Phase::Stopped(StopReason::Expired), true, 0.2);
        assert_eq!(feedback.stage, 2);
        assert!(matches!(feedback.outcome, Some((Outcome::Failure, _))));
        assert!(feedback.shake.is_none(), "only a rejected code shakes");
        feedback.observe(Phase::Idle, true, 0.3);
        assert!(feedback.outcome.is_none() && feedback.stage == 0);
        // A stop the user asked for is not drawn as a failure.
        feedback.observe(Phase::Stopped(StopReason::Cancelled), true, 0.4);
        assert!(feedback.outcome.is_none());
        // Success draws once; reopening on a finished phase shows it already drawn.
        feedback.observe(Phase::Finishing, true, 0.5);
        feedback.observe(Phase::Completed, true, 0.6);
        let Some((Outcome::Success, shot)) = feedback.outcome else {
            panic!("completion must draw a check");
        };
        assert!(shot.progress(0.6) < 1.0);
        assert_eq!(shot.progress(1.0), 1.0);
        assert_eq!(feedback.stage, STAGES.len());
        let mut reopened = Feedback::default();
        reopened.observe(Phase::Completed, false, 9.0);
        let Some((Outcome::Success, shot)) = reopened.outcome else {
            panic!("a finished phase keeps its check");
        };
        assert_eq!(shot.progress(9.0), 1.0);
    }
    fn text_center(shape: &egui::epaint::Shape, label: &str) -> Option<egui::Pos2> {
        match shape {
            egui::epaint::Shape::Text(text) if text.galley.job.text == label => {
                Some(text.visual_bounding_rect().center())
            }
            egui::epaint::Shape::Vec(shapes) => shapes.iter().find_map(|s| text_center(s, label)),
            _ => None,
        }
    }
    fn window_pass(
        app: &mut App,
        ctx: &egui::Context,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 800.0));
        let input = egui::RawInput {
            screen_rect: Some(screen),
            events,
            ..Default::default()
        };
        ctx.run_ui(input, |root| app.desktop(root))
    }
    #[test]
    fn entry_cards_switch_paths_without_starting_pairing_or_creating_identity() -> Result<()> {
        let fixture = crate::tests::Fixture::new()?;
        let mut app = App::load(fixture.path.clone());
        let before = app.config.clone();
        app.open_setup();
        let ctx = crate::tests::ui_context();
        app.theme(&ctx);
        for (label, join) in [("我来输入配对码", true), ("我来显示配对码", false)] {
            let mut at = None;
            for _ in 0..2 {
                let output = window_pass(&mut app, &ctx, vec![]);
                let shapes = &output.shapes;
                at = shapes.iter().find_map(|s| text_center(&s.shape, label));
                output.drop_without_applying_deltas();
            }
            let at = at.with_context(|| format!("{label} absent"))?;
            for pressed in [true, false] {
                let click = vec![
                    egui::Event::PointerMoved(at),
                    egui::Event::PointerButton {
                        pos: at,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ];
                window_pass(&mut app, &ctx, click).drop_without_applying_deltas();
            }
            assert_eq!(app.pairing_panel.join_entry, join, "{label}");
            assert!(app.pairing_panel.open && !app.pairing_panel.needs_poll());
            assert!(app.config == before && app.child.is_none() && !app.native_confirmation);
            assert!(!app.identity_dir().exists() && !app.dir.join("settings.json").exists());
        }
        drop(app);
        fixture.close()?;
        Ok(())
    }
    #[test]
    fn first_pairing_window_keeps_prepare_and_close_visible_in_both_entry_modes() -> Result<()> {
        fn text_rect(shape: &egui::epaint::Shape, label: &str) -> Option<egui::Rect> {
            match shape {
                egui::epaint::Shape::Text(text) if text.galley.job.text == label => {
                    Some(text.visual_bounding_rect())
                }
                egui::epaint::Shape::Vec(shapes) => shapes.iter().find_map(|s| text_rect(s, label)),
                _ => None,
            }
        }
        let fixture = crate::tests::Fixture::new()?;
        let mut app = App::load(fixture.path.clone());
        let before = app.config.clone();
        app.open_setup();
        let ctx = crate::tests::ui_context();
        app.theme(&ctx);
        for size in [egui::vec2(1280.0, 800.0), egui::vec2(920.0, 700.0)] {
            for join in [false, true] {
                app.pairing_panel.select_entry(join);
                for pass in 0..2 {
                    let output = ctx.run_ui(
                        egui::RawInput {
                            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                            ..Default::default()
                        },
                        |root| app.desktop(root),
                    );
                    let controls: Vec<_> = ["准备这台电脑", "关闭"]
                        .into_iter()
                        .map(|label| {
                            let bounds = output.shapes.iter().find_map(|s| {
                                text_rect(&s.shape, label).map(|rect| (s.clip_rect, rect))
                            });
                            (label, bounds)
                        })
                        .collect();
                    output.drop_without_applying_deltas();
                    if pass == 1 {
                        for (label, bounds) in controls {
                            let (clip, rect) = bounds.with_context(|| format!("{label} absent"))?;
                            assert!(clip.contains_rect(rect), "{label} clipped at {size:?}");
                        }
                    }
                }
                assert!(app.config == before && app.child.is_none());
                assert!(!app.pairing_panel.needs_poll() && !app.native_confirmation);
                assert!(!app.identity_dir().exists() && !app.dir.join("settings.json").exists());
            }
        }
        drop(app);
        fixture.close()?;
        Ok(())
    }
    #[test]
    fn changing_entry_clears_old_short_code_without_starting_pairing_or_network() {
        let mut panel = Panel::default();
        panel.code = "12345678".into(); // Only discarded input, never a credential accepted by a worker.
        panel.select_entry(true);
        assert!(panel.join_entry && panel.code.is_empty() && panel.origin.is_none());
        assert!(matches!(panel.supervisor.phase(), Phase::Idle) && !panel.needs_poll());
    }
    use std::time::{Duration, Instant};
    #[test]
    fn selecting_a_pairing_hint_only_fills_address_and_invalidates_old_code() -> Result<()> {
        let fixture = crate::tests::Fixture::new()?;
        let app = App::load(fixture.path.join("profile"));
        let before = app.config.clone();
        let mut panel = Panel::default();
        panel.open("测试设备", "");
        let generation = panel.supervisor.generation();
        panel.code = "12345678".into(); // Synthetic UI buffer, never authentication.
        panel.select_address("192.168.1.2:24871".parse()?);
        assert_eq!(panel.address, "192.168.1.2:24871");
        assert!(panel.code.is_empty() && panel.origin.is_none() && !panel.needs_poll());
        assert_eq!(panel.supervisor.generation(), generation);
        assert!(app.config == before && app.child.is_none() && app.confirmed_fingerprint.is_none());
        assert!(!app.identity_dir().exists() && !app.dir.join("settings.json").exists());
        Ok(())
    }
    #[test]
    fn address_errors_are_chinese_and_never_echo_untrusted_text() -> Result<()> {
        assert_eq!(
            parse_address("127.0.0.1")?,
            "127.0.0.1:24871".parse::<SocketAddr>()?
        );
        for text in [
            "",
            "private-invalid-marker",
            "0.0.0.0:0",
            "255.255.255.255:24871",
            "[::1]:24871",
        ] {
            let error = parse_address(text).unwrap_err().to_string();
            assert!(error
                .chars()
                .any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)));
            assert!(!error.contains("private-invalid-marker") && !error.contains("syntax"));
        }
        Ok(())
    }
    #[test]
    fn pending_disk_metadata_never_recreates_a_session_or_bypasses_future_record_protection(
    ) -> Result<()> {
        let fixture = crate::tests::Fixture::new()?;
        let dir = fixture.path.join("profile");
        std::fs::create_dir(&dir)?;
        let local = Identity::create(&dir.join("identity"))?;
        let peer = Identity::generate()?;
        std::fs::write(dir.join("trusted-peer.crt.der"), &peer.certificate)?;
        let config = Config {
            fingerprint: Some(peer.fingerprint()),
            pairing_receipt: Some(pairing_receipt::Record::pending(
                [17; 32],
                &local.fingerprint(),
                &peer.fingerprint(),
            )?),
            ..Config::default()
        };
        // Synthetic context exercises local metadata validation, never auth.
        let bytes = config_store::encode(&config)?;
        std::fs::write(dir.join("settings.json"), &bytes)?;
        let mut app = App::load(dir.clone());
        assert!(
            app.pairing_pending() && app.config_store.is_some() && !app.pairing_panel.needs_poll()
        );
        assert!(app.start(false).is_err() && app.start(true).is_err());
        app.native_confirmation = true;
        assert!(app.request_native().is_err());
        assert!(!app.config.native && app.child.is_none());
        let mut future: serde_json::Value = serde_json::from_slice(&bytes)?;
        future["data"]["pairing_receipt"]["receipt_version"] = serde_json::json!(99);
        let future = serde_json::to_vec(&future)?;
        std::fs::write(dir.join("settings.json"), &future)?;
        // Even an already-open window cannot reset over a later foreign write.
        assert!(app.reset_settings().is_err());
        assert!(std::fs::read(dir.join("settings.json"))? == future);
        drop(app);
        let mut app = App::load(dir.clone());
        assert!(app.config_store.is_none());
        assert!(app.reset_settings().is_err() && app.start(false).is_err());
        assert!(std::fs::read(dir.join("settings.json"))? == future);
        Ok(())
    }
    fn worker() -> Result<PathBuf> {
        let path = std::env::var_os("PEERGLIDE_PAIRING_TEST_WORKER")
            .context("此专项必须提供真实CLI路径")?;
        let path = PathBuf::from(path);
        anyhow::ensure!(path.is_file(), "专项CLI不存在");
        Ok(path)
    }
    fn pump(
        a: &mut App,
        b: &mut App,
        pa: &mut Panel,
        pb: &mut Panel,
        ctx: &egui::Context,
        ready: impl Fn(&App, &App, &Panel, &Panel) -> bool,
    ) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            pa.tick(a, ctx);
            pb.tick(b, ctx);
            if ready(a, b, pa, pb) {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        anyhow::bail!("GUI配对模型专项超时")
    }
    fn claim_order_case(binary: &Path, case: u8) -> Result<()> {
        let fixture = crate::tests::Fixture::new()?;
        let mut a = App::load(fixture.path.join("a"));
        let mut b = App::load(fixture.path.join("b"));
        Identity::create(&a.identity_dir())?;
        Identity::create(&b.identity_dir())?;
        let ctx = crate::tests::ui_context();
        let mut pa = Panel::default();
        let mut pb = Panel::default();
        pa.start(&a, &ctx, Mode::Offer("127.0.0.1:0".parse()?), None, binary)?;
        pump(&mut a, &mut b, &mut pa, &mut pb, &ctx, |_, _, pa, _| {
            pa.supervisor.displayed_code().is_some()
        })?;
        let (code, address) = pa.supervisor.displayed_code().unwrap();
        let code = code.to_owned();
        pb.start(
            &b,
            &ctx,
            Mode::connect(address, code)?,
            Some(address),
            binary,
        )?;
        pump(&mut a, &mut b, &mut pa, &mut pb, &ctx, |_, _, pa, pb| {
            pa.supervisor.observation().is_some() && pb.supervisor.observation().is_some()
        })?;
        pa.approve(&a)?;
        pb.approve(&b)?;
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            // Pause before the real GUI transaction callback to inject ordering.
            pa.supervisor.poll();
            pb.supervisor.poll();
            if pa.supervisor.phase() == Phase::SaveReady
                && pb.supervisor.phase() == Phase::SaveReady
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let claim = pa
            .supervisor
            .take_save_request()
            .context("missing actual save barrier")?;
        let origin = pa.origin.as_ref().unwrap().clone();
        let mut next = origin.next.clone();
        next.fingerprint = Some(transport::fingerprint(claim.certificate()));
        next.pairing_receipt = Some(pairing_receipt::Record::pending(
            claim.context(),
            &transport::fingerprint(claim.local_certificate()),
            next.fingerprint.as_deref().unwrap(),
        )?);
        let dir = a.dir.clone();
        let current = a.config.clone();
        if case == 0 {
            // An invalid key file must be noticed in the final locked guard.
            let key = a.identity_dir().join("node.key.der");
            let original = std::fs::read(&key)?;
            std::fs::write(&key, b"synthetic invalid key")?;
            let result = enrollment_store::commit_guarded(
                a.config_store.as_mut().unwrap(),
                &next,
                claim.certificate(),
                || {
                    origin.check(&dir, &current)?;
                    claim.claim()
                },
            );
            std::fs::write(&key, &original)?;
            assert!(
                result.is_err()
                    && !dir.join("settings.json").exists()
                    && !dir.join("enrollment-pending.json").exists()
            );
            pa.supervisor.cancel();
        } else if case == 1 {
            // Let actual peer/process EOF invalidate a held claim before use.
            pb.supervisor.stop_and_reap();
            let until = Instant::now() + Duration::from_secs(5);
            while pa.supervisor.needs_poll() && Instant::now() < until {
                pa.supervisor.poll();
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        let result = enrollment_store::commit_guarded(
            a.config_store.as_mut().unwrap(),
            &next,
            claim.certificate(),
            || {
                origin.check(&dir, &current)?;
                claim.claim()?;
                if case == 2 {
                    pa.supervisor.cancel();
                }
                Ok(())
            },
        );
        if case == 2 {
            result?;
            a.config = next;
            assert!(a.pairing_pending() && pa.supervisor.local_saved(claim.context()).is_err());
            let disk: Config =
                config_store::decode(&config_store::read(&dir.join("settings.json"))?.unwrap())?;
            assert!(disk
                .pairing_receipt
                .as_ref()
                .is_some_and(|r| r.is_pending()));
            assert!(!disk.native && !disk.allow && !disk.reconnect);
        } else {
            assert!(
                result.is_err()
                    && !dir.join("settings.json").exists()
                    && !dir.join("trusted-peer.crt.der").exists()
                    && !dir.join("enrollment-pending.json").exists()
            );
        }
        pa.supervisor.stop_and_reap();
        pb.supervisor.stop_and_reap();
        Ok(())
    }
    // Workspace unit tests compile this, then CI MUST run it after the matching
    // release CLI exists. It is not counted as passed while ignored.
    #[test]
    #[ignore = "requires exact-built CLI; mandatory post-release CI step"]
    fn real_gui_model_pairing_commits_pending_then_confirmed_without_native() -> Result<()> {
        let binary = worker()?;
        for case in 0..3 {
            claim_order_case(&binary, case)?;
        }
        let fixture = crate::tests::Fixture::new()?;
        let mut a = App::load(fixture.path.join("a"));
        let mut b = App::load(fixture.path.join("b"));
        let aid = Identity::create(&a.identity_dir())?;
        let bid = Identity::create(&b.identity_dir())?;
        let akey = std::fs::read(a.identity_dir().join("node.key.der"))?;
        let bkey = std::fs::read(b.identity_dir().join("node.key.der"))?;
        let ctx = crate::tests::ui_context();
        let mut pa = Panel::default();
        let mut pb = Panel::default();
        pa.name = "测试电脑B".into();
        pb.name = "测试电脑A".into();
        a.pairing_open = true;
        b.pairing_open = true;
        pa.start(&a, &ctx, Mode::Offer("127.0.0.1:0".parse()?), None, &binary)?;
        pump(&mut a, &mut b, &mut pa, &mut pb, &ctx, |_, _, pa, _| {
            pa.supervisor.displayed_code().is_some()
        })?;
        let (code, address) = pa.supervisor.displayed_code().unwrap();
        let code = code.to_owned();
        pb.start(
            &b,
            &ctx,
            Mode::connect(address, code)?,
            Some(address),
            &binary,
        )?;
        pump(&mut a, &mut b, &mut pa, &mut pb, &ctx, |_, _, pa, pb| {
            pa.supervisor.observation().is_some() && pb.supervisor.observation().is_some()
        })?;
        assert!(
            !a.dir.join("trusted-peer.crt.der").exists()
                && !b.dir.join("trusted-peer.crt.der").exists()
        );
        pa.approve(&a)?;
        pb.approve(&b)?;
        pump(&mut a, &mut b, &mut pa, &mut pb, &ctx, |a, b, _, _| {
            a.pairing_pending() || b.pairing_pending()
        })?;
        let pending = if a.pairing_pending() { &mut a } else { &mut b };
        let was_open = pending.pairing_open;
        pending.pairing_open = false;
        assert!(pending.start(false).is_err() && pending.start(true).is_err());
        assert!(pending
            .start_mode(super::super::SessionMode::Native)
            .is_err());
        pending.native_confirmation = true;
        assert!(pending.request_native().is_err());
        assert!(
            !pending.config.native
                && !pending.config.allow
                && !pending.config.reconnect
                && pending.child.is_none()
        );
        pending.pairing_open = was_open;
        pump(&mut a, &mut b, &mut pa, &mut pb, &ctx, |a, b, pa, pb| {
            a.config
                .pairing_receipt
                .as_ref()
                .is_some_and(|r| !r.is_pending())
                && b.config
                    .pairing_receipt
                    .as_ref()
                    .is_some_and(|r| !r.is_pending())
                && !pa.needs_poll()
                && !pb.needs_poll()
        })?;
        assert!(a.confirmed_fingerprint.is_none() && b.confirmed_fingerprint.is_none());
        assert!(a.config.fingerprint.as_deref() == Some(bid.fingerprint().as_str()));
        assert!(b.config.fingerprint.as_deref() == Some(aid.fingerprint().as_str()));
        for app in [&a, &b] {
            assert!(
                !app.config.native
                    && !app.config.allow
                    && !app.config.reconnect
                    && app.child.is_none()
            );
            app.validate_pairing_receipt()?;
        }
        assert!(std::fs::read(a.identity_dir().join("node.key.der"))? == akey);
        assert!(std::fs::read(b.identity_dir().join("node.key.der"))? == bkey);
        pa.open("再次核验", &a.config.address);
        assert!(pa.supervisor.phase() == Phase::Idle);
        pa.start(&a, &ctx, Mode::Offer("127.0.0.1:0".parse()?), None, &binary)?;
        pa.cancel();
        pa.supervisor.stop_and_reap();
        assert!(a
            .config
            .pairing_receipt
            .as_ref()
            .is_some_and(|r| !r.is_pending()));
        let aa = a.dir.clone();
        let bb = b.dir.clone();
        drop(pa);
        drop(pb);
        drop(a);
        drop(b);
        for dir in [aa, bb] {
            let app = App::load(dir);
            assert!(!app.pairing_pending());
            app.validate_pairing_receipt()?;
            assert!(
                app.config_store.is_some()
                    && !app.config.native
                    && !app.config.allow
                    && !app.config.reconnect
            );
        }
        Ok(())
    }
}
