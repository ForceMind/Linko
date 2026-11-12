//! Local multi-device layout draft and export for the configured sender.
use super::{
    config_store::{self, Store},
    data_access, display_inventory, egui, transport, App, Identity,
};
use anyhow::{ensure, Context, Result};
use peerglide_platform::{
    display_layout::Layout,
    display_routes::{Link, Span, Table, Target, MAX_LINKS},
};
use peerglide_protocol::{
    route_plan::{PeerSpec, Plan, RouteSpec, VERSION},
    Edge,
};
use std::{
    collections::BTreeSet,
    io::Write,
    net::SocketAddr,
    path::{Path, PathBuf},
};
use transport::trusted_listener::MAX_TRUSTED_PEERS;

#[derive(Default)]
pub(super) struct Panel {
    local: Option<Result<Draft>>,
    peer: PeerForm,
    route: RouteForm,
    export_name: String,
}

struct Draft {
    store: Store,
    plan: Plan,
}

impl Draft {
    fn open(path: PathBuf) -> Result<Self> {
        let (store, plan) = Store::open::<Plan>(path)?;
        validate_shape(&plan)?;
        Ok(Self { store, plan })
    }

    fn save(&mut self, plan: Plan) -> Result<()> {
        validate_shape(&plan)?;
        self.store.save(&plan)?;
        self.plan = plan;
        Ok(())
    }
}

impl Panel {
    pub(super) fn ui(&mut self, app: &mut App, ui: &mut egui::Ui) {
        ui.collapsing("多电脑分段布局", |ui| {
            let mut local = self
                .local
                .take()
                .unwrap_or_else(|| Draft::open(app.dir.join("route-draft.json")));
            match &mut local {
                Ok(draft) => self.draw(app, ui, draft),
                Err(error) => {
                    ui.label(format!("布局草稿只读，原文件已保留。{error:#}"));
                }
            }
            self.local = Some(local);
        });
    }

