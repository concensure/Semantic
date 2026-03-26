use anyhow::Result;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

/// Builds a compact, LLM-ready project map from the existing index.
/// No LLM call required — rule-based from storage.
pub struct ProjectSummariser<'a> {
    storage: &'a storage::Storage,
}

impl<'a> ProjectSummariser<'a> {
    pub fn new(storage: &'a storage::Storage) -> Self {
        Self { storage }
    }

    /// Build the full summary document. Returns JSON + markdown text.
    pub fn build(&self, max_tokens: usize) -> Result<SummaryDocument> {
        let modules = self.storage.list_modules()?;
        let named_deps = self.storage.list_named_module_dependencies()?;

        // Build file→module map
        let mut file_to_module: HashMap<String, String> = HashMap::new();
        for module in &modules {
            let module_id = module.id.unwrap_or_default();
            for mf in self.storage.list_module_files(module_id)? {
                file_to_module.insert(mf.file_path, module.name.clone());
            }
        }

        // Build symbols per file
        let all_symbols = self.storage.list_symbols()?;
        let mut symbols_by_file: HashMap<String, Vec<String>> = HashMap::new();
        for sym in &all_symbols {
            symbols_by_file
                .entry(sym.file.clone())
                .or_default()
                .push(sym.name.clone());
        }

        // Token budget: ~15 tokens per file line, cap at max_tokens
        let max_files = ((max_tokens.saturating_sub(100)) / 15).max(5);

        // Build per-module file summaries
        let mut module_summaries: Vec<Value> = Vec::new();
        let mut entry_points: Vec<String> = Vec::new();
        let mut total_files = 0usize;

        // Collect files in module order; fall back to unmodule'd files
        let mut seen_files: std::collections::HashSet<String> = std::collections::HashSet::new();

        for module in &modules {
            let module_id = module.id.unwrap_or_default();
            let module_files = self.storage.list_module_files(module_id)?;
            let mut file_entries: Vec<Value> = Vec::new();

            for mf in module_files {
                if total_files >= max_files {
                    break;
                }
                seen_files.insert(mf.file_path.clone());
                let top_symbols = symbols_by_file
                    .get(&mf.file_path)
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .take(6)
                    .collect::<Vec<_>>();
                let objective = infer_file_objective(&mf.file_path, &top_symbols);
                let purpose = purpose_sentence(&mf.file_path, &objective, &top_symbols);
                if is_entry_point(&mf.file_path, &top_symbols) {
                    entry_points.push(mf.file_path.clone());
                }
                file_entries.push(json!({
                    "path": mf.file_path,
                    "objective": objective,
                    "purpose": purpose,
                    "top_symbols": top_symbols,
                }));
                total_files += 1;
            }

            if !file_entries.is_empty() {
                module_summaries.push(json!({
                    "name": module.name,
                    "files": file_entries,
                }));
            }
        }

        // Add files not in any module (up to budget)
        let all_files = self.storage.list_files()?;
        let mut unmodule_entries: Vec<Value> = Vec::new();
        for file in &all_files {
            if total_files >= max_files {
                break;
            }
            if seen_files.contains(file) {
                continue;
            }
            let top_symbols = symbols_by_file
                .get(file)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .take(6)
                .collect::<Vec<_>>();
            let objective = infer_file_objective(file, &top_symbols);
            let purpose = purpose_sentence(file, &objective, &top_symbols);
            if is_entry_point(file, &top_symbols) {
                entry_points.push(file.clone());
            }
            unmodule_entries.push(json!({
                "path": file,
                "objective": objective,
                "purpose": purpose,
                "top_symbols": top_symbols,
            }));
            total_files += 1;
        }
        if !unmodule_entries.is_empty() {
            module_summaries.push(json!({
                "name": "other",
                "files": unmodule_entries,
            }));
        }

        // Dependency sketch
        let dep_sketch = if named_deps.is_empty() {
            String::new()
        } else {
            named_deps
                .iter()
                .map(|(from, to)| format!("{from} → {to}"))
                .collect::<Vec<_>>()
                .join(", ")
        };

        // Narrative
        let narrative = build_narrative(
            &modules.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(),
            &entry_points,
            &dep_sketch,
            total_files,
            all_symbols.len(),
        );

        // Markdown text
        let summary_text = build_markdown(&narrative, &module_summaries, &dep_sketch, &entry_points);
        let token_estimate = (summary_text.len() / 4).max(1);

        Ok(SummaryDocument {
            narrative,
            modules: module_summaries,
            dependency_sketch: dep_sketch,
            entry_points,
            summary_text,
            token_estimate,
            file_count: total_files,
            module_count: modules.len(),
            cached_at_epoch_s: now_epoch_s(),
        })
    }
}

