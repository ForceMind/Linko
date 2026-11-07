//! Original desktop presentation: quiet surfaces, compact status and device art.
//! No runtime state, native queries, external assets or repaint timers live here.
use eframe::egui::{self, Color32, FontId, RichText, Stroke, TextStyle};

#[derive(Clone, Copy)]
pub(super) enum Tone {
    Neutral,
    Good,
    Warning,
    Danger,
    Accent,
}

struct Palette {
    canvas: Color32,
    surface: Color32,
    soft: Color32,
    hover: Color32,
    text: Color32,
    muted: Color32,
    line: Color32,
    accent: Color32,
}

fn palette(dark: bool) -> Palette {
    if dark {
        Palette {
            canvas: Color32::from_rgb(23, 27, 34),
            surface: Color32::from_rgb(31, 37, 46),
            soft: Color32::from_rgb(39, 47, 59),
            hover: Color32::from_rgb(48, 60, 83),
            text: Color32::from_rgb(237, 241, 247),
            muted: Color32::from_rgb(170, 184, 204),
            line: Color32::from_rgb(65, 77, 96),
            accent: Color32::from_rgb(154, 179, 255),
        }
    } else {
        Palette {
            canvas: Color32::from_rgb(247, 248, 250),
            surface: Color32::WHITE,
            soft: Color32::from_rgb(241, 244, 249),
            hover: Color32::from_rgb(232, 238, 255),
            text: Color32::from_rgb(32, 37, 45),
            muted: Color32::from_rgb(95, 111, 133),
            line: Color32::from_rgb(226, 231, 239),
            accent: Color32::from_rgb(66, 99, 235),
        }
    }
}

fn tone_colors(dark: bool, tone: Tone) -> (Color32, Color32) {
    let (fill, text) = match (dark, tone) {
        (false, Tone::Neutral) => ((232, 237, 244), (68, 84, 106)),
        (false, Tone::Good) => ((226, 244, 237), (30, 103, 78)),
        (false, Tone::Warning) => ((255, 243, 213), (128, 82, 8)),
        (false, Tone::Danger) => ((253, 233, 235), (160, 44, 60)),
        (false, Tone::Accent) => ((232, 238, 255), (54, 81, 204)),
        (true, Tone::Neutral) => ((48, 57, 71), (205, 215, 230)),
        (true, Tone::Good) => ((29, 63, 53), (145, 222, 186)),
        (true, Tone::Warning) => ((74, 56, 26), (244, 204, 127)),
        (true, Tone::Danger) => ((76, 36, 43), (255, 173, 183)),
        (true, Tone::Accent) => ((44, 58, 91), (177, 197, 255)),
    };
    (
        Color32::from_rgb(fill.0, fill.1, fill.2),
        Color32::from_rgb(text.0, text.1, text.2),
    )
}

