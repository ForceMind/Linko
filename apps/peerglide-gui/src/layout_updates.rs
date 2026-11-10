use super::*;
use peerglide_protocol::{
    layout_ipc::{LayoutHint, LayoutUpdate},
    Edge,
};

pub(super) struct Pending {
    request: u32,
    edge: Edge,
    started: Instant,
    warned: bool,
}

fn edge_name(edge: Edge) -> &'static str {
    match edge {
        Edge::Left => "left",
        Edge::Right => "right",
        Edge::Top => "top",
        Edge::Bottom => "bottom",
    }
}

impl App {
    /// The worker's latest direction-mismatch observation. It applies only to the live,
    /// connected child and only while the displayed direction is the one it compared.
    pub(super) fn layout_hint(&self) -> Option<LayoutHint> {
        if self.child.is_none() || !self.connected || self.stopping.is_some() {
            return None;
        }
        let snapshot = self.worker_state.as_ref()?;
        if snapshot.2.load(Ordering::Acquire) || snapshot.3.load(Ordering::Acquire) {
            return None;
        }
        let hint = snapshot.6.lock().ok().and_then(|slot| *slot)?;
        let current = edge_name(hint.local) == self.displayed_edge();
        (hint.mismatched() && current).then_some(hint)
    }

    /// Uses the ordinary edit path, so the same capture and pending-request gates apply.
    /// The offer stays until the worker confirms a new edge (which hides it) or the peer
    /// reports a consistent pair; a rejected request leaves it available to retry.
    pub(super) fn apply_layout_hint(&mut self) -> bool {
        if self.layout_request.is_some() {
            return false;
        }
        let Some(hint) = self.layout_hint() else {
            return false;
        };
        self.try_set_edge(edge_name(hint.suggested()))
    }

    pub(super) fn layout_hint_row(&mut self, ui: &mut egui::Ui) {
        let Some(hint) = self.layout_hint() else {
            return;
        };
        let peer = edge_label(edge_name(hint.peer));
        let button = format!("改为{}", edge_label(edge_name(hint.suggested())));
        ui.horizontal_wrapped(|ui| {
            ui.small(format!("两台电脑的位置不一致（对端认为本机在其{peer}）"));
            let enabled = self.layout_editable() && self.layout_request.is_none();
            if ui
                .add_enabled(enabled, egui::Button::new(button).small())
                .clicked()
            {
                self.apply_layout_hint();
            }
        });
    }

    pub(super) fn displayed_edge(&self) -> &str {
        match self.layout_active {
            Some(Edge::Left) => "left",
            Some(Edge::Right) => "right",
            Some(Edge::Top) => "top",
            Some(Edge::Bottom) => "bottom",
            None => &self.config.edge,
        }
    }

    pub(super) fn request_edge(&mut self, edge: &str) -> bool {
        let edge = match edge {
            "left" => Edge::Left,
            "right" => Edge::Right,
            "top" => Edge::Top,
            "bottom" => Edge::Bottom,
            _ => return false,
        };
        let Some(request) = self.layout_sequence.checked_add(1) else {
            self.notice = "布局请求次数已达上限，请停止会话后再调整。".into();
            return false;
        };
        self.layout_sequence = request;
        self.layout_request = Some(Pending {
            request,
            edge,
            started: Instant::now(),
            warned: false,
        });
        let result = self
            .child
            .as_mut()
            .and_then(|child| child.stdin.as_mut())
            .context("工作进程的配置通道不可用")
            .and_then(|stdin| {
                let line = LayoutUpdate::request(request, edge).line() + "\n";
                stdin.write_all(line.as_bytes())?;
                stdin.flush()?;
                Ok(())
            });
        if let Err(error) = result {
            self.notice = format!("布局请求未确认，请停止会话后重新同步：{error:#}");
            return false;
        }
        self.notice = "正在等待工作进程应用新布局。".into();
        true
    }

