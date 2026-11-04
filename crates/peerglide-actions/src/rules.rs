//! Closed local rule schema. Selection is not authorization or execution.
use crate::{ActionSpec, SCHEMA_VERSION};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const MAX_RULE_BYTES: usize = 64 * 1024;
const MAX_RULES: usize = 128;
const MAX_APPS: usize = 32;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Scope {
    Global,
    Group { apps: Vec<String> },
    Application { id: String },
}
impl Scope {
    fn rank(&self) -> u8 {
        match self {
            Self::Global => 0,
            Self::Group { .. } => 1,
            Self::Application { .. } => 2,
        }
    }
    fn matches(&self, app: &str) -> bool {
        match self {
            Self::Global => true,
            Self::Group { apps } => apps.iter().any(|a| a == app),
            Self::Application { id } => id == app,
        }
    }
    fn overlaps(&self, other: &Self) -> bool {
        match self {
            Self::Global => true,
            Self::Application { id } => other.matches(id),
            Self::Group { apps } => apps.iter().any(|a| other.matches(a)),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub id: u64,
    pub enabled: bool,
    pub gesture: u32,
    pub scope: Scope,
    pub priority: i16,
    pub action: ActionSpec,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleBook {
    pub schema_version: u16,
    pub enabled: bool,
    pub exclusions: Vec<String>,
    pub rules: Vec<Rule>,
}
impl Default for RuleBook {
    fn default() -> Self {
        Self {
            schema_version: 1,
            enabled: false,
            exclusions: vec![],
            rules: vec![],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuleError {
    Size,
    Json,
    Schema,
    Limit,
    Application,
    DuplicateId,
    Action,
    Conflict,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resolution {
    Disabled,
    Excluded,
    Unmapped,
    InvalidApplication,
    Selected { rule_id: u64, action: ActionSpec },
}
pub struct CompiledRules(RuleBook);
/// Shared exact-match rule identifier bound. Platform IDs may be namespaced, but
/// existing user-specified IDs retain their original case and representation.
pub fn valid_application_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 256 && id.trim() == id && !id.chars().any(char::is_control)
}
impl CompiledRules {
    pub fn from_json(bytes: &[u8]) -> Result<Self, RuleError> {
        if bytes.len() > MAX_RULE_BYTES {
            return Err(RuleError::Size);
        }
        Self::new(serde_json::from_slice(bytes).map_err(|_| RuleError::Json)?)
    }
    pub fn new(book: RuleBook) -> Result<Self, RuleError> {
        if book.schema_version != 1 {
            return Err(RuleError::Schema);
        }
        if book.rules.len() > MAX_RULES || book.exclusions.len() > MAX_RULES {
            return Err(RuleError::Limit);
        }
        if book.exclusions.iter().any(|a| !valid_application_id(a)) {
            return Err(RuleError::Application);
        }
        let mut ids = BTreeSet::new();
        for rule in &book.rules {
            if rule.id == 0 || !ids.insert(rule.id) {
                return Err(RuleError::DuplicateId);
            }
            if rule.gesture == 0 || rule.action.schema_version != SCHEMA_VERSION {
                return Err(RuleError::Action);
            }
            match &rule.scope {
                Scope::Global => (),
                Scope::Application { id } if valid_application_id(id) => (),
                Scope::Group { apps }
                    if !apps.is_empty()
                        && apps.len() <= MAX_APPS
                        && apps.iter().all(|a| valid_application_id(a)) => {}
                _ => return Err(RuleError::Application),
            }
        }
        for (i, a) in book.rules.iter().enumerate().filter(|(_, r)| r.enabled) {
            for b in book.rules.iter().skip(i + 1).filter(|r| r.enabled) {
                if a.gesture == b.gesture
                    && a.scope.rank() == b.scope.rank()
                    && a.priority == b.priority
                    && a.scope.overlaps(&b.scope)
                {
                    return Err(RuleError::Conflict);
                }
            }
        }
        Ok(Self(book))
    }
    /// App IDs are exact caller-provided local identities, not window titles,
    /// wildcards or a substitute for authenticating an OS application.
    pub fn resolve(&self, gesture: u32, app: &str) -> Resolution {
        if !self.0.enabled {
            return Resolution::Disabled;
        }
        if !valid_application_id(app) {
            return Resolution::InvalidApplication;
        }
        if self.0.exclusions.iter().any(|a| a == app) {
            return Resolution::Excluded;
        }
        self.0
            .rules
            .iter()
            .filter(|r| r.enabled && r.gesture == gesture && r.scope.matches(app))
            .max_by_key(|r| (r.scope.rank(), r.priority))
            .map_or(Resolution::Unmapped, |r| Resolution::Selected {
                rule_id: r.id,
                action: r.action,
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Action;
    fn rule(id: u64, scope: Scope, priority: i16) -> Rule {
        Rule {
            id,
            enabled: true,
            gesture: 2,
            scope,
            priority,
            action: ActionSpec::new(Action::Copy),
        }
    }
    fn book() -> RuleBook {
        RuleBook {
            enabled: true,
            rules: vec![
                rule(1, Scope::Global, 100),
                rule(
                    2,
                    Scope::Group {
                        apps: vec!["app:a".into(), "app:b".into()],
                    },
                    50,
                ),
                rule(3, Scope::Application { id: "app:a".into() }, -100),
            ],
            ..RuleBook::default()
        }
    }
    fn id(r: Resolution) -> u64 {
        match r {
            Resolution::Selected { rule_id, .. } => rule_id,
            _ => panic!("no rule"),
        }
    }
    #[test]
    fn exclusions_and_specificity_win_over_priority_and_file_order() {
        let mut b = book();
        b.exclusions.push("app:b".into());
        let compiled = CompiledRules::new(b.clone()).unwrap();
        assert_eq!(id(compiled.resolve(2, "app:a")), 3);
        assert_eq!(compiled.resolve(2, "app:b"), Resolution::Excluded);
        assert_eq!(id(compiled.resolve(2, "app:c")), 1);
        assert_eq!(compiled.resolve(99, "app:a"), Resolution::Unmapped);
        b.rules.reverse();
        assert_eq!(id(CompiledRules::new(b).unwrap().resolve(2, "app:a")), 3);
        assert_eq!(
            CompiledRules::new(RuleBook::default())
                .unwrap()
                .resolve(2, "app:a"),
            Resolution::Disabled
        );
    }
    #[test]
    fn overlapping_equal_rank_priority_rejects_instead_of_guessing() {
        let mut b = book();
        b.rules.push(rule(
            4,
            Scope::Group {
                apps: vec!["app:b".into()],
            },
            50,
        ));
        assert!(matches!(
            CompiledRules::new(b.clone()),
            Err(RuleError::Conflict)
        ));
        b.rules.last_mut().unwrap().priority = 51;
        assert_eq!(
            id(CompiledRules::new(b.clone()).unwrap().resolve(2, "app:b")),
            4
        );
        b.rules.last_mut().unwrap().id = 1;
        assert!(matches!(CompiledRules::new(b), Err(RuleError::DuplicateId)));
    }
    #[test]
    fn strict_schema_and_bounded_inputs_do_not_enable_unknown_commands() {
        let encoded = serde_json::to_vec(&book()).unwrap();
        assert!(CompiledRules::from_json(&encoded).is_ok());
        assert!(matches!(
            CompiledRules::from_json(&vec![0; MAX_RULE_BYTES + 1]),
            Err(RuleError::Size)
        ));
        assert!(matches!(
            CompiledRules::from_json(
                br#"{"schema_version":1,"enabled":true,"exclusions":[],"rules":[],"command":"x"}"#
            ),
            Err(RuleError::Json)
        ));
        let mut b = book();
        b.schema_version = 2;
        assert!(matches!(CompiledRules::new(b), Err(RuleError::Schema)));
        let mut b = book();
        b.rules[0].action.schema_version = 2;
        assert!(matches!(CompiledRules::new(b), Err(RuleError::Action)));
        assert_eq!(
            CompiledRules::new(book()).unwrap().resolve(2, "app:a\n"),
            Resolution::InvalidApplication
        );
    }
}
