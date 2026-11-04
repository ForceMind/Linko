//! Local rule editing and sandbox selection. Never enables capture or actions.
use super::config_store::Store;
use eframe::egui;
use peerglide_actions::{
    rules::{CompiledRules, Resolution, Rule, RuleBook, RuleError, Scope},
    Action, ActionSpec, Risk, ACTIONS,
};
use std::path::Path;

fn action_label(action: Action) -> &'static str {
    match action {
        Action::Copy => "复制",
        Action::Paste => "粘贴",
        Action::Back => "后退",
        Action::Forward => "前进",
        Action::CloseTab => "关闭标签页",
        Action::MinimizeWindow => "最小化窗口",
        Action::MaximizeWindow => "最大化窗口",
        Action::ToggleFullscreen => "切换全屏",
        Action::CloseWindow => "关闭窗口",
        Action::QuitApplication => "退出应用",
        Action::ShowPeerGlide => "显示屏跃",
    }
}

fn rule_error_label(error: RuleError) -> &'static str {
    match error {
        RuleError::Size => "规则文件过大",
        RuleError::Json => "规则文件的 JSON 格式无效",
        RuleError::Schema => "不支持此规则文件版本",
        RuleError::Limit => "规则或排除项数量超出上限",
        RuleError::Application => "应用标识或应用组无效",
        RuleError::DuplicateId => "规则编号为零或重复",
        RuleError::Action => "手势编号或动作版本无效",
        RuleError::Conflict => "规则的适用范围和优先级存在冲突",
    }
}

// IDs remain the recognizer's stable IDs; the editor presents their trajectories.
const GESTURES: [&str; 8] = [
    "向左",
    "向右",
    "向上",
    "向下",
    "先向下再向右",
    "先向下再向左",
    "先向上再向右",
    "先向上再向左",
];
fn gesture_label(id: u32) -> &'static str {
    id.checked_sub(1)
        .and_then(|i| GESTURES.get(i as usize))
        .copied()
        .unwrap_or("未知轨迹（请重新选择）")
}
fn gesture_icon(ui: &mut egui::Ui, id: u32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(44., 36.), egui::Sense::hover());
    let path: &[[f32; 2]] = match id {
        1 => &[[0.85, 0.5], [0.15, 0.5]],
        2 => &[[0.15, 0.5], [0.85, 0.5]],
        3 => &[[0.5, 0.85], [0.5, 0.15]],
        4 => &[[0.5, 0.15], [0.5, 0.85]],
        5 => &[[0.2, 0.15], [0.2, 0.8], [0.85, 0.8]],
        6 => &[[0.8, 0.15], [0.8, 0.8], [0.15, 0.8]],
        7 => &[[0.2, 0.85], [0.2, 0.2], [0.85, 0.2]],
        8 => &[[0.8, 0.85], [0.8, 0.2], [0.15, 0.2]],
        _ => return,
    };
    let position = |p: [f32; 2]| rect.min + egui::vec2(rect.width() * p[0], rect.height() * p[1]);
    let stroke = egui::Stroke::new(2., ui.visuals().text_color());
    for pair in path.windows(2) {
        ui.painter()
            .line_segment([position(pair[0]), position(pair[1])], stroke);
    }
    let end = position(path[path.len() - 1]);
    let previous = position(path[path.len() - 2]);
    let direction = (end - previous).normalized();
    let side = egui::vec2(-direction.y, direction.x);
    for sign in [-1., 1.] {
        ui.painter()
            .line_segment([end, end - direction * 6. + side * (sign * 4.)], stroke);
    }
}
fn choose_action(rule: &mut Rule, action: Action) {
    if rule.action.action != action {
        rule.action.action = action;
        if action.risk() == Risk::Destructive {
            rule.enabled = false;
        }
    }
}
fn platform_action_status(action: Action) -> &'static str {
    if cfg!(target_os = "windows")
        && matches!(
            action,
            Action::MinimizeWindow | Action::MaximizeWindow | Action::CloseWindow
        )
        || cfg!(target_os = "macos")
            && matches!(action, Action::MinimizeWindow | Action::CloseWindow)
    {
        "本机平台已有 Native 实现；真实窗口效果未验收，GUI 未接通执行。"
    } else {
        "此动作在本机平台尚无 Native 实现；可保存语义映射，不能执行。"
    }
}
fn scope_rank(scope: &Scope) -> u8 {
    match scope {
        Scope::Global => 0,
        Scope::Group { .. } => 1,
        Scope::Application { .. } => 2,
    }
}
fn scopes_overlap(a: &Scope, b: &Scope) -> bool {
    let contains = |scope: &Scope, app: &str| match scope {
        Scope::Global => true,
        Scope::Application { id } => id == app,
        Scope::Group { apps } => apps.iter().any(|id| id == app),
    };
    match a {
        Scope::Global => true,
        Scope::Application { id } => contains(b, id),
        Scope::Group { apps } => apps.iter().any(|id| contains(b, id)),
    }
}
fn conflict_details(book: &RuleBook) -> Option<String> {
    for (i, a) in book.rules.iter().enumerate().filter(|(_, r)| r.enabled) {
        for (j, b) in book
            .rules
            .iter()
            .enumerate()
            .skip(i + 1)
            .filter(|(_, r)| r.enabled)
        {
            if a.gesture == b.gesture
                && scope_rank(&a.scope) == scope_rank(&b.scope)
                && a.priority == b.priority
                && scopes_overlap(&a.scope, &b.scope)
            {
                return Some(format!(
                    "第 {} 条「{} → {}」与第 {} 条「{} → {}」范围重叠且优先级相同。请修改范围、停用其中一条，或在高级详情调整优先级。",
                    i + 1, gesture_label(a.gesture), action_label(a.action.action),
                    j + 1, gesture_label(b.gesture), action_label(b.action.action),
                ));
            }
        }
    }
    None
}

