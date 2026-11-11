//! Window shell: title bar with the status capsule and fixed safety controls, the collapsible
//! device sidebar and the back header of subpages. Presentation only: every action goes
//! through the existing App methods and their permission, enrollment and worker gates.
//! Motion follows the design budget; nothing here schedules a repeating repaint.
use super::*;
use desktop_style::{tokens, Tone};
use icons::{Direction, Icon};

/// CI sets LINKO_VERSION (commit time, UTC+08:00); local builds fall back to the crate version.
const DISPLAY_VERSION: &str = match option_env!("LINKO_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};

/// Without an explicit choice, windows at least this wide show the expanded sidebar.
const SIDEBAR_AUTO_EXPAND_MIN_W: f32 = 1100.0;
/// Horizontal inner margin of the sidebar; the collapsed content column is 48 points wide.
pub(super) const SIDEBAR_MARGIN: i8 = 8;
/// Distance from a sidebar row's left edge to its icon center, centered when collapsed.
const ROW_ICON_X: f32 = 24.0;
const ROW_HEIGHT: f32 = 48.0;
const CAPSULE_HEIGHT: f32 = 28.0;
/// Width around a label: both side paddings, the icon and the icon-to-text gap.
const LABEL_PADDING: f32 = tokens::ICON_SMALL + tokens::SPACE_SM + 2.0 * tokens::SPACE_MD;
/// Arriving views and capsules never start fully transparent, so content stays readable.
const MIN_FADE_ALPHA: f32 = 0.35;

impl App {
    /// Sidebar state for this window width: the user's last choice, else wide windows expand.
    pub(super) fn sidebar_expanded(&self, window_width: f32) -> bool {
        self.sidebar_pref
            .unwrap_or(window_width >= SIDEBAR_AUTO_EXPAND_MIN_W)
    }

    fn on_home(&self) -> bool {
        matches!(self.page, Page::Devices | Page::Displays)
    }

    /// Title bar: sidebar switch and brand, status capsule, then the fixed safety controls.
    pub(super) fn title_bar(&mut self, ui: &mut egui::Ui, window_width: f32) {
        ui.spacing_mut().item_spacing.x = tokens::SPACE_SM;
        let expanded = self.sidebar_expanded(window_width);
        ui.allocate_ui_with_layout(
            Vec2::new(ui.available_width(), 40.0),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                let label = if expanded {
                    "收起侧栏"
                } else {
                    "展开侧栏"
                };
                let toggle = icon_button(ui, Icon::Sidebar, label, false);
                self.record_layout_control("nav_sidebar", &toggle, ui.clip_rect());
                if toggle.clicked() {
                    self.sidebar_pref = Some(!expanded);
                }
                desktop_style::brand_mark(ui);
                ui.label(desktop_style::semibold("屏跃", 17.0));
                ui.allocate_ui_with_layout(
                    Vec2::new(ui.available_width(), 40.0),
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| {
                        self.safety_controls(ui);
                        let free = ui.available_rect_before_wrap();
                        self.status_capsule(ui, free, window_width);
                    },
                );
            },
        );
    }

    /// The capsule always carries its short text; the full title and detail are its hover.
    fn status_capsule(&mut self, ui: &mut egui::Ui, free: egui::Rect, window_width: f32) {
        let view = self.workspace_view();
        let ctx = ui.ctx().clone();
        let now = ctx.input(|input| input.time);
        self.capsule_motion
            .observe((view.title, view.tone), now, tokens::CAPSULE_SECS);
        let progress = self.capsule_motion.tick(&ctx);
        let (fill, ink) = desktop_style::status_colors(ui.visuals().dark_mode, view.tone);
        let font = egui::FontId::new(13.0, egui::FontFamily::Name("linko-emphasis".into()));
        let short = view.short.to_owned();
        let galley = ui.painter().layout_no_wrap(short, font, ink);
        let width = galley.size().x + LABEL_PADDING;
        let half = width / 2.0;
        let lowest = free.left() + half;
        let center_x = (window_width / 2.0).clamp(lowest, (free.right() - half).max(lowest));
        let rect = egui::Rect::from_center_size(
            egui::pos2(center_x, free.center().y),
            Vec2::new(width, CAPSULE_HEIGHT),
        );
        let response = ui.interact(
            rect,
            egui::Id::unique("linko-status-capsule"),
            egui::Sense::hover(),
        );
        response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, view.title));
        response.on_hover_text(format!("{}\n{}", view.title, view.detail));
        let mut painter = ui.painter().clone();
        painter.multiply_opacity(view_alpha(progress));
        let scale = motion::capsule_scale(progress);
        let shown = egui::Rect::from_center_size(rect.center(), rect.size() * scale);
        painter.rect_filled(shown, CAPSULE_HEIGHT / 2.0, fill);
        if matches!(view.tone, Tone::Danger) {
            painter.rect_stroke(
                shown,
                CAPSULE_HEIGHT / 2.0,
                egui::Stroke::new(1.5, ink),
                egui::StrokeKind::Inside,
            );
        }
        let glyph = egui::Rect::from_center_size(
            egui::pos2(
                rect.left() + tokens::SPACE_MD + tokens::ICON_SMALL / 2.0,
                rect.center().y,
            ),
            Vec2::splat(tokens::ICON_SMALL),
        );
        let spinning = view.is_waiting() && self.activity_tick_active();
        paint_capsule_glyph(&painter, glyph, ink, view.tone, spinning, now);
        let text = egui::pos2(
            glyph.right() + tokens::SPACE_SM,
            rect.center().y - galley.size().y / 2.0,
        );
        painter.galley(text, galley, ink);
    }

    /// Collapsible device list with the add entry and the bottom shortcut and settings icons.
    pub(super) fn sidebar(&mut self, ui: &mut egui::Ui, mix: f32) {
        let alpha = motion::sidebar_label_alpha(mix);
        let expanded = mix >= 0.5;
        ui.spacing_mut().item_spacing.y = tokens::SPACE_XS;
        ui.add_space(tokens::SPACE_MD);
        let registered = self.config.fingerprint.is_some();
        let local = desktop_ui::local_device_name();
        let local_selected = self.on_home() && (!self.workspace_peer_selected || !registered);
        let row = device_row(ui, local, "本机", Tone::Accent, local_selected, alpha);
        self.record_layout_control("nav_layout", &row, ui.clip_rect());
        if row.clicked() {
            self.workspace_peer_selected = false;
            self.device_tab = DeviceTab::Device;
            self.navigate_to(Page::Displays);
        }
        if registered {
            let (status, tone) = if self.pairing_pending() {
                ("核验尚未完成", Tone::Warning)
            } else if self.connected && self.child.is_some() {
                ("已连接", Tone::Good)
            } else {
                ("未连接", Tone::Neutral)
            };
            let name = self.config.peer_name.clone();
            let selected = self.on_home() && self.workspace_peer_selected;
            if device_row(ui, &name, status, tone, selected, alpha).clicked() {
                self.workspace_peer_selected = true;
                self.device_tab = DeviceTab::Device;
                self.navigate_to(Page::Displays);
            }
        }
        ui.add_space(tokens::SPACE_SM);
        let add_label = if registered {
            "重新核验"
        } else {
            "添加电脑"
        };
        let add = ui.add_enabled(self.idle_writable(), |ui: &mut egui::Ui| {
            icon_button(ui, Icon::Plus, add_label, expanded)
        });
        if add.clicked() {
            self.open_setup();
        }
        let align = if expanded {
            egui::Align::LEFT
        } else {
            egui::Align::Center
        };
        ui.allocate_ui_with_layout(ui.available_size(), egui::Layout::bottom_up(align), |ui| {
            if expanded {
                let muted = tokens::faded(tokens::muted_color(ui.visuals().dark_mode), alpha);
                let version = format!("{DISPLAY_VERSION} · 开发预览");
                ui.label(RichText::new(version).small().color(muted));
            }
            ui.separator();
            if expanded {
                ui.horizontal(|ui| self.sidebar_entries(ui, true));
            } else {
                self.sidebar_entries(ui, false);
            }
        });
    }

    fn sidebar_entries(&mut self, ui: &mut egui::Ui, expanded: bool) {
        let shortcuts = (Page::Shortcuts, Icon::Keyboard, "快捷键", "nav_shortcuts");
        let settings = (Page::Settings, Icon::Sliders, "设置", "nav_settings");
        // Expanded rows read left to right; the collapsed column is laid out from the bottom.
        let entries = if expanded {
            [shortcuts, settings]
        } else {
            [settings, shortcuts]
        };
        for (page, icon, label, key) in entries {
            let selected = match page {
                Page::Shortcuts => matches!(
                    self.page,
                    Page::Shortcuts | Page::Home | Page::GestureLab | Page::GestureRules
                ),
                _ => self.page == page,
            };
            let response = icon_control(ui, icon, label, expanded, selected, false);
            self.record_layout_control(key, &response, ui.clip_rect());
            if response.clicked() {
                self.navigate_to(page);
                self.device_tab = DeviceTab::Device;
            }
        }
    }

    /// Back arrow and title of a subpage. The arrow returns to `back`.
    pub(super) fn subpage_header(&mut self, ui: &mut egui::Ui, title: &str, back: Page) {
        ui.horizontal(|ui| {
            if icon_button(ui, Icon::Arrow(Direction::Left), "返回", true).clicked() {
                self.navigate_to(back);
            }
            ui.heading(desktop_style::heading_text(title));
        });
        ui.add_space(tokens::SPACE_LG);
    }
}

