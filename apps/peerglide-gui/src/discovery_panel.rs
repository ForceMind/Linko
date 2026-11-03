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
    pub fn poll(&mut self, listener_running: bool) {
        if let Some(stop) = &self.stopping {
            if stop.finished() {
                self.stopping = None;
                self.handle = None;
                self.status = "Discovery stopped".into();
            }
            return;
        }
        if let Some(handle) = &mut self.handle {
            if !listener_running && handle.is_advertising() {
                if let Err(e) = handle.unpublish() {
                    self.status = format!("Could not withdraw advertisement: {e}");
                }
            }
            if let Err(e) = handle.poll(self.own.as_deref()) {
                handle.table.clear();
                self.status = format!("Discovery unavailable: {e}");
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
        ui.strong("LAN discovery · untrusted hints");
        ui.weak("Discovery never enrolls a certificate or grants control. Independently verify the public certificate.");
        if let Some(handle) = &mut self.handle {
            if self.stopping.is_none() {
                if ui.button("Stop discovery").clicked() {
                    match handle.request_stop() {
                        Ok(stop) => {
                            self.stopping = Some(stop);
                            self.status = "Stopping discovery".into();
                        }
                        Err(e) => self.status = format!("Stop failed; retry: {e}"),
                    }
                }
                if self.stopping.is_none() {
                    if handle.is_advertising() {
                        if ui.button("Hide this listener").clicked() {
                            if let Err(e) = handle.unpublish() {
                                self.status = format!("Withdraw failed: {e}");
                            }
                        }
                    } else if ui
                        .add_enabled(
                            listener_running && own.is_some(),
                            egui::Button::new(
                                "Advertise this listener's public fingerprint on LAN",
                            ),
                        )
                        .clicked()
                    {
                        if let Some(fp) = &own {
                            match handle.advertise(fp, 24870) {
                                Ok(()) => {
                                    self.own = own.clone();
                                    self.status="Listener advertised; trust still requires certificate verification".into();
                                }
                                Err(e) => self.status = format!("Advertise failed: {e}"),
                            }
                        }
                    }
                    for hint in handle.table.hints() {
                        ui.group(|ui| {
                            ui.label(&hint.name);
                            ui.weak("Unauthenticated advertised fingerprint");
                            ui.monospace(&hint.fingerprint);
                            for address in &hint.addresses {
                                if ui
                                    .add_enabled(
                                        idle && writable,
                                        egui::Button::new(format!("Use address {address}")),
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
                .add_enabled(writable, egui::Button::new("Find LAN computers"))
                .clicked()
            {
                match Discovery::start() {
                    Ok(handle) => {
                        self.handle = Some(handle);
                        self.own = own.clone();
                        self.status = "Browsing IPv4 LAN hints".into();
                    }
                    Err(e) => self.status = format!("Discovery failed: {e}"),
                }
            }
        }
        if !self.status.is_empty() {
            ui.label(&self.status);
        }
        chosen
    }
}
