//! Named, per-certificate primary layouts. Applying one uses the ordinary edit gate.
use super::{config_store::Store, data_access, edge_label, egui, App};
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, path::PathBuf};

const MAX_PROFILES: usize = 64;
const NAME_CHARS: usize = 32;

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Profile {
    peer: String,
    name: String,
    edge: String,
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Profiles {
    entries: Vec<Profile>,
}

impl Profiles {
    fn validate(&self) -> Result<()> {
        ensure!(self.entries.len() <= MAX_PROFILES, "布局档案数量超过限制");
        let mut names = BTreeSet::new();
        for entry in &self.entries {
            ensure!(
                entry.peer.len() == 64 && entry.peer.bytes().all(|byte| byte.is_ascii_hexdigit())
                    && valid_name(&entry.name) && matches!(entry.edge.as_str(), "left" | "right" | "top" | "bottom")
                    && names.insert((entry.peer.as_str(), entry.name.as_str())),
                "布局档案内容无效或重名"
            );
        }
        Ok(())
    }
}

fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.trim() == name && name.chars().count() <= NAME_CHARS
        && !name.chars().any(char::is_control)
}

struct Local {
    store: Store,
    profiles: Profiles,
}

impl Local {
    fn open(path: PathBuf) -> Result<Self> {
        let (store, profiles) = Store::open::<Profiles>(path)?;
        profiles.validate()?;
        Ok(Self { store, profiles })
    }

    fn save(&mut self, next: Profiles) -> Result<()> {
        next.validate()?;
        self.store.save(&next)?;
        self.profiles = next;
        Ok(())
    }
}

#[derive(Default)]
pub(super) struct Panel {
    loaded: Option<Result<Local>>,
    name: String,
}

impl Panel {
    pub(super) fn ui(&mut self, app: &mut App, ui: &mut egui::Ui) {
        if !data_access::ready(app) {
            return;
        }
        ui.collapsing("主屏布局档案", |ui| self.draw(app, ui));
    }

    fn draw(&mut self, app: &mut App, ui: &mut egui::Ui) {
        let Some(peer) = app.config.fingerprint.clone() else { return; };
        let store = self.loaded.get_or_insert_with(|| Local::open(app.dir.join("layout-profiles.json")));
        let Ok(store) = store else {
            ui.label("布局档案只读，原文件已保留。");
            return;
        };
        let editable = app.layout_editable();
        let mut action = None;
        ui.add_enabled_ui(editable, |ui| {
            for (index, entry) in store.profiles.entries.iter().enumerate().filter(|(_, entry)| entry.peer == peer) {
                ui.horizontal_wrapped(|ui| {
                    ui.label(format!("{} · {}", entry.name, edge_label(&entry.edge)));
                    if ui.small_button("应用").clicked() {
                        action = Some(Action::Apply(index));
                    }
                    if ui.small_button("删除").clicked() {
                        action = Some(Action::Delete(index));
                    }
                });
            }
            ui.horizontal_wrapped(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.name).char_limit(NAME_CHARS).hint_text("档案名称"));
                if ui.add_enabled(valid_name(self.name.trim()), egui::Button::new("保存当前方向")).clicked() {
                    action = Some(Action::Save);
                }
            });
        });
        let Some(action) = action else { return; };
        if let Err(error) = data_access::Pin::load(app) {
            app.feedback(Err(error), "");
            return;
        }
        match action {
            Action::Apply(index) => {
                app.try_set_edge(&store.profiles.entries[index].edge);
            }
            Action::Delete(index) => {
                let mut next = store.profiles.clone();
                next.entries.remove(index);
                app.feedback(store.save(next), "已删除主屏布局档案。");
            }
            Action::Save => {
                let mut next = store.profiles.clone();
                next.entries.push(Profile {
                    peer,
                    name: self.name.trim().to_owned(),
                    edge: app.displayed_edge().to_owned(),
                });
                let result = store.save(next);
                if result.is_ok() {
                    self.name.clear();
                }
                app.feedback(result, "已保存主屏布局档案。");
            }
        }
    }
}

enum Action {
    Apply(usize),
    Delete(usize),
    Save,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layouts_are_per_peer_and_future_store_is_kept() -> Result<()> {
        let fixture = super::super::tests::Fixture::new()?;
        let path = fixture.path.join("layout-profiles.json");
        let mut store = Local::open(path.clone())?;
        let a = Profile { peer: "a".repeat(64), name: "工作台".into(), edge: "left".into() };
        let b = Profile { peer: "b".repeat(64), name: a.name.clone(), edge: "top".into() };
        store.save(Profiles { entries: vec![a.clone(), b] })?;
        assert!(store.save(Profiles { entries: vec![a.clone(), a] }).is_err());
        let reopened = Local::open(path.clone())?;
        assert_eq!(reopened.profiles.entries[0].edge, "left");
        assert_eq!(reopened.profiles.entries[1].edge, "top");
        let future = br#"{"store_version":2,"data":{"entries":[]}}"#;
        std::fs::write(&path, future)?;
        assert!(Local::open(path.clone()).is_err());
        assert_eq!(std::fs::read(&path)?, future);
        fixture.close()
    }
}
