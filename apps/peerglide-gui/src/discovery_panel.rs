//! LAN hints are separate from enrollment, session authority and native input.
use eframe::egui;
use peerglide_discovery::{Discovery, PendingStop};
use std::net::SocketAddr;
#[derive(Default)]
pub struct Panel {
    handle: Option<Discovery>,
    stopping: Option<PendingStop>,
    own: Option<String>,
    status: String,
}
impl Panel {
    pub fn needs_poll(&self) -> bool {
        self.handle.is_some() || self.stopping.is_some()
    }
    pub fn poll(&mut self, listener_running: bool) {
        if let Some(stop) = &self.stopping {
            if stop.finished() {
                self.stopping = None;
                self.handle = None;
                self.status = "设备发现已停止".into();
            }
            return;
        }
        if let Some(handle) = &mut self.handle {
            if !listener_running && handle.is_advertising() {
                if let Err(e) = handle.unpublish() {
                    self.status = format!("无法撤下设备广播：{e}");
                }
            }
            if let Err(e) = handle.poll(self.own.as_deref()) {
                handle.table.clear();
                self.status = format!("设备发现不可用：{e}");
            }
        }
    }
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        own: Option<String>,
        listener_running: bool,
        idle: bool,
        writable: bool,
    ) -> Option<SocketAddr> {
        let mut chosen = None;
        ui.separator();
        ui.strong("局域网设备发现 · 信息未经验证");
        ui.weak("设备发现不会登记证书或授予控制权限。请通过独立渠道核对公开证书。");
        if let Some(handle) = &mut self.handle {
            if self.stopping.is_none() {
                if ui.button("停止发现").clicked() {
                    match handle.request_stop() {
                        Ok(stop) => {
                            self.stopping = Some(stop);
                            self.status = "正在停止设备发现".into();
                        }
                        Err(e) => self.status = format!("停止失败，请重试：{e}"),
                    }
                }
                if self.stopping.is_none() {
                    if handle.is_advertising() {
                        if ui.button("隐藏本机监听服务").clicked() {
                            if let Err(e) = handle.unpublish() {
                                self.status = format!("撤下广播失败：{e}");
                            }
                        }
                    } else if ui
                        .add_enabled(
                            listener_running && own.is_some(),
                            egui::Button::new("在局域网广播本机监听服务的公开指纹"),
                        )
                        .clicked()
                    {
                        if let Some(fp) = &own {
                            match handle.advertise(fp, 24870) {
                                Ok(()) => {
                                    self.own = own.clone();
                                    self.status = "监听服务已广播，建立信任仍需核对证书".into();
                                }
                                Err(e) => self.status = format!("广播失败：{e}"),
                            }
                        }
                    }
                    for hint in handle.table.hints() {
                        ui.group(|ui| {
                            ui.label(&hint.name);
                            ui.weak("广播的指纹未经身份验证");
                            ui.monospace(&hint.fingerprint);
                            for address in &hint.addresses {
                                if ui
                                    .add_enabled(
                                        idle && writable,
                                        egui::Button::new(format!("带入核验 {address}")),
                                    )
                                    .clicked()
                                {
                                    chosen = Some(*address);
                                }
                            }
                        });
                    }
                }
            }
        } else {
            if ui
                .add_enabled(writable, egui::Button::new("查找局域网电脑"))
                .clicked()
            {
                match Discovery::start() {
                    Ok(handle) => {
                        self.handle = Some(handle);
                        self.own = own.clone();
                        self.status = "正在查找 IPv4 局域网设备信息".into();
                    }
                    Err(e) => self.status = format!("设备发现失败：{e}"),
                }
            }
        }
        if !self.status.is_empty() {
            ui.label(&self.status);
        }
        chosen
    }
}
