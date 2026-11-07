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
        let requested_height: f32 = if app.local_fingerprint().is_none() {
            500.0
        } else if self.join_entry {
            560.0
        } else {
            520.0
        };
        let height = requested_height.min((ctx.viewport_rect().height() - 48.0).max(1.0));
        egui::Window::new("添加电脑").open(&mut open).collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER,egui::Vec2::ZERO).fixed_size(egui::vec2(510.0,height)).vscroll(true).show(ctx,|ui|{
            ui.label(super::desktop_style::semibold("把另一台电脑加入工作区",21.0));
            ui.label("两台电脑连到同一 Wi-Fi 或局域网。任选一台显示配对码，另一台输入；双方确认后会记住彼此。键鼠与数据权限仍单独选择。");
            ui.add_space(12.0);
            if !self.supervisor.needs_poll() && !matches!(self.supervisor.phase(),Phase::Completed) {
                ui.horizontal(|ui| {
                    if ui.selectable_label(!self.join_entry,"我来显示配对码").clicked(){self.select_entry(false);}
                    if ui.selectable_label(self.join_entry,"我来输入配对码").clicked(){self.select_entry(true);}
                });
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
                    if ui.add(super::desktop_style::primary("显示我的配对码")).clicked(){
                        let result=Self::worker().and_then(|worker|self.start(app,ctx,Mode::Offer(SocketAddr::from((Ipv4Addr::UNSPECIFIED,24871))),None,&worker));
                        if let Err(error)=result{self.message=format!("尚未开始：{error:#}");}
                    }
                    }else{
                    ui.strong("找到另一台电脑，输入它显示的短码");
                    if let Some(address)=self.discovery.ui(ui,app.local_fingerprint(),None,true){self.select_address(address);}
                    ui.add(egui::TextEdit::singleline(&mut self.address).hint_text("另一台的 IPv4 或 IPv4:24871"));
                    ui.add(egui::TextEdit::singleline(&mut self.code).hint_text("八位数字配对码"));
                    if ui.add(super::desktop_style::primary("开始核验这台电脑")).clicked(){
                        let result=(||->Result<()>{
                            let address=parse_address(&self.address)?;
                            let mode=Mode::connect(address,std::mem::take(&mut self.code))?;
                            self.start(app,ctx,mode,Some(address),&Self::worker()?)
                        })();
                        if let Err(error)=result{self.message=format!("尚未开始：{error:#}");}
                    }
                    }
                    ui.add_space(12.0);
                    if ui.button("高级：独立核对公开连接信息…").clicked(){self.open=false;app.open_advanced_setup();}
                }
            }
            if let Some((code,address))=self.supervisor.displayed_code(){
                ui.heading(code);
                ui.label("请在另一台输入这个短码；不要发到公开群聊。");
                for interface in if_addrs::get_if_addrs().unwrap_or_default(){
                    if let IpAddr::V4(ip)=interface.ip(){if !ip.is_loopback()&&!ip.is_unspecified()&&(ip.is_private()||ip.is_link_local()){ui.label(format!("本机地址：{ip}:{}",address.port()));}}
                }
                if let Some(offer)=self.discovery_offer(){
                    let own=self.supervisor.local_certificate_snapshot().map(transport::fingerprint);
                    self.discovery.ui(ui,own,Some(offer),false);
                }
            }
            ui.label(match self.supervisor.phase(){
                Phase::Idle=>"尚未开始",Phase::Starting=>"正在准备核验",Phase::Listening=>"等待另一台电脑",
                Phase::Verifying=>"正在核验身份",Phase::Verified=>"身份已核验，尚未记住设备",
                Phase::WaitingPeer=>"等待两边分别确认记住设备",Phase::SaveReady=>"正在检查并保存本机登记",
                Phase::AwaitingPeerSaved=>"本机已记住，等待对端保存回执",Phase::ReceiptReady=>"正在保存完成回执",
                Phase::Finishing=>"本机回执已保存，正在结束核验",Phase::Completed=>"本机已记住，并已收到对端保存回执",
                Phase::Stopping=>"正在停止核验",Phase::Stopped(_)=>"核验已停止；已写入的记录会保留",Phase::Failed(_)=>"核验未完成，请检查两端状态",
            });
            if self.supervisor.phase()==Phase::Verified{
                ui.label("两台电脑都要点击一次。记住设备后，本机接收、键鼠和自动重连仍关闭。");
                if app.config.fingerprint.is_some(){ui.weak("若身份不同，这会替换原设备登记。");}
                if ui.button("记住这台设备").clicked(){if let Err(error)=self.approve(app){self.cancel();self.message=format!("未确认：{error:#}");}}
            }
            if app.pairing_pending(){ui.colored_label(egui::Color32::from_rgb(185,110,25),"本机保存了待回执登记，暂不能连接。断线或退出后请重新核验或撤销，不会恢复旧授权。");}
            if app.pairing_pending() && !self.needs_poll() && ui.button("撤销待确认登记").clicked(){
                self.cancel();let result=app.revoke();self.message=match result{Ok(())=>"待确认登记已撤销，本机身份保留。".into(),Err(error)=>format!("撤销未确认：{error:#}")};
            }
            if !self.message.is_empty(){ui.separator();ui.label(&self.message);}
            if self.supervisor.phase()==Phase::Completed&&ui.button("下一步：摆好屏幕位置").clicked(){
                app.page=super::Page::Devices;app.device_tab=super::DeviceTab::Layout;self.open=false;
            }
            if self.supervisor.needs_poll()&&ui.button("停止核验").clicked(){self.cancel();}
            if ui.button("关闭").clicked(){self.open=false;self.cancel();}
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
}

#[cfg(test)]
mod tests {
    use super::*;
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
