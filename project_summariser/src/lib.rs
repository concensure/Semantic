use anyhow::Result;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::time::{SystemTime, UNIX_EPOCH};

/// Controls how much detail the project summary includes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SummaryTier {
    /// ~50 tokens: narrative (truncated to 300 chars) + entry_points list only.
    Nano,
    /// ~200 tokens: narrative + modules with top files (existing behaviour).
    Standard,
    /// ~800 tokens: everything including dependency_sketch.
    Full,
}

pub struct ProjectSummariser<'a> {
    storage: &'a storage::Storage,
}

impl<'a> ProjectSummariser<'a> {
    pub fn new(storage: &'a storage::Storage) -> Self {
        Self { storage }
    }

    pub fn build(&self, max_tokens: usize) -> Result<SummaryDocument> {
        self.build_with_options(max_tokens, false)
    }

    /// Build a tiered summary. Token budget is chosen per tier:
    /// Nano=50, Standard=200, Full=800. The `include_error_hints` flag
    /// is still honoured for Standard and Full tiers.
    pub fn build_tiered(
        &self,
        tier: SummaryTier,
        include_error_hints: bool,
    ) -> Result<SummaryDocument> {
        let max_tokens = match tier {
            SummaryTier::Nano => 50,
            SummaryTier::Standard => 200,
            SummaryTier::Full => 800,
        };
        let mut doc = self.build_with_options(max_tokens, include_error_hints)?;
        if tier == SummaryTier::Nano {
            // Truncate narrative to 300 chars and clear full module list
            doc.narrative = doc.narrative.chars().take(300).collect();
            doc.modules.clear();
            doc.dependency_sketch = String::new();
            // Rebuild a minimal summary_text
            let ep = doc.entry_points.iter()
                .map(|p| std::path::Path::new(p)
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or(p))
                .collect::<Vec<_>>()
                .join(", ");
            doc.summary_text = format!("{}\nEntry points: {}", doc.narrative, ep);
            doc.token_estimate = (doc.summary_text.len() / 4).max(1);
        } else if tier == SummaryTier::Standard {
            // Standard: keep narrative + modules, but omit dep_sketch from summary_text
            doc.dependency_sketch = String::new();
            doc.summary_text = build_markdown(&doc.narrative, &doc.modules, "", &doc.entry_points);
            doc.token_estimate = (doc.summary_text.len() / 4).max(1);
        }
        Ok(doc)
    }

    /// Build a summary filtered to modules relevant to `target_symbol`.
    /// For Nano tier, module filtering is skipped (already minimal).
    /// Returns `summary_scope = "symbol_filtered"` when filtering is applied.
    pub fn build_with_symbol_filter(
        &self,
        tier: SummaryTier,
        target_symbol: &str,
        include_error_hints: bool,
    ) -> Result<(SummaryDocument, bool)> {
        let mut doc = self.build_tiered(tier, include_error_hints)?;
        if tier == SummaryTier::Nano || target_symbol.is_empty() {
            return Ok((doc, false));
        }
        // Collect files that contain the target symbol or its direct neighbours
        let relevant_files: HashSet<String> = {
            let mut set = HashSet::new();
            if let Ok(Some(sym)) = self.storage.get_symbol_any(target_symbol) {
                set.insert(sym.file.clone());
                if let Some(id) = sym.id {
                    if let Ok(deps) = self.storage.get_symbol_dependencies(id) {
                        for d in &deps { set.insert(d.file.clone()); }
                    }
                    if let Ok(callers) = self.storage.get_dependency_neighbors(id, 1) {
                        for c in &callers { set.insert(c.file.clone()); }
                    }
                }
            }
            set
        };
        if relevant_files.is_empty() {
            return Ok((doc, false));
        }
        // Filter modules to only those with at least one relevant file
        doc.modules.retain(|m| {
            m.get("files")
                .and_then(|v| v.as_array())
                .map(|files| files.iter().any(|f| {
                    f.get("path").and_then(|p| p.as_str()).map(|p| relevant_files.contains(p)).unwrap_or(false)
                }))
                .unwrap_or(false)
        });
        if doc.modules.is_empty() {
            // No module filter applied (symbol not found in modules) — return unfiltered
            let unfiltered = self.build_tiered(tier, include_error_hints)?;
            return Ok((unfiltered, false));
        }
        doc.summary_text = build_markdown(&doc.narrative, &doc.modules, &doc.dependency_sketch, &doc.entry_points);
        doc.token_estimate = (doc.summary_text.len() / 4).max(1);
        Ok((doc, true))
    }