    fn draw(&mut self, app: &mut App, ui: &mut egui::Ui, draft: &mut Draft) {
        let editable = app.idle_writable() && !app.pairing_pending() && !app.enrollment_blocked;
        ui.add_enabled_ui(editable, |ui| {
            self.draw_peers(app, ui, draft);
            if let Some(layout) = app.display_inventory.snapshot() {
                self.draw_routes(app, ui, draft, &layout);
                ui.horizontal_wrapped(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.export_name)
                            .hint_text("routes.json")
                            .char_limit(64),
                    );
                    if ui
                        .add_enabled(
                            !draft.plan.routes.is_empty(),
                            egui::Button::new("导出布局文件"),
                        )
                        .clicked()
                    {
                        let name = self.export_name.trim();
                        let name = if name.is_empty() { "routes.json" } else { name };
                        match export(app, &draft.plan, &layout, name) {
                            Ok(path) => {
                                app.notice = format!(
                                    "已导出 {}，可交给 connect-routes --layout。",
                                    path.display()
                                )
                            }
                            Err(error) => app.notice = format!("无法导出分段布局：{error:#}"),
                        }
                    }
                });
            } else {
                ui.label("请先在“本机显示器”读取当前布局。");
            }
        });
    }

    fn draw_peers(&mut self, app: &mut App, ui: &mut egui::Ui, draft: &mut Draft) {
        let mut remove = None;
        for (index, peer) in draft.plan.peers.iter().enumerate() {
            ui.push_id(("route-peer", index), |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(peer.address.to_string());
                    if ui.small_button("删除电脑及其分段").clicked() {
                        remove = Some(index);
                    }
                })
            });
        }
        if let Some(index) = remove {
            let mut next = draft.plan.clone();
            next.peers.remove(index);
            next.routes.retain(|route| route.peer != index);
            for route in &mut next.routes {
                if route.peer > index {
                    route.peer -= 1;
                }
            }
            let result = draft.save(next);
            if result.is_ok() {
                self.route = RouteForm::default();
            }
            app.feedback(result, "已移除该电脑及其分段。");
        }
        ui.collapsing("添加目标电脑", |ui| {
            if ui
                .add_enabled(
                    app.config.fingerprint.is_some(),
                    egui::Button::new("填入当前已登记电脑"),
                )
                .clicked()
            {
                match app
                    .ensure_pairing_ready()
                    .and_then(|()| app.verified_peer_certificate())
                {
                    Ok(path) => {
                        self.peer.certificate = path.display().to_string();
                        self.peer.address = app.config.address.clone();
                        self.peer.fingerprint = app.config.fingerprint.clone().unwrap_or_default();
                    }
                    Err(error) => app.notice = format!("无法读取已登记电脑：{error:#}"),
                }
            }
            text(ui, "地址和端口", &mut self.peer.address, 96);
            text(ui, "公钥证书路径", &mut self.peer.certificate, 4096);
            text(
                ui,
                "已独立核对的 BLAKE3 指纹",
                &mut self.peer.fingerprint,
                64,
            );
            text(ui, "副屏预留地址，可留空", &mut self.peer.secondary, 96);
            if ui
                .add_enabled(
                    draft.plan.peers.len() < MAX_TRUSTED_PEERS,
                    egui::Button::new("保存目标电脑"),
                )
                .clicked()
            {
                let result = self.peer.checked(app).and_then(|peer| {
                    let mut next = draft.plan.clone();
                    next.peers.push(peer);
                    draft.save(next)
                });
                if result.is_ok() {
                    self.peer = PeerForm::default();
                }
                app.feedback(result, "已保存目标电脑。");
            }
        });
    }

    fn draw_routes(
        &mut self,
        app: &mut App,
        ui: &mut egui::Ui,
        draft: &mut Draft,
        layout: &Layout,
    ) {
        let mut remove = None;
        for (index, route) in draft.plan.routes.iter().enumerate() {
            ui.push_id(("route-segment", index), |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(format!(
                        "({},{}) {} {:.0}–{:.0}% → {} {}",
                        route.local_origin[0],
                        route.local_origin[1],
                        edge_label(route.local_edge),
                        route.local_span[0] * 100.0,
                        route.local_span[1] * 100.0,
                        draft.plan.peers[route.peer].address,
                        edge_label(route.remote_edge)
                    ));
                    if ui.small_button("编辑").clicked() {
                        self.route = RouteForm::edit(index, route);
                    }
                    if ui.small_button("删除").clicked() {
                        remove = Some(index);
                    }
                })
            });
        }
        if let Some(index) = remove {
            let mut next = draft.plan.clone();
            next.routes.remove(index);
            let result = draft.save(next);
            if result.is_ok() {
                self.route = RouteForm::default();
            }
            app.feedback(result, "已删除分段。");
        }
        if draft.plan.peers.is_empty() {
            return;
        }
        self.route.ui(ui, layout, &draft.plan);
        let label = if self.route.edit.is_some() {
            "保存分段修改"
        } else {
            "添加分段"
        };
        if ui
            .add_enabled(
                self.route.origin.is_some() && self.route.peer.is_some(),
                egui::Button::new(label),
            )
            .clicked()
        {
            let result = self.route.spec().and_then(|spec| {
                let mut next = draft.plan.clone();
                match self.route.edit {
                    Some(index) => *next.routes.get_mut(index).context("分段已变化")? = spec,
                    None => next.routes.push(spec),
                }
                validate_layout(&next, layout)?;
                draft.save(next)
            });
            if result.is_ok() {
                self.route = RouteForm::default();
            }
            app.feedback(result, "已保存分段。");
        }
        if self.route.edit.is_some() && ui.small_button("取消编辑").clicked() {
            self.route = RouteForm::default();
        }
    }
}

