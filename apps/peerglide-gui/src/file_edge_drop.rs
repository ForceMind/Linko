//! An explicit, one-shot native window for files dropped by other applications.
use super::{cursor_overlay, data_access::Pin, egui, native_drop_panel, App, Page};
use anyhow::{ensure, Context, Result};
use peerglide_platform::display_layout::{Bounds, Layout};
use peerglide_protocol::Edge;
use std::{
    net::SocketAddr,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc, Arc,
    },
    time::{Duration, Instant},
};

const LIFETIME: Duration = Duration::from_secs(60);
const LAYOUT_POLL: Duration = Duration::from_millis(500);
const SIZE: [f32; 2] = [208.0, 72.0];
const INSET: f64 = 48.0;
const ALIGNMENT: f32 = 4.0;

#[derive(Clone, Copy)]
struct Placement {
    origin: [f64; 2],
}

impl Placement {
    fn points(self, ctx: &egui::Context) -> Option<egui::Pos2> {
        cursor_overlay::points(
            self.origin,
            cfg!(windows),
            ctx.pixels_per_point(),
            ctx.zoom_factor(),
        )
    }
}

pub(super) enum Event {
    File(PathBuf),
    Cancelled,
    InvalidFile,
    PositionUnavailable,
}

pub(super) struct Offer {
    pub(super) pin: Pin,
    pub(super) target: SocketAddr,
    edge: Edge,
    layout: Layout,
    zoom: f32,
    expires: Instant,
    next_layout_check: Instant,
    placement: Placement,
    id: egui::ViewportId,
    parent: egui::Context,
    live: Arc<AtomicBool>,
    attempts: Arc<AtomicUsize>,
    sender: mpsc::SyncSender<Event>,
    events: mpsc::Receiver<Event>,
}

impl Offer {
    pub(super) fn arm(
        app: &App,
        ctx: &egui::Context,
        pin: Pin,
        target: SocketAddr,
    ) -> Result<Self> {
        ensure!(eligible(app), "当前状态无法开启边沿发送");
        ensure!(!ctx.embed_viewports(), "当前环境无法打开边沿接收条");
        let edge = configured_edge(app.displayed_edge()).context("当前发送边沿不可用")?;
        let layout = peerglide_platform::read_display_layout().context("无法读取本机显示器")?;
        let zoom = ctx.zoom_factor();
        let placement = placement(&layout, edge, zoom)?;
        let now = Instant::now();
        let (sender, events) = mpsc::sync_channel(1);
        Ok(Self {
            pin,
            target,
            edge,
            layout,
            zoom,
            expires: now + LIFETIME,
            next_layout_check: now + LAYOUT_POLL,
            placement,
            id: egui::ViewportId::from_hash_of(("linko-file-edge-send", now)),
            parent: ctx.clone(),
            live: Arc::new(AtomicBool::new(true)),
            attempts: Arc::new(AtomicUsize::new(0)),
            sender,
            events,
        })
    }

    pub(super) fn current(&self, app: &App, now: Instant) -> bool {
        now < self.expires
            && eligible(app)
            && self.pin.matches(app)
            && self.parent.zoom_factor() == self.zoom
            && configured_edge(app.displayed_edge()) == Some(self.edge)
    }

    pub(super) fn validate_drop(&self, app: &App, now: Instant) -> Result<()> {
        ensure!(self.current(app, now), "边沿发送许可已失效");
        self.validate_layout()
    }

    pub(super) fn check_layout_if_due(&mut self, now: Instant) -> Result<()> {
        if now < self.next_layout_check {
            return Ok(());
        }
        self.next_layout_check = now + LAYOUT_POLL;
        self.validate_layout()
    }

    fn validate_layout(&self) -> Result<()> {
        let layout = peerglide_platform::read_display_layout().context("无法重新读取本机显示器")?;
        ensure!(
            self.layout.same_configuration(&layout),
            "屏幕布局已变化，请重新开启边沿发送"
        );
        Ok(())
    }

    pub(super) fn poll(&self) -> Option<Event> {
        self.events.try_recv().ok()
    }