    /// Build with optional recurring-issues note appended (Phase D).
    pub fn build_with_options(&self, max_tokens: usize, include_error_hints: bool) -> Result<SummaryDocument> {
        let cache_key = format!("project_summary::{max_tokens}");
        if let Some(mut cached) = self.try_get_cached(&cache_key, 3600)? {
            if include_error_hints {
                self.append_error_hints(&mut cached);
            }
            return Ok(cached);
        }
        let mut doc = self.build_fresh(max_tokens)?;
        if include_error_hints {
            self.append_error_hints(&mut doc);
        }
        self.store_cached(&cache_key, &doc)?;
        Ok(doc)
    }

    fn append_error_hints(&self, doc: &mut SummaryDocument) {
        // Requires error_log schema to exist; silently skip if not.
        let Ok(patterns) = self.storage.list_error_patterns(20) else { return; };
        let recurring: Vec<String> = patterns
            .into_iter()
            .filter(|p| p.hit_count >= 3)
            .take(5)
            .map(|p| format!("{:?} (×{})", p.message, p.hit_count))
            .collect();
        if !recurring.is_empty() {
            let note = format!("Recurring issues: {}", recurring.join(", "));
            doc.summary_text.push_str(&format!("\n\n### Recurring Issues\n{note}\n"));
            doc.narrative.push_str(&format!(" {note}."));
        }
    }

