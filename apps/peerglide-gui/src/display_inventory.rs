//! Explicit, read-only inspection of the local display snapshot.
use super::egui;
use peerglide_platform::display_layout::{Display, Layout, Rotation};
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
enum State {
    #[default]
    Idle,
    Pending,
    Ready(Arc<Layout>),
    Failed,
}

#[derive(Default)]
pub(super) struct Panel {
    state: Arc<Mutex<State>>,
}

impl Panel {
    fn refresh(&self, wake: Arc<dyn Fn() + Send + Sync>) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if matches!(*state, State::Pending) {
            return;
        }
        *state = State::Pending;
        let shared = self.state.clone();
        if std::thread::Builder::new()
            .name("linko-display-snapshot".into())
            .spawn(move || {
                let result = peerglide_platform::read_display_layout()
                    .map_or(State::Failed, |layout| State::Ready(Arc::new(layout)));
                if let Ok(mut snapshot) = shared.lock() {
                    *snapshot = result;
                }
                wake();
            })
            .is_err()
        {
            *state = State::Failed;
        }
    }

    pub(super) fn ui(&self, ui: &mut egui::Ui, enabled: bool, wake: &Arc<dyn Fn() + Send + Sync>) {
        ui.collapsing("本机显示器", |ui| {
            let state = self.state.lock().map_or(State::Failed, |state| state.clone());
            if ui
                .add_enabled(
                    enabled && !matches!(state, State::Pending),
                    egui::Button::new("读取当前显示器"),
                )
                .clicked()
            {
                self.refresh(wake.clone());
                ui.ctx().request_repaint();
            }
            match state {
                State::Idle => {}
                State::Pending => {
                    ui.label("正在读取显示器。");
                }
                State::Ready(layout) => {
                    for display in layout.displays() {
                        ui.label(description(display, display.id == layout.primary()));
                    }
                    ui.weak("当前键鼠布局使用两台电脑的主屏方向。");
                }
                State::Failed => {
                    ui.label("暂时无法读取本机显示器。");
                }
            }
        });
    }
}

fn description(display: &Display, primary: bool) -> String {
    let bounds = display.bounds;
    let name = if primary { "主屏" } else { "副屏" };
    let angle = match display.rotation {
        Rotation::Upright => 0,
        Rotation::Clockwise90 => 90,
        Rotation::Clockwise180 => 180,
        Rotation::Clockwise270 => 270,
    };
    format!(
        "{name} · {} × {} · ({}, {}) · {:.2}× · {angle}°",
        bounds.right - bounds.left,
        bounds.bottom - bounds.top,
        bounds.left,
        bounds.top,
        display.scale,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use peerglide_platform::display_layout::{Bounds, DisplayId};

    #[test]
    fn readout_keeps_position_scale_and_orientation_in_desktop_units() {
        let display = Display {
            id: DisplayId(2),
            bounds: Bounds {
                left: -1800.0,
                top: 120.0,
                right: -600.0,
                bottom: 2000.0,
            },
            scale: 1.75,
            rotation: Rotation::Clockwise90,
        };
        let text = description(&display, false);
        assert!(text.contains("副屏"));
        assert!(text.contains("1200 × 1880"));
        assert!(text.contains("(-1800, 120)"));
        assert!(text.contains("1.75× · 90°"));
    }
}