/// Key of a page for the view switch motion; aliases of one view share a key.
pub(super) fn view_key(page: Page) -> u8 {
    match page {
        Page::Devices | Page::Displays => 0,
        Page::Home => 1,
        Page::Transfers | Page::Clipboard => 2,
        Page::Shortcuts => 3,
        Page::Settings => 4,
        Page::GestureLab | Page::GestureRules => 5,
    }
}

/// Opacity of an arriving view at `progress`, never below the readability floor.
pub(super) fn view_alpha(progress: f32) -> f32 {
    motion::fade_alpha(progress).max(MIN_FADE_ALPHA)
}

/// Framed button with an icon and, when `show_label`, its text. An icon-only button keeps
/// the label as its accessible name and hover text.
pub(super) fn icon_button(
    ui: &mut egui::Ui,
    icon: Icon,
    label: &str,
    show_label: bool,
) -> egui::Response {
    icon_control(ui, icon, label, show_label, false, true)
}

fn icon_control(
    ui: &mut egui::Ui,
    icon: Icon,
    label: &str,
    show_label: bool,
    selected: bool,
    framed: bool,
) -> egui::Response {
    let ink = if selected {
        desktop_style::selection_colors(ui).1
    } else {
        ui.visuals().text_color()
    };
    let font = egui::TextStyle::Button.resolve(ui.style());
    let galley = if show_label {
        Some(ui.painter().layout_no_wrap(label.to_owned(), font, ink))
    } else {
        None
    };
    let height = ui.spacing().interact_size.y;
    let width = match &galley {
        Some(galley) => galley.size().x + LABEL_PADDING,
        None => height,
    };
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, height), egui::Sense::click());
    let enabled = ui.is_enabled();
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, label));
    if ui.is_rect_visible(rect) {
        paint_control_surface(ui, &response, rect, selected, framed);
        let icon_center = if galley.is_some() {
            egui::pos2(
                rect.left() + tokens::SPACE_MD + tokens::ICON_SMALL / 2.0,
                rect.center().y,
            )
        } else {
            rect.center()
        };
        let icon_rect = egui::Rect::from_center_size(icon_center, Vec2::splat(tokens::ICON_SMALL));
        icon.paint(ui.painter(), icon_rect, ink);
        if let Some(galley) = galley {
            let text = egui::pos2(
                icon_rect.right() + tokens::SPACE_SM,
                rect.center().y - galley.size().y / 2.0,
            );
            ui.painter().galley(text, galley, ink);
        }
    }
    if show_label {
        response
    } else {
        response.on_hover_text(label)
    }
}

