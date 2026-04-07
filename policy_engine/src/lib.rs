use anyhow::Result;
use engine::{EditPlan, EditType};
use serde::Deserialize;

#[derive(Debug, Deserialize, Default)]
pub struct PolicyConfig {
    pub protected_paths: Option<Vec<String>>,
    pub require_confirmation_for: Option<Vec<String>>,
}

pub struct PolicyEngine {
    config: PolicyConfig,
}

fn parse_assignment(line: &str) -> Option<(&str, &str)> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    let mut parts = trimmed.splitn(2, '=');
    let key = parts.next()?.trim();
    let value = parts.next()?.trim();
    if key.is_empty() || value.is_empty() {
        return None;
    }
    Some((key, value))
}

fn parse_string_array(value: &str) -> Vec<String> {
    let v = value.trim();
    if !(v.starts_with('[') && v.ends_with(']')) {
        return Vec::new();
    }
    let inner = &v[1..v.len() - 1];
    inner
        .split(',')
        .map(str::trim)
        .filter(|s| s.len() >= 2 && s.starts_with('"') && s.ends_with('"'))
        .map(|s| s[1..s.len() - 1].to_string())
        .collect()
}

impl PolicyEngine {
    pub fn from_toml(content: &str) -> Result<Self> {
        let mut config = PolicyConfig::default();
        for raw in content.lines() {
            if let Some((key, value)) = parse_assignment(raw) {
                match key {
                    "protected_paths" => config.protected_paths = Some(parse_string_array(value)),
                    "require_confirmation_for" => {
                        config.require_confirmation_for = Some(parse_string_array(value))
                    }
                    _ => {}
                }
            }
        }
        Ok(Self { config })
    }

    pub fn validate_edit_plan(&self, plan: &EditPlan) -> Result<()> {
        let protected = self.config.protected_paths.clone().unwrap_or_default();
        for ctx in &plan.required_context {
            if protected.iter().any(|p| ctx.file_path.starts_with(p)) {
                anyhow::bail!("edit touches protected path: {}", ctx.file_path);
            }
        }
        Ok(())
    }

    pub fn requires_confirmation(&self, edit_type: &EditType) -> bool {
        let required = self
            .config
            .require_confirmation_for
            .clone()
            .unwrap_or_default();
        let key = match edit_type {
            EditType::ModifyLogic => "ModifyLogic",
            EditType::ChangeSignature => "ChangeSignature",
            EditType::RefactorFunction => "RefactorFunction",
            EditType::RenameSymbol => "RenameSymbol",
        };
        required.iter().any(|v| v == key)
    }
}

#[cfg(test)]
mod tests {
    use super::PolicyEngine;
    use engine::{EditContextItem, EditPlan, EditType};

    fn mk_plan(file_path: &str, edit_type: EditType) -> EditPlan {
        EditPlan {
            target_symbol: "retryRequest".to_string(),
            edit_type,
            impacted_symbols: vec!["retryRequest".to_string()],
            required_context: vec![EditContextItem {
                file_path: file_path.to_string(),
                start_line: 1,
                end_line: 10,
                priority: 0,
                text: "code".to_string(),
            }],
        }
    }

    #[test]
    fn blocks_protected_paths() {
        let cfg = r#"
protected_paths = ["core/", "security/"]
require_confirmation_for = ["ChangeSignature"]
"#;
        let engine = PolicyEngine::from_toml(cfg).expect("policy");
        let plan = mk_plan("core/retry.ts", EditType::ModifyLogic);
        assert!(engine.validate_edit_plan(&plan).is_err());
    }

    #[test]
    fn requires_confirmation_for_configured_edit_types() {
        let cfg = r#"
protected_paths = []
require_confirmation_for = ["ChangeSignature", "RenameSymbol"]
"#;
        let engine = PolicyEngine::from_toml(cfg).expect("policy");
        assert!(engine.requires_confirmation(&EditType::ChangeSignature));
        assert!(engine.requires_confirmation(&EditType::RenameSymbol));
        assert!(!engine.requires_confirmation(&EditType::ModifyLogic));
    }
}
