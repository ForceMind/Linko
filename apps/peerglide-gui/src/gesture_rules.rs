//! Local rule editing and sandbox selection. Never enables capture or actions.
use super::config_store::Store;
use eframe::egui;
use peerglide_actions::{
    rules::{CompiledRules, Resolution, Rule, RuleBook, RuleError, Scope},
    Action, ActionSpec, ACTIONS,
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
                let status = match CompiledRules::new(draft.clone()) {
                    Ok(_) => String::new(),
                    Err(e) => format!("规则需要修正：{}", rule_error_label(e)),
                };
                Self {
                    draft,
                    store: Some(store),
                    application: "app:example".into(),
                    status,
                }
            }
            Err(e) => Self {
                draft: RuleBook::default(),
                store: None,
                application: "app:example".into(),
                status: format!("已保留原规则文件，需要恢复：{e:#}"),
            },
        }
    }
    fn save(&mut self) -> anyhow::Result<()> {
        CompiledRules::new(self.draft.clone())
            .map_err(|e| anyhow::anyhow!("规则无效：{}", rule_error_label(e)))?;
        self.store
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("需要先恢复规则文件"))?
            .save(&self.draft)
    }
    pub fn preview(&self, gesture: u32) -> Result<Resolution, peerglide_actions::rules::RuleError> {
        // Preview consent is not global G1 consent. No native hook/action backend.
        let mut book = self.draft.clone();
        book.enabled = true;
        CompiledRules::new(book).map(|r| r.resolve(gesture, &self.application))
    }
    pub fn preview_ui(&mut self, ui: &mut egui::Ui, gesture: Option<u32>) {
        ui.separator();
        ui.label("规则预览 · 不执行动作");
        ui.horizontal(|ui| {
            ui.label("应用标识");
            ui.text_edit_singleline(&mut self.application);
        });
        if let Some(id) = gesture {
            ui.label(match self.preview(id) {
                Ok(Resolution::Selected { rule_id, action }) => {
                    format!("规则 {rule_id} → {}（仅预览）", action_label(action.action))
                }
                Ok(Resolution::Excluded) => "此应用已被排除".into(),
                Ok(Resolution::InvalidApplication) => "请输入有效的应用标识".into(),
                Ok(_) => "没有匹配的规则".into(),
                Err(e) => format!("请先修正规则再预览：{}", rule_error_label(e)),
            });
        } else {
            ui.weak("请先绘制可识别的手势，以预览该应用标识对应的规则。");
        }
    }
    pub fn ui(&mut self, ui: &mut egui::Ui, dir: &Path, writable: bool) {
        ui.heading("手势规则");
        ui.label("配置手势动作映射和排除项。预览不会捕获输入或执行动作。");
        if self.store.is_none() && writable && ui.button("备份并重置无效的规则文件").clicked()
        {
            match Store::reset::<RuleBook>(dir.join("gesture-rules.json")) {
                Ok((store, draft)) => {
                    self.store = Some(store);
                    self.draft = draft;
                    self.status = "规则已重置，原文件内容已备份".into();
                }
                Err(e) => self.status = format!("重置失败：{e:#}"),
            }
        }
        ui.add_enabled_ui(writable, |ui| {
            let mut delete = None;
            for (index, rule) in self.draft.rules.iter_mut().enumerate() {
                ui.push_id(rule.id, |ui| {
                    ui.group(|ui| {
                        ui.horizontal(|ui| {
                            ui.checkbox(&mut rule.enabled, format!("规则 {}", rule.id));
                            ui.label("手势编号");
                            ui.add(egui::DragValue::new(&mut rule.gesture).range(1..=8));
                            ui.label("优先级");
                            ui.add(egui::DragValue::new(&mut rule.priority));
                            if ui.button("删除").clicked() {
                                delete = Some(index);
                            }
                        });
                        let mut kind = match rule.scope {
                            Scope::Global => 0,
                            Scope::Application { .. } => 1,
                            Scope::Group { .. } => 2,
                        };
                        let before = kind;
                        egui::ComboBox::from_id_salt("scope")
                            .selected_text(["全局", "应用", "应用组"][kind])
                            .show_ui(ui, |ui| {
                                for (i, label) in ["全局", "应用", "应用组"].iter().enumerate()
                                {
                                    ui.selectable_value(&mut kind, i, *label);
                                }
                            });
                        if kind != before {
                            rule.scope = match kind {
                                1 => Scope::Application { id: String::new() },
                                2 => Scope::Group {
                                    apps: vec![String::new()],
                                },
                                _ => Scope::Global,
                            };
                        }
                        match &mut rule.scope {
                            Scope::Application { id } => {
                                ui.horizontal(|ui| {
                                    ui.label("精确应用标识");
                                    ui.text_edit_singleline(id);
                                });
                            }
                            Scope::Group { apps } => {
                                for id in apps.iter_mut() {
                                    ui.text_edit_singleline(id);
                                }
                                if apps.len() < 32 && ui.button("添加应用").clicked() {
                                    apps.push(String::new());
                                }
                                if apps.len() > 1 && ui.button("删除最后一个应用").clicked()
                                {
                                    apps.pop();
                                }
                            }
                            Scope::Global => (),
                        }
                        egui::ComboBox::from_id_salt("action")
                            .selected_text(action_label(rule.action.action))
                            .show_ui(ui, |ui| {
                                for action in ACTIONS {
                                    ui.selectable_value(
                                        &mut rule.action.action,
                                        action,
                                        action_label(action),
                                    );
                                }
                            });
                    });
                });
            }
            if let Some(index) = delete {
                self.draft.rules.remove(index);
            }
            if self.draft.rules.len() < 128 && ui.button("添加规则").clicked() {
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
                        enabled: true,
                        gesture: 2,
                        scope: Scope::Global,
                        priority: 0,
                        action: ActionSpec::new(ACTIONS[0]),
                    });
                }
            }
            ui.separator();
            ui.label("排除的应用标识（精确匹配，始终优先）");
            for exclusion in &mut self.draft.exclusions {
                ui.text_edit_singleline(exclusion);
            }
            if self.draft.exclusions.len() < 128 && ui.button("添加排除项").clicked() {
                self.draft.exclusions.push(String::new());
            }
            if !self.draft.exclusions.is_empty() && ui.button("删除最后一个排除项").clicked()
            {
                self.draft.exclusions.pop();
            }
            let valid = CompiledRules::new(self.draft.clone());
            if let Err(e) = &valid {
                ui.label(format!(
                    "无法保存：{}。请先解决冲突和空应用标识。",
                    rule_error_label(*e)
                ));
            }
            if ui
                .add_enabled(
                    valid.is_ok() && self.store.is_some(),
                    egui::Button::new("保存规则"),
                )
                .clicked()
            {
                // Files never grant global input capture permission.
                self.draft.enabled = false;
                self.status = match self.save() {
                    Ok(()) => "规则已保存，全局手势仍保持关闭。".into(),
                    Err(e) => format!("保存失败：{e:#}"),
                };
            }
        });
        if !self.status.is_empty() {
            ui.label(&self.status);
        }
        ui.weak("应用标识须与本机读取结果精确一致；当前预览仍不捕获全局输入或执行动作。");
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn saved_mapping_roundtrips_but_never_enables_global_capture() -> anyhow::Result<()> {
        let fixture = super::super::tests::Fixture::new()?;
        let mut editor = RuleEditor::new(&fixture.path);
        editor.draft.rules.push(Rule {
            id: 1,
            enabled: true,
            gesture: 2,
            scope: Scope::Global,
            priority: 0,
            action: ActionSpec::new(ACTIONS[1]),
        });
        editor.save()?;
        let loaded = RuleEditor::new(&fixture.path);
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
