//! Opt-in pairing addresses only. Never consumes a code or grants trust.
use eframe::egui;
use peerglide_discovery::{Discovery, PendingStop};
use std::net::SocketAddr;

use crate::desktop_style::tokens;
use crate::icons;
use crate::motion::{self, OneShot};

/// Upper bound on animated rows; the table lists at most 64 hints of 4 addresses each.
const MAX_ROWS: usize = 320;

/// Presentation of one listed address (M12): when it appeared and, once gone, when it
/// started to leave. Rows never authorize anything; they only fade buttons in and out.
#[derive(Clone, Copy, Debug)]
struct HintRow {
    address: SocketAddr,
    enter: OneShot,
    leave: Option<OneShot>,
}

impl HintRow {
    /// Opacity: rises while entering and falls while leaving.
    #[cfg(test)]
    fn alpha(&self, now: f64) -> f32 {
        match &self.leave {
            Some(leave) => 1.0 - motion::cubic_out(leave.progress(now)),
            None => motion::cubic_out(self.enter.progress(now)),
        }
    }

    /// Opacity at egui time. Asks for frames only while the row is still moving.
    fn tick(&self, ctx: &egui::Context) -> f32 {
        let shot = self.leave.unwrap_or(self.enter);
        let opacity = motion::cubic_out(shot.tick(ctx));
        if self.leave.is_some() {
            1.0 - opacity
        } else {
            opacity
        }
    }
}

/// Matches rows to the addresses listed now. New addresses enter, missing ones start to
/// leave, returning ones enter again, and rows that finished leaving are dropped.
fn sync_rows(rows: &mut Vec<HintRow>, current: &[SocketAddr], now: f64, secs: f32) {
    rows.retain(|row| {
        if secs == 0.0 {
            current.contains(&row.address)
        } else {
            row.leave.is_none_or(|leave| leave.is_active(now))
        }
    });
    for row in rows.iter_mut() {
        let listed = current.contains(&row.address);
        if listed && row.leave.is_some() {
            row.leave = None;
            row.enter = OneShot::start(now, secs);
        } else if !listed && row.leave.is_none() {
            row.leave = Some(OneShot::start(now, secs));
        }
    }
    for address in current {
        let known = rows.iter().any(|row| row.address == *address);
        if !known && rows.len() < MAX_ROWS {
            rows.push(HintRow {
                address: *address,
                enter: OneShot::start(now, secs),
                leave: None,
            });
        }
    }
}

/// Addresses the table lists now, in table order.
fn listed(table: &peerglide_discovery::HintTable) -> Vec<SocketAddr> {
    let mut addresses = Vec::new();
    for hint in table.hints() {
        addresses.extend_from_slice(&hint.addresses);
    }
    addresses
}

/// One address button. A leaving row fades out and no longer accepts clicks.
fn hint_row(ui: &mut egui::Ui, row: &HintRow) -> bool {
    let alpha = row.tick(ui.ctx());
    let leaving = row.leave.is_some();
    let label = format!("选择配对地址 {}", row.address);
    let inner = ui.scope(|ui| {
        ui.multiply_opacity(alpha);
        ui.add_enabled(!leaving, egui::Button::new(label))
    });
    inner.inner.clicked()
}

/// Discrete spinner while a search runs; it steps only on the existing discovery poll beat.
fn search_spinner(ui: &mut egui::Ui) {
    let size = egui::Vec2::splat(tokens::ICON_SMALL);
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let now = ui.input(|input| input.time);
    let ink = tokens::muted_color(ui.visuals().dark_mode);
    icons::paint_spinner(ui.painter(), rect, ink, motion::spinner_head(ui.ctx(), now));
}

/// Only invalidates an old publication; these counters never authorize a peer.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Offer {
    pub generation: u64,
    pub attempts_remaining: u8,
    pub port: u16,
    pub ipv6: bool,
}

#[derive(Default)]
pub(super) struct Panel {
    handle: Option<Discovery>,
    stopping: Option<PendingStop>,
    own: Option<String>,
    published: Option<Offer>,
    stop_requested: bool,
    status: String,
    rows: Vec<HintRow>,
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
        self.rows.clear();
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
                    let handle = self.handle.as_mut().unwrap();
                    if offer.ipv6 {
                        handle.advertise_pairing_ipv6(fp, offer.port)?;
                    } else {
                        handle.advertise(fp, offer.port)?;
                    }
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
                ui.horizontal(|ui| {
                    search_spinner(ui);
                    ui.weak("发现结果未经核验，选择只填写地址，不会发送配对码");
                });
                if let Some(handle) = &self.handle {
                    let current = listed(&handle.table);
                    let now = ui.input(|input| input.time);
                    let secs = motion::duration(ui.ctx(), tokens::LIST_PRESENCE_SECS);
                    sync_rows(&mut self.rows, &current, now, secs);
                    if current.is_empty() {
                        ui.label("还没找到？可直接填写另一台显示的 IP 地址");
                    }
                    let rows = &self.rows;
                    egui::ScrollArea::vertical()
                        .max_height(140.0)
                        .show(ui, |ui| {
                            for row in rows {
                                if hint_row(ui, row) {
                                    chosen = Some(row.address);
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
    #[test]
    fn listed_addresses_enter_and_leave_once_and_rows_stay_bounded() {
        let a = SocketAddr::from(([192, 168, 1, 2], 24871));
        let b = SocketAddr::from(([192, 168, 1, 3], 24871));
        let secs = tokens::LIST_PRESENCE_SECS;
        let mut rows = Vec::new();
        sync_rows(&mut rows, &[a, b], 0.0, secs);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].alpha(0.0), 0.0);
        assert!(rows[0].alpha(0.09) > 0.0 && rows[0].alpha(0.09) < 1.0);
        assert_eq!(rows[0].alpha(1.0), 1.0);
        let entered = rows[0].enter;
        // A repeated listing changes nothing, so nothing replays.
        sync_rows(&mut rows, &[a, b], 1.0, secs);
        assert_eq!(rows[0].enter, entered);
        assert!(rows.iter().all(|row| row.leave.is_none()));
        // b leaves: it fades from fully visible and is dropped once the fade ends.
        sync_rows(&mut rows, &[a], 2.0, secs);
        assert!(rows[1].leave.is_some());
        assert_eq!(rows[1].alpha(2.0), 1.0);
        assert!(rows[1].alpha(2.09) > 0.0 && rows[1].alpha(2.09) < 1.0);
        assert_eq!(rows[1].alpha(2.5), 0.0);
        sync_rows(&mut rows, &[a], 3.0, secs);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].address, a);
        // An address returning while it leaves enters again.
        sync_rows(&mut rows, &[], 4.0, secs);
        sync_rows(&mut rows, &[a], 4.05, secs);
        assert!(rows[0].leave.is_none());
        assert_eq!(rows[0].alpha(4.05), 0.0);
        let mut many = Vec::new();
        for port in 1..=400 {
            many.push(SocketAddr::from(([10, 0, 0, 1], port)));
        }
        let mut bounded = Vec::new();
        sync_rows(&mut bounded, &many, 0.0, secs);
        assert_eq!(bounded.len(), MAX_ROWS);
        let mut panel = Panel {
            rows: bounded,
            ..Panel::default()
        };
        panel.stop();
        assert!(panel.rows.is_empty() && !panel.needs_poll());
    }
}