    fn build_fresh(&self, max_tokens: usize) -> Result<SummaryDocument> {
        let modules = self.storage.list_modules()?;
        let named_deps = self.storage.list_named_module_dependencies()?;
        let all_symbols = self.storage.list_symbols()?;
        let all_deps = self.storage.list_all_dependencies()?;

        // ── file → module map ────────────────────────────────────────────────
        let mut file_to_module: HashMap<String, String> = HashMap::new();
        for module in &modules {
            let module_id = module.id.unwrap_or_default();
            for mf in self.storage.list_module_files(module_id)? {
                file_to_module.insert(mf.file_path, module.name.clone());
            }
        }

        // ── symbols per file (deduplicated, sorted by frequency desc) ────────
        let mut symbols_by_file: HashMap<String, Vec<String>> = HashMap::new();
        for sym in &all_symbols {
            symbols_by_file
                .entry(sym.file.clone())
                .or_default()
                .push(sym.name.clone());
        }

        // ── callee call-count across all dependencies ────────────────────────
        let mut callee_count: HashMap<String, usize> = HashMap::new();
        let mut caller_set: HashSet<String> = HashSet::new();
        for dep in &all_deps {
            *callee_count.entry(dep.callee_symbol.clone()).or_insert(0) += 1;
            caller_set.insert(dep.caller_symbol.clone());
        }

        // ── true entry points: symbols that are never called by anything ─────
        // (exported roots in the dependency graph)
        let graph_roots: HashSet<String> = all_symbols
            .iter()
            .filter(|s| {
                !matches!(s.symbol_type, engine::SymbolType::Import)
                    && !callee_count.contains_key(&s.name)
            })
            .map(|s| s.file.clone())
            .collect();

        // ── cross-module hot paths: top-5 most-called symbols ────────────────
        let mut hot: Vec<(&String, usize)> = callee_count.iter().map(|(k, v)| (k, *v)).collect();
        hot.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
        let hot_paths: Vec<String> = hot
            .into_iter()
            .take(5)
            .map(|(name, count)| format!("{name}(×{count})"))
            .collect();

        // ── module priority: entry-point modules first, test modules last ─────
        let module_priority = |name: &str| -> u8 {
            let n = name.to_lowercase();
            if n.contains("test") || n.contains("spec") { 2 }
            else if n.contains("api") || n.contains("server") || n.contains("app") { 0 }
            else { 1 }
        };
        let mut sorted_modules = modules.clone();
        sorted_modules.sort_by_key(|m| module_priority(&m.name));

        // ── token budget: ~15 tokens per file line ───────────────────────────
        let max_files = ((max_tokens.saturating_sub(120)) / 15).max(5);

        let mut module_summaries: Vec<Value> = Vec::new();
        let mut entry_points: Vec<String> = Vec::new();
        let mut total_files = 0usize;
        let mut seen_files: HashSet<String> = HashSet::new();

        for module in &sorted_modules {
            let module_id = module.id.unwrap_or_default();
            let module_files = self.storage.list_module_files(module_id)?;
            let mut file_entries: Vec<Value> = Vec::new();

            for mf in module_files {
                if total_files >= max_files {
                    break;
                }
                seen_files.insert(mf.file_path.clone());

                let raw_symbols = symbols_by_file
                    .get(&mf.file_path)
                    .cloned()
                    .unwrap_or_default();

                // Sort symbols: most-called first, then alphabetical
                let mut ranked: Vec<(String, usize)> = raw_symbols
                    .iter()
                    .map(|s| (s.clone(), *callee_count.get(s).unwrap_or(&0)))
                    .collect();
                ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
                let top_symbols: Vec<String> =
                    ranked.into_iter().take(6).map(|(s, _)| s).collect();

                let objective = infer_file_objective_rich(&mf.file_path, &top_symbols, &caller_set);
                let purpose = purpose_sentence_rich(&mf.file_path, objective, &top_symbols, &callee_count);

                // Entry point: path heuristic OR graph root (never called)
                let is_ep = is_entry_point_path(&mf.file_path)
                    || graph_roots.contains(&mf.file_path);
                if is_ep && !entry_points.contains(&mf.file_path) {
                    entry_points.push(mf.file_path.clone());
                }

                file_entries.push(json!({
                    "path": mf.file_path,
                    "objective": objective,
                    "purpose": purpose,
                    "top_symbols": top_symbols,
                    "is_entry_point": is_ep,
                }));
                total_files += 1;
            }

            if !file_entries.is_empty() {
                module_summaries.push(json!({
                    "name": module.name,
                    "priority": module_priority(&module.name),
                    "files": file_entries,
                }));
            }
        }

        // ── unmodule'd files (lowest priority, fill remaining budget) ─────────
        let all_files = self.storage.list_files()?;
        let mut unmodule_entries: Vec<Value> = Vec::new();
        for file in &all_files {
            if total_files >= max_files {
                break;
            }
            if seen_files.contains(file) {
                continue;
            }
            let raw_symbols = symbols_by_file.get(file).cloned().unwrap_or_default();
            let mut ranked: Vec<(String, usize)> = raw_symbols
                .iter()
                .map(|s| (s.clone(), *callee_count.get(s).unwrap_or(&0)))
                .collect();
            ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            let top_symbols: Vec<String> = ranked.into_iter().take(6).map(|(s, _)| s).collect();

            let objective = infer_file_objective_rich(file, &top_symbols, &caller_set);
            let purpose = purpose_sentence_rich(file, objective, &top_symbols, &callee_count);
            let is_ep = is_entry_point_path(file) || graph_roots.contains(file);
            if is_ep && !entry_points.contains(file) {
                entry_points.push(file.clone());
            }
            unmodule_entries.push(json!({
                "path": file,
                "objective": objective,
                "purpose": purpose,
                "top_symbols": top_symbols,
                "is_entry_point": is_ep,
            }));
            total_files += 1;
        }
        if !unmodule_entries.is_empty() {
            module_summaries.push(json!({ "name": "other", "priority": 3, "files": unmodule_entries }));
        }

        // ── dependency sketch (module edges + hot paths) ──────────────────────
        let module_edge_sketch = if named_deps.is_empty() {
            String::new()
        } else {
            named_deps
                .iter()
                .map(|(from, to)| format!("{from} → {to}"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let hot_path_sketch = if hot_paths.is_empty() {
            String::new()
        } else {
            format!("Hot symbols: {}", hot_paths.join(", "))
        };
        let dep_sketch = match (module_edge_sketch.is_empty(), hot_path_sketch.is_empty()) {
            (false, false) => format!("{module_edge_sketch}. {hot_path_sketch}"),
            (false, true) => module_edge_sketch,
            (true, false) => hot_path_sketch,
            (true, true) => String::new(),
        };

        let narrative = build_narrative(
            &sorted_modules.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(),
            &entry_points,
            &dep_sketch,
            total_files,
            all_symbols.len(),
        );
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
            cache_hit: false,
        })
    }

    // ── cache helpers ─────────────────────────────────────────────────────────

    fn try_get_cached(&self, key: &str, ttl_s: u64) -> Result<Option<SummaryDocument>> {
        let now = now_epoch_s();
        let current_rev = current_index_revision(self.storage);
        let Some(entry) = self
            .storage
            .get_retrieval_cache_entry(key, "project_summary")?
        else {
            return Ok(None);
        };
        if now.saturating_sub(entry.cached_at_epoch_s) > ttl_s
            || entry.source_revision != current_rev
        {
            let _ = self
                .storage
                .delete_retrieval_cache_entry(key, "project_summary");
            return Ok(None);
        }
        let raw = entry.value_json.as_deref().unwrap_or("{}");
        let v: Value = serde_json::from_str(raw)?;
        Ok(Some(SummaryDocument {
            narrative: v["narrative"].as_str().unwrap_or("").to_string(),
            modules: v["modules"].as_array().cloned().unwrap_or_default(),
            dependency_sketch: v["dependency_sketch"].as_str().unwrap_or("").to_string(),
            entry_points: v["entry_points"]
                .as_array()
                .map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect())
                .unwrap_or_default(),
            summary_text: v["summary_text"].as_str().unwrap_or("").to_string(),
            token_estimate: v["token_estimate"].as_u64().unwrap_or(0) as usize,
            file_count: v["file_count"].as_u64().unwrap_or(0) as usize,
            module_count: v["module_count"].as_u64().unwrap_or(0) as usize,
            cached_at_epoch_s: entry.cached_at_epoch_s,
            cache_hit: true,
        }))
    }