/// One sidebar device: icon with a status badge, then name and status once labels show.
fn device_row(
    ui: &mut egui::Ui,
    name: &str,
    status: &str,
    tone: Tone,
    selected: bool,
    label_alpha: f32,
) -> egui::Response {
    let size = Vec2::new(ui.available_width(), ROW_HEIGHT);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    let enabled = ui.is_enabled();
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::Role::Button, enabled, format!("{name} · {status}"))
    });
    if ui.is_rect_visible(rect) {
        paint_control_surface(ui, &response, rect, selected, false);
        let dark = ui.visuals().dark_mode;
        let ink = if selected {
            desktop_style::selection_colors(ui).1
        } else {
            ui.visuals().text_color()
        };
        let icon_center = egui::pos2(rect.left() + ROW_ICON_X, rect.center().y);
        let icon_rect = egui::Rect::from_center_size(icon_center, Vec2::splat(tokens::ICON_MEDIUM));
        let painter = ui.painter();
        Icon::Device.paint(painter, icon_rect, ink);
        // Badge with a cut-out ring in the panel color, so it stays legible over the icon.
        let badge = icon_rect.right_bottom() + egui::vec2(-2.0, -3.0);
        painter.circle_filled(badge, 4.5, ui.visuals().panel_fill);
        painter.circle_filled(badge, 3.0, desktop_style::status_colors(dark, tone).1);
        if label_alpha > 0.0 {
            let mut labels = painter.with_clip_rect(rect.intersect(ui.clip_rect()));
            labels.multiply_opacity(label_alpha);
            let mut title: String = name.chars().take(16).collect();
            if name.chars().nth(16).is_some() {
                title.push('…');
            }
            let font = egui::FontId::new(14.0, egui::FontFamily::Name("linko-emphasis".into()));
            let title = labels.layout_no_wrap(title, font, ink);
            let muted = tokens::muted_color(dark);
            let small = egui::FontId::proportional(12.0);
            let status = labels.layout_no_wrap(status.to_owned(), small, muted);
            let x = icon_rect.right() + tokens::SPACE_MD;
            let title_top = rect.center().y - title.size().y;
            labels.galley(egui::pos2(x, title_top), title, ink);
            labels.galley(egui::pos2(x, rect.center().y + 1.0), status, muted);
        }
    }
    response.on_hover_text(format!("{name}\n{status}"))
}