pub struct RuleEditor {
    draft: RuleBook,
    store: Option<Store>,
    application: String,
    status: String,
}
impl RuleEditor {
    pub fn new(dir: &Path) -> Self {
        match Store::open::<RuleBook>(dir.join("gesture-rules.json")) {
            Ok((store, draft)) => {
                let (store, status) = match CompiledRules::new(draft.clone()) {
                    Ok(_) => (Some(store), String::new()),
                    Err(e) => (
                        None,
                        format!("原规则文件已保留，当前只读：{}", rule_error_label(e)),
                    ),
                };
                Self {
                    draft,
                    store,
                    application: String::new(),
                    status,
                }
            }
            Err(e) => Self {
                draft: RuleBook::default(),
                store: None,
                application: String::new(),
                status: format!("已保留原规则文件，需要恢复：{e:#}"),
            },
        }
    }
    fn save(&mut self) -> anyhow::Result<()> {
        // A saved book is never independent consent to start global G1.
        self.draft.enabled = false;
        CompiledRules::new(self.draft.clone())
            .map_err(|e| anyhow::anyhow!("规则无效：{}", rule_error_label(e)))?;
        self.store
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("需要先恢复规则文件"))?
            .save(&self.draft)
    }
    fn add_rule(&mut self) {
        if self.draft.rules.len() >= 128 {
            return;
        }
        if let Some(id) = self
            .draft
            .rules
            .iter()
            .map(|r| r.id)
            .max()
            .unwrap_or(0)
            .checked_add(1)
        {
            self.draft.rules.push(Rule {
                id,
                enabled: false,
                gesture: 2,
                scope: Scope::Global,
                priority: 0,
                action: ActionSpec::new(Action::Copy),
            });
        }
    }
    pub fn add_from_gesture(&mut self, gesture: u32) -> anyhow::Result<()> {
        anyhow::ensure!((1..=8).contains(&gesture), "此轨迹不在当前八种模板中");
        anyhow::ensure!(
            self.store.is_some() && self.draft.rules.len() < 128,
            "规则当前只读或已达数量上限"
        );
        let id = self
            .draft
            .rules
            .iter()
            .map(|rule| rule.id)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("规则编号已达上限"))?;
        self.draft.rules.push(Rule {
            id,
            enabled: false,
            gesture,
            scope: Scope::Global,
            priority: 0,
            action: ActionSpec::new(Action::MinimizeWindow),
        });
        self.status = "已从窗口轨迹创建未保存规则；请选择动作并保存，全局手势仍关闭。".into();
        Ok(())
    }
    pub fn preview(&self, gesture: u32) -> Result<Resolution, RuleError> {
        // Preview consent is not global G1 consent. No native hook/action backend.
        let mut book = self.draft.clone();
        book.enabled = true;
        CompiledRules::new(book).map(|r| r.resolve(gesture, &self.application))
    }
    pub fn preview_ui(&mut self, ui: &mut egui::Ui, gesture: Option<u32>) {
        ui.separator();
        ui.strong("生效预览 · 本窗口测试，不执行动作");
        ui.label("匹配顺序：排除优先 → 精确应用 → 应用组 → 全局兜底。");
        ui.horizontal(|ui| {
            ui.label("测试应用标识");
            ui.add(
                egui::TextEdit::singleline(&mut self.application).hint_text("手动填入真实应用标识"),
            );
        });
        ui.weak("应用选择器尚未实现。此字段仅供手动测试，不会读取或切换当前应用。");
        if let Some(id) = gesture {
            ui.label(match self.preview(id) {
                Ok(Resolution::Selected { rule_id, action }) => {
                    let position = self
                        .draft
                        .rules
                        .iter()
                        .position(|r| r.id == rule_id)
                        .map_or(0, |i| i + 1);
                    format!(
                        "{} → {} · 命中第 {position} 条（仅预览）",
                        gesture_label(id),
                        action_label(action.action)
                    )
                }
                Ok(Resolution::Excluded) => "此应用已被排除，所有映射均不匹配。".into(),
                Ok(Resolution::InvalidApplication) => "请输入有效的测试应用标识。".into(),
                Ok(_) => "没有匹配的规则；请检查轨迹、范围及规则是否纳入预览。".into(),
                Err(e) => format!("请先修正规则再预览：{}", rule_error_label(e)),
            });
        } else {
            ui.weak("请先在录制与测试页绘制可识别的轨迹。");
        }
    }
    pub fn ui(&mut self, ui: &mut egui::Ui, dir: &Path, writable: bool) {
        ui.heading("轨迹、范围与动作");
        ui.label("保存的是本机规则。全局手势 G1 仍关闭，勾选规则仅用于匹配预览。");
        ui.weak("Windows 已实现最小化 / 最大化 / 关闭窗口的 Native 库；macOS 已实现最小化 / 关闭窗口；Linux 暂无。均未在此 GUI 接通执行，真实效果未验收。");
        if !writable {
            ui.label("当前配置只读，可以查看与测试，不能编辑或保存。");
        }
        if self.store.is_none() {
            ui.label("规则文件不可用：原文件已保留。显式备份重置后才能保存新规则。");
            if writable && ui.button("备份并重置无效的规则文件").clicked() {
                match Store::reset::<RuleBook>(dir.join("gesture-rules.json")) {
                    Ok((store, draft)) => {
                        self.store = Some(store);
                        self.draft = draft;
                        self.status = "规则已重置，原文件内容已备份".into();
                    }
                    Err(e) => self.status = format!("重置失败：{e:#}"),
                }
            }
        }
        ui.add_enabled_ui(writable && self.store.is_some(), |ui| {
            let mut delete = None;
            for (index, rule) in self.draft.rules.iter_mut().enumerate() {
                ui.push_id(index, |ui| {
                    ui.group(|ui| {
                        ui.horizontal_wrapped(|ui| {
                            ui.strong(format!("第 {} 条", index + 1));
                            gesture_icon(ui, rule.gesture);
                            egui::ComboBox::from_id_salt("gesture")
                                .selected_text(gesture_label(rule.gesture))
                                .show_ui(ui, |ui| {
                                    for id in 1..=8 {
                                        ui.horizontal(|ui| {
                                            gesture_icon(ui, id);
                                            ui.selectable_value(&mut rule.gesture, id, gesture_label(id));
                                        });
                                    }
                                });
                            if ui.button("移除此规则").clicked() { delete = Some(index); }
                        });
                        ui.horizontal_wrapped(|ui| {
                            ui.label("作用范围");
                            let mut kind = scope_rank(&rule.scope);
                            let before = kind;
                            egui::ComboBox::from_id_salt("scope")
                                .selected_text(["全局兜底", "应用组", "精确应用"][kind as usize])
                                .show_ui(ui, |ui| {
                                    for (i, label) in ["全局兜底", "应用组", "精确应用"].iter().enumerate() {
                                        ui.selectable_value(&mut kind, i as u8, *label);
                                    }
                                });
                            if kind != before {
                                rule.scope = match kind {
                                    2 => Scope::Application { id: String::new() },
                                    1 => Scope::Group { apps: vec![String::new()] },
                                    _ => Scope::Global,
                                };
                            }
                        });
                        match &mut rule.scope {
                            Scope::Application { id } => {
                                ui.label("应用选择器尚未实现，请手动填写精确应用标识：");
                                ui.add(egui::TextEdit::singleline(id).hint_text("本机真实应用标识，不是窗口标题"));
                            }
                            Scope::Group { apps } => {
                                ui.label("应用选择器尚未实现，请手动填写组内每个精确应用标识：");
                                for id in apps.iter_mut() { ui.text_edit_singleline(id); }
                                ui.horizontal(|ui| {
                                    if apps.len() < 32 && ui.button("加入应用标识").clicked() { apps.push(String::new()); }
                                    if apps.len() > 1 && ui.button("移除最后一个应用").clicked() { apps.pop(); }
                                });
                            }
                            Scope::Global => { ui.weak("未命中精确应用或应用组时使用；排除项仍优先。"); }
                        }
                        ui.horizontal_wrapped(|ui| {
                            ui.label("动作");
                            let mut selected = rule.action.action;
                            egui::ComboBox::from_id_salt("action")
                                .selected_text(action_label(rule.action.action))
                                .show_ui(ui, |ui| {
                                    for action in ACTIONS {
                                        ui.selectable_value(&mut selected, action, action_label(action));
                                    }
                                });
                            choose_action(rule, selected);
                        });
                        ui.weak(platform_action_status(rule.action.action));
                        if rule.action.action.risk() == Risk::Destructive {
                            ui.label("危险动作：可能丢失未保存内容。选择此动作后默认停用，执行前还需独立确认。 ");
                            ui.checkbox(&mut rule.enabled, "将此危险动作纳入规则预览（不授予执行同意）");
                        } else {
                            ui.checkbox(&mut rule.enabled, "纳入规则预览");
                        }
                        ui.collapsing("高级详情 · 技术编号与优先级", |ui| {
                            ui.label(format!("规则 ID：{} · 轨迹 ID：{} · 动作版本：{}", rule.id, rule.gesture, rule.action.schema_version));
                            ui.horizontal(|ui| {
                                ui.label("同等范围内的优先级");
                                ui.add(egui::DragValue::new(&mut rule.priority));
                            });
                        });
                    });
                });
            }
            if let Some(index) = delete { self.draft.rules.remove(index); }
            if self.draft.rules.is_empty() { ui.label("尚无规则。先添加一条，选择轨迹、范围与动作，再到录制页测试。"); }
            if self.draft.rules.len() < 128 && ui.button("添加规则").clicked() { self.add_rule(); }
            ui.separator();
            ui.strong("应用排除 · 始终优先");
            ui.weak("应用选择器尚未实现。手动填写精确标识后，该应用不会命中任何规则。");
            for exclusion in &mut self.draft.exclusions { ui.text_edit_singleline(exclusion); }
            ui.horizontal(|ui| {
                if self.draft.exclusions.len() < 128 && ui.button("添加排除项").clicked() { self.draft.exclusions.push(String::new()); }
                if !self.draft.exclusions.is_empty() && ui.button("删除最后一个排除项").clicked() { self.draft.exclusions.pop(); }
            });
            let valid = CompiledRules::new(self.draft.clone());
            if let Err(e) = &valid {
                ui.label(format!("无法保存：{}。", rule_error_label(*e)));
                if *e == RuleError::Conflict {
                    if let Some(details) = conflict_details(&self.draft) { ui.label(details); }
                } else if *e == RuleError::Application {
                    ui.label("应用范围、应用组及排除项不能留空；标识必须精确且没有首尾空格。");
                }
            }
            if ui.add_enabled(valid.is_ok() && self.store.is_some(), egui::Button::new("保存规则")).clicked() {
                self.status = match self.save() {
                    Ok(()) => "规则已保存，全局手势仍保持关闭。".into(),
                    Err(e) => format!("保存失败：{e:#}"),
                };
            }
        });
        if !self.status.is_empty() {
            ui.label(&self.status);
        }
        ui.weak("应用标识须与本机读取结果精确一致；此页不捕获全局输入、不执行动作。");
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn render(editor: &mut RuleEditor, dir: &Path, writable: bool) {
        let ctx = egui::Context::default();
        let output = ctx.run_ui(egui::RawInput::default(), |root| {
            egui::CentralPanel::default().show(root, |ui| {
                editor.ui(ui, dir, writable);
                editor.preview_ui(ui, Some(2));
            });
        });
        output.drop_without_applying_deltas();
    }
    #[test]
    fn recorded_gesture_creates_only_an_unsaved_disabled_rule() -> anyhow::Result<()> {
        let fixture = super::super::tests::Fixture::new()?;
        let mut editor = RuleEditor::new(&fixture.path);
        editor.add_from_gesture(3)?;
        let rule = editor.draft.rules.last().unwrap();
        assert_eq!(rule.gesture, 3);
        assert!(!rule.enabled);
        assert!(!editor.draft.enabled);
        assert!(!fixture.path.join("gesture-rules.json").exists());
        assert!(editor.add_from_gesture(99).is_err());
        Ok(())
    }
    #[test]
    fn ordinary_render_and_preview_do_not_write_or_enable_global_capture() -> anyhow::Result<()> {
        let fixture = super::super::tests::Fixture::new()?;
        let mut editor = RuleEditor::new(&fixture.path);
        editor.add_rule();
        assert!(!editor.draft.rules[0].enabled);
        assert!(editor.application.is_empty());
        assert_eq!(editor.preview(2), Ok(Resolution::InvalidApplication));
        render(&mut editor, &fixture.path, true);
        render(&mut editor, &fixture.path, false);
        assert!(!editor.draft.enabled);
        assert_eq!(std::fs::read_dir(&fixture.path)?.count(), 0);
        Ok(())
    }
    #[test]
    fn future_rule_schema_is_not_overwritten_by_render_or_save() -> anyhow::Result<()> {
        let fixture = super::super::tests::Fixture::new()?;
        let path = fixture.path.join("gesture-rules.json");
        for original in [
            br#"{"store_version":2,"data":{}}"#.as_slice(),
            br#"{"schema_version":2,"enabled":true,"rules":[],"exclusions":[]}"#.as_slice(),
            b"invalid JSON".as_slice(),
        ] {
            std::fs::write(&path, original)?;
            let mut editor = RuleEditor::new(&fixture.path);
            assert!(editor.store.is_none());
            render(&mut editor, &fixture.path, true);
            assert!(editor.save().is_err());
            assert_eq!(std::fs::read(&path)?, original);
        }
        Ok(())
    }
    #[test]
    fn selecting_a_dangerous_action_requires_explicit_rule_opt_in_again() -> anyhow::Result<()> {
        let fixture = super::super::tests::Fixture::new()?;
        let mut editor = RuleEditor::new(&fixture.path);
        editor.add_rule();
        let rule = &mut editor.draft.rules[0];
        assert!(!rule.enabled);
        rule.enabled = true;
        choose_action(rule, Action::CloseWindow);
        assert!(!rule.enabled);
        choose_action(rule, Action::QuitApplication);
        assert!(!rule.enabled);
        assert!(!editor.draft.enabled);
        assert!(!fixture.path.join("gesture-rules.json").exists());
        Ok(())
    }
    #[test]
    fn conflict_description_identifies_both_rows_and_overlapping_scope() {
        let mut book = RuleBook::default();
        let rule = Rule {
            id: 4,
            enabled: true,
            gesture: 2,
            scope: Scope::Application {
                id: "app:browser".into(),
            },
            priority: 0,
            action: ActionSpec::new(Action::CloseWindow),
        };
        book.rules = vec![rule.clone(), Rule { id: 5, ..rule }];
        let description = conflict_details(&book).unwrap();
        assert!(description.contains("第 1 条") && description.contains("第 2 条"));
        assert!(matches!(
            CompiledRules::new(book.clone()),
            Err(RuleError::Conflict)
        ));
        book.rules[1].scope = Scope::Application {
            id: "app:editor".into(),
        };
        assert!(conflict_details(&book).is_none());
        assert!(CompiledRules::new(book).is_ok());
    }
    #[test]
    fn saved_mapping_roundtrips_but_never_enables_global_capture() -> anyhow::Result<()> {
        let fixture = super::super::tests::Fixture::new()?;
        let mut editor = RuleEditor::new(&fixture.path);
        editor.application = "app:example".into();
        editor.draft.rules.push(Rule {
            id: 1,
            enabled: true,
            gesture: 2,
            scope: Scope::Global,
            priority: 0,
            action: ActionSpec::new(ACTIONS[1]),
        });
        editor.draft.enabled = true;
        editor.save()?;
        let mut loaded = RuleEditor::new(&fixture.path);
        loaded.application = "app:example".into();
        assert!(!loaded.draft.enabled);
        assert!(matches!(
            loaded.preview(2),
            Ok(Resolution::Selected { rule_id: 1, .. })
        ));
        editor.draft.exclusions.push("app:example".into());
        assert_eq!(editor.preview(2), Ok(Resolution::Excluded));
        editor.draft.rules.push(editor.draft.rules[0].clone());
        assert!(editor.save().is_err());
        assert_eq!(RuleEditor::new(&fixture.path).draft.rules.len(), 1);
        Ok(())
    }
}
