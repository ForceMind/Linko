//! Consent presentation. No clipboard, connection, or worker access.
use super::{desktop_style, icons::Icon, visual_assets};
use eframe::egui;
use peerglide_transport::clipboard_worker::Mode;

pub(super) enum Decision {
    Import,
    Decline,
    #[cfg(any(
        all(windows, target_env = "msvc"),
        target_os = "macos",
        all(test, target_os = "linux")
    ))]
    ImportForDrag,
    #[cfg(any(
        all(windows, target_env = "msvc"),
        target_os = "macos",
        all(test, target_os = "linux")
    ))]
    PrepareDrop,
}

pub(super) fn pending(
    ui: &mut egui::Ui,
    mode: Mode,
    for_drop: bool,
    _drag_ready: bool,
) -> (Option<Decision>, bool) {
    let (kind, icon) = match mode {
        Mode::Text => ("纯文本", Icon::Log),
        Mode::Image => ("PNG 图片", Icon::Image),
    };
    ui.vertical_centered(|ui| {
        ui.add_space(12.0);
        visual_assets::handoff_art(ui, icon);
        ui.heading(kind);
        ui.label(if for_drop {
            "同意后仅准备本机拖放。"
        } else {
            "导入后会替换本机剪贴板。"
        });
        ui.add_space(20.0);
        let decision = ui
            .allocate_ui_with_layout(
                egui::vec2(ui.available_width().min(255.0), 40.0),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    if ui.add_sized([102.0, 38.0], egui::Button::new("拒绝")).clicked() {
                        return Some(Decision::Decline);
                    }
                    if !for_drop
                        && ui
                            .add(desktop_style::primary("导入"))
                            .on_hover_text("导入到本机剪贴板")
                            .clicked()
                    {
                        return Some(Decision::Import);
                    }
                    #[cfg(any(
                        all(windows, target_env = "msvc"),
                        target_os = "macos",
                        all(test, target_os = "linux")
                    ))]
                    if for_drop
                        && ui
                            .add_enabled(_drag_ready, desktop_style::primary("同意准备拖放"))
                            .clicked()
                    {
                        return Some(Decision::PrepareDrop);
                    }
                    None
                },
            )
            .inner;
        #[cfg(any(
            all(windows, target_env = "msvc"),
            target_os = "macos",
            all(test, target_os = "linux")
        ))]
        let decision = if !for_drop
            && ui
                .add_enabled(_drag_ready, egui::Button::new("导入并准备拖放"))
                .on_hover_text("导入到本机剪贴板，并准备一次本机复制拖放。")
                .clicked()
        {
            Some(Decision::ImportForDrag)
        } else {
            decision
        };
        let stop = ui.small_button("停止本次共享").clicked();
        (decision, stop)
    })
    .inner
}
