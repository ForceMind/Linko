//! Opt-in pairing addresses only. Never consumes a code or grants trust.
use eframe::egui;
use peerglide_discovery::{Discovery, PendingStop};
use std::net::SocketAddr;

/// Only invalidates an old publication; these counters never authorize a peer.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Offer {
    pub generation: u64,
    pub attempts_remaining: u8,
    pub port: u16,
}

#[derive(Default)]
pub(super) struct Panel {
    handle: Option<Discovery>,
    stopping: Option<PendingStop>,
    own: Option<String>,
    published: Option<Offer>,
    stop_requested: bool,
    status: String,
}
impl Panel {
    pub(super) fn needs_poll(&self) -> bool {
        self.handle.is_some() || self.stopping.is_some()
    }
    fn start(&mut self, own: Option<String>) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.stop_requested && self.stopping.is_none(),
            "请等待局域网查找停止"
        );
        if self.handle.is_none() {
            self.handle = Some(Discovery::start_pairing()?);
        }
        self.own = own;
        Ok(())
    }
    pub(super) fn stop(&mut self) {
        self.stop_requested = self.handle.is_some();
        if self.stopping.is_some() {
            return;
        }
        if let Some(handle) = self.handle.as_mut() {
            handle.table.clear();
            match handle.request_stop() {
                Ok(stop) => {
                    self.stopping = Some(stop);
                    self.status = "正在停止局域网查找与广播".into();
                }
                Err(_) => self.status = "停止未确认，正在重试；请保留窗口".into(),
            }
        }
    }
    pub(super) fn poll(&mut self, listening: Option<Offer>) {
        if let Some(stop) = &self.stopping {
            if stop.finished() {
                self.handle = None;
                self.stopping = None;
                self.published = None;
                self.stop_requested = false;
                self.status = "局域网查找与广播已停止".into();
            }
            return;
        }
        if self.stop_requested {
            self.stop();
            return;
        }
        if self.published.is_some() && self.published != listening {
            self.stop();
            return;
        }
        let mut failed = false;
        if let Some(handle) = &mut self.handle {
            if handle.poll(self.own.as_deref()).is_err() {
                failed = true;
            }
        }
        if failed {
            self.stop();
            self.status = "局域网发现不可用，正在停止；可手动填写另一台地址".into();
        }
    }
    pub(super) fn ui(
        &mut self,
        ui: &mut egui::Ui,
        own: Option<String>,
        listening: Option<Offer>,
        can_browse: bool,
    ) -> Option<SocketAddr> {
        let mut chosen = None;
        if self.stop_requested || self.stopping.is_some() {
            ui.weak("正在停止局域网查找与广播，请稍候");
            return None;
        }
        if let Some(offer) = listening {
            if self.published.is_some() {
                ui.label("已请求在局域网显示本次配对；不会广播配对码");
                if ui.button("隐藏本次配对").clicked() {
                    self.stop();
                }
            } else if ui.button("让局域网内的电脑找到本次配对").clicked() {
                let result = (|| -> anyhow::Result<()> {
                    let fp = own
                        .as_ref()
                        .ok_or_else(|| anyhow::anyhow!("本机身份不可用"))?;
                    self.start(own.clone())?;
                    self.handle.as_mut().unwrap().advertise(fp, offer.port)?;
                    self.published = Some(offer);
                    Ok(())
                })();
                self.status = if result.is_ok() {
                    "仅广播地址、公开指纹和版本；仍需短码核验及双方确认".into()
                } else {
                    self.stop();
                    "本次配对未能广播，请在另一台手动填写地址".into()
                };
            }
        } else if can_browse {
            if self.handle.is_none() {
                if ui.button("查找正在配对的电脑").clicked() {
                    self.status = if self.start(own).is_ok() {
                        "正在查找；请在另一台显示配对码，并允许局域网查找".into()
                    } else {
                        "无法开始局域网查找，请手动填写另一台地址".into()
                    };
                }
            } else {
                ui.weak("发现结果未经核验，选择只填写地址，不会发送配对码");
                if let Some(handle) = &self.handle {
                    if handle.table.hints().next().is_none() {
                        ui.label("还没找到？可直接填写另一台显示的 IPv4 地址");
                    }
                    egui::ScrollArea::vertical()
                        .max_height(140.0)
                        .show(ui, |ui| {
                            for hint in handle.table.hints() {
                                for address in &hint.addresses {
                                    if ui.button(format!("选择配对地址 {address}")).clicked()
                                    {
                                        chosen = Some(*address);
                                    }
                                }
                            }
                        });
                }
            }
        }
        if self.handle.is_some() && ui.button("停止局域网查找与广播").clicked() {
            self.stop();
        }
        if !self.status.is_empty() {
            ui.weak(&self.status);
        }
        chosen
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn default_poll_and_stop_never_create_a_network_owner() {
        let mut panel = Panel::default();
        panel.poll(None);
        panel.stop();
        panel.poll(None);
        assert!(!panel.needs_poll());
        assert!(panel.handle.is_none() && panel.published.is_none());
        assert!(!panel.stop_requested && panel.stopping.is_none());
    }
}