/// Apply once at startup/theme changes. Keep the existing Chinese font setup.
pub(super) fn install(ctx: &egui::Context, dark: bool) {
    let colors = palette(dark);
    let theme = if dark {
        egui::Theme::Dark
    } else {
        egui::Theme::Light
    };
    let mut style = (*ctx.style_of(theme)).clone();
    let mut visuals = if dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    visuals.weak_text_color = Some(colors.muted);
    visuals.panel_fill = colors.canvas;
    visuals.window_fill = colors.surface;
    visuals.window_stroke = Stroke::new(1.0, colors.line);
    visuals.window_corner_radius = egui::CornerRadius::same(14);
    visuals.menu_corner_radius = egui::CornerRadius::same(10);
    visuals.extreme_bg_color = colors.canvas;
    visuals.text_edit_bg_color = Some(colors.surface);
    visuals.faint_bg_color = colors.soft;
    visuals.code_bg_color = colors.soft;
    visuals.hyperlink_color = colors.accent;
    visuals.warn_fg_color = tone_colors(dark, Tone::Warning).1;
    visuals.error_fg_color = tone_colors(dark, Tone::Danger).1;
    visuals.selection.bg_fill = colors.accent;
    visuals.selection.stroke = Stroke::new(
        1.0,
        if dark {
            Color32::from_rgb(32, 37, 45)
        } else {
            Color32::WHITE
        },
    );

    for widget in [
        &mut visuals.widgets.noninteractive,
        &mut visuals.widgets.inactive,
        &mut visuals.widgets.hovered,
        &mut visuals.widgets.active,
        &mut visuals.widgets.open,
    ] {
        widget.corner_radius = egui::CornerRadius::same(9);
        widget.fg_stroke = Stroke::new(1.0, colors.text);
        widget.bg_stroke = Stroke::new(1.0, colors.line);
        widget.expansion = 0.0;
    }
    visuals.widgets.noninteractive.bg_fill = colors.surface;
    visuals.widgets.noninteractive.weak_bg_fill = colors.surface;
    visuals.widgets.inactive.bg_fill = colors.soft;
    visuals.widgets.inactive.weak_bg_fill = colors.soft;
    visuals.widgets.hovered.bg_fill = colors.hover;
    visuals.widgets.hovered.weak_bg_fill = colors.hover;
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, colors.accent);
    visuals.widgets.active.bg_fill = colors.hover;
    visuals.widgets.active.weak_bg_fill = colors.hover;
    visuals.widgets.active.bg_stroke = Stroke::new(1.5, colors.accent);
    visuals.widgets.open.bg_fill = colors.hover;
    visuals.widgets.open.weak_bg_fill = colors.hover;
    style.visuals = visuals;
    style.spacing.item_spacing = egui::vec2(10.0, 8.0);
    style.spacing.button_padding = egui::vec2(12.0, 7.0);
    style.spacing.interact_size = egui::vec2(32.0, 32.0);
    style.spacing.window_margin = egui::Margin::same(18);
    style.spacing.menu_margin = egui::Margin::same(8);
    style
        .text_styles
        .insert(TextStyle::Heading, FontId::proportional(24.0));
    style
        .text_styles
        .insert(TextStyle::Body, FontId::proportional(14.0));
    style
        .text_styles
        .insert(TextStyle::Button, FontId::proportional(14.0));
    style
        .text_styles
        .insert(TextStyle::Small, FontId::proportional(12.0));
    style
        .text_styles
        .insert(TextStyle::Monospace, FontId::monospace(13.0));
    ctx.set_style_of(theme, style);
    ctx.set_theme(theme);
}

/// Small original dual-screen mark, painted locally without assets or timers.
pub(super) fn brand_mark(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(40.0, 40.0), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 10.0, Color32::from_rgb(66, 99, 235));
    for (position, size) in [
        (egui::vec2(7.5, 10.0), egui::vec2(17.0, 13.0)),
        (egui::vec2(18.0, 19.0), egui::vec2(15.0, 13.0)),
    ] {
        painter.rect_stroke(
            egui::Rect::from_min_size(rect.min + position, size),
            3.0,
            Stroke::new(2.0, Color32::WHITE),
            egui::StrokeKind::Inside,
        );
    }
    painter.line_segment(
        [rect.min + egui::vec2(16.0, 25.0), rect.min + egui::vec2(26.0, 16.0)],
        Stroke::new(2.0, Color32::WHITE),
    );
}

pub(super) fn card(ui: &egui::Ui) -> egui::Frame {
    let colors = palette(ui.visuals().dark_mode);
    egui::Frame::group(ui.style())
        .fill(colors.surface)
        .stroke(Stroke::new(1.0, colors.line))
        .corner_radius(egui::CornerRadius::same(14))
        .inner_margin(18.0)
}

/// A noninteractive label: color supplements the text and does not grant access.
pub(super) fn pill(ui: &mut egui::Ui, label: &str, tone: Tone) {
    let (fill, text) = tone_colors(ui.visuals().dark_mode, tone);
    egui::Frame::NONE
        .fill(fill)
        .corner_radius(egui::CornerRadius::same(12))
        .inner_margin(egui::Margin::symmetric(9, 4))
        .show(ui, |ui| {
            ui.add(egui::Label::new(RichText::new(label).size(12.0).color(text)).truncate());
        });
}

pub(super) fn primary(label: &str) -> egui::Button<'static> {
    egui::Button::new(
        RichText::new(label.to_owned())
            .size(14.0)
            .strong()
            .color(Color32::WHITE),
    )
    .fill(Color32::from_rgb(66, 99, 235))
    .stroke(Stroke::new(1.0, Color32::from_rgb(54, 81, 204)))
    .corner_radius(egui::CornerRadius::same(9))
    .min_size(egui::vec2(112.0, 36.0))
}

pub(super) fn selection_colors(ui: &egui::Ui) -> (Color32, Color32) {
    tone_colors(ui.visuals().dark_mode, Tone::Accent)
}

