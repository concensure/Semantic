use anyhow::Result;
use engine::PatchApplicationMode;
use std::fs;
use std::path::Path;

const LLM_CONFIG_DEFAULT: &str = r#"# Provider endpoints
[providers]
primary = "<PRIMARY_PROVIDER_BASE_URL>"
secondary = "<SECONDARY_PROVIDER_BASE_URL>"

[provider_settings.primary]
model = "<PRIMARY_MODEL_NAME>"
api_key_env = "PRIMARY_LLM_API_KEY"

[provider_settings.secondary]
model = "<SECONDARY_MODEL_NAME>"
api_key_env = "SECONDARY_LLM_API_KEY"
"#;

const LLM_ROUTING_DEFAULT: &str = r#"[planning]
preferred = ["primary", "secondary"]

[execution]
preferred = ["primary", "secondary"]

[interactive]
preferred = ["secondary", "primary"]
"#;

const VALIDATION_DEFAULT: &str = r#"protected_paths = ["core/", "security/"]

require_confirmation_for = [
  "ChangeSignature",
  "RenameSymbol"
]
"#;

const POLICIES_DEFAULT: &str = r#"run_tests = false
run_lint = false
run_typecheck = false
"#;

const EDIT_CONFIG_DEFAULT: &str = r#"default_patch_mode = "confirm"
default_run_tests = true
require_session = true
require_recent_context = true
require_symbol_in_recent_context = true
max_session_age_secs = 900
allow_auto_apply = false
require_tests_for_auto_apply = true
require_clean_validation_for_auto_apply = true
block_on_index_mismatch = true
"#;

const WORKSPACE_DEFAULT: &str = r#"[roots]
paths = []
"#;

const IMPROVEMENT_LOOP_DEFAULT: &str = r#"enabled = true
export_regression_candidates = true
recent_incident_limit = 50
"#;

const RUST_SUPPORT_DEFAULT: &str = r#"enabled = false
small_project_mode = true
"#;

#[derive(Debug, Clone)]
pub struct EditSafetyConfig {
    pub default_patch_mode: PatchApplicationMode,
    pub default_run_tests: bool,
    pub require_session: bool,
    pub require_recent_context: bool,
    pub require_symbol_in_recent_context: bool,
    pub max_session_age_secs: u64,
    pub allow_auto_apply: bool,
    pub require_tests_for_auto_apply: bool,
    pub require_clean_validation_for_auto_apply: bool,
    pub block_on_index_mismatch: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct ImprovementLoopConfig {
    pub enabled: bool,
    pub export_regression_candidates: bool,
    pub recent_incident_limit: usize,
}

#[derive(Debug, Clone, Copy)]
pub struct RustSupportConfig {
    pub enabled: bool,
    pub small_project_mode: bool,
}

impl Default for ImprovementLoopConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            export_regression_candidates: true,
            recent_incident_limit: 50,
        }
    }
}

impl Default for RustSupportConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            small_project_mode: true,
        }
    }
}

impl Default for EditSafetyConfig {
    fn default() -> Self {
        Self {
            default_patch_mode: PatchApplicationMode::Confirm,
            default_run_tests: true,
            require_session: true,
            require_recent_context: true,
            require_symbol_in_recent_context: true,
            max_session_age_secs: 900,
            allow_auto_apply: false,
            require_tests_for_auto_apply: true,
            require_clean_validation_for_auto_apply: true,
            block_on_index_mismatch: true,
        }
    }
}

pub fn ensure_semantic_config(repo_root: &Path) -> Result<()> {
    let semantic_dir = repo_root.join(".semantic");
    fs::create_dir_all(&semantic_dir)?;

    ensure_file(&semantic_dir.join("llm_config.toml"), LLM_CONFIG_DEFAULT)?;
    ensure_file(&semantic_dir.join("llm_routing.toml"), LLM_ROUTING_DEFAULT)?;
    ensure_file(&semantic_dir.join("validation.toml"), VALIDATION_DEFAULT)?;
    ensure_file(&semantic_dir.join("policies.toml"), POLICIES_DEFAULT)?;
    ensure_file(&semantic_dir.join("edit_config.toml"), EDIT_CONFIG_DEFAULT)?;
    ensure_file(&semantic_dir.join("workspace.toml"), WORKSPACE_DEFAULT)?;
    ensure_file(
        &semantic_dir.join("improvement_loop.toml"),
        IMPROVEMENT_LOOP_DEFAULT,
    )?;
    ensure_file(&semantic_dir.join("rust.toml"), RUST_SUPPORT_DEFAULT)?;

    let metrics_path = semantic_dir.join("model_metrics.json");
    if !metrics_path.exists() {
        fs::write(metrics_path, "{}")?;
    }

    Ok(())
}