    pub(super) fn show(&self, ctx: &egui::Context) {
        let Some(position) = self.placement.points(ctx) else {
            finish(
                &self.live,
                &self.sender,
                ctx,
                self.id,
                Event::PositionUnavailable,
            );
            return;
        };
        let builder = egui::ViewportBuilder::default()
            .with_title("屏跃边沿文件发送")
            .with_position(position)
            .with_inner_size(SIZE)
            .with_resizable(false)
            .with_decorations(false)
            .with_has_shadow(false)
            .with_taskbar(false)
            .with_active(false)
            .with_always_on_top()
            .with_drag_and_drop(true);
        let live = self.live.clone();
        let attempts = self.attempts.clone();
        let sender = self.sender.clone();
        let placement = self.placement;
        let expires = self.expires;
        let id = self.id;
        ctx.show_viewport_deferred(self.id, builder, move |ui, class| {
            let ctx = ui.ctx();
            if !live.load(Ordering::Acquire) {
                return;
            }
            if class != egui::ViewportClass::Deferred || Instant::now() >= expires {
                finish(&live, &sender, ctx, id, Event::Cancelled);
                return;
            }
            let info = ui.input(|input| input.viewport().clone());
            if info.close_requested() {
                finish(&live, &sender, ctx, id, Event::Cancelled);
                return;
            }
            let Some(expected) = placement.points(ctx) else {
                finish(&live, &sender, ctx, id, Event::PositionUnavailable);
                return;
            };
            let aligned = info.inner_rect.is_some_and(|rect| aligned(rect, expected));
            if let Some(event) = ui.input(|input| drop_event(&input.raw.dropped_files, aligned)) {
                finish(&live, &sender, ctx, id, event);
                return;
            }
            if !aligned {
                if let Some(rect) = info.inner_rect {
                    if attempts.fetch_add(1, Ordering::Relaxed) >= 2 {
                        finish(&live, &sender, ctx, id, Event::PositionUnavailable);
                        return;
                    }
                    let outer = info.outer_rect.unwrap_or(rect);
                    ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(
                        outer.min + (expected - rect.min),
                    ));
                }
                ctx.request_repaint_after(Duration::from_millis(80));
                ui.centered_and_justified(|ui| ui.label("正在定位屏幕边沿"));
                return;
            }
            ui.painter()
                .rect_filled(ui.max_rect(), 0.0, ui.visuals().panel_fill);
            ui.centered_and_justified(|ui| ui.strong("拖入单个文件发送"));
        });
    }
}

impl Drop for Offer {
    fn drop(&mut self) {
        self.live.store(false, Ordering::Release);
        self.parent
            .send_viewport_cmd_to(self.id, egui::ViewportCommand::Close);
    }
}

fn eligible(app: &App) -> bool {
    app.page == Page::Transfers
        && !app.quit_check_mode
        && !app.resource_check_mode
        && native_drop_panel::eligible(app)
}

fn configured_edge(name: &str) -> Option<Edge> {
    Some(match name {
        "left" => Edge::Left,
        "right" => Edge::Right,
        "top" => Edge::Top,
        "bottom" => Edge::Bottom,
        _ => return None,
    })
}

fn aligned(rect: egui::Rect, expected: egui::Pos2) -> bool {
    (rect.min - expected).abs().max_elem() <= ALIGNMENT
        && (rect.size() - egui::vec2(SIZE[0], SIZE[1]))
            .abs()
            .max_elem()
            <= ALIGNMENT
}

fn drop_event(files: &[egui::DroppedFileHandle], aligned: bool) -> Option<Event> {
    match files {
        [] => None,
        _ if !aligned => Some(Event::PositionUnavailable),
        [file] if file.path().is_absolute() => Some(Event::File(file.path().to_path_buf())),
        _ => Some(Event::InvalidFile),
    }
}

fn finish(
    live: &AtomicBool,
    sender: &mpsc::SyncSender<Event>,
    ctx: &egui::Context,
    id: egui::ViewportId,
    event: Event,
) {
    if live.swap(false, Ordering::AcqRel) {
        let _ = sender.try_send(event);
        ctx.send_viewport_cmd_to(id, egui::ViewportCommand::Visible(false));
        ctx.request_repaint_of(egui::ViewportId::ROOT);
    }
}

