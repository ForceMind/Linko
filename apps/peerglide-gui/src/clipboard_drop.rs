//! One copy drag of explicitly imported clipboard content, on the local UI owner.
use super::{data_access::Pin, egui, native_drop_panel, App, Page};
use anyhow::{ensure, Result};
use peerglide_transport::clipboard_worker::DragPayload;
use std::time::{Duration, Instant};

const OFFER_DURATION: Duration = Duration::from_secs(60);
const CANCELLED: &str = "已撤下本次复制拖放。";
const EXPIRED: &str = "复制拖放已过期，请检查目标应用。";
const COPIED: &str = "系统报告复制拖放已完成。";
const UNKNOWN: &str = "拖放结果未确认，请检查目标应用。";

enum State {
    Ready(DragPayload),
    #[cfg(target_os = "macos")]
    Dragging(peerglide_platform::macos_native_drop::Drag),
}

pub(super) struct Offer {
    pin: Pin,
    expires: Instant,
    state: State,
}

pub(super) fn eligible(app: &App, ctx: &egui::Context) -> bool {
    app.page == Page::Clipboard
        && native_drop_panel::eligible(app)
        && ctx.input(|input| {
            !input.viewport().close_requested() && !input.viewport().minimized.unwrap_or(false)
        })
}

pub(super) fn focused(ctx: &egui::Context) -> bool {
    ctx.input(|input| {
        input.focused
            && !input
                .events
                .iter()
                .any(|event| matches!(event, egui::Event::WindowFocused(false)))
    })
}

impl Offer {
    pub(super) fn new(pin: Pin, payload: DragPayload) -> Self {
        Self {
            pin,
            expires: Instant::now() + OFFER_DURATION,
            state: State::Ready(payload),
        }
    }

    // Returning a terminal notice tells the panel to discard this one-shot offer.
    pub(super) fn poll(&self, app: &App, ctx: &egui::Context) -> Option<&'static str> {
        #[cfg(target_os = "macos")]
        if let State::Dragging(drag) = &self.state {
            use peerglide_platform::macos_native_drop::Outcome;
            if let Some(outcome) = drag.outcome() {
                return Some(match outcome {
                    Outcome::Copied => COPIED,
                    Outcome::Cancelled => CANCELLED,
                    Outcome::Unknown => UNKNOWN,
                });
            }
        }
        if !self.pin.matches(app)
            || !eligible(app, ctx)
            || (matches!(self.state, State::Ready(_)) && !focused(ctx))
        {
            return Some(CANCELLED);
        }
        (Instant::now() >= self.expires).then_some(EXPIRED)
    }

    pub(super) fn ui(
        &mut self,
        app: &App,
        ui: &mut egui::Ui,
    ) -> Result<Option<&'static str>> {
        if let Some(notice) = self.poll(app, ui.ctx()) {
            return Ok(Some(notice));
        }
        ui.label("请在一分钟内将内容拖入支持该格式的本机应用。");
        #[cfg(target_os = "macos")]
        if matches!(self.state, State::Dragging(_)) {
            ui.label("正在复制拖放。");
            return Ok(None);
        }
        if ui.button("取消准备").clicked() {
            return Ok(Some(CANCELLED));
        }
        let target = ui.add(egui::Button::new("按住并拖出复制").sense(egui::Sense::drag()));
        let pressed = ui.input(|input| input.pointer.button_pressed(egui::PointerButton::Primary));
        if target.hovered() && pressed {
            return self.start(app);
        }
        Ok(None)
    }

    fn start(&mut self, app: &App) -> Result<Option<&'static str>> {
        let (pin, _, _) = Pin::load(app)?;
        let remaining = self.expires.saturating_duration_since(Instant::now());
        ensure!(pin == self.pin && !remaining.is_zero(), "复制拖放已失效");
        let payload = match &self.state {
            State::Ready(payload) => payload,
            #[cfg(target_os = "macos")]
            State::Dragging(_) => return Ok(None),
        };
        #[cfg(all(windows, target_env = "msvc"))]
        {
            use peerglide_platform::windows_native_drop::{drag_png, drag_text, Outcome};
            let outcome = match payload {
                DragPayload::Text(text) => drag_text(text, remaining)?,
                DragPayload::Png(png) => drag_png(png, remaining)?,
            };
            Ok(Some(match outcome {
                Outcome::Copied => COPIED,
                Outcome::Cancelled => CANCELLED,
                Outcome::Unknown => UNKNOWN,
            }))
        }
        #[cfg(target_os = "macos")]
        {
            use peerglide_platform::macos_native_drop::Drag;
            let drag = match payload {
                DragPayload::Text(text) => Drag::start_text(text, remaining)?,
                DragPayload::Png(png) => Drag::start_png(png, remaining)?,
            };
            self.state = State::Dragging(drag);
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ready_offer_is_bound_to_pairing_visibility_focus_and_expiry() -> Result<()> {
        use super::super::{tests::Fixture, Identity, SessionMode};

        let fixture = Fixture::new()?;
        let mut app = App::load(fixture.path.clone());
        Identity::create(&app.identity_dir())?;
        let peer = Identity::generate()?;
        std::fs::write(app.dir.join("trusted-peer.crt.der"), &peer.certificate)?;
        app.config.fingerprint = Some(peer.fingerprint());
        app.page = Page::Clipboard;
        let (pin, _, _) = Pin::load(&app)?;
        let mut offer = Offer::new(pin, DragPayload::Text("屏跃".into()));
        let ctx = egui::Context::default();
        let frame = |focused| egui::RawInput {
            focused,
            ..egui::RawInput::default()
        };
        let _ = ctx.run_ui(frame(false), |ui| {
            assert_eq!(offer.poll(&app, ui.ctx()), Some(CANCELLED));
        });
        let _ = ctx.run_ui(frame(true), |ui| {
            let ctx = ui.ctx();
            assert_eq!(offer.poll(&app, ctx), None);
            app.window_hidden = true;
            assert_eq!(offer.poll(&app, ctx), Some(CANCELLED));
            app.window_hidden = false;
            app.pending_start = Some(SessionMode::Native);
            assert_eq!(offer.poll(&app, ctx), Some(CANCELLED));
            app.pending_start = None;
            offer.expires = Instant::now();
            assert_eq!(offer.poll(&app, ctx), Some(EXPIRED));
            app.config.fingerprint = None;
            assert_eq!(offer.poll(&app, ctx), Some(CANCELLED));
        });
        drop(app);
        fixture.close()
    }
}