    pub(super) fn poll_layout(&mut self) {
        let reply = self.worker_state.as_ref().and_then(|snapshot| {
            if snapshot.2.load(Ordering::Acquire) || snapshot.3.load(Ordering::Acquire) {
                return None;
            }
            snapshot.5.lock().ok()?.take()
        });
        if let Some(reply) = reply {
            let current = self.layout_request.as_ref().is_some_and(|pending| {
                pending.request == reply.request && pending.edge == reply.edge
            });
            if current && self.child.is_some() && self.connected && self.stopping.is_none() {
                self.layout_request = None;
                if reply.applied == Some(true) {
                    self.layout_active = Some(reply.edge);
                    let desired = self.displayed_edge().to_owned();
                    let previous = std::mem::replace(&mut self.config.edge, desired);
                    if let Err(error) = self.save_config() {
                        self.config.edge = previous;
                        self.notice = format!("本次布局已生效，保存失败：{error:#}");
                    } else {
                        self.notice = "新布局已生效。先将鼠标移回屏幕内部，再跨屏。".into();
                    }
                    self.screen_drag = None;
                } else {
                    self.notice = "布局未应用，请收回本机控制、松开按键和按钮后重试。".into();
                }
            }
        }
        if let Some(pending) = &mut self.layout_request {
            if !pending.warned && pending.started.elapsed() >= Duration::from_secs(3) {
                pending.warned = true;
                self.notice = "尚未收到布局确认。请等待，或停止会话后重新同步。".into();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_ack_controls_visibility_persistence_and_stale_reply_rejection() -> Result<()> {
        let fixture = crate::tests::Fixture::new()?;
        let mut app = App::load(fixture.path.clone());
        app.config.fingerprint = Some("a".repeat(64));
        app.save_config()?;
        app.child = Some(crate::lifecycle_tests::managed_child()?);
        app.connected = true;
        app.input_state = Some(WorkerInputState::Local);
        app.active_mode = Some(SessionMode::Native);
        app.worker_state = Some(Arc::new(WorkerSnapshot::default()));
        assert!(app.layout_editable());
        assert!(app.try_set_edge("left"));
        assert_eq!(app.displayed_edge(), "right");
        assert!(!app.layout_editable());
        let snapshot = app.worker_state.as_ref().unwrap().clone();
        *snapshot.5.lock().unwrap() = Some(LayoutUpdate::request(99, Edge::Left).reply(true));
        app.poll_layout();
        assert_eq!(app.displayed_edge(), "right");
        assert!(app.layout_request.is_some());
        *snapshot.5.lock().unwrap() = Some(LayoutUpdate::request(1, Edge::Left).reply(false));
        app.poll_layout();
        assert_eq!(app.config.edge, "right");
        assert!(app.layout_request.is_none());
        assert!(app.try_set_edge("left"));
        *snapshot.5.lock().unwrap() = Some(LayoutUpdate::request(2, Edge::Left).reply(true));
        app.poll_layout();
        assert_eq!(app.config.edge, "left");
        assert_eq!(app.displayed_edge(), "left");
        let stored: serde_json::Value =
            serde_json::from_slice(&std::fs::read(fixture.path.join("settings.json"))?)?;
        assert_eq!(stored["data"]["edge"], "left");
        app.stop();
        *snapshot.5.lock().unwrap() = Some(LayoutUpdate::request(3, Edge::Right).reply(true));
        app.poll_layout();
        assert_eq!(app.config.edge, "left");
        drop(app);
        fixture.close()?;
        Ok(())
    }

    #[test]
    fn hint_follows_the_connected_child_and_one_click_requests_the_mirror() -> Result<()> {
        let fixture = crate::tests::Fixture::new()?;
        let mut app = App::load(fixture.path.clone());
        app.config.fingerprint = Some("d".repeat(64));
        app.save_config()?;
        app.child = Some(crate::lifecycle_tests::managed_child()?);
        app.connected = true;
        app.input_state = Some(WorkerInputState::Local);
        app.active_mode = Some(SessionMode::Native);
        app.worker_state = Some(Arc::new(WorkerSnapshot::default()));
        let snapshot = app.worker_state.as_ref().unwrap().clone();
        assert!(app.layout_hint().is_none());
        let hint = LayoutHint::observe(Edge::Right, Edge::Right);
        *snapshot.6.lock().unwrap() = Some(hint);
        assert_eq!(app.layout_hint(), Some(hint));
        app.config.edge = "top".into();
        assert!(app.layout_hint().is_none());
        app.config.edge = "right".into();
        app.connected = false;
        assert!(app.layout_hint().is_none());
        assert!(!app.apply_layout_hint());
        app.connected = true;
        assert!(app.apply_layout_hint());
        let requested = app.layout_request.as_ref().map(|p| (p.request, p.edge));
        assert_eq!(requested, Some((1, Edge::Left)));
        // The offer survives until the worker answers; a second click is refused.
        assert_eq!(app.layout_hint(), Some(hint));
        assert!(!app.apply_layout_hint());
        let rejected = LayoutUpdate::request(1, Edge::Left).reply(false);
        *snapshot.5.lock().unwrap() = Some(rejected);
        app.poll_layout();
        assert_eq!(app.displayed_edge(), "right");
        assert_eq!(app.layout_hint(), Some(hint));
        assert!(app.apply_layout_hint());
        let retried = app.layout_request.as_ref().map(|p| (p.request, p.edge));
        assert_eq!(retried, Some((2, Edge::Left)));
        let applied = LayoutUpdate::request(2, Edge::Left).reply(true);
        *snapshot.5.lock().unwrap() = Some(applied);
        app.poll_layout();
        assert_eq!(app.displayed_edge(), "left");
        assert!(app.layout_hint().is_none());
        // A consistent observation from the peer withdraws a stale offer.
        app.layout_active = Some(Edge::Right);
        *snapshot.6.lock().unwrap() = Some(hint);
        assert_eq!(app.layout_hint(), Some(hint));
        let consistent = LayoutHint::observe(Edge::Right, Edge::Left);
        *snapshot.6.lock().unwrap() = Some(consistent);
        assert!(app.layout_hint().is_none());
        app.stop();
        drop(app);
        fixture.close()?;
        Ok(())
    }

    #[test]
    fn hint_line_from_worker_output_reaches_the_slot_not_diagnostics() -> Result<()> {
        let hint = LayoutHint::observe(Edge::Bottom, Edge::Bottom);
        let text = format!("{}\nplain diagnostic\n", hint.line());
        let snapshot = Arc::new(WorkerSnapshot::default());
        let (tx, rx) = mpsc::sync_channel(4);
        read_worker_output(
            std::io::Cursor::new(text),
            snapshot.clone(),
            tx,
            Arc::new(|| {}),
        );
        assert_eq!(*snapshot.6.lock().unwrap(), Some(hint));
        assert_eq!(rx.try_recv()?, "plain diagnostic");
        assert!(rx.try_recv().is_err());
        Ok(())
    }

    #[test]
    fn saving_conflict_keeps_the_actual_applied_edge_visible() -> Result<()> {
        let fixture = crate::tests::Fixture::new()?;
        let mut app = App::load(fixture.path.clone());
        app.config.fingerprint = Some("b".repeat(64));
        app.save_config()?;
        app.child = Some(crate::lifecycle_tests::managed_child()?);
        app.connected = true;
        app.input_state = Some(WorkerInputState::Local);
        app.worker_state = Some(Arc::new(WorkerSnapshot::default()));
        assert!(app.try_set_edge("top"));
        let file = fixture.path.join("settings.json");
        let mut external = std::fs::read(&file)?;
        external.push(b'\n');
        std::fs::write(&file, &external)?;
        let snapshot = app.worker_state.as_ref().unwrap().clone();
        *snapshot.5.lock().unwrap() = Some(LayoutUpdate::request(1, Edge::Top).reply(true));
        app.poll_layout();
        assert_eq!(app.config.edge, "right");
        assert_eq!(app.displayed_edge(), "top");
        assert!(app.notice.contains("保存失败"));
        assert_eq!(std::fs::read(&file)?, external);
        app.stop();
        drop(app);
        fixture.close()?;
        Ok(())
    }

    #[test]
    fn uncertain_reply_stays_pending_and_remote_or_recovering_input_blocks_edits() -> Result<()> {
        let fixture = crate::tests::Fixture::new()?;
        let mut app = App::load(fixture.path.clone());
        app.config.fingerprint = Some("c".repeat(64));
        app.child = Some(crate::lifecycle_tests::managed_child()?);
        app.connected = true;
        app.worker_state = Some(Arc::new(WorkerSnapshot::default()));
        for state in [
            WorkerInputState::ControllingRemote,
            WorkerInputState::ReceivingRemote,
            WorkerInputState::Recovering,
            WorkerInputState::RecoveryUnconfirmed,
        ] {
            app.input_state = Some(state);
            assert!(!app.try_set_edge("left"));
            assert!(app.layout_request.is_none());
        }
        app.input_state = Some(WorkerInputState::Local);
        assert!(app.try_set_edge("bottom"));
        app.layout_request.as_mut().unwrap().started = Instant::now() - Duration::from_secs(4);
        app.poll_layout();
        assert_eq!(app.displayed_edge(), "right");
        assert!(app.layout_request.as_ref().unwrap().warned);
        assert!(!app.try_set_edge("top"));
        app.stop();
        assert!(app.layout_request.is_none());
        drop(app);
        fixture.close()?;
        Ok(())
    }
}
