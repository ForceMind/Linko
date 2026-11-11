//! Original desktop presentation: quiet surfaces, compact status and device art.
//! No runtime state, native queries, external assets or repaint timers live here.
use eframe::egui::{self, Color32, FontId, RichText, Stroke, TextStyle};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
    visuals.widgets.inactive.bg_fill = colors.surface;
    visuals.widgets.inactive.weak_bg_fill = colors.surface;
    visuals.widgets.hovered.bg_fill = colors.hover;
    visuals.widgets.hovered.weak_bg_fill = colors.hover;
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, colors.accent);
    visuals.widgets.active.bg_fill = colors.hover;
    visuals.widgets.active.weak_bg_fill = colors.hover;
    visuals.widgets.active.bg_stroke = Stroke::new(1.5, colors.accent);
    visuals.widgets.open.bg_fill = colors.hover;
    visuals.widgets.open.weak_bg_fill = colors.hover;
    style.visuals = visuals;
    style.spacing.item_spacing = egui::vec2(12.0, 8.0);
    style.spacing.button_padding = egui::vec2(14.0, 8.0);
    style.spacing.interact_size = egui::vec2(36.0, 36.0);
    style.spacing.window_margin = egui::Margin::same(18);
    style.spacing.menu_margin = egui::Margin::same(8);
    style.text_styles.insert(
        TextStyle::Heading,
        FontId::new(28.0, egui::FontFamily::Name("linko-emphasis".into())),
    );
    style
        .text_styles
        .insert(TextStyle::Body, FontId::proportional(15.0));
    style
        .text_styles
        .insert(TextStyle::Button, FontId::proportional(15.0));
    style
        .text_styles
        .insert(TextStyle::Small, FontId::proportional(13.0));
    style
        .text_styles
        .insert(TextStyle::Monospace, FontId::monospace(13.0));
    ctx.set_style_of(theme, style);
    ctx.set_theme(theme);
}

pub(super) fn heading_text(text: impl Into<String>) -> RichText {
    RichText::new(text.into()).font(FontId::new(
        28.0,
        egui::FontFamily::Name("linko-emphasis".into()),
    ))
}

pub(super) fn semibold(text: impl Into<String>, size: f32) -> RichText {
    RichText::new(text.into()).font(FontId::new(
        size,
        egui::FontFamily::Name("linko-emphasis".into()),
    ))
}

pub(super) fn status_text(text: &str, dark: bool, tone: Tone) -> RichText {
    RichText::new(text)
        .size(14.0)
        .color(tone_colors(dark, tone).1)
}

/// Small original dual-screen mark, painted locally without assets or timers.
pub(super) fn brand_mark(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(36.0, 36.0), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    let color = if ui.visuals().dark_mode {
        Color32::from_rgb(177, 166, 255)
    } else {
        Color32::from_rgb(101, 84, 232)
    };
    let polygons = [
        [
            [54.0, 87.0],
            [111.0, 49.0],
            [199.0, 106.0],
            [165.0, 129.0],
            [111.0, 94.0],
            [87.0, 111.0],
        ],
        [
            [202.0, 169.0],
            [145.0, 207.0],
            [57.0, 150.0],
            [91.0, 127.0],
            [145.0, 162.0],
            [169.0, 145.0],
        ],
    ];
    for points in polygons {
        let mut mesh = egui::epaint::Mesh::default();
        for point in points {
            mesh.vertices.push(egui::epaint::Vertex {
                pos: rect.min + egui::vec2(point[0], point[1]) * (rect.width() / 256.0),
                uv: egui::epaint::WHITE_UV,
                color,
            });
        }
        mesh.indices
            .extend_from_slice(&[0, 1, 4, 1, 2, 3, 1, 3, 4, 0, 4, 5]);
        painter.add(egui::Shape::mesh(mesh));
    }
}

