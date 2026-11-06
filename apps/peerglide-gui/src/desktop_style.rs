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
            canvas: Color32::from_rgb(20, 24, 31),
            surface: Color32::from_rgb(29, 34, 43),
            soft: Color32::from_rgb(39, 47, 60),
            hover: Color32::from_rgb(49, 61, 78),
            text: Color32::from_rgb(233, 237, 244),
            muted: Color32::from_rgb(165, 177, 194),
            line: Color32::from_rgb(68, 80, 97),
            accent: Color32::from_rgb(147, 189, 255),
        }
    } else {
        Palette {
            canvas: Color32::from_rgb(244, 246, 250),
            surface: Color32::WHITE,
            soft: Color32::from_rgb(235, 239, 246),
            hover: Color32::from_rgb(225, 232, 244),
            text: Color32::from_rgb(32, 41, 55),
            muted: Color32::from_rgb(89, 103, 121),
            line: Color32::from_rgb(214, 222, 233),
            accent: Color32::from_rgb(43, 91, 190),
        }
    }
}

fn tone_colors(dark: bool, tone: Tone) -> (Color32, Color32) {
    let (fill, text) = match (dark, tone) {
        (false, Tone::Neutral) => ((232, 237, 244), (68, 84, 106)),
        (false, Tone::Good) => ((226, 244, 237), (30, 103, 78)),
        (false, Tone::Warning) => ((255, 243, 213), (128, 82, 8)),
        (false, Tone::Danger) => ((253, 233, 235), (160, 44, 60)),
        (false, Tone::Accent) => ((230, 239, 255), (38, 81, 168)),
        (true, Tone::Neutral) => ((48, 57, 71), (205, 215, 230)),
        (true, Tone::Good) => ((29, 63, 53), (145, 222, 186)),
        (true, Tone::Warning) => ((74, 56, 26), (244, 204, 127)),
        (true, Tone::Danger) => ((76, 36, 43), (255, 173, 183)),
        (true, Tone::Accent) => ((32, 53, 88), (174, 206, 255)),
    };
    (
        Color32::from_rgb(fill.0, fill.1, fill.2),
        Color32::from_rgb(text.0, text.1, text.2),
    )
}

/// Apply once at startup/theme changes. Keep the existing Chinese font setup.
pub(super) fn install(ctx: &egui::Context, dark: bool) {
    let colors = palette(dark);
    let mut style = (*ctx.style()).clone();
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
    visuals.selection.bg_fill = Color32::from_rgb(45, 94, 196);
    visuals.selection.stroke = Stroke::new(1.0, Color32::WHITE);

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
    style.text_styles.insert(TextStyle::Heading, FontId::proportional(24.0));
    style.text_styles.insert(TextStyle::Body, FontId::proportional(14.0));
    style.text_styles.insert(TextStyle::Button, FontId::proportional(14.0));
    style.text_styles.insert(TextStyle::Small, FontId::proportional(12.0));
    style.text_styles.insert(TextStyle::Monospace, FontId::monospace(13.0));
    ctx.set_style(style);
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
    egui::Button::new(RichText::new(label.to_owned()).size(14.0).strong().color(Color32::WHITE))
        .fill(Color32::from_rgb(45, 94, 196))
        .stroke(Stroke::new(1.0, Color32::from_rgb(38, 80, 170)))
        .corner_radius(egui::CornerRadius::same(9))
        .min_size(egui::vec2(112.0, 36.0))
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
    if ![rect.min.x, rect.min.y, rect.max.x, rect.max.y].iter().all(|v| v.is_finite())
        || !rect.width().is_finite()
        || !rect.height().is_finite()
        || rect.width() < 64.0
        || rect.height() < 64.0
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
    painter.rect_stroke(screen, 10.0, Stroke::new(if active { 2.0 } else { 1.2 }, line), egui::StrokeKind::Inside);
    let glass = screen.shrink(5.0);
    let glass_color = match (dark, local) {
        (false, true) => Color32::from_rgb(235, 242, 252),
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
            if local { "本机 · 主屏示意" } else { "对端 · 主屏示意" },
            FontId::proportional(11.0),
            colors.muted,
        );
    }
    if active && glass.width() >= 90.0 {
        painter.circle_filled(glass.min + egui::vec2(10.0, 10.0), 3.0, colors.accent);
    }
}
