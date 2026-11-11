use super::*;

pub(super) fn ui_context() -> egui::Context {
    let ctx = egui::Context::default();
    install_fonts(&ctx);
    desktop_style::install(&ctx, false);
    ctx
}

#[test]
fn communication_and_probe_commands_never_inherit_saved_native_or_receive_grants() -> Result<()>
{
    let fixture = Fixture::new()?;
    let mut app = App::load(fixture.path.clone());
    app.config.native = true;
    app.config.allow = true;
    app.config.listen = false;
    for mode in [SessionMode::Communication, SessionMode::Probe] {
        let command = app.worker_command(
            PathBuf::from("not-executed-worker"),
            PathBuf::from("public-test.crt"),
            mode,
            true,
            ("test-local", "test-peer"),
        );
        let args: Vec<_> = command
            .get_args()
            .map(|s| s.to_string_lossy().to_string())
            .collect();
        assert!(!args
            .iter()
            .any(|s| s == "--native" || s == "--allow-control" || s == "--input-diagnostics"));
        assert_eq!(
            args.iter().any(|s| s == "--probe"),
            mode == SessionMode::Probe
        );
        assert!(args.windows(2).any(|pair| {
            pair[0] == "--expected-local-fingerprint" && pair[1] == "test-local"
        }));
        assert!(args.windows(2).any(|pair| {
            pair[0] == "--expected-peer-fingerprint" && pair[1] == "test-peer"
        }));
    }
    let native = app.worker_command(
        PathBuf::from("not-executed-worker"),
        PathBuf::from("public-test.crt"),
        SessionMode::Native,
        false,
        ("test-local", "test-peer"),
    );
    let args: Vec<_> = native
        .get_args()
        .map(|s| s.to_string_lossy().to_string())
        .collect();
    assert!(
        args.iter().any(|s| s == "--native") && args.iter().any(|s| s == "--allow-control")
    );
    Ok(())
}
#[test]
fn resend_stop_really_writes_again_without_resetting_recovery_deadline() -> Result<()> {
    use std::io::Read;
    let fixture = Fixture::new()?;
    let mut app = App::load(fixture.path.clone());
    let python = if cfg!(windows) { "python" } else { "python3" };
    let mut child = Command::new(python)
        .args([
            "-c",
            "import sys; print(len([sys.stdin.readline(), sys.stdin.readline()]), flush=True)",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    let mut out = child.stdout.take().unwrap();
    app.child = Some(child);
    app.stop();
    let deadline = app.stopping;
    app.recovery_blocked = true;
    app.resend_stop()?;
    let mut output = String::new();
    out.read_to_string(&mut output)?;
    assert_eq!(output.trim(), "2");
    assert_eq!(app.stopping, deadline);
    assert!(app.recovery_blocked);
    app.child.take().unwrap().wait()?;
    Ok(())
}
#[test]
fn stop_after_terminal_receipt_preserves_cleanup_while_process_is_exiting() -> Result<()> {
    for state in [WorkerInputState::Local, WorkerInputState::Disabled] {
        let fixture = Fixture::new()?;
        let mut app = App::load(fixture.path.clone());
        let python = if cfg!(windows) { "python" } else { "python3" };
        app.child = Some(
            Command::new(python)
                .args(["-c", "import sys; sys.stdin.readline()"])
                .stdin(Stdio::piped())
                .spawn()?,
        );
        app.active_mode = Some(SessionMode::Native);
        app.input_state = Some(state);
        app.input_terminal = true;
        let drained = Arc::new(WorkerSnapshot::default());
        drained.2.store(true, Ordering::Release);
        app.worker_state = Some(drained);
        app.stopping = Some(Instant::now() - Duration::from_secs(4));
        app.poll();
        app.stop();
        assert_eq!(app.input_state, Some(state));
        assert!(app.native_cleanup_reported());
        assert!(!app.recovery_blocked);
        let child = app.child.as_mut().unwrap();
        child.stdin.as_mut().unwrap().write_all(b"exit\n")?;
        assert!(child.wait()?.success());
        app.poll();
        assert!(app.child.is_none());
        assert!(!app.recovery_blocked);
    }
    Ok(())
}
#[test]
fn communication_stop_timeout_cancels_upgrade_without_inventing_input_failure() -> Result<()> {
    let fixture = Fixture::new()?;
    let mut app = App::load(fixture.path.clone());
    app.active_mode = Some(SessionMode::Communication);
    app.pending_start = Some(SessionMode::Native);
    app.mark_stop_timeout();
    assert!(!app.recovery_blocked);
    assert!(app.pending_start.is_none());
    app.active_mode = Some(SessionMode::Native);
    app.mark_stop_timeout();
    assert!(app.recovery_blocked);
    Ok(())
}
#[test]
fn old_routing_state_and_process_eof_are_not_a_terminal_cleanup_receipt() -> Result<()> {
    let fixture = Fixture::new()?;
    let mut app = App::load(fixture.path.clone());
    for input in [WorkerInputState::Local, WorkerInputState::Disabled] {
        app.input_state = Some(input);
        app.input_terminal = false;
        assert!(!app.native_cleanup_reported());
    }
    let drained = Arc::new(WorkerSnapshot::default());
    drained.2.store(true, Ordering::Release);
    app.worker_state = Some(drained.clone());
    app.input_terminal = true;
    assert!(app.native_cleanup_reported());
    drained.3.store(true, Ordering::Release);
    assert!(!app.native_cleanup_reported());
    drained.3.store(false, Ordering::Release);
    app.recovery_blocked = true;
    assert!(!app.native_cleanup_reported());
    let snapshot = WorkerSnapshot::default();
    snapshot.publish_input(WorkerInputState::Local, false);
    assert_eq!(
        snapshot.take_input(),
        Some((WorkerInputState::Local, false))
    );
    snapshot.publish_input(WorkerInputState::Local, true);
    assert_eq!(snapshot.take_input(), Some((WorkerInputState::Local, true)));
    Ok(())
}
#[test]
fn authentication_and_saved_preferences_do_not_invent_input_ownership() -> Result<()> {
    let fixture = Fixture::new()?;
    let mut app = App::load(fixture.path.clone());
    app.config.native = true;
    app.connected = true;
    app.status = "已通过身份验证".into();
    assert_eq!(app.input_label(), "尚未开启键鼠会话");
    app.input_state = Some(WorkerInputState::ReceivingRemote);
    assert_eq!(app.input_label(), "另一台正在控制本机");
    app.recovery_blocked = true;
    assert_eq!(app.input_label(), "输入恢复未确认");
    app.native_confirmation = true;
    assert!(app.request_native().is_err());
    assert!(app.child.is_none());
    Ok(())
}
#[test]
fn native_confirmation_without_fresh_worker_check_does_not_write_or_spawn() -> Result<()> {
    let fixture = Fixture::new()?;
    let mut app = App::load(fixture.path.clone());
    app.native_confirmation = true;
    assert!(app.request_native().is_err());
    assert!(!app.native_confirmation && !app.config.native && !app.config.allow);
    assert!(app.child.is_none() && app.pending_start.is_none());
    assert!(!app.dir.join("settings.json").exists() && !app.identity_dir().exists());
    Ok(())
}
#[test]
fn workspace_start_only_opens_fresh_consent_and_readonly_recovery_blocks_it() -> Result<()> {
    let fixture = Fixture::new()?;
    let mut app = App::load(fixture.path.clone());
    // Presentation fixture only: no identity, certificate or worker is created.
    app.config.fingerprint = Some("presentation-only".into());
    app.native_confirmation = true;
    app.workspace_action(desktop_state::Action::StartInput);
    if cfg!(any(windows, target_os = "macos")) {
        assert_eq!(app.dialog, Some(Dialog::NativeStart));
        assert!(!app.native_confirmation);
    }
    assert!(app.child.is_none());
    assert!(!app.config.native && !app.config.allow);
    assert!(!app.identity_dir().exists());
    app.native_confirmation = true;
    app.stop();
    assert!(!app.native_confirmation);
    app.native_confirmation = true;
    app.workspace_action(desktop_state::Action::AddDevice);
    assert!(!app.native_confirmation && app.dialog.is_none());
    assert!(app.pairing_open && app.pairing_panel.open && app.wizard.is_none());
    assert!(!app.pairing_panel.needs_poll() && !app.identity_dir().exists());
    app.pairing_panel.open = false;
    app.pairing_open = false;
    app.dialog = None;
    app.recovery_blocked = true;
    app.workspace_action(desktop_state::Action::StartInput);
    assert!(app.dialog.is_none());
    app.workspace_action(desktop_state::Action::Recovery);
    assert_eq!(app.dialog, Some(Dialog::Recovery));
    let mut readonly = App::load(fixture.path.clone());
    readonly.config.fingerprint = app.config.fingerprint.clone();
    for action in [
        desktop_state::Action::StartInput,
        desktop_state::Action::CheckPermissions,
        desktop_state::Action::Connect,
        desktop_state::Action::AddDevice,
    ] {
        readonly.workspace_action(action);
        assert!(
            readonly.child.is_none()
                && !readonly.worker_permissions.needs_poll()
                && readonly.dialog.is_none()
                && readonly.wizard.is_none()
                && !readonly.pairing_open
                && !readonly.pairing_panel.needs_poll()
        );
    }
    Ok(())
}
#[test]
fn workspace_primary_action_stays_visible_in_supported_windows() -> Result<()> {
    fn text_rect(shape: &egui::epaint::Shape, label: &str) -> Option<egui::Rect> {
        match shape {
            egui::epaint::Shape::Text(text) if text.galley.job.text == label => {
                Some(text.visual_bounding_rect())
            }
            egui::epaint::Shape::Vec(shapes) => {
                shapes.iter().find_map(|shape| text_rect(shape, label))
            }
            _ => None,
        }
    }
    let fixture = Fixture::new()?;
    let mut app = App::load(fixture.path.clone());
    // Rendering metadata only. No peer certificate, identity or worker is created.
    app.config.fingerprint = Some("workspace-presentation-only".into());
    let ctx = ui_context();
    install_fonts(&ctx);
    let label = app.workspace_view().action.label();
    for dark in [false, true] {
        app.config.dark = dark;
        app.theme(&ctx);
        for size in [Vec2::new(920.0, 700.0), Vec2::new(1180.0, 820.0)] {
            for page in [Page::Home, Page::Devices, Page::Displays] {
                app.page = page;
                let output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                        ..Default::default()
                    },
                    |ui| app.desktop(ui),
                );
                let (clip, bounds) = output
                    .shapes
                    .iter()
                    .find_map(|paint| {
                        text_rect(&paint.shape, label).map(|bounds| (paint.clip_rect, bounds))
                    })
                    .context("primary action was not painted in the initial viewport")?;
                assert!(
                    clip.contains_rect(bounds),
                    "primary action clipped at {size:?}: {bounds:?} outside {clip:?}"
                );
                assert!(app.child.is_none());
                output.drop_without_applying_deltas();
            }
        }
    }
    Ok(())
}
#[test]
fn workspace_pointer_drag_and_direction_buttons_commit_only_editable_layout() -> Result<()> {
    fn text_center(shape: &egui::epaint::Shape, label: &str) -> Option<egui::Pos2> {
        match shape {
            egui::epaint::Shape::Text(text) if text.galley.job.text == label => {
                Some(text.visual_bounding_rect().center())
            }
            egui::epaint::Shape::Vec(shapes) => {
                shapes.iter().find_map(|shape| text_center(shape, label))
            }
            _ => None,
        }
    }
    fn frame(
        app: &mut App,
        ctx: &egui::Context,
        time: f64,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        ctx.run_ui(
            egui::RawInput {
                time: Some(time),
                events,
                focused: true,
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    Vec2::new(920.0, 700.0),
                )),
                ..Default::default()
            },
            |ui| app.desktop(ui),
        )
    }
    fn step(app: &mut App, ctx: &egui::Context, time: f64, events: Vec<egui::Event>) {
        frame(app, ctx, time, events).drop_without_applying_deltas();
    }
    fn button(pos: egui::Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        }
    }
    fn press(pos: egui::Pos2) -> Vec<egui::Event> {
        vec![egui::Event::PointerMoved(pos), button(pos, true)]
    }
    let fixture = Fixture::new()?;
    let mut app = App::load(fixture.path.clone());
    // Presentation fixture only: this test never authenticates or starts a worker.
    app.config.fingerprint = Some("layout-presentation-only".into());
    app.config.peer_name = "B".into();
    app.page = Page::Devices;
    app.device_tab = DeviceTab::Layout;
    let ctx = ui_context();
    install_fonts(&ctx);
    app.theme(&ctx);
    step(&mut app, &ctx, 0.0, vec![]);
    let output = frame(&mut app, &ctx, 0.1, vec![]);
    let start = output
        .shapes
        .iter()
        .find_map(|paint| text_center(&paint.shape, "B"))
        .context("peer screen label not painted")?;
    output.drop_without_applying_deltas();
    let end = start - Vec2::new(260.0, 0.0);
    step(&mut app, &ctx, 0.2, press(start));
    step(&mut app, &ctx, 0.3, vec![egui::Event::PointerMoved(end)]);
    assert!(
        app.screen_drag.is_some(),
        "drag did not acquire the peer screen"
    );
    step(&mut app, &ctx, 0.4, vec![button(end, false)]);
    assert!(
        app.screen_drag.is_none(),
        "released drag left the layout locked"
    );
    assert_eq!(app.config.edge, "left");
    let stored: serde_json::Value =
        serde_json::from_slice(&std::fs::read(app.dir.join("settings.json"))?)?;
    assert_eq!(stored["data"]["edge"], "left");
    let output = frame(&mut app, &ctx, 0.5, vec![]);
    let right = output
        .shapes
        .iter()
        .find_map(|paint| text_center(&paint.shape, "右侧"))
        .context("direction button not painted")?;
    output.drop_without_applying_deltas();
    step(&mut app, &ctx, 0.6, press(right));
    step(&mut app, &ctx, 0.7, vec![button(right, false)]);
    assert_eq!(app.config.edge, "right");
    // Paint the new position before pressing it: egui hit testing uses painted geometry.
    step(&mut app, &ctx, 0.75, vec![]);
    step(
        &mut app,
        &ctx,
        0.8,
        vec![
            egui::Event::PointerMoved(start),
            button(start, true),
            egui::Event::PointerMoved(end),
            button(end, false),
        ],
    );
    assert!(app.screen_drag.is_none());
    assert_eq!(app.config.edge, "left", "coalesced drag was lost");
    step(&mut app, &ctx, 0.81, press(right));
    step(&mut app, &ctx, 0.82, vec![button(right, false)]);
    assert_eq!(app.config.edge, "right");
    let before = std::fs::read(app.dir.join("settings.json"))?;
    drop(app.instance_lock.take());
    step(&mut app, &ctx, 0.85, vec![]);
    step(&mut app, &ctx, 0.9, press(start));
    step(&mut app, &ctx, 1.0, vec![egui::Event::PointerMoved(end)]);
    step(&mut app, &ctx, 1.1, vec![button(end, false)]);
    assert!(app.screen_drag.is_none());
    assert_eq!(app.config.edge, "right");
    assert_eq!(std::fs::read(app.dir.join("settings.json"))?, before);
    assert!(app.child.is_none());
    Ok(())
}
#[test]
fn desktop_preferences_preserve_input_schema_and_future_desktop_file() -> Result<()> {
    let fixture = Fixture::new()?;
    let mut app = App::load(fixture.path.clone());
    app.save_config()?;
    let original = std::fs::read(app.dir.join("settings.json"))?;
    let preferences = DesktopPreferences {
        tray_enabled: true,
        close_to_tray: true,
    };
    app.desktop_store.as_mut().unwrap().save(&preferences)?;
    assert_eq!(std::fs::read(app.dir.join("settings.json"))?, original);
    drop(app);
    let app = App::load(fixture.path.clone());
    assert!(app.desktop_preferences.tray_enabled && app.desktop_preferences.close_to_tray);
    assert!(app.tray.is_none() && app.child.is_none());
    drop(app);
    let future = br#"{"store_version":999,"data":{"tray_enabled":true}}"#;
    std::fs::write(fixture.path.join("desktop-preferences.json"), future)?;
    let app = App::load(fixture.path.clone());
    assert!(app.desktop_store.is_none());
    assert!(!app.desktop_preferences.tray_enabled && !app.desktop_preferences.close_to_tray);
    assert_eq!(
        std::fs::read(app.dir.join("desktop-preferences.json"))?,
        future
    );
    assert_eq!(std::fs::read(app.dir.join("settings.json"))?, original);
    Ok(())
}
#[test]
fn full_desktop_and_dialogs_render_without_starting_workers_or_exporting_samples() -> Result<()>
{
    let fixture = Fixture::new()?;
    let mut app = App::load(fixture.path.clone());
    let ctx = ui_context();
    install_fonts(&ctx);
    for page in [
        Page::Home,
        Page::Devices,
        Page::GestureLab,
        Page::Transfers,
        Page::Clipboard,
        Page::Shortcuts,
        Page::Displays,
        Page::Settings,
    ] {
        app.page = page;
        for dialog in [
            None,
            Some(Dialog::NativeStart),
            Some(Dialog::Recovery),
            Some(Dialog::Reset),
        ] {
            app.dialog = dialog;
            let output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        Vec2::new(1180.0, 820.0),
                    )),
                    ..Default::default()
                },
                |ui| app.desktop(ui),
            );
            assert!(!output.shapes.is_empty());
            output.drop_without_applying_deltas();
            assert!(app.child.is_none());
        }
    }
    assert!(!app.identity_dir().exists());
    assert!(!app.dir.join("gesture-samples").exists());
    Ok(())
}
#[test]
fn bundled_font_covers_chinese_ui_without_system_fonts() {
    let ctx = ui_context();
    install_fonts(&ctx);
    let output = ctx.run_ui(egui::RawInput::default(), |ui| {
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            let font = egui::FontId::new(16.0, family);
            assert!(ui.fonts_mut(|fonts| fonts.has_glyphs(
                &font,
                "屏跃主页设备显示器传输设置手势实验室规则离线可信权限恢复"
            )));
        }
        ui.label("屏跃：多台电脑，一种体验。");
    });
    assert!(!output.shapes.is_empty());
    output.drop_without_applying_deltas();
}

