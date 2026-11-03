//! Local rule editing and sandbox selection. Never enables capture or actions.
use super::config_store::Store;
use eframe::egui;
use peerglide_actions::{
    rules::{CompiledRules, Resolution, Rule, RuleBook, Scope},
    ActionSpec, ACTIONS,
};
use std::path::Path;

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
                    Err(e) => format!("Rules need correction: {e:?}"),
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
                status: format!("Rules preserved; recovery required: {e:#}"),
            },
        }
    }
    fn save(&mut self) -> anyhow::Result<()> {
        CompiledRules::new(self.draft.clone())
            .map_err(|e| anyhow::anyhow!("invalid rules: {e:?}"))?;
        self.store
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("rule-file recovery required"))?
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
        ui.label("Rule preview · no actions execute");
        ui.horizontal(|ui| {
            ui.label("Application ID");
            ui.text_edit_singleline(&mut self.application);
        });
        if let Some(id) = gesture {
            ui.label(match self.preview(id) {
                Ok(Resolution::Selected { rule_id, action }) => {
                    format!("Rule {rule_id} → {:?} (preview only)", action.action)
                }
                Ok(Resolution::Excluded) => "Application excluded".into(),
                Ok(Resolution::InvalidApplication) => "Enter a valid application ID".into(),
                Ok(_) => "No matching rule".into(),
                Err(e) => format!("Fix invalid rules before preview: {e:?}"),
            });
        } else {
            ui.weak("Draw a recognized gesture to preview its rule for this application ID.");
        }
    }
    pub fn ui(&mut self, ui: &mut egui::Ui, dir: &Path, writable: bool) {
        ui.heading("Gesture rules");
        ui.label("Configure typed mappings and exclusions. Preview does not capture input or execute actions.");
        if self.store.is_none()
            && writable
            && ui.button("Back up and reset invalid rule file").clicked()
        {
            match Store::reset::<RuleBook>(dir.join("gesture-rules.json")) {
                Ok((store, draft)) => {
                    self.store = Some(store);
                    self.draft = draft;
                    self.status = "Rules reset; previous bytes backed up".into();
                }
                Err(e) => self.status = format!("Reset failed: {e:#}"),
            }
        }
        ui.add_enabled_ui(writable, |ui| {
            let mut delete = None;
            for (index, rule) in self.draft.rules.iter_mut().enumerate() {
                ui.push_id(rule.id, |ui| {
                    ui.group(|ui| {
                        ui.horizontal(|ui| {
                            ui.checkbox(&mut rule.enabled, format!("Rule {}", rule.id));
                            ui.label("Gesture ID");
                            ui.add(egui::DragValue::new(&mut rule.gesture).range(1..=8));
                            ui.label("Priority");
                            ui.add(egui::DragValue::new(&mut rule.priority));
                            if ui.button("Remove").clicked() {
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
                            .selected_text(["Global", "Application", "Application group"][kind])
                            .show_ui(ui, |ui| {
                                for (i, label) in ["Global", "Application", "Application group"]
                                    .iter()
                                    .enumerate()
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
                                    ui.label("Exact application ID");
                                    ui.text_edit_singleline(id);
                                });
                            }
                            Scope::Group { apps } => {
                                for id in apps.iter_mut() {
                                    ui.text_edit_singleline(id);
                                }
                                if apps.len() < 32 && ui.button("Add application").clicked() {
                                    apps.push(String::new());
                                }
                                if apps.len() > 1 && ui.button("Remove last application").clicked()
                                {
                                    apps.pop();
                                }
                            }
                            Scope::Global => (),
                        }
                        egui::ComboBox::from_id_salt("action")
                            .selected_text(format!("{:?}", rule.action.action))
                            .show_ui(ui, |ui| {
                                for action in ACTIONS {
                                    ui.selectable_value(
                                        &mut rule.action.action,
                                        action,
                                        format!("{action:?}"),
                                    );
                                }
                            });
                    });
                });
            }
            if let Some(index) = delete {
                self.draft.rules.remove(index);
            }
            if self.draft.rules.len() < 128 && ui.button("Add rule").clicked() {
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
            ui.label("Excluded application IDs (exact match, always take precedence)");
            for exclusion in &mut self.draft.exclusions {
                ui.text_edit_singleline(exclusion);
            }
            if self.draft.exclusions.len() < 128 && ui.button("Add exclusion").clicked() {
                self.draft.exclusions.push(String::new());
            }
            if !self.draft.exclusions.is_empty() && ui.button("Remove last exclusion").clicked() {
                self.draft.exclusions.pop();
            }
            let valid = CompiledRules::new(self.draft.clone());
            if let Err(e) = &valid {
                ui.label(format!(
                    "Cannot save: {e:?}. Resolve conflicts and empty IDs first."
                ));
            }
            if ui
                .add_enabled(
                    valid.is_ok() && self.store.is_some(),
                    egui::Button::new("Save rules"),
                )
                .clicked()
            {
                // Files never grant global input capture permission.
                self.draft.enabled = false;
                self.status = match self.save() {
                    Ok(()) => "Rules saved. Global gestures remain disabled.".into(),
                    Err(e) => format!("Save failed: {e:#}"),
                };
            }
        });
        if !self.status.is_empty() {
            ui.label(&self.status);
        }
        ui.weak("Exact IDs are supplied by the future platform adapter; window titles and wildcard matching are not used.");
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