pub fn load_edit_safety_config(repo_root: &Path) -> EditSafetyConfig {
    let path = repo_root.join(".semantic").join("edit_config.toml");
    let raw = fs::read_to_string(path).unwrap_or_default();
    parse_edit_safety_config(&raw)
}

pub fn load_improvement_loop_config(repo_root: &Path) -> ImprovementLoopConfig {
    let path = repo_root.join(".semantic").join("improvement_loop.toml");
    let raw = fs::read_to_string(path).unwrap_or_default();
    parse_improvement_loop_config(&raw)
}

pub fn load_rust_support_config(repo_root: &Path) -> RustSupportConfig {
    let path = repo_root.join(".semantic").join("rust.toml");
    let raw = fs::read_to_string(path).unwrap_or_default();
    parse_rust_support_config(&raw)
}

fn ensure_file(path: &Path, contents: &str) -> Result<()> {
    if !path.exists() {
        fs::write(path, contents)?;
    }
    Ok(())
}

fn parse_edit_safety_config(raw: &str) -> EditSafetyConfig {
    let mut cfg = EditSafetyConfig::default();
    for line in raw.lines() {
        let line = line.split('#').next().unwrap_or_default().trim();
        if line.is_empty() {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim().trim_matches('"');
        match key {
            "default_patch_mode" => {
                cfg.default_patch_mode = match value.to_ascii_lowercase().as_str() {
                    "preview_only" | "preview" => PatchApplicationMode::PreviewOnly,
                    "auto_apply" | "auto" => PatchApplicationMode::AutoApply,
                    _ => PatchApplicationMode::Confirm,
                };
            }
            "default_run_tests" => cfg.default_run_tests = parse_bool(value, cfg.default_run_tests),
            "require_session" => cfg.require_session = parse_bool(value, cfg.require_session),
            "require_recent_context" => {
                cfg.require_recent_context = parse_bool(value, cfg.require_recent_context)
            }
            "require_symbol_in_recent_context" => {
                cfg.require_symbol_in_recent_context =
                    parse_bool(value, cfg.require_symbol_in_recent_context)
            }
            "max_session_age_secs" => {
                cfg.max_session_age_secs = value.parse().unwrap_or(cfg.max_session_age_secs)
            }
            "allow_auto_apply" => cfg.allow_auto_apply = parse_bool(value, cfg.allow_auto_apply),
            "require_tests_for_auto_apply" => {
                cfg.require_tests_for_auto_apply =
                    parse_bool(value, cfg.require_tests_for_auto_apply)
            }
            "require_clean_validation_for_auto_apply" => {
                cfg.require_clean_validation_for_auto_apply =
                    parse_bool(value, cfg.require_clean_validation_for_auto_apply)
            }
            "block_on_index_mismatch" => {
                cfg.block_on_index_mismatch = parse_bool(value, cfg.block_on_index_mismatch)
            }
            _ => {}
        }
    }
    cfg
}

fn parse_bool(raw: &str, default: bool) -> bool {
    match raw.trim().to_ascii_lowercase().as_str() {
        "true" => true,
        "false" => false,
        _ => default,
    }
}

fn parse_improvement_loop_config(raw: &str) -> ImprovementLoopConfig {
    let mut cfg = ImprovementLoopConfig::default();
    for line in raw.lines() {
        let line = line.split('#').next().unwrap_or_default().trim();
        if line.is_empty() {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim().trim_matches('"');
        match key {
            "enabled" => cfg.enabled = parse_bool(value, cfg.enabled),
            "export_regression_candidates" => {
                cfg.export_regression_candidates =
                    parse_bool(value, cfg.export_regression_candidates)
            }
            "recent_incident_limit" => {
                cfg.recent_incident_limit = value.parse().unwrap_or(cfg.recent_incident_limit)
            }
            _ => {}
        }
    }
    cfg
}

fn parse_rust_support_config(raw: &str) -> RustSupportConfig {
    let mut cfg = RustSupportConfig::default();
    for line in raw.lines() {
        let line = line.split('#').next().unwrap_or_default().trim();
        if line.is_empty() {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim().trim_matches('"');
        match key {
            "enabled" => cfg.enabled = parse_bool(value, cfg.enabled),
            "small_project_mode" => {
                cfg.small_project_mode = parse_bool(value, cfg.small_project_mode)
            }
            _ => {}
        }
    }
    cfg
}

#[cfg(test)]
mod tests {
    use super::{
        load_edit_safety_config, load_improvement_loop_config, load_rust_support_config,
        EditSafetyConfig, ImprovementLoopConfig, RustSupportConfig,
    };
    use engine::PatchApplicationMode;
    use std::fs;

    #[test]
    fn edit_safety_config_defaults_to_conservative_values() {
        let cfg = EditSafetyConfig::default();
        assert!(matches!(cfg.default_patch_mode, PatchApplicationMode::Confirm));
        assert!(cfg.default_run_tests);
        assert!(cfg.require_session);
        assert!(cfg.require_recent_context);
        assert!(cfg.require_symbol_in_recent_context);
        assert!(!cfg.allow_auto_apply);
        assert!(cfg.require_tests_for_auto_apply);
        assert!(cfg.require_clean_validation_for_auto_apply);
        assert!(cfg.block_on_index_mismatch);
    }

    #[test]
    fn edit_safety_config_loads_overrides_from_repo_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let semantic_dir = tmp.path().join(".semantic");
        fs::create_dir_all(&semantic_dir).expect("mkdir .semantic");
        fs::write(
            semantic_dir.join("edit_config.toml"),
            r#"
default_patch_mode = "preview_only"
default_run_tests = false
require_session = false
max_session_age_secs = 42
allow_auto_apply = false
"#,
        )
        .expect("write edit config");

        let cfg = load_edit_safety_config(tmp.path());
        assert!(matches!(cfg.default_patch_mode, PatchApplicationMode::PreviewOnly));
        assert!(!cfg.default_run_tests);
        assert!(!cfg.require_session);
        assert_eq!(cfg.max_session_age_secs, 42);
        assert!(!cfg.allow_auto_apply);
    }

    #[test]
    fn improvement_loop_config_defaults_to_enabled_capture() {
        let cfg = ImprovementLoopConfig::default();
        assert!(cfg.enabled);
        assert!(cfg.export_regression_candidates);
        assert_eq!(cfg.recent_incident_limit, 50);
    }

    #[test]
    fn improvement_loop_config_loads_overrides_from_repo_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let semantic_dir = tmp.path().join(".semantic");
        fs::create_dir_all(&semantic_dir).expect("mkdir .semantic");
        fs::write(
            semantic_dir.join("improvement_loop.toml"),
            r#"
enabled = false
export_regression_candidates = false
recent_incident_limit = 12
"#,
        )
        .expect("write improvement loop config");

        let cfg = load_improvement_loop_config(tmp.path());
        assert!(!cfg.enabled);
        assert!(!cfg.export_regression_candidates);
        assert_eq!(cfg.recent_incident_limit, 12);
    }

    #[test]
    fn rust_support_config_defaults_to_disabled() {
        let cfg = RustSupportConfig::default();
        assert!(!cfg.enabled);
        assert!(cfg.small_project_mode);
    }

    #[test]
    fn rust_support_config_loads_repo_overrides() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let semantic_dir = tmp.path().join(".semantic");
        fs::create_dir_all(&semantic_dir).expect("mkdir .semantic");
        fs::write(
            semantic_dir.join("rust.toml"),
            r#"
enabled = true
small_project_mode = false
"#,
        )
        .expect("write rust config");

        let cfg = load_rust_support_config(tmp.path());
        assert!(cfg.enabled);
        assert!(!cfg.small_project_mode);
    }
}