/// Hover (M1), selection (M2) and focus feedback shared by the shell controls.
fn paint_control_surface(
    ui: &egui::Ui,
    response: &egui::Response,
    rect: egui::Rect,
    selected: bool,
    framed: bool,
) {
    let ctx = ui.ctx();
    let dark = ui.visuals().dark_mode;
    let widgets = &ui.visuals().widgets;
    let hovered = response.hovered() && ui.is_enabled();
    let hover = motion::hover_mix(ctx, response.id.with("hover"), hovered);
    let select = motion::select_mix(ctx, response.id.with("select"), selected);
    let rest = if framed {
        widgets.inactive.weak_bg_fill
    } else {
        egui::Color32::TRANSPARENT
    };
    let mut fill = mix(rest, widgets.hovered.weak_bg_fill, hover);
    fill = mix(fill, desktop_style::selection_colors(ui).0, select);
    if response.is_pointer_button_down_on() {
        fill = widgets.active.weak_bg_fill;
    }
    let radius = f32::from(tokens::RADIUS_CONTROL);
    let painter = ui.painter();
    painter.rect_filled(rect, radius, fill);
    let accent = tokens::accent_color(dark);
    let stroke = if response.has_focus() {
        egui::Stroke::new(1.5, accent)
    } else if framed {
        egui::Stroke::new(1.0, mix(tokens::line_color(dark), accent, hover))
    } else {
        egui::Stroke::NONE
    };
    painter.rect_stroke(rect.shrink(0.5), radius, stroke, egui::StrokeKind::Inside);
    if selected && !framed {
        let marker = egui::Rect::from_min_size(
            rect.min + egui::vec2(0.0, tokens::SPACE_SM),
            egui::vec2(3.0, rect.height() - 2.0 * tokens::SPACE_SM),
        );
        painter.rect_filled(marker, 1.5, tokens::faded(accent, select));
    }
}

/// Shape per tone, so color is never the only cue (spec 4.3). Waiting states spin only
/// while an existing activity beat repaints the window.
fn paint_capsule_glyph(
    painter: &egui::Painter,
    rect: egui::Rect,
    ink: egui::Color32,
    tone: Tone,
    spinning: bool,
    now: f64,
) {
    if spinning {
        icons::paint_spinner(painter, rect, ink, motion::spinner_head_at(now));
        return;
    }
    match tone {
        Tone::Good => Icon::Check.paint(painter, rect, ink),
        Tone::Warning | Tone::Danger => Icon::Warning.paint(painter, rect, ink),
        Tone::Accent => {
            painter.circle_filled(rect.center(), rect.width() * 0.22, ink);
        }
        Tone::Neutral => {
            let stroke = egui::Stroke::new(icons::STROKE_WIDTH, ink);
            painter.circle_stroke(rect.center(), rect.width() * 0.25, stroke);
        }
    }
}

