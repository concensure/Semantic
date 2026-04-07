use anyhow::Result;
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
default_run_tests = false
"#;

const WORKSPACE_DEFAULT: &str = r#"[roots]
paths = []
"#;

pub fn ensure_semantic_config(repo_root: &Path) -> Result<()> {
    let semantic_dir = repo_root.join(".semantic");
    fs::create_dir_all(&semantic_dir)?;

    ensure_file(&semantic_dir.join("llm_config.toml"), LLM_CONFIG_DEFAULT)?;
    ensure_file(&semantic_dir.join("llm_routing.toml"), LLM_ROUTING_DEFAULT)?;
    ensure_file(&semantic_dir.join("validation.toml"), VALIDATION_DEFAULT)?;
    ensure_file(&semantic_dir.join("policies.toml"), POLICIES_DEFAULT)?;
    ensure_file(&semantic_dir.join("edit_config.toml"), EDIT_CONFIG_DEFAULT)?;
    ensure_file(&semantic_dir.join("workspace.toml"), WORKSPACE_DEFAULT)?;

    let metrics_path = semantic_dir.join("model_metrics.json");
    if !metrics_path.exists() {
        fs::write(metrics_path, "{}")?;
    }

    Ok(())
}

fn ensure_file(path: &Path, contents: &str) -> Result<()> {
    if !path.exists() {
        fs::write(path, contents)?;
    }
    Ok(())
}