pub(super) fn card(ui: &egui::Ui) -> egui::Frame {
    let colors = palette(ui.visuals().dark_mode);
    egui::Frame::group(ui.style())
        .fill(colors.surface)
        .stroke(Stroke::new(1.0, colors.line))
        .corner_radius(egui::CornerRadius::same(14))
        .inner_margin(18.0)
        .shadow(egui::epaint::Shadow {
            offset: [0, 2],
            blur: 14,
            spread: 0,
            color: Color32::from_black_alpha(if ui.visuals().dark_mode { 18 } else { 8 }),
        })
}

/// A quiet, single-layer wash; no second rounded panel sits behind the hardware.
pub(super) fn canvas_wash(painter: &egui::Painter, rect: egui::Rect, dark: bool) {
    let colors = palette(dark);
    let mut mesh = egui::Mesh::default();
    for band in 0..24 {
        let middle = 1.0 - ((band as f32 + 0.5) / 12.0 - 1.0).abs();
        let blend = |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * middle) as u8;
        let color = Color32::from_rgb(
            blend(colors.surface.r(), colors.canvas.r()),
            blend(colors.surface.g(), colors.canvas.g()),
            blend(colors.surface.b(), colors.canvas.b()),
        );
        mesh.add_colored_rect(
            egui::Rect::from_min_max(
                rect.min + egui::vec2(0.0, rect.height() * band as f32 / 24.0),
                egui::pos2(
                    rect.max.x,
                    rect.min.y + rect.height() * (band + 1) as f32 / 24.0,
                ),
            ),
            color,
        );
    }
    painter.add(egui::Shape::mesh(mesh));
}

pub(super) fn status_colors(dark: bool, tone: Tone) -> (Color32, Color32) {
    tone_colors(dark, tone)
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
    egui::Button::new(semibold(label, 15.0).color(Color32::WHITE))
        .fill(Color32::from_rgb(66, 99, 235))
        .stroke(Stroke::new(1.0, Color32::from_rgb(54, 81, 204)))
        .corner_radius(egui::CornerRadius::same(9))
        .min_size(egui::vec2(120.0, 38.0))
}

pub(super) fn selection_colors(ui: &egui::Ui) -> (Color32, Color32) {
    tone_colors(ui.visuals().dark_mode, Tone::Accent)
}

/// Pure design tokens for the interface redesign: constants and side-effect free functions.
/// Existing widgets keep their literal values until the slice that migrates them lands.
/// The shell, canvas, context column, pairing window and dialogs consume these tokens. The
/// few items still marked `allow(dead_code)` have no caller outside tests; each says why.
pub(super) mod tokens {
    use super::palette;
    use eframe::egui::Color32;

    /// Hover fill and outline fade (M1), seconds.
    pub(crate) const HOVER_SECS: f32 = 0.08;
    /// Selection fill, selection ring and focus ring fade (M2), seconds.
    pub(crate) const SELECT_SECS: f32 = 0.12;
    /// Disclosure expand and collapse, seconds.
    pub(crate) const EXPAND_SECS: f32 = 0.16;
    /// Sidebar width change (M3), seconds.
    pub(crate) const SIDEBAR_SECS: f32 = 0.20;
    /// Page and view switch slide plus fade (M4), seconds.
    pub(crate) const VIEW_SWITCH_SECS: f32 = 0.16;
    /// Title bar status capsule change (M5), seconds.
    pub(crate) const CAPSULE_SECS: f32 = 0.18;
    /// Status badge pop-in (M6), seconds.
    pub(crate) const BADGE_POP_SECS: f32 = 0.20;
    /// Display rebound after a drag is released (M7), seconds.
    pub(crate) const REBOUND_SECS: f32 = 0.22;
    /// Shake after a rejected code or address (M8), seconds.
    pub(crate) const SHAKE_SECS: f32 = 0.36;
    /// Check mark and cross stroke drawing (M9), seconds.
    pub(crate) const STROKE_DRAW_SECS: f32 = 0.24;
    /// Dialog height change (M10), seconds.
    pub(crate) const RESIZE_SECS: f32 = 0.20;
    /// List item entering or leaving (M12), seconds.
    pub(crate) const LIST_PRESENCE_SECS: f32 = 0.18;
    /// Upper bound for everything one interaction may play, seconds.
    // Read by the budget test below; no screen needs the bound at run time.
    #[allow(dead_code)]
    pub(crate) const MAX_INTERACTION_SECS: f32 = 0.36;
    /// Spinner step (M11). Matches the existing 100 ms activity tick; it is not a timer.
    pub(crate) const SPINNER_STEP_SECS: f32 = 0.1;
    /// Number of cells in the discrete spinner (M11).
    pub(crate) const SPINNER_CELLS: usize = 8;

