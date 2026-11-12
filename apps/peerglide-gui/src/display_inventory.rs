//! Inspect the local display snapshot and explicitly choose a receive target.
use super::{egui, App, SessionMode};
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
    receive_display: Option<Display>,
}

impl Panel {
    pub(super) fn snapshot(&self) -> Option<Arc<Layout>> {
        let state = self.state.lock().ok()?;
        match &*state {
            State::Ready(layout) => Some(layout.clone()),
            _ => None,
        }
    }

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

    fn receive_origin(&self) -> anyhow::Result<Option<(f64, f64)>> {
        let Some(selected) = self.receive_display else {
            return Ok(None);
        };
        let current = self.state.lock().is_ok_and(|state| {
            matches!(&*state, State::Ready(layout) if layout.displays().contains(&selected)
                && layout.displays().iter().filter(|display| {
                    display.bounds.left == selected.bounds.left && display.bounds.top == selected.bounds.top
                }).count() == 1)
        });
        anyhow::ensure!(current, "接收屏幕已变化或正在读取，请重新选择");
        Ok(Some((selected.bounds.left, selected.bounds.top)))
    }

    pub(super) fn ui(
        &mut self,
        ui: &mut egui::Ui,
        enabled: bool,
        can_choose: bool,
        wake: &Arc<dyn Fn() + Send + Sync>,
    ) {
        ui.collapsing("本机显示器", |ui| {
            let state = self
                .state
                .lock()
                .map_or(State::Failed, |state| state.clone());
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
                    ui.label("接收键鼠时的本机目标");
                    ui.add_enabled_ui(can_choose, |ui| {
                        ui.radio_value(&mut self.receive_display, None, "当前主屏");
                        for display in layout.displays() {
                            if display.id == layout.primary() {
                                ui.weak(description(display, true));
                            } else {
                                ui.radio_value(
                                    &mut self.receive_display,
                                    Some(*display),
                                    description(display, false),
                                );
                            }
                        }
                    });
                    if self.receive_origin().is_err() {
                        ui.label("先重新选择接收屏幕，再开启键鼠协同。");
                    }
                }
                State::Failed => {
                    ui.label("暂时无法读取本机显示器。");
                }
            }
        });
    }
}

impl App {
    pub(super) fn configure_receive_display(
        &self,
        command: &mut std::process::Command,
        mode: SessionMode,
    ) -> anyhow::Result<()> {
        if mode == SessionMode::Native && self.config.allow {
            if let Some((x, y)) = self.display_inventory.receive_origin()? {
                command.arg(format!("--receive-display={x},{y}"));
            }
        }
        Ok(())
    }
}

pub(super) fn description(display: &Display, primary: bool) -> String {
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
    fn a_changed_receive_display_requires_explicit_selection() -> anyhow::Result<()> {
        let main = Display {
            id: DisplayId(1),
            bounds: Bounds {
                left: 0.0,
                top: 0.0,
                right: 1920.0,
                bottom: 1080.0,
            },
            scale: 1.0,
            rotation: Rotation::Upright,
        };
        let mut secondary = Display {
            id: DisplayId(2),
            bounds: Bounds {
                left: -1600.0,
                top: 0.0,
                right: 0.0,
                bottom: 900.0,
            },
            ..main
        };
        let layout = Layout::new(main.id, vec![main, secondary]).unwrap();
        let panel = Panel {
            state: Arc::new(Mutex::new(State::Ready(Arc::new(layout)))),
            receive_display: Some(secondary),
        };
        let fixture = super::super::tests::Fixture::new()?;
        let mut app = App::load(fixture.path.clone());
        app.display_inventory = panel;
        assert_eq!(
            app.display_inventory.receive_origin()?,
            Some((-1600.0, 0.0))
        );
        let mut command = std::process::Command::new("peerglide");
        app.configure_receive_display(&mut command, SessionMode::Native)?;
        assert_eq!(command.get_args().len(), 0);
        app.config.allow = true;
        app.configure_receive_display(&mut command, SessionMode::Communication)?;
        assert_eq!(command.get_args().len(), 0);
        app.configure_receive_display(&mut command, SessionMode::Native)?;
        assert_eq!(
            command.get_args().next().unwrap(),
            "--receive-display=-1600,0"
        );

        secondary.scale = 1.5;
        let updated = Layout::new(main.id, vec![main, secondary]).unwrap();
        *app.display_inventory.state.lock().unwrap() = State::Ready(Arc::new(updated));
        assert!(app
            .configure_receive_display(&mut command, SessionMode::Native)
            .is_err());
        drop(app);
        fixture.close()
    }

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