#[test]
fn explicit_data_directory_keeps_profiles_and_permissions_separate() {
    let fixture = Fixture::new().unwrap();
    let original = fixture.path.join("original");
    std::fs::create_dir(&original).unwrap();
    let sentinel = original.join("settings.json");
    let preserved = br#"{"store_version":999,"data":{"native":true}}"#;
    std::fs::write(&sentinel, preserved).unwrap();
    let selected = fixture.path.join("isolated profile 屏跃");
    let options = Options::try_parse_from([
        std::ffi::OsString::from("peerglide-gui"),
        std::ffi::OsString::from("--data-dir"),
        selected.clone().into_os_string(),
        std::ffi::OsString::from("--gesture-lab"),
    ])
    .unwrap();
    assert!(options.gesture_lab);
    let mut app = App::load(options.data_dir.unwrap());
    assert_eq!(app.dir, selected);
    assert!(!app.config.native && !app.config.allow && !app.config.reconnect);
    assert!(app.config.fingerprint.is_none());
    assert!(app.local_fingerprint().is_none());
    app.save_config().unwrap();
    assert!(selected.join("settings.json").is_file());
    assert!(!selected.join("identity").exists());
    assert_eq!(std::fs::read(sentinel).unwrap(), preserved);
}

#[test]
fn malformed_gui_options_fail_before_creating_a_profile() {
    assert!(Options::try_parse_from(["peerglide-gui", "--data-dir"]).is_err());
    assert!(Options::try_parse_from(["peerglide-gui", "--unknown-option"]).is_err());
    let options = Options::try_parse_from(["peerglide-gui"]).unwrap();
    assert!(options.data_dir.is_none());
    assert!(!options.gesture_lab);
}
#[test]
fn ordinary_geometry_option_requires_an_explicit_fresh_profile() -> Result<()> {
    assert!(
        Options::try_parse_from(["peerglide-gui", "--ui-layout-report", "report.json"])
            .is_err()
    );
    let fixture = Fixture::new()?;
    let options = Options::try_parse_from([
        "peerglide-gui",
        "--data-dir",
        fixture.path.to_str().unwrap(),
        "--ui-layout-report",
        "report.json",
    ])?;
    validate_layout_report_profile(&options)?;
    std::fs::write(fixture.path.join("existing-data"), b"preserve")?;
    assert!(validate_layout_report_profile(&options).is_err());
    assert!(
        !fixture.path.join("identity").exists() && !fixture.path.join("settings.json").exists()
    );
    fixture.close()?;
    Ok(())
}
pub(super) struct Fixture {
    pub(super) path: PathBuf,
    live: bool,
}
impl Fixture {
    pub(super) fn new() -> Result<Self> {
        static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        for _ in 0..100 {
            let path = std::env::temp_dir().join(format!(
                "peerglide-ui-{}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)?
                    .as_nanos(),
                NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            match std::fs::create_dir(&path) {
                Ok(()) => return Ok(Self { path, live: true }),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => {
                    return Err(e)
                        .with_context(|| format!("create GUI fixture {}", path.display()))
                }
            }
        }
        anyhow::bail!("unable to reserve a unique GUI test directory")
    }
    pub(super) fn close(mut self) -> Result<()> {
        std::fs::remove_dir_all(&self.path)
            .with_context(|| format!("cleanup GUI fixture {}", self.path.display()))?;
        self.live = false;
        Ok(())
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if self.live {
            if let Err(e) = std::fs::remove_dir_all(&self.path) {
                eprintln!("GUI fixture cleanup failed at {}: {e}", self.path.display());
            }
        }
    }
}
#[test]
fn parallel_fixtures_have_exclusive_ownership_and_cleanup() -> Result<()> {
    let fixtures: Vec<_> = (0..32).map(|_| std::thread::spawn(Fixture::new)).collect();
    let fixtures: Vec<_> = fixtures
        .into_iter()
        .map(|t| t.join().expect("fixture thread panicked"))
        .collect::<Result<_>>()?;
    let paths: std::collections::BTreeSet<_> =
        fixtures.iter().map(|f| f.path.clone()).collect();
    assert_eq!(paths.len(), 32);
    for fixture in &fixtures {
        assert!(fixture.path.is_dir());
    }
    for fixture in fixtures {
        fixture.close()?;
    }
    for path in paths {
        assert!(!path.exists());
    }
    let path = {
        let fixture = Fixture::new()?;
        fixture.path.clone()
    };
    assert!(!path.exists(), "RAII cleanup on early return");
    Ok(())
}
#[test]
fn pasted_connection_material_keeps_following_controls_within_a_small_window() -> Result<()> {
    let fixture = Fixture::new()?;
    let mut app = App::load(fixture.path.clone());
    app.connection_text = connection_info::encode(&Identity::generate()?.certificate, &[])?;
    let ctx = ui_context();
    install_fonts(&ctx);
    for width in [300.0, 600.0] {
        let output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    Vec2::new(width, 600.0),
                )),
                ..Default::default()
            },
            |root| {
                egui::CentralPanel::default().show(root, |ui| {
                    let top = ui.cursor().min.y;
                    app.connection_exchange_ui(ui);
                    assert!(ui.cursor().min.y - top < 400.0, "pasted certificate pushed read/confirmation controls below a small window");
                });
            },
        );
        output.drop_without_applying_deltas();
    }
    Ok(())
}
#[test]
fn public_connection_exchange_enrolls_both_peers_without_candidate_files() -> Result<()> {
    let waiting = Fixture::new()?;
    let connecting = Fixture::new()?;
    let mut a = App::load(waiting.path.clone());
    let mut b = App::load(connecting.path.clone());
    let identity_a = Identity::create(&a.identity_dir())?;
    let identity_b = Identity::create(&b.identity_dir())?;
    assert!(
        a.config.address.is_empty(),
        "no example IP may look like a real peer"
    );
    let address = "192.168.20.8:24870".parse()?;
    b.connection_text = connection_info::encode(&identity_a.certificate, &[address])?;
    b.read_connection_info()?;
    assert!(!b.wizard_listen);
    assert_eq!(b.wizard_address, address.to_string());
    assert!(b.config.fingerprint.is_none());
    assert!(!connecting.path.join("trusted-peer.crt.der").exists());
    assert!(
        b.enroll().is_err(),
        "reading public data must not confer trust"
    );
    b.confirmed_fingerprint = Some(identity_a.fingerprint());
    b.config.address = b.wizard_address.clone();
    b.config.listen = b.wizard_listen;
    b.enroll()?;
    assert!(!b.config.allow && !b.config.native);
    // The connector's reply needs no address, and selects the waiting role.
    a.wizard_listen = false;
    a.wizard_address = "192.168.20.99:24870".into();
    a.connection_text = connection_info::encode(&identity_b.certificate, &[])?;
    a.read_connection_info()?;
    assert!(a.wizard_listen);
    assert!(a.wizard_address.is_empty());
    a.confirmed_fingerprint = Some(identity_b.fingerprint());
    a.enroll()?;
    assert!(!a.config.allow && !a.config.native);
    assert_eq!(
        transport::read_certificate(&waiting.path.join("trusted-peer.crt.der"))?,
        identity_b.certificate
    );
    assert_eq!(
        transport::read_certificate(&connecting.path.join("trusted-peer.crt.der"))?,
        identity_a.certificate
    );
    assert_eq!(
        Identity::load(&a.identity_dir())?.certificate,
        identity_a.certificate
    );
    assert_eq!(
        Identity::load(&b.identity_dir())?.certificate,
        identity_b.certificate
    );
    Ok(())
}
#[test]
fn edited_unread_self_and_invalid_connection_info_cannot_reuse_confirmation() -> Result<()> {
    let fixture = Fixture::new()?;
    let mut app = App::load(fixture.path.clone());
    let local = Identity::create(&app.identity_dir())?;
    let peer = Identity::generate()?;
    app.connection_text =
        connection_info::encode(&peer.certificate, &["192.168.20.8:24870".parse()?])?;
    app.read_connection_info()?;
    app.confirmed_fingerprint = Some(peer.fingerprint());
    // Even editing just the address requires a fresh read and confirmation.
    app.connection_text =
        connection_info::encode(&peer.certificate, &["192.168.20.9:24870".parse()?])?;
    assert!(app.enroll().is_err());
    assert!(!fixture.path.join("trusted-peer.crt.der").exists());
    app.read_connection_info()?;
    assert!(app.confirmed_fingerprint.is_none());
    app.confirmed_fingerprint = Some(peer.fingerprint());
    app.connection_text = "invalid public connection data".into();
    assert!(app.read_connection_info().is_err());
    assert!(app.loaded_connection_info.is_none());
    assert!(app.confirmed_fingerprint.is_none());
    assert!(app.enroll().is_err());
    app.connection_text = connection_info::encode(&local.certificate, &[])?;
    assert!(app.read_connection_info().is_err());
    assert!(app.loaded_connection_info.is_none());
    assert!(app.config.fingerprint.is_none());
    assert!(!app.config.allow && !app.config.native);
    Ok(())
}
#[test]
fn multihomed_connection_info_requires_explicit_address_selection() -> Result<()> {
    let fixture = Fixture::new()?;
    let mut app = App::load(fixture.path.clone());
    Identity::create(&app.identity_dir())?;
    let peer = Identity::generate()?;
    let addresses = ["192.168.20.8:24870".parse()?, "10.0.0.8:24870".parse()?];
    app.connection_text = connection_info::encode(&peer.certificate, &addresses)?;
    app.read_connection_info()?;
    assert_eq!(app.connection_addresses, addresses);
    assert!(app.wizard_address.is_empty());
    assert!(!app.wizard_listen);
    // A second GUI cannot import into a profile locked by the first.
    let mut second = App::load(fixture.path.clone());
    second.connection_text = app.connection_text.clone();
    assert!(second.read_connection_info().is_err());
    assert!(second.config.fingerprint.is_none());
    Ok(())
}
#[test]
fn enrollment_requires_confirmation_and_detects_changed_certificate() -> Result<()> {
    let fixture = Fixture::new()?;
    let dir = fixture.path.clone();
    {
        let mut app = App::load(dir.clone());
        let local = Identity::create(&app.identity_dir())?;
        let peer = Identity::generate()?;
        let candidate = dir.join("candidate.der");
        std::fs::write(&candidate, &peer.certificate)
            .context("write peer candidate fixture")?;
        app.import_path = candidate.display().to_string();
        assert!(app.enroll().is_err());
        app.confirmed_fingerprint = Some(peer.fingerprint());
        app.enroll()?;
        assert!(!app.config.allow);
        assert!(!app.config.native);
        let loaded = App::load(dir.clone());
        assert_eq!(loaded.config.fingerprint, Some(peer.fingerprint()));
        drop(loaded);
        std::fs::write(dir.join("trusted-peer.crt.der"), &local.certificate)
            .context("replace pinned certificate fixture")?;
        assert!(app.start(false).unwrap_err().to_string().contains("变化"));
        app.import_path = app
            .identity_dir()
            .join("node.crt.der")
            .display()
            .to_string();
        app.confirmed_fingerprint = Some(local.fingerprint());
        assert!(app.enroll().is_err());
    }
    fixture.close()?;
    Ok(())
}
#[test]
fn enrollment_confirmation_cannot_follow_replaced_file_contents() -> Result<()> {
    let fixture = Fixture::new()?;
    let mut app = App::load(fixture.path.clone());
    Identity::create(&app.identity_dir())?;
    let first = Identity::generate()?;
    let replacement = Identity::generate()?;
    let candidate = fixture.path.join("candidate.der");
    std::fs::write(&candidate, &first.certificate)?;
    app.import_path = candidate.display().to_string();
    app.confirmed_fingerprint = Some(first.fingerprint());
    std::fs::write(&candidate, &replacement.certificate)?;
    assert!(
        app.enroll().is_err(),
        "confirmation followed a different certificate"
    );
    assert!(app.config.fingerprint.is_none());
    assert!(!fixture.path.join("trusted-peer.crt.der").exists());
    app.confirmed_fingerprint = Some(replacement.fingerprint());
    app.enroll()?;
    assert_eq!(app.config.fingerprint, Some(replacement.fingerprint()));
    let enrolled = std::fs::read(fixture.path.join("trusted-peer.crt.der"))?;
    assert!(enrolled == replacement.certificate);
    std::fs::write(&candidate, vec![0; transport::MAX_CERTIFICATE_BYTES + 1])?;
    app.confirmed_fingerprint = Some(replacement.fingerprint());
    assert!(app.enroll().is_err());
    assert_eq!(app.config.fingerprint, Some(replacement.fingerprint()));
    assert!(std::fs::read(fixture.path.join("trusted-peer.crt.der"))? == enrolled);
    std::fs::write(&candidate, &replacement.certificate)?;
    app.confirmed_fingerprint = Some(replacement.fingerprint());
    app.revoke()?;
    assert!(app.confirmed_fingerprint.is_none());
    assert!(
        app.enroll().is_err(),
        "revocation must invalidate pending confirmation"
    );
    Ok(())
}
#[test]
fn settings_conflict_during_enrollment_preserves_existing_peer_and_consumes_confirmation(
) -> Result<()> {
    let fixture = Fixture::new()?;
    let mut app = App::load(fixture.path.clone());
    Identity::create(&app.identity_dir())?;
    let original = Identity::generate()?;
    let candidate = fixture.path.join("candidate.der");
    std::fs::write(&candidate, &original.certificate)?;
    app.import_path = candidate.display().to_string();
    app.confirmed_fingerprint = Some(original.fingerprint());
    app.enroll()?;
    let other = Identity::generate()?;
    std::fs::write(&candidate, &other.certificate)?;
    app.confirmed_fingerprint = Some(other.fingerprint());
    app.native_confirmation = true;
    app.pending_start = Some(SessionMode::Native);
    let future = br#"{"store_version":999,"data":{}}"#;
    std::fs::write(fixture.path.join("settings.json"), future)?;
    assert!(app.enroll().is_err());
    assert_eq!(app.config.fingerprint, Some(original.fingerprint()));
    assert!(app.confirmed_fingerprint.is_none() && app.config_store.is_none());
    assert!(!app.native_confirmation && app.pending_start.is_none() && app.child.is_none());
    assert_eq!(std::fs::read(fixture.path.join("settings.json"))?, future);
    assert_eq!(
        std::fs::read(fixture.path.join("trusted-peer.crt.der"))?,
        original.certificate
    );
    Ok(())
}
#[test]
fn replacing_a_peer_never_inherits_its_control_grant_and_second_gui_cannot_write() -> Result<()>
{
    let fixture = Fixture::new()?;
    let mut app = App::load(fixture.path.clone());
    Identity::create(&app.identity_dir())?;
    let candidate = fixture.path.join("candidate.der");
    for _ in 0..2 {
        let peer = Identity::generate()?;
        std::fs::write(&candidate, &peer.certificate)?;
        app.import_path = candidate.display().to_string();
        app.confirmed_fingerprint = Some(peer.fingerprint());
        app.config.allow = true;
        app.enroll()?;
        assert!(!app.config.allow);
    }
    let mut second = App::load(fixture.path.clone());
    assert!(second.instance_lock.is_none());
    assert!(second.save_config().is_err());
    assert!(second.start(true).is_err());
    drop(second);
    drop(app);
    let replacement = App::load(fixture.path.clone());
    assert!(replacement.instance_lock.is_some());
    Ok(())
}
#[test]
fn all_configuration_pages_render_in_both_themes_without_a_display() -> Result<()> {
    let fixture = Fixture::new()?;
    let dir = fixture.path.clone();
    {
        let mut app = App::load(dir.clone());
        let ctx = ui_context();
        install_fonts(&ctx);
        for dark in [true, false] {
            app.config.dark = dark;
            app.theme(&ctx);
            for page in [
                Page::Home,
                Page::Devices,
                Page::Displays,
                Page::Settings,
                Page::GestureLab,
                Page::GestureRules,
            ] {
                let output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            Vec2::new(1000.0, 720.0),
                        )),
                        ..Default::default()
                    },
                    |root| {
                        let ctx = root.ctx().clone();
                        egui::CentralPanel::default().show(root, |ui| match page {
                            Page::Home => app.home(ui),
                            Page::Devices | Page::Displays => app.devices(ui),
                            Page::Settings => app.settings(ui, &ctx),
                            Page::GestureLab => {
                                app.gesture_lab.ui(ui, false);
                            }
                            Page::GestureRules => app.gesture_rules.ui(ui, &app.dir, true),
                            _ => (),
                        });
                    },
                );
                assert!(!output.shapes.is_empty());
                output.drop_without_applying_deltas();
            }
        }
    }
    fixture.close()?;
    Ok(())
}
#[test]
fn saved_peer_address_changes_only_the_endpoint_and_rejects_stale_edits() -> Result<()> {
    let fixture = Fixture::new()?;
    let mut app = App::load(fixture.path.clone());
    Identity::create(&app.identity_dir())?;
    let peer = Identity::generate()?;
    let certificate = fixture.path.join("trusted-peer.crt.der");
    let settings = fixture.path.join("settings.json");
    std::fs::write(&certificate, &peer.certificate)?;
    app.config.fingerprint = Some(peer.fingerprint());
    app.config.allow = true;
    app.config.reconnect = true;
    app.config.listen = false;
    app.config.address = "192.168.1.2:24870".into();
    app.save_config()?;

    let mut expected = app.config.clone();
    let before = std::fs::read(&settings)?;
    app.open_peer_address_editor();
    let mut edit = app
        .peer_address_edit
        .take()
        .context("address editor did not open")?;
    edit.address = "0.0.0.0:24870".into();
    assert!(app.save_peer_address(&edit).is_err());
    assert!(app.config == expected);
    assert_eq!(std::fs::read(&settings)?, before);

    let replacement = Identity::generate()?;
    std::fs::write(&certificate, &replacement.certificate)?;
    edit.address = "192.168.1.3:24870".into();
    assert!(app.save_peer_address(&edit).is_err());
    assert!(app.config == expected);
    assert_eq!(std::fs::read(&settings)?, before);

    std::fs::write(&certificate, &peer.certificate)?;
    app.native_confirmation = true;
    app.pending_start = Some(SessionMode::Native);
    app.save_peer_address(&edit)?;
    expected.address = edit.address.clone();
    assert!(app.config == expected && app.child.is_none());
    assert!(!app.native_confirmation && app.pending_start.is_none());
    assert!(config_store::decode::<Config>(&std::fs::read(&settings)?)? == expected);
    assert_eq!(std::fs::read(&certificate)?, peer.certificate);

    edit.address = "192.168.1.4:24870".into();
    assert!(app.save_peer_address(&edit).is_err());
    assert!(app.config == expected);

    app.open_peer_address_editor();
    let mut fresh = app.peer_address_edit.take().unwrap();
    fresh.address = edit.address;
    let mut other_writer = expected.clone();
    other_writer.dark = !other_writer.dark;
    let updated = config_store::encode(&other_writer)?;
    std::fs::write(&settings, &updated)?;
    assert!(app.save_peer_address(&fresh).is_err());
    assert!(app.config == expected);
    assert_eq!(std::fs::read(&settings)?, updated);
    drop(app);
    fixture.close()?;
    Ok(())
}