fn placement(layout: &Layout, edge: Edge, zoom: f32) -> Result<Placement> {
    let primary = layout
        .displays()
        .iter()
        .find(|display| display.id == layout.primary())
        .context("无法识别本机主屏")?;
    let screen = primary.bounds;
    let scale = f64::from(zoom) * if cfg!(windows) { primary.scale } else { 1.0 };
    ensure!(scale.is_finite() && scale > 0.0, "无法确定本机屏幕缩放");
    let width = f64::from(SIZE[0]) * scale;
    let height = f64::from(SIZE[1]) * scale;
    let inset = INSET * scale;
    ensure!(
        screen.right - screen.left >= width + 2.0 * inset
            && screen.bottom - screen.top >= height + 2.0 * inset,
        "主屏空间不足，无法打开边沿接收条"
    );
    let vertical = matches!(edge, Edge::Left | Edge::Right);
    let along = |bounds: Bounds| {
        if vertical {
            (bounds.top, bounds.bottom)
        } else {
            (bounds.left, bounds.right)
        }
    };
    let (low, high) = along(screen);
    let mut limits = vec![low, high];
    for display in layout
        .displays()
        .iter()
        .filter(|display| display.id != primary.id)
    {
        let bounds = display.bounds;
        ensure!(
            bounds.right <= screen.left
                || bounds.left >= screen.right
                || bounds.bottom <= screen.top
                || bounds.top >= screen.bottom,
            "主屏与其他显示器重叠，无法定位边沿"
        );
        let (start, end) = along(bounds);
        limits.extend([start.clamp(low, high), end.clamp(low, high)]);
    }
    limits.sort_by(f64::total_cmp);
    limits.dedup();
    let mut spans: Vec<(f64, f64)> = Vec::new();
    for limits in limits.windows(2) {
        let (start, end) = (limits[0], limits[1]);
        let middle = (start + end) * 0.5;
        let (x, y) = match edge {
            Edge::Left => (screen.left + 0.5, middle),
            Edge::Right => (screen.right - 0.5, middle),
            Edge::Top => (middle, screen.top + 0.5),
            Edge::Bottom => (middle, screen.bottom - 0.5),
        };
        if layout
            .edge_at(edge, x, y, 1.0)
            .is_some_and(|anchor| anchor.display == primary.id)
        {
            if let Some(previous) = spans.last_mut().filter(|span| span.1 == start) {
                previous.1 = end;
            } else {
                spans.push((start, end));
            }
        }
    }
    let extent = if vertical { height } else { width };
    let middle = (low + high - extent) * 0.5;
    let along = spans
        .into_iter()
        .filter(|(start, end)| end - start >= extent + 2.0 * inset)
        .map(|(start, end)| middle.clamp(start + inset, end - extent - inset))
        .min_by(|a, b| (a - middle).abs().total_cmp(&(b - middle).abs()))
        .context("主屏没有足够的可用发送边沿")?;
    Ok(Placement {
        origin: match edge {
            Edge::Left => [screen.left + inset, along],
            Edge::Right => [screen.right - width - inset, along],
            Edge::Top => [along, screen.top + inset],
            Edge::Bottom => [along, screen.bottom - height - inset],
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use peerglide_platform::display_layout::{Display, DisplayId, Rotation};
    use std::path::Path;

    #[derive(Debug)]
    struct Dropped(PathBuf);

    impl egui::DroppedFile for Dropped {
        fn path(&self) -> &Path {
            &self.0
        }

        fn bytes(&self) -> std::result::Result<Vec<u8>, String> {
            panic!("the native drop gate must not read file contents")
        }
    }

    fn display(id: u64, bounds: [f64; 4]) -> Display {
        Display {
            id: DisplayId(id),
            bounds: Bounds {
                left: bounds[0],
                top: bounds[1],
                right: bounds[2],
                bottom: bounds[3],
            },
            scale: 1.0,
            rotation: Rotation::Upright,
        }
    }

    #[test]
    fn edge_send_uses_an_exposed_span_with_negative_desktop_origins() {
        let primary = display(1, [-1200.0, -200.0, 0.0, 700.0]);
        let neighbor = display(2, [0.0, 0.0, 800.0, 400.0]);
        let layout = Layout::new(primary.id, vec![primary, neighbor]).unwrap();
        let placed = placement(&layout, Edge::Right, 1.0).unwrap();
        assert_eq!(placed.origin, [-256.0, 448.0]);
        let blocker = display(2, [0.0, -200.0, 800.0, 700.0]);
        let layout = Layout::new(primary.id, vec![primary, blocker]).unwrap();
        assert!(placement(&layout, Edge::Right, 1.0).is_err());
        let mirrored = display(3, [-1200.0, -200.0, 0.0, 700.0]);
        let layout = Layout::new(primary.id, vec![primary, mirrored]).unwrap();
        assert!(placement(&layout, Edge::Left, 1.0).is_err());
    }

    #[test]
    fn only_one_absolute_file_is_accepted_by_a_positioned_window() {
        let absolute = PathBuf::from(if cfg!(windows) {
            "C:\\Source\\one.txt"
        } else {
            "/tmp/one.txt"
        });
        let file: egui::DroppedFileHandle = Arc::new(Dropped(absolute.clone()));
        assert!(
            matches!(drop_event(&[file.clone()], true), Some(Event::File(path)) if path == absolute)
        );
        assert!(matches!(
            drop_event(&[file.clone()], false),
            Some(Event::PositionUnavailable)
        ));
        assert!(matches!(
            drop_event(&[file.clone(), file], true),
            Some(Event::InvalidFile)
        ));
        let relative: egui::DroppedFileHandle = Arc::new(Dropped(PathBuf::from("relative.txt")));
        assert!(matches!(
            drop_event(&[relative], true),
            Some(Event::InvalidFile)
        ));
        assert!(drop_event(&[], true).is_none());
    }

    #[test]
    fn a_deferred_drop_consumes_the_offer_once() {
        let ctx = egui::Context::default();
        let (sender, receiver) = mpsc::sync_channel(1);
        let live = AtomicBool::new(true);
        let id = egui::ViewportId::from_hash_of("edge-drop-test");
        let path = PathBuf::from("queued-file");
        finish(&live, &sender, &ctx, id, Event::File(path.clone()));
        finish(&live, &sender, &ctx, id, Event::InvalidFile);
        assert!(matches!(receiver.try_recv(), Ok(Event::File(first)) if first == path));
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn an_armed_offer_expires_or_stops_with_pairing_and_page_changes() -> Result<()> {
        let fixture = super::super::tests::Fixture::new()?;
        let mut app = App::load(fixture.path.clone());
        super::super::Identity::create(&app.identity_dir())?;
        let peer = super::super::Identity::generate()?;
        std::fs::write(app.dir.join("trusted-peer.crt.der"), &peer.certificate)?;
        app.config.fingerprint = Some(peer.fingerprint());
        app.config.edge = "right".into();
        app.page = Page::Transfers;
        let (pin, _, _) = Pin::load(&app)?;
        let now = Instant::now();
        let primary = display(1, [0.0, 0.0, 1200.0, 900.0]);
        let layout = Layout::new(primary.id, vec![primary]).unwrap();
        let (sender, events) = mpsc::sync_channel(1);
        let ctx = egui::Context::default();
        let offer = Offer {
            pin,
            target: "127.0.0.1:24873".parse()?,
            edge: Edge::Right,
            placement: placement(&layout, Edge::Right, ctx.zoom_factor())?,
            layout,
            zoom: ctx.zoom_factor(),
            expires: now + LIFETIME,
            next_layout_check: now + LAYOUT_POLL,
            id: egui::ViewportId::from_hash_of("edge-lifetime-test"),
            parent: ctx,
            live: Arc::new(AtomicBool::new(true)),
            attempts: Arc::new(AtomicUsize::new(0)),
            sender,
            events,
        };
        assert!(offer.current(&app, now));
        assert!(!offer.current(&app, now + LIFETIME));
        app.page = Page::Home;
        assert!(!offer.current(&app, now));
        app.page = Page::Transfers;
        app.config.edge = "left".into();
        assert!(!offer.current(&app, now));
        app.config.edge = "right".into();
        app.config.fingerprint = None;
        assert!(!offer.current(&app, now));
        drop(app);
        fixture.close()?;
        Ok(())
    }
}