#[derive(Default)]
struct PeerForm {
    address: String,
    certificate: String,
    fingerprint: String,
    secondary: String,
}

impl PeerForm {
    fn checked(&self, app: &App) -> Result<PeerSpec> {
        let certificate =
            std::fs::canonicalize(self.certificate.trim()).context("无法定位证书文件")?;
        let fingerprint = self.fingerprint.trim().to_ascii_lowercase();
        let local = Identity::load(&app.identity_dir()).context("请先准备本机身份")?;
        let peer = PeerSpec {
            address: destination(&self.address)?,
            certificate,
            secondary_layout: (!self.secondary.trim().is_empty())
                .then(|| destination(&self.secondary))
                .transpose()?,
            fingerprint: Some(fingerprint),
        };
        verify_peer(&peer, &local.certificate)?;
        Ok(peer)
    }
}

struct RouteForm {
    edit: Option<usize>,
    origin: Option<[f64; 2]>,
    peer: Option<usize>,
    local_edge: Edge,
    remote_edge: Edge,
    local_span: [f64; 2],
    remote_span: [f64; 2],
}

impl Default for RouteForm {
    fn default() -> Self {
        Self {
            edit: None,
            origin: None,
            peer: None,
            local_edge: Edge::Right,
            remote_edge: Edge::Left,
            local_span: [0.0, 100.0],
            remote_span: [0.0, 100.0],
        }
    }
}

impl RouteForm {
    fn edit(index: usize, route: &RouteSpec) -> Self {
        Self {
            edit: Some(index),
            origin: Some(route.local_origin),
            peer: Some(route.peer),
            local_edge: route.local_edge,
            remote_edge: route.remote_edge,
            local_span: route.local_span.map(|n| n * 100.0),
            remote_span: route.remote_span.map(|n| n * 100.0),
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, layout: &Layout, plan: &Plan) {
        egui::ComboBox::from_id_salt("route-source-display")
            .selected_text(
                self.origin
                    .map_or_else(|| "选择本机屏幕".into(), |[x, y]| format!("屏幕 ({x},{y})")),
            )
            .show_ui(ui, |ui| {
                for display in layout.displays() {
                    ui.selectable_value(
                        &mut self.origin,
                        Some([display.bounds.left, display.bounds.top]),
                        display_inventory::description(display, display.id == layout.primary()),
                    );
                }
            });
        egui::ComboBox::from_id_salt("route-target-peer")
            .selected_text(
                self.peer
                    .and_then(|index| plan.peers.get(index))
                    .map_or_else(|| "选择目标电脑".into(), |peer| peer.address.to_string()),
            )
            .show_ui(ui, |ui| {
                for (index, peer) in plan.peers.iter().enumerate() {
                    ui.selectable_value(&mut self.peer, Some(index), peer.address.to_string());
                }
            });
        edge_span(ui, "本机出边", &mut self.local_edge, &mut self.local_span);
        edge_span(ui, "目标入边", &mut self.remote_edge, &mut self.remote_span);
    }

    fn spec(&self) -> Result<RouteSpec> {
        Ok(RouteSpec {
            local_origin: self.origin.context("请选择本机屏幕")?,
            local_edge: self.local_edge,
            local_span: self.local_span.map(|n| n / 100.0),
            peer: self.peer.context("请选择目标电脑")?,
            remote_edge: self.remote_edge,
            remote_span: self.remote_span.map(|n| n / 100.0),
        })
    }
}

fn edge_span(ui: &mut egui::Ui, label: &str, edge: &mut Edge, span: &mut [f64; 2]) {
    ui.horizontal_wrapped(|ui| {
        ui.label(label);
        egui::ComboBox::from_id_salt(label)
            .selected_text(edge_label(*edge))
            .show_ui(ui, |ui| {
                for side in [Edge::Left, Edge::Right, Edge::Top, Edge::Bottom] {
                    ui.selectable_value(edge, side, edge_label(side));
                }
            });
        ui.add(
            egui::DragValue::new(&mut span[0])
                .range(0.0..=100.0)
                .max_decimals(4)
                .suffix("%"),
        );
        ui.label("到");
        ui.add(
            egui::DragValue::new(&mut span[1])
                .range(0.0..=100.0)
                .max_decimals(4)
                .suffix("%"),
        );
    });
}

fn edge_label(edge: Edge) -> &'static str {
    match edge {
        Edge::Left => "左沿",
        Edge::Right => "右沿",
        Edge::Top => "上沿",
        Edge::Bottom => "下沿",
    }
}