#[test]
fn discovery_selection_stages_an_address_without_changing_saved_trust() -> Result<()> {
    let fixture = Fixture::new()?;
    let mut app = App::load(fixture.path.clone());
    let settings = fixture.path.join("settings.json");
    let original = app.config.clone();
    app.stage_discovery_address("192.168.1.2:24870".parse()?)?;
    assert!(app.config == original && !settings.exists());
    assert!(matches!(app.wizard, Some(SetupStep::Identity)));
    assert!(!app.pairing_open && !app.identity_dir().exists());

    Identity::create(&app.identity_dir())?;
    let peer = Identity::generate()?;
    let certificate = fixture.path.join("trusted-peer.crt.der");
    std::fs::write(&certificate, &peer.certificate)?;
    app.config.fingerprint = Some(peer.fingerprint());
    app.config.allow = true;
    app.config.listen = true;
    app.save_config()?;
    let original = app.config.clone();
    let original_settings = std::fs::read(&settings)?;
    app.confirmed_fingerprint = Some(peer.fingerprint());
    app.stage_discovery_address("192.168.1.3:24870".parse()?)?;
    assert!(matches!(app.wizard, Some(SetupStep::Find)));
    assert!(!app.wizard_listen && !app.pairing_open);
    assert_eq!(app.wizard_address, "192.168.1.3:24870");
    assert!(app.confirmed_fingerprint.is_none());
    assert!(app.config == original && app.child.is_none());
    assert_eq!(std::fs::read(&settings)?, original_settings);
    assert_eq!(std::fs::read(&certificate)?, peer.certificate);
    drop(app);
    fixture.close()?;
    Ok(())
}