    fn store_cached(&self, key: &str, doc: &SummaryDocument) -> Result<()> {
        let value = json!({
            "narrative": doc.narrative,
            "modules": doc.modules,
            "dependency_sketch": doc.dependency_sketch,
            "entry_points": doc.entry_points,
            "summary_text": doc.summary_text,
            "token_estimate": doc.token_estimate,
            "file_count": doc.file_count,
            "module_count": doc.module_count,
        });
        self.storage.upsert_retrieval_cache_entry(&storage::RetrievalCacheEntry {
            cache_key: key.to_string(),
            cache_kind: "project_summary".to_string(),
            value_json: Some(serde_json::to_string(&value)?),
            prompt_text: None,
            cached_at_epoch_s: now_epoch_s(),
            source_revision: current_index_revision(self.storage),
        })?;
        // Keep at most 10 project_summary entries
        let _ = self.storage.prune_retrieval_cache_kind("project_summary", 10);
        Ok(())
    }
}

// ── public output type ────────────────────────────────────────────────────────

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
    pub cache_hit: bool,
}

impl SummaryDocument {
    pub fn to_json(&self) -> Value {
        json!({
            "narrative": self.narrative,
            "modules": self.modules,
            "dependency_sketch": self.dependency_sketch,
            "entry_points": self.entry_points,
            "cache_hit": self.cache_hit,
        })
    }
}

// ── helpers ───────────────────────────────────────────────────────────────────

/// Richer objective inference: checks path keywords first, then falls back to
/// symbol-name patterns, then call-graph signals.
fn infer_file_objective_rich(
    path: &str,
    top_symbols: &[String],
    caller_set: &HashSet<String>,
) -> &'static str {
    let p = path.to_lowercase();

    // Path-based (high confidence)
    if p.contains("test") || p.contains("spec") { return "tests"; }
    if p.contains("route") || p.contains("handler") || p.contains("controller") { return "api_surface"; }
    if p.contains("api") || p.contains("server") || p.contains("endpoint") { return "api_surface"; }
    if p.contains("store") || p.contains("repo") || p.contains("db") || p.contains("model") { return "data_layer"; }
    if p.contains("migration") || p.contains("schema") || p.contains("seed") { return "data_layer"; }
    if p.contains("component") || p.contains("widget") || p.contains("view") { return "ui_layer"; }
    if p.ends_with(".tsx") || p.ends_with(".jsx") { return "ui_layer"; }

    // Symbol-name patterns (medium confidence)
    let sym_lower: Vec<String> = top_symbols.iter().map(|s| s.to_lowercase()).collect();
    if sym_lower.iter().any(|s| s.contains("render") || s.contains("component") || s.contains("view")) {
        return "ui_layer";
    }
    if sym_lower.iter().any(|s| s.contains("handler") || s.contains("route") || s.contains("endpoint")) {
        return "api_surface";
    }
    if sym_lower.iter().any(|s| s.contains("store") || s.contains("repo") || s.contains("query") || s.contains("insert") || s.contains("update") || s.contains("delete")) {
        return "data_layer";
    }

    // Call-graph signal: if none of the file's symbols are ever called by others,
    // it's likely an entry point / application logic layer
    let any_called = top_symbols.iter().any(|s| caller_set.contains(s));
    if !any_called && !top_symbols.is_empty() {
        return "application_logic";
    }

    "application_logic"
}