#[derive(Debug, Clone)]
pub struct SummaryDocument {
    pub narrative: String,
    pub modules: Vec<Value>,
    pub dependency_sketch: String,
    pub entry_points: Vec<String>,
    pub summary_text: String,
    pub token_estimate: usize,
    pub file_count: usize,
    pub module_count: usize,
    pub cached_at_epoch_s: u64,
}

impl SummaryDocument {
    pub fn to_json(&self) -> Value {
        json!({
            "narrative": self.narrative,
            "modules": self.modules,
            "dependency_sketch": self.dependency_sketch,
            "entry_points": self.entry_points,
        })
    }
}

// ── helpers ──────────────────────────────────────────────────────────────────

fn infer_file_objective(path: &str, top_symbols: &[String]) -> &'static str {
    let p = path.to_lowercase();
    if p.contains("test") || p.contains("spec") {
        return "tests";
    }
    if p.contains("api") || p.contains("server") || p.contains("route") {
        return "api_surface";
    }
    if p.contains("store") || p.contains("repo") || p.contains("db") {
        return "data_layer";
    }
    if p.contains("ui") || p.contains("component") || p.contains("view") || p.contains(".tsx") {
        return "ui_layer";
    }
    if top_symbols
        .iter()
        .any(|s| s.to_lowercase().contains("render") || s.to_lowercase().contains("component"))
    {
        return "ui_layer";
    }
    "application_logic"
}

fn purpose_sentence(path: &str, objective: &str, top_symbols: &[String]) -> String {
    let filename = std::path::Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(path);
    match objective {
        "tests" => format!("test suite for {filename}"),
        "api_surface" => format!("HTTP API surface — {}", top_symbols.first().cloned().unwrap_or_else(|| filename.to_string())),
        "data_layer" => format!("data layer — {}", top_symbols.iter().take(3).cloned().collect::<Vec<_>>().join(", ")),
        "ui_layer" => format!("UI layer — {}", top_symbols.iter().take(3).cloned().collect::<Vec<_>>().join(", ")),
        _ => format!("application logic — {}", top_symbols.iter().take(3).cloned().collect::<Vec<_>>().join(", ")),
    }
}

fn is_entry_point(path: &str, top_symbols: &[String]) -> bool {
    let p = path.to_lowercase();
    if p.contains("main") || p.contains("app") || p.contains("index") || p.contains("server") {
        return true;
    }
    top_symbols.iter().any(|s| {
        let sl = s.to_lowercase();
        sl == "main" || sl == "run" || sl.contains("start") || sl.contains("init")
    })
}

fn build_narrative(
    module_names: &[&str],
    entry_points: &[String],
    dep_sketch: &str,
    file_count: usize,
    symbol_count: usize,
) -> String {
    let module_list = if module_names.is_empty() {
        "no named modules".to_string()
    } else {
        module_names.join(", ")
    };
    let entry = entry_points
        .first()
        .map(|p| {
            std::path::Path::new(p)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(p)
                .to_string()
        })
        .unwrap_or_else(|| "unknown".to_string());
    let dep_note = if dep_sketch.is_empty() {
        String::new()
    } else {
        format!(" Dependencies: {dep_sketch}.")
    };
    format!(
        "Project with {file_count} indexed files and {symbol_count} symbols across modules: {module_list}. Entry point: {entry}.{dep_note}"
    )
}

fn build_markdown(
    narrative: &str,
    modules: &[Value],
    dep_sketch: &str,
    entry_points: &[String],
) -> String {
    let mut out = String::new();
    out.push_str("## Project Map\n\n");
    out.push_str(&format!("**What this project does:** {narrative}\n\n"));

    if !entry_points.is_empty() {
        out.push_str(&format!(
            "**Entry points:** {}\n\n",
            entry_points
                .iter()
                .map(|p| std::path::Path::new(p)
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or(p))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    out.push_str("### Modules\n\n");
    for module in modules {
        let name = module.get("name").and_then(|v| v.as_str()).unwrap_or("?");
        out.push_str(&format!("**{name}**\n"));
        if let Some(files) = module.get("files").and_then(|v| v.as_array()) {
            for file in files {
                let path = file.get("path").and_then(|v| v.as_str()).unwrap_or("");
                let purpose = file.get("purpose").and_then(|v| v.as_str()).unwrap_or("");
                let symbols = file
                    .get("top_symbols")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
                    .unwrap_or_default();
                out.push_str(&format!("- `{path}` — {purpose}\n"));
                if !symbols.is_empty() {
                    out.push_str(&format!("  Symbols: {symbols}\n"));
                }
            }
        }
        out.push('\n');
    }

    if !dep_sketch.is_empty() {
        out.push_str("### Dependencies\n");
        out.push_str(dep_sketch);
        out.push('\n');
    }

    out
}

fn now_epoch_s() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}