    pub(crate) const SIDEBAR_EXPANDED_W: f32 = 240.0;
    pub(crate) const SIDEBAR_COLLAPSED_W: f32 = 64.0;
    /// Horizontal travel of a view switch slide, in points.
    pub(crate) const VIEW_SLIDE_PX: f32 = 12.0;

    pub(crate) const ICON_SMALL: f32 = 16.0;
    pub(crate) const ICON_MEDIUM: f32 = 20.0;
    pub(crate) const ICON_STROKE: f32 = 1.4;

    pub(crate) const RADIUS_CHIP: u8 = 8;
    pub(crate) const RADIUS_CONTROL: u8 = 9;
    // Menu and pill radii of the design scale; no menu is painted by hand yet and the status
    // capsule rounds by its own height.
    #[allow(dead_code)]
    pub(crate) const RADIUS_MENU: u8 = 10;
    #[allow(dead_code)]
    pub(crate) const RADIUS_PILL: u8 = 12;
    pub(crate) const RADIUS_CARD: u8 = 14;

    pub(crate) const SPACE_XS: f32 = 4.0;
    pub(crate) const SPACE_SM: f32 = 8.0;
    pub(crate) const SPACE_MD: f32 = 12.0;
    pub(crate) const SPACE_LG: f32 = 18.0;

    pub(crate) fn accent_color(dark: bool) -> Color32 {
        palette(dark).accent
    }

    pub(crate) fn line_color(dark: bool) -> Color32 {
        palette(dark).line
    }

    pub(crate) fn muted_color(dark: bool) -> Color32 {
        palette(dark).muted
    }

    // Widgets read text color from `Visuals`, which `install` fills from the same palette.
    #[allow(dead_code)]
    pub(crate) fn text_color(dark: bool) -> Color32 {
        palette(dark).text
    }

    pub(crate) fn surface_color(dark: bool) -> Color32 {
        palette(dark).surface
    }

    // The canvas paints its wash through `canvas_wash`; the theme test reads this one.
    #[allow(dead_code)]
    pub(crate) fn canvas_color(dark: bool) -> Color32 {
        palette(dark).canvas
    }

    /// `color` faded by `alpha` (0 is invisible, 1 is unchanged).
    pub(crate) fn faded(color: Color32, alpha: f32) -> Color32 {
        color.gamma_multiply(alpha.clamp(0.0, 1.0))
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn one_shot_durations_fit_the_interaction_budget() {
            let durations = [
                VIEW_SWITCH_SECS,
                CAPSULE_SECS,
                BADGE_POP_SECS,
                REBOUND_SECS,
                SHAKE_SECS,
                STROKE_DRAW_SECS,
                RESIZE_SECS,
                LIST_PRESENCE_SECS,
            ];
            for secs in durations {
                assert!(secs > 0.0 && secs <= MAX_INTERACTION_SECS, "{secs}");
            }
        }

        #[test]
        fn fading_scales_alpha_without_leaving_the_range() {
            let color = Color32::from_rgb(10, 20, 30);
            assert_eq!(faded(color, 1.0), color);
            assert_eq!(faded(color, 2.0), color);
            assert_eq!(faded(color, 0.0).a(), 0);
            assert_eq!(faded(color, -1.0).a(), 0);
        }

        #[test]
        fn palette_colors_follow_the_theme() {
            assert_ne!(canvas_color(false), canvas_color(true));
            assert_ne!(accent_color(false), accent_color(true));
            assert_eq!(surface_color(false), Color32::WHITE);
        }
    }
}