pub(super) fn navigation(ui: &mut egui::Ui, label: &str, selected: bool) -> egui::Response {
    let (fill, text) = selection_colors(ui);
    let color = if selected {
        text
    } else {
        ui.visuals().text_color()
    };
    let mut button = egui::Button::new(RichText::new(label).size(14.0).color(color))
        .selected(selected)
        .frame(selected);
    if selected {
        button = button.fill(fill).stroke(Stroke::NONE);
    }
    let response = ui.add_sized([ui.available_width(), 36.0], button);
    if selected {
        ui.painter().rect_filled(
            egui::Rect::from_min_size(
                response.rect.min + egui::vec2(0.0, 8.0),
                egui::vec2(3.0, 20.0),
            ),
            1.5,
            palette(ui.visuals().dark_mode).accent,
        );
    }
    response
}

/// Draw one illustrative main monitor inside `rect`, including its stand.
/// Geometry is decorative, never measured display size, connectivity or consent.
pub(super) fn monitor(
    painter: &egui::Painter,
    rect: egui::Rect,
    name: &str,
    local: bool,
    active: bool,
    dark: bool,
) {
    if ![rect.min.x, rect.min.y, rect.max.x, rect.max.y]
        .iter()
        .all(|v| v.is_finite())
        || !rect.width().is_finite()
        || !rect.height().is_finite()
        || rect.width() < 48.0
        || rect.height() < 36.0
    {
        return;
    }
    let painter = painter.with_clip_rect(rect);
    let colors = palette(dark);
    let inset = 3.0;
    let stand_height = (rect.height() * 0.16).clamp(12.0, 22.0);
    let screen = egui::Rect::from_min_max(
        rect.min + egui::vec2(inset, inset),
        egui::pos2(rect.max.x - inset, rect.max.y - stand_height),
    );
    let line = if active { colors.accent } else { colors.line };
    painter.rect_filled(screen.translate(egui::vec2(0.0, 2.0)), 10.0, colors.soft);
    painter.rect_filled(screen, 10.0, colors.surface);
    painter.rect_stroke(
        screen,
        10.0,
        Stroke::new(if active { 2.0 } else { 1.2 }, line),
        egui::StrokeKind::Inside,
    );
    let glass = screen.shrink(5.0);
    let glass_color = match (dark, local) {
        (false, true) => Color32::from_rgb(232, 238, 255),
        (false, false) => Color32::from_rgb(244, 246, 249),
        (true, true) => Color32::from_rgb(33, 45, 64),
        (true, false) => Color32::from_rgb(38, 42, 50),
    };
    painter.rect_filled(glass, 6.0, glass_color);
    let stem = egui::Rect::from_min_max(
        egui::pos2(screen.center().x - 4.5, screen.max.y - 0.5),
        egui::pos2(screen.center().x + 4.5, rect.max.y - 4.0),
    );
    painter.rect_filled(stem, 2.0, colors.line);
    let foot = egui::Rect::from_center_size(
        egui::pos2(screen.center().x, rect.max.y - 3.0),
        egui::vec2((rect.width() * 0.32).clamp(28.0, 70.0), 4.0),
    );
    painter.rect_filled(foot, 2.0, colors.line);

    let font_size = (glass.width() * 0.085).clamp(12.0, 17.0);
    let capacity = ((glass.width() - 24.0) / font_size).floor().max(1.0) as usize;
    // A bounded, character-safe label prevents very long user names from
    // generating oversized paint jobs or overflowing a neighbouring monitor.
    let mut title: String = name.chars().take(capacity.min(32)).collect();
    if name.chars().nth(capacity.min(32)).is_some() {
        title.push('…');
    }
    let text_clip = glass.shrink(8.0);
    let labels = painter.with_clip_rect(text_clip);
    labels.text(
        glass.center() - egui::vec2(0.0, 9.0),
        egui::Align2::CENTER_CENTER,
        title,
        FontId::proportional(font_size),
        colors.text,
    );
    if glass.height() >= 62.0 {
        labels.text(
            glass.center() + egui::vec2(0.0, 14.0),
            egui::Align2::CENTER_CENTER,
            if local {
                "本机 · 主屏示意"
            } else {
                "对端 · 主屏示意"
            },
            FontId::proportional(11.0),
            colors.muted,
        );
    }
    if active && glass.width() >= 90.0 {
        painter.circle_filled(glass.min + egui::vec2(10.0, 10.0), 3.0, colors.accent);
    }
}