/// Blend of two colors, `t` from 0 (`from`) to 1 (`to`).
fn mix(from: egui::Color32, to: egui::Color32, t: f32) -> egui::Color32 {
    let t = motion::unit(t);
    let channel = |a: u8, b: u8| motion::lerp(f32::from(a), f32::from(b), t).round() as u8;
    egui::Color32::from_rgba_premultiplied(
        channel(from.r(), to.r()),
        channel(from.g(), to.g()),
        channel(from.b(), to.b()),
        channel(from.a(), to.a()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_mix_hits_both_ends_and_clamps() {
        let from = egui::Color32::from_rgb(10, 20, 30);
        let to = egui::Color32::from_rgb(110, 220, 130);
        assert_eq!(mix(from, to, 0.0), from);
        assert_eq!(mix(from, to, 1.0), to);
        assert_eq!(mix(from, to, -2.0), from);
        assert_eq!(mix(from, to, 9.0), to);
        assert_eq!(mix(from, to, 0.5), egui::Color32::from_rgb(60, 120, 80));
    }

    fn run_frames(app: &mut App, ctx: &egui::Context, time: &mut f64, count: usize) {
        for _ in 0..count {
            let input = egui::RawInput {
                time: Some(*time),
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    Vec2::new(1280.0, 800.0),
                )),
                ..Default::default()
            };
            ctx.run_ui(input, |root| app.desktop(root))
                .drop_without_applying_deltas();
            *time += 1.0 / 60.0;
        }
    }

    #[test]
    fn shell_motion_settles_without_periodic_repaint_or_side_effects() -> Result<()> {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let fixture = crate::tests::Fixture::new()?;
        let mut app = App::load(fixture.path.clone());
        let original = app.config.clone();
        let ctx = crate::tests::ui_context();
        app.theme(&ctx);
        let mut time = 0.0;
        run_frames(&mut app, &ctx, &mut time, 20);
        let requests = std::sync::Arc::new(AtomicUsize::new(0));
        let observed = std::sync::Arc::clone(&requests);
        ctx.set_request_repaint_callback(move |request| {
            if request.delay < std::time::Duration::from_secs(1) {
                observed.fetch_add(1, Ordering::SeqCst);
            }
        });
        // Idle: nothing animates, so nothing asks for another frame.
        run_frames(&mut app, &ctx, &mut time, 60);
        assert_eq!(requests.load(Ordering::SeqCst), 0);
        // Collapse the sidebar and switch view: both play once and then stop.
        app.sidebar_pref = Some(false);
        app.navigate_to(Page::Settings);
        run_frames(&mut app, &ctx, &mut time, 60);
        assert!(requests.load(Ordering::SeqCst) > 0, "motion never ran");
        run_frames(&mut app, &ctx, &mut time, 10);
        let settled = requests.load(Ordering::SeqCst);
        run_frames(&mut app, &ctx, &mut time, 120);
        assert_eq!(requests.load(Ordering::SeqCst), settled);
        assert!(app.page == Page::Settings && !app.sidebar_expanded(1280.0));
        assert!(app.config == original && app.child.is_none() && !app.pairing_open);
        assert!(
            !fixture.path.join("identity").exists() && !fixture.path.join("settings.json").exists()
        );
        drop(app);
        fixture.close()?;
        Ok(())
    }

    #[test]
    fn view_switch_keys_group_aliases_and_fade_never_hides_content() {
        assert_eq!(view_key(Page::Devices), view_key(Page::Displays));
        assert_eq!(view_key(Page::Transfers), view_key(Page::Clipboard));
        assert_eq!(view_key(Page::GestureLab), view_key(Page::GestureRules));
        assert_ne!(view_key(Page::Settings), view_key(Page::Displays));
        assert_ne!(view_key(Page::Shortcuts), view_key(Page::Home));
        assert_eq!(view_alpha(0.0), MIN_FADE_ALPHA);
        assert_eq!(view_alpha(1.0), 1.0);
    }
}