fn text(ui: &mut egui::Ui, label: &str, value: &mut String, limit: usize) {
    ui.label(label);
    ui.add(
        egui::TextEdit::singleline(value)
            .char_limit(limit)
            .desired_width(280.0),
    );
}

fn destination(text: &str) -> Result<SocketAddr> {
    let address = data_access::parse_address(text).context("请填写单播地址和端口")?;
    ensure!(!address.ip().is_unspecified(), "地址须指向目标电脑");
    Ok(address)
}

fn verify_peer(peer: &PeerSpec, local: &[u8]) -> Result<()> {
    let certificate = transport::read_certificate(&peer.certificate)?;
    ensure!(certificate != local, "目标证书属于本机");
    ensure!(
        peer.fingerprint.as_deref() == Some(transport::fingerprint(&certificate).as_str()),
        "目标证书与已核对的指纹不一致"
    );
    Ok(())
}

fn validate_shape(plan: &Plan) -> Result<()> {
    ensure!(
        plan.version == VERSION
            && plan.peers.len() <= MAX_TRUSTED_PEERS
            && plan.routes.len() <= MAX_LINKS,
        "分段布局版本或数量不受支持"
    );
    let mut pins = BTreeSet::new();
    for peer in &plan.peers {
        let fingerprint = peer.fingerprint.as_deref().context("目标缺少证书指纹")?;
        let pin = blake3::Hash::from_hex(fingerprint).context("目标证书指纹格式无效")?;
        ensure!(
            pins.insert(*pin.as_bytes()) && peer.certificate.is_absolute(),
            "目标证书重复或路径无效"
        );
        destination(&peer.address.to_string())?;
        if let Some(address) = peer.secondary_layout {
            destination(&address.to_string())?;
        }
    }
    for route in &plan.routes {
        ensure!(
            route.peer < plan.peers.len()
                && route.local_origin.iter().all(|value| value.is_finite())
                && [route.local_span, route.remote_span]
                    .into_iter()
                    .all(|[start, end]| 0.0 <= start && start < end && end <= 1.0),
            "分段范围或目标无效"
        );
    }
    Ok(())
}

fn validate_layout(plan: &Plan, layout: &Layout) -> Result<()> {
    validate_shape(plan)?;
    let mut links = Vec::with_capacity(plan.routes.len());
    for route in &plan.routes {
        let [x, y] = route.local_origin;
        let mut screens = layout
            .displays()
            .iter()
            .filter(|display| display.bounds.left == x && display.bounds.top == y);
        let source = screens.next().context("分段的本机屏幕已变化，请重新选择")?;
        ensure!(screens.next().is_none(), "分段的屏幕原点重复");
        let [start, end] = route.local_span;
        let [remote_start, remote_end] = route.remote_span;
        let peer = blake3::Hash::from_hex(
            plan.peers[route.peer]
                .fingerprint
                .as_deref()
                .context("目标缺少证书指纹")?,
        )?;
        links.push(Link {
            source_display: source.id,
            source_edge: route.local_edge,
            source_span: Span { start, end },
            target: Target {
                peer: *peer.as_bytes(),
                display: None,
                edge: route.remote_edge,
            },
            target_span: Span {
                start: remote_start,
                end: remote_end,
            },
        });
    }
    Table::new(layout, links).map_err(|_| anyhow::anyhow!("屏幕分段相互重叠或无效"))?;
    Ok(())
}