/// Richer purpose sentence: uses actual symbol names and call counts to produce
/// a more informative one-liner.
fn purpose_sentence_rich(
    path: &str,
    objective: &str,
    top_symbols: &[String],
    callee_count: &HashMap<String, usize>,
) -> String {
    let filename = std::path::Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(path);

    // Pick the most-called symbol as the "primary" one
    let primary = top_symbols
        .iter()
        .max_by_key(|s| callee_count.get(*s).copied().unwrap_or(0))
        .cloned()
        .unwrap_or_else(|| filename.to_string());

    let rest: Vec<&String> = top_symbols.iter().filter(|s| *s != &primary).take(3).collect();
    let rest_str = if rest.is_empty() {
        String::new()
    } else {
        format!("; also {}", rest.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", "))
    };

    match objective {
        "tests" => format!("test suite for {filename}"),
        "api_surface" => format!("API surface — entry: {primary}{rest_str}"),
        "data_layer" => format!("data layer — primary: {primary}{rest_str}"),
        "ui_layer" => format!("UI layer — primary: {primary}{rest_str}"),
        _ => format!("application logic — primary: {primary}{rest_str}"),
    }
}

/// Entry point detection based on path only (graph-root detection is done in build_fresh).
fn is_entry_point_path(path: &str) -> bool {
    let p = path.to_lowercase();
    p.contains("main") || p.contains("/app.") || p.ends_with("app.ts")
        || p.ends_with("app.tsx") || p.ends_with("app.js")
        || p.contains("index.") || p.contains("server.")
        || p.contains("entrypoint") || p.contains("entry.")
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
        format!(" {dep_sketch}.")
    };
    format!(
        "Project with {file_count} indexed files and {symbol_count} symbols across modules: {module_list}. Entry: {entry}.{dep_note}"
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
        let ep_names: Vec<&str> = entry_points
            .iter()
            .map(|p| {
                std::path::Path::new(p)
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or(p)
            })
            .collect();
        out.push_str(&format!("**Entry points:** {}\n\n", ep_names.join(", ")));
    }

    out.push_str("### Modules\n\n");
    for module in modules {
        let name = module.get("name").and_then(|v| v.as_str()).unwrap_or("?");
        out.push_str(&format!("**{name}**\n"));
        if let Some(files) = module.get("files").and_then(|v| v.as_array()) {
            for file in files {
                let path = file.get("path").and_then(|v| v.as_str()).unwrap_or("");
                let purpose = file.get("purpose").and_then(|v| v.as_str()).unwrap_or("");
                let is_ep = file.get("is_entry_point").and_then(|v| v.as_bool()).unwrap_or(false);
                let ep_marker = if is_ep { " ⬡" } else { "" };
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
                out.push_str(&format!("- `{path}`{ep_marker} — {purpose}\n"));
                if !symbols.is_empty() {
                    out.push_str(&format!("  Symbols: {symbols}\n"));
                }
            }
        }
        out.push('\n');
    }

    if !dep_sketch.is_empty() {
        out.push_str("### Dependencies & Hot Paths\n");
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

fn current_index_revision(storage: &storage::Storage) -> u64 {
    // Use the count of symbols as a lightweight revision proxy —
    // changes on index will shift this value.
    storage.list_symbols().map(|v| v.len() as u64).unwrap_or(0)
}