fn export(app: &App, plan: &Plan, layout: &Layout, name: &str) -> Result<PathBuf> {
    validate_layout(plan, layout)?;
    let local = Identity::load(&app.identity_dir())?;
    for peer in &plan.peers {
        verify_peer(peer, &local.certificate)?;
    }
    let path = Path::new(name);
    ensure!(
        path.file_name() == Some(path.as_os_str())
            && path.extension().is_some_and(|ext| ext == "json")
            && name.chars().all(
                |character| character.is_alphanumeric() || matches!(character, '.' | '-' | '_')
            ),
        "请输入单独的 .json 文件名"
    );
    let directory = app.dir.join("route-exports");
    std::fs::create_dir_all(&directory)?;
    let path = directory.join(path);
    write_new(&path, plan)?;
    Ok(path)
}

fn write_new(path: &Path, plan: &Plan) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(plan)?;
    ensure!(
        bytes.len() <= config_store::MAX_BYTES,
        "布局文件超过大小限制"
    );
    let mut file = config_store::options()
        .create_new(true)
        .open(path)
        .context("无法创建新文件，请检查文件名是否已存在")?;
    let result = file.write_all(&bytes).and_then(|()| file.sync_all());
    drop(file);
    if let Err(error) = result
        .map_err(anyhow::Error::from)
        .and_then(|()| config_store::sync_parent(path))
    {
        let _ = std::fs::remove_file(path);
        return Err(error);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use peerglide_platform::display_layout::{Bounds, Display, DisplayId, Rotation};

    #[test]
    fn validates_export_geometry_and_pins_and_keeps_an_existing_export() -> Result<()> {
        let fixture = crate::tests::Fixture::new()?;
        let local = Identity::generate()?;
        let remote = Identity::generate()?;
        let certificate = fixture.path.join("peer.der");
        std::fs::write(&certificate, &remote.certificate)?;
        let peer = PeerSpec {
            address: "127.0.0.1:24870".parse()?,
            certificate: certificate.clone(),
            fingerprint: Some(remote.fingerprint()),
            secondary_layout: Some("127.0.0.1:24877".parse()?),
        };
        let display = Display {
            id: DisplayId(1),
            bounds: Bounds {
                left: -200.0,
                top: 0.0,
                right: 0.0,
                bottom: 150.0,
            },
            scale: 1.0,
            rotation: Rotation::Upright,
        };
        let layout = Layout::new(display.id, vec![display]).unwrap();
        let route = RouteSpec {
            local_origin: [-200.0, 0.0],
            local_edge: Edge::Right,
            local_span: [0.125, 0.5],
            peer: 0,
            remote_edge: Edge::Top,
            remote_span: [0.0, 1.0],
        };
        let mut plan = Plan {
            peers: vec![peer.clone()],
            routes: vec![route.clone()],
            ..Plan::default()
        };
        validate_layout(&plan, &layout)?;
        verify_peer(&peer, &local.certificate)?;
        assert_eq!(
            RouteForm::edit(0, &route).spec()?.local_span,
            route.local_span
        );
        plan.routes.push(route);
        assert!(validate_layout(&plan, &layout).is_err());
        plan.routes.pop();
        let output = fixture.path.join("routes.json");
        write_new(&output, &plan)?;
        let before = std::fs::read(&output)?;
        let decoded: Plan = serde_json::from_slice(&before)?;
        assert_eq!(decoded.peers[0].fingerprint, peer.fingerprint);
        assert!(write_new(&output, &Plan::default()).is_err());
        assert_eq!(std::fs::read(&output)?, before);
        std::fs::write(certificate, Identity::generate()?.certificate)?;
        assert!(verify_peer(&peer, &local.certificate).is_err());
        fixture.close()?;
        Ok(())
    }
}
