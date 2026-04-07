use anyhow::Result;
use parser::SupportedLanguage;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::Instant;
use walkdir::WalkDir;

pub struct Indexer {
    parser: parser::CodeParser,
    pub storage: storage::Storage,
    repo_id: i64,
    perf_stats: IndexerPerfStats,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct IndexerPerfStats {
    repo_runs: u64,
    file_updates: u64,
    files_indexed: u64,
    files_skipped: u64,
    files_deleted: u64,
    files_parse_failed: u64,
    files_read_failed: u64,
    #[serde(default)]
    files_excluded: u64,
    #[serde(default)]
    last_parse_fail_paths: Vec<String>,
    total_repo_ms: u128,
    total_update_ms: u128,
    max_repo_ms: u128,
    max_update_ms: u128,
    last_repo_ms: u128,
    last_update_ms: u128,
    last_repo_file_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct IndexCoverageManifest {
    #[serde(default = "default_manifest_version")]
    version: u8,
    #[serde(default)]
    coverage_mode: String,
    #[serde(default)]
    targeted_paths: Vec<String>,
}

fn default_manifest_version() -> u8 {
    1
}

impl Default for IndexCoverageManifest {
    fn default() -> Self {
        Self {
            version: default_manifest_version(),
            coverage_mode: "targeted".to_string(),
            targeted_paths: Vec::new(),
        }
    }
}

impl Indexer {
    pub fn new(storage: storage::Storage) -> Self {
        Self {
            parser: parser::CodeParser::new(),
            storage,
            repo_id: 0,
            perf_stats: IndexerPerfStats::default(),
        }
    }

    pub fn set_repo_id(&mut self, repo_id: i64) {
        self.repo_id = repo_id;
    }

    /// Index every project listed in `workspace_roots` into the shared DB.
    /// Each file is stored with a `<project_name>/` prefix so paths are
    /// namespaced and do not collide across projects.
    pub fn index_workspace(
        &mut self,
        primary_root: &Path,
        workspace_roots: &[std::path::PathBuf],
    ) -> Result<()> {
        // Always index the primary root first (no prefix — preserves existing behaviour).
        self.index_repo(primary_root)?;
        for root in workspace_roots {
            if root == primary_root {
                continue;
            }
            let project_name = root
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown")
                .to_string();
            self.index_repo_with_prefix(root, &project_name)?;
        }
        Ok(())
    }

    /// Index a repo root, prefixing every stored file path with `<prefix>/`.
    pub fn index_repo_with_prefix(&mut self, repo_path: &Path, prefix: &str) -> Result<()> {
        let started = Instant::now();
        let mut seen = HashSet::new();

        for entry in WalkDir::new(repo_path).into_iter().filter_map(|e| e.ok()) {
            if !entry.file_type().is_file() {
                continue;
            }
            let full_path = entry.path();
            let rel_raw = match full_path.strip_prefix(repo_path) {
                Ok(p) => p.to_string_lossy().replace('\\', "/"),
                Err(_) => continue,
            };
            if should_skip_indexing_path(&rel_raw) {
                self.perf_stats.files_excluded += 1;
                continue;
            }
            if SupportedLanguage::from_path(&rel_raw).is_none() {
                continue;
            }
            let rel = format!("{prefix}/{rel_raw}");
            seen.insert(rel.clone());
            // Read the actual file but store under the prefixed path.
            let content = match fs::read_to_string(full_path) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("[semantic] read failed: {rel_raw} — {e}");
                    self.perf_stats.files_read_failed += 1;
                    continue;
                }
            };
            let checksum = checksum(&content);
            if let Some(existing) = self.storage.get_file_checksum(&rel)? {
                if existing == checksum {
                    self.perf_stats.files_skipped += 1;
                    continue;
                }
            }
            let parsed = match self.parser.parse(&rel_raw, &content) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("[semantic] parse failed: {rel_raw} — {e}");
                    self.perf_stats.files_parse_failed += 1;
                    if self.perf_stats.last_parse_fail_paths.len() < 20 {
                        self.perf_stats.last_parse_fail_paths.push(rel_raw.clone());
                    }
                    continue;
                }
            };
            // Re-prefix symbol file references.
            let prefixed_symbols: Vec<engine::SymbolRecord> = parsed
                .symbols
                .iter()
                .map(|s| {
                    let mut s2 = s.clone();
                    s2.file = format!("{prefix}/{}", s2.file);
                    s2
                })
                .collect();
            let mut prefixed_deps: Vec<engine::DependencyRecord> = parsed
                .dependencies
                .iter()
                .map(|d| {
                    let mut d2 = d.clone();
                    d2.file = format!("{prefix}/{}", d2.file);
                    d2.callee_file = d2.callee_file.as_ref().map(|file| format!("{prefix}/{file}"));
                    d2
                })
                .collect();
            enrich_dependency_targets(repo_path, &mut prefixed_deps);
            self.storage.replace_file_index(
                self.repo_id,
                &rel,
                &parsed.language,
                &checksum,
                &prefixed_symbols,
                &prefixed_deps,
                &parsed.logic_nodes,
                &parsed.control_flow_edges,
                &parsed.data_flow_edges,
                &parsed.logic_clusters,
            )?;
            self.perf_stats.files_indexed += 1;
        }

        // Remove stale prefixed files.
        let prefix_slash = format!("{prefix}/");
        for existing in self.storage.list_files()? {
            if existing.starts_with(&prefix_slash) && !seen.contains(&existing) {
                self.storage.delete_file_records(&existing)?;
                self.storage.delete_file_metadata(&existing)?;
                self.perf_stats.files_deleted += 1;
            }
        }

        self.rebuild_module_graph()?;
        self.storage.refresh_symbol_index()?;
        self.storage.clear_retrieval_cache()?;
        self.perf_stats.repo_runs += 1;
        self.perf_stats.last_repo_file_count = seen.len();
        self.record_repo_elapsed(started.elapsed().as_millis());
        self.persist_perf_stats(repo_path)?;
        write_index_coverage_manifest(
            repo_path,
            IndexCoverageManifest {
                version: default_manifest_version(),
                coverage_mode: "full".to_string(),
                targeted_paths: vec![".".to_string()],
            },
        )?;
        Ok(())
    }

    pub fn index_repo(&mut self, repo_path: &Path) -> Result<()> {
        let started = Instant::now();
        let mut seen = HashSet::new();

        for entry in WalkDir::new(repo_path).into_iter().filter_map(|e| e.ok()) {
            if !entry.file_type().is_file() {
                continue;
            }

            let full_path = entry.path();
            let rel = match full_path.strip_prefix(repo_path) {
                Ok(p) => p.to_string_lossy().replace('\\', "/"),
                Err(_) => continue,
            };
            if should_skip_indexing_path(&rel) {
                self.perf_stats.files_excluded += 1;
                continue;
            }

            if SupportedLanguage::from_path(&rel).is_none() {
                continue;
            }

            seen.insert(rel.clone());
            self.index_file_internal(repo_path, &rel, false)?;
        }

        for existing in self.storage.list_files()? {
            if !seen.contains(&existing) {
                self.storage.delete_file_records(&existing)?;
                self.storage.delete_file_metadata(&existing)?;
            }
        }

        self.rebuild_module_graph()?;
        self.storage.refresh_symbol_index()?;
        self.storage.clear_retrieval_cache()?;
        self.perf_stats.repo_runs += 1;
        self.perf_stats.last_repo_file_count = seen.len();
        self.record_repo_elapsed(started.elapsed().as_millis());
        self.persist_perf_stats(repo_path)?;
        write_index_coverage_manifest(
            repo_path,
            IndexCoverageManifest {
                version: default_manifest_version(),
                coverage_mode: "full".to_string(),
                targeted_paths: vec![".".to_string()],
            },
        )?;
        Ok(())
    }

    pub fn index_file(&mut self, repo_path: &Path, relative_file: &str) -> Result<()> {
        self.index_file_internal(repo_path, relative_file, true)
    }

    pub fn index_paths(&mut self, repo_path: &Path, relative_paths: &[String]) -> Result<()> {
        let started = Instant::now();
        let files = collect_targeted_files(repo_path, relative_paths, &mut self.perf_stats)?;
        for relative_file in &files {
            self.index_file_internal(repo_path, relative_file, false)?;
        }
        self.rebuild_module_graph()?;
        self.storage.refresh_symbol_index()?;
        self.storage.clear_retrieval_cache()?;
        self.perf_stats.repo_runs += 1;
        self.perf_stats.last_repo_file_count = files.len();
        self.record_repo_elapsed(started.elapsed().as_millis());
        self.persist_perf_stats(repo_path)?;
        let mut manifest = load_index_coverage_manifest(repo_path).unwrap_or_default();
        if manifest.coverage_mode != "full" {
            manifest.coverage_mode = "targeted".to_string();
            for path in relative_paths {
                let normalized = normalize_manifest_path(path);
                if !normalized.is_empty() && !manifest.targeted_paths.iter().any(|p| p == &normalized)
                {
                    manifest.targeted_paths.push(normalized);
                }
            }
            manifest.targeted_paths.sort();
            write_index_coverage_manifest(repo_path, manifest)?;
        }
        Ok(())
    }

    fn index_file_internal(
        &mut self,
        repo_path: &Path,
        relative_file: &str,
        refresh_after: bool,
    ) -> Result<()> {
        let started = Instant::now();
        let file_path = repo_path.join(relative_file);
        let content = fs::read_to_string(&file_path)?;
        let checksum = checksum(&content);

        if let Some(existing) = self.storage.get_file_checksum(relative_file)? {
            if existing == checksum {
                self.perf_stats.files_skipped += 1;
                self.record_update_elapsed(started.elapsed().as_millis());
                self.persist_perf_stats(repo_path)?;
                return Ok(());
            }
        }

        let parsed = self.parser.parse(relative_file, &content)?;
        let mut dependencies = parsed.dependencies.clone();
        enrich_dependency_targets(repo_path, &mut dependencies);
        self.storage.replace_file_index(
            self.repo_id,
            relative_file,
            &parsed.language,
            &checksum,
            &parsed.symbols,
            &dependencies,
            &parsed.logic_nodes,
            &parsed.control_flow_edges,
            &parsed.data_flow_edges,
            &parsed.logic_clusters,
        )?;
        self.perf_stats.file_updates += 1;
        self.perf_stats.files_indexed += 1;
        if refresh_after {
            self.rebuild_module_graph()?;
            self.storage.refresh_symbol_index()?;
        }
        self.storage.clear_retrieval_cache()?;
        self.record_update_elapsed(started.elapsed().as_millis());
        self.persist_perf_stats(repo_path)?;
        Ok(())
    }

    pub fn delete_file(&mut self, relative_file: &str) -> Result<()> {
        self.storage.delete_file_records(relative_file)?;
        self.storage.delete_file_metadata(relative_file)?;
        self.rebuild_module_graph()?;
        self.storage.refresh_symbol_index()?;
        self.storage.clear_retrieval_cache()?;
        self.perf_stats.files_deleted += 1;
        Ok(())
    }

    fn rebuild_module_graph(&mut self) -> Result<()> {
        let mut files = self.storage.list_files()?;
        files.sort();

        self.storage.clear_module_graph()?;
        let mut modules: HashMap<String, i64> = HashMap::new();
        let mut file_to_module: HashMap<String, i64> = HashMap::new();

        for file in &files {
            let (module_name, module_path) = detect_module_from_file(file);
            let key = format!("{module_path}:{module_name}");
            let module_id = if let Some(id) = modules.get(&key) {
                *id
            } else {
                let id = self.storage.insert_module(&module_name, &module_path)?;
                modules.insert(key, id);
                id
            };
            self.storage.insert_module_file(module_id, file)?;
            file_to_module.insert(file.clone(), module_id);
        }

        let mut symbol_to_file: HashMap<String, String> = HashMap::new();
        let mut symbols = self.storage.list_symbols()?;
        symbols.sort_by(|a, b| {
            a.name
                .cmp(&b.name)
                .then_with(|| a.file.cmp(&b.file))
                .then_with(|| a.start_line.cmp(&b.start_line))
        });
        for symbol in symbols {
            symbol_to_file.entry(symbol.name).or_insert(symbol.file);
        }

        let deps = self.storage.list_all_dependencies()?;
        let mut module_edges = HashSet::new();
        for dep in deps {
            let caller_file = symbol_to_file
                .get(&dep.caller_symbol)
                .cloned()
                .unwrap_or(dep.file.clone());
            let Some(callee_file) = symbol_to_file.get(&dep.callee_symbol).cloned() else {
                continue;
            };

            let Some(caller_module) = file_to_module.get(&caller_file) else {
                continue;
            };
            let Some(callee_module) = file_to_module.get(&callee_file) else {
                continue;
            };

            if caller_module != callee_module {
                module_edges.insert((*caller_module, *callee_module));
            }
        }

        let mut edges: Vec<(i64, i64)> = module_edges.into_iter().collect();
        edges.sort_unstable();
        for (from_module, to_module) in edges {
            self.storage
                .insert_module_dependency(from_module, to_module)?;
        }

        Ok(())
    }

    fn record_repo_elapsed(&mut self, elapsed_ms: u128) {
        self.perf_stats.total_repo_ms += elapsed_ms;
        self.perf_stats.last_repo_ms = elapsed_ms;
        self.perf_stats.max_repo_ms = self.perf_stats.max_repo_ms.max(elapsed_ms);
    }

    fn record_update_elapsed(&mut self, elapsed_ms: u128) {
        self.perf_stats.total_update_ms += elapsed_ms;
        self.perf_stats.last_update_ms = elapsed_ms;
        self.perf_stats.max_update_ms = self.perf_stats.max_update_ms.max(elapsed_ms);
    }

    fn persist_perf_stats(&self, repo_path: &Path) -> Result<()> {
        let path = repo_path.join(".semantic").join("index_performance.json");
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let payload = serde_json::json!({
            "repo_runs": self.perf_stats.repo_runs,
            "file_updates": self.perf_stats.file_updates,
            "files_indexed": self.perf_stats.files_indexed,
            "files_skipped": self.perf_stats.files_skipped,
            "files_deleted": self.perf_stats.files_deleted,
            "files_parse_failed": self.perf_stats.files_parse_failed,
            "files_read_failed": self.perf_stats.files_read_failed,
            "files_excluded": self.perf_stats.files_excluded,
            "last_parse_fail_paths": self.perf_stats.last_parse_fail_paths,
            "last_repo_ms": self.perf_stats.last_repo_ms,
            "last_update_ms": self.perf_stats.last_update_ms,
            "max_repo_ms": self.perf_stats.max_repo_ms,
            "max_update_ms": self.perf_stats.max_update_ms,
            "avg_repo_ms": average_ms(self.perf_stats.total_repo_ms, self.perf_stats.repo_runs),
            "avg_update_ms": average_ms(
                self.perf_stats.total_update_ms,
                self.perf_stats.file_updates + self.perf_stats.files_skipped,
            ),
            "last_repo_file_count": self.perf_stats.last_repo_file_count,
        });
        fs::write(path, serde_json::to_string_pretty(&payload)?)?;
        Ok(())
    }
}

fn index_manifest_path(repo_path: &Path) -> PathBuf {
    repo_path.join(".semantic").join("index_manifest.json")
}

fn normalize_manifest_path(path: &str) -> String {
    path.trim().replace('\\', "/").trim_matches('/').to_string()
}

fn load_index_coverage_manifest(repo_path: &Path) -> Option<IndexCoverageManifest> {
    let raw = fs::read_to_string(index_manifest_path(repo_path)).ok()?;
    serde_json::from_str(&raw).ok()
}

fn write_index_coverage_manifest(repo_path: &Path, manifest: IndexCoverageManifest) -> Result<()> {
    let path = index_manifest_path(repo_path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, serde_json::to_vec_pretty(&manifest)?)?;
    Ok(())
}

fn should_skip_indexing_path(relative_path: &str) -> bool {
    let normalized = relative_path.replace('\\', "/");
    let lower = normalized.to_ascii_lowercase();
    let heavy_dirs = [
        "node_modules/",
        "target/",
        "dist/",
        "build/",
        ".cache/",
        ".git/",
        ".idea/",
        ".vscode/",
        "coverage/",
        "tmp/",
    ];
    if heavy_dirs.iter().any(|dir| lower.starts_with(dir) || lower.contains(&format!("/{dir}"))) {
        return true;
    }
    let heavy_suffixes = [".log", ".lock", ".min.js", ".d.ts", ".png", ".pdf", ".exe"];
    heavy_suffixes.iter().any(|suffix| lower.ends_with(suffix))
}

fn collect_targeted_files(
    repo_path: &Path,
    relative_paths: &[String],
    perf_stats: &mut IndexerPerfStats,
) -> Result<Vec<String>> {
    let mut files = Vec::new();
    let mut seen = HashSet::new();
    for raw_path in relative_paths {
        let relative = raw_path.trim().replace('\\', "/").trim_matches('/').to_string();
        if relative.is_empty() {
            continue;
        }
        let absolute = repo_path.join(&relative);
        if absolute.is_file() {
            push_targeted_file(&mut files, &mut seen, &relative, perf_stats);
            continue;
        }
        if absolute.is_dir() {
            for entry in WalkDir::new(&absolute).into_iter().filter_map(|e| e.ok()) {
                if !entry.file_type().is_file() {
                    continue;
                }
                let full_path = entry.path();
                let rel = match full_path.strip_prefix(repo_path) {
                    Ok(path) => path.to_string_lossy().replace('\\', "/"),
                    Err(_) => continue,
                };
                push_targeted_file(&mut files, &mut seen, &rel, perf_stats);
            }
        }
    }
    files.sort();
    Ok(files)
}

fn push_targeted_file(
    files: &mut Vec<String>,
    seen: &mut HashSet<String>,
    relative_file: &str,
    perf_stats: &mut IndexerPerfStats,
) {
    if should_skip_indexing_path(relative_file) {
        perf_stats.files_excluded += 1;
        return;
    }
    if SupportedLanguage::from_path(relative_file).is_none() {
        return;
    }
    if seen.insert(relative_file.to_string()) {
        files.push(relative_file.to_string());
    }
}

fn detect_module_from_file(file: &str) -> (String, String) {
    let parts: Vec<&str> = file.split('/').collect();
    if parts.len() >= 3 && (parts[0] == "src" || parts[0] == "lib") {
        (parts[1].to_string(), format!("{}/{}", parts[0], parts[1]))
    } else if parts.len() >= 2 && (parts[0] == "src" || parts[0] == "lib") {
        (parts[0].to_string(), parts[0].to_string())
    } else if parts.len() >= 2 {
        (parts[0].to_string(), parts[0].to_string())
    } else {
        ("root".to_string(), "root".to_string())
    }
}

fn checksum(content: &str) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    content.hash(&mut hasher);
    format!("{:x}", hasher.finish())
}

fn enrich_dependency_targets(repo_path: &Path, deps: &mut [engine::DependencyRecord]) {
    for dep in deps {
        let Some(mut current_file) = dep.callee_file.clone() else {
            continue;
        };
        if let Some(resolved) = repair_missing_target_path(repo_path, &current_file) {
            current_file = resolved;
        }
        let mut current_symbol = dep.callee_symbol.clone();
        for _ in 0..4 {
            if current_symbol == "default" {
                if let Some((resolved_symbol, resolved_file)) =
                    resolve_default_export_target(repo_path, &current_file)
                {
                    current_symbol = resolved_symbol;
                    current_file = resolved_file;
                    continue;
                }
            }
            let Some((next_symbol, next_file)) =
                resolve_reexport_target(repo_path, &current_file, &current_symbol)
            else {
                break;
            };
            current_symbol = next_symbol;
            current_file = next_file;
        }
        dep.callee_symbol = current_symbol;
        dep.callee_file = Some(current_file);
    }
}

fn resolve_default_export_target(repo_path: &Path, relative_file: &str) -> Option<(String, String)> {
    let path = repo_path.join(relative_file);
    let content = fs::read_to_string(path).ok()?;
    for statement in content.split(';') {
        let statement = statement.replace('\n', " ");
        let statement = statement.trim();
        for prefix in ["export default function ", "export default class "] {
            if let Some(rest) = statement.strip_prefix(prefix) {
                let name = rest
                    .split(|ch: char| !(ch.is_alphanumeric() || ch == '_' || ch == '$'))
                    .find(|part| !part.is_empty())?;
                return Some((name.to_string(), relative_file.to_string()));
            }
        }
    }
    None
}

fn repair_missing_target_path(repo_path: &Path, relative_file: &str) -> Option<String> {
    if repo_path.join(relative_file).exists() {
        return Some(relative_file.to_string());
    }

    let stem = relative_file
        .strip_suffix(".ts")
        .or_else(|| relative_file.strip_suffix(".tsx"))
        .or_else(|| relative_file.strip_suffix(".js"))
        .or_else(|| relative_file.strip_suffix(".jsx"))
        .unwrap_or(relative_file);

    let candidates = [
        format!("{stem}/index.ts"),
        format!("{stem}/index.tsx"),
        format!("{stem}/index.js"),
        format!("{stem}/index.jsx"),
    ];
    candidates
        .into_iter()
        .find(|candidate| repo_path.join(candidate).exists())
}

fn resolve_reexport_target(
    repo_path: &Path,
    relative_file: &str,
    exported_name: &str,
) -> Option<(String, String)> {
    let path = repo_path.join(relative_file);
    let content = fs::read_to_string(path).ok()?;
    for statement in content.split(';') {
        let statement = statement.trim();
        if let Some((specifier, bindings)) = extract_reexport_binding_info(statement) {
            let resolved = resolve_relative_specifier(relative_file, &specifier)?;
            for (exported, source) in bindings {
                if exported == exported_name {
                    return Some((source, resolved));
                }
            }
        } else if let Some(specifier) = extract_export_star_specifier(statement) {
            let resolved = resolve_relative_specifier(relative_file, &specifier)?;
            return Some((exported_name.to_string(), resolved));
        }
    }
    None
}

fn extract_reexport_binding_info(statement: &str) -> Option<(String, Vec<(String, String)>)> {
    let normalized = statement.replace('\n', " ");
    let normalized = normalized.trim();
    let export_body = normalized.strip_prefix("export ")?;
    let (bindings, specifier_part) = export_body.split_once(" from ")?;
    let specifier = extract_quoted_string(specifier_part.trim())?;
    let bindings = bindings.trim();
    let start = bindings.find('{')?;
    let end = bindings.rfind('}')?;
    let named = &bindings[start + 1..end];
    let mut out = Vec::new();
    for part in named.split(',') {
        let part = part.trim().trim_start_matches("type ").trim();
        if part.is_empty() {
            continue;
        }
        let (source_name, exported_name) = part
            .split_once(" as ")
            .map(|(source, alias)| (source.trim(), alias.trim()))
            .unwrap_or((part, part));
        if !exported_name.is_empty() && !source_name.is_empty() {
            out.push((exported_name.to_string(), source_name.to_string()));
        }
    }
    if out.is_empty() {
        None
    } else {
        Some((specifier, out))
    }
}

fn extract_export_star_specifier(statement: &str) -> Option<String> {
    let normalized = statement.replace('\n', " ");
    let normalized = normalized.trim();
    let export_body = normalized.strip_prefix("export ")?;
    let star_body = export_body.strip_prefix("* from ")?;
    extract_quoted_string(star_body.trim())
}

fn resolve_relative_specifier(file: &str, specifier: &str) -> Option<String> {
    if !specifier.starts_with('.') {
        return None;
    }
    let base = Path::new(file).parent()?;
    let joined = base.join(specifier);
    let normalized = normalize_relative_path(&joined);
    let trimmed = normalized.trim_end_matches('/');
    let has_known_extension =
        trimmed.ends_with(".ts") || trimmed.ends_with(".tsx") || trimmed.ends_with(".js") || trimmed.ends_with(".jsx");
    let candidates = if has_known_extension {
        vec![trimmed.to_string()]
    } else {
        vec![
            format!("{trimmed}.ts"),
            format!("{trimmed}.tsx"),
            format!("{trimmed}.js"),
            format!("{trimmed}.jsx"),
            format!("{trimmed}/index.ts"),
            format!("{trimmed}/index.tsx"),
            format!("{trimmed}/index.js"),
            format!("{trimmed}/index.jsx"),
            trimmed.to_string(),
        ]
    };
    candidates.into_iter().find(|candidate| !candidate.is_empty())
}

fn normalize_relative_path(path: &Path) -> String {
    let mut parts: Vec<String> = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::Normal(value) => {
                parts.push(value.to_string_lossy().to_string());
            }
            std::path::Component::ParentDir => {
                parts.pop();
            }
            std::path::Component::CurDir => {}
            std::path::Component::Prefix(value) => {
                parts.push(value.as_os_str().to_string_lossy().to_string());
            }
            std::path::Component::RootDir => {}
        }
    }
    parts.join("/")
}

fn extract_quoted_string(value: &str) -> Option<String> {
    let value = value.trim();
    for quote in ['"', '\''] {
        if let Some(start) = value.find(quote) {
            let rest = &value[start + 1..];
            if let Some(end) = rest.find(quote) {
                return Some(rest[..end].to_string());
            }
        }
    }
    None
}

fn average_ms(total_ms: u128, count: u64) -> f64 {
    if count == 0 {
        0.0
    } else {
        total_ms as f64 / count as f64
    }
}

#[cfg(test)]
mod tests {
    use super::Indexer;
    use engine::{LogicNodeType, SymbolType};
    use std::fs;
    use std::path::Path;
    use storage::Storage;
    use test_support::materialize_quality_fixture;

    fn open_indexer(tmp: &tempfile::TempDir) -> Indexer {
        let db = tmp.path().join("semantic.db");
        let idx = tmp.path().join("tantivy");
        let storage = Storage::open(&db, &idx).expect("open storage");
        Indexer::new(storage)
    }

    fn assert_symbol_span(
        storage: &Storage,
        name: &str,
        symbol_type: SymbolType,
        file: &str,
        start_line: u32,
        end_line: u32,
    ) {
        let symbol = storage
            .get_symbol_exact(name, symbol_type)
            .expect("get symbol exact")
            .expect("symbol exists");
        assert_eq!(symbol.file, file);
        assert_eq!(symbol.start_line, start_line);
        assert_eq!(symbol.end_line, end_line);
    }

    #[test]
    fn indexes_simple_repo() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join("src")).expect("mkdir src");
        fs::write(
            repo.join("src").join("a.py"),
            "def retry_request():\n    return 1\n",
        )
        .expect("write file");

        let db = tmp.path().join("semantic.db");
        let idx = tmp.path().join("tantivy");
        let storage = Storage::open(&db, &idx).expect("open storage");
        let mut indexer = Indexer::new(storage);
        indexer.index_repo(&repo).expect("index repo");

        let symbols = indexer
            .storage
            .search_symbol_by_name("retry", 10)
            .expect("query symbols");
        assert_eq!(symbols.len(), 1);
    }

    #[test]
    fn builds_module_graph() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join("src").join("api")).expect("mkdir api");
        fs::create_dir_all(repo.join("src").join("utils")).expect("mkdir utils");
        fs::write(
            repo.join("src").join("utils").join("retry.ts"),
            "export function retryRequest(){ return 1; }\n",
        )
        .expect("write retry");
        fs::write(
            repo.join("src").join("api").join("client.ts"),
            "import { retryRequest } from '../utils/retry';\nexport function fetchData(){ return retryRequest(); }\n",
        )
        .expect("write client");

        let db = tmp.path().join("semantic.db");
        let idx = tmp.path().join("tantivy");
        let storage = Storage::open(&db, &idx).expect("open storage");
        let mut indexer = Indexer::new(storage);
        indexer.index_repo(&repo).expect("index repo");

        let modules = indexer.storage.list_modules().expect("list modules");
        assert!(modules.iter().any(|m| m.name == "api"));
        assert!(modules.iter().any(|m| m.name == "utils"));

        let deps = indexer
            .storage
            .list_named_module_dependencies()
            .expect("module deps");
        assert!(deps.iter().any(|(from, to)| from == "api" && to == "utils"));
    }

    #[test]
    fn writes_index_performance_stats() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join("src")).expect("mkdir src");
        fs::write(
            repo.join("src").join("a.py"),
            "def retry_request():\n    return 1\n",
        )
        .expect("write file");

        let db = tmp.path().join("semantic.db");
        let idx = tmp.path().join("tantivy");
        let storage = Storage::open(&db, &idx).expect("open storage");
        let mut indexer = Indexer::new(storage);
        indexer.index_repo(&repo).expect("index repo");

        let stats_path = repo.join(".semantic").join("index_performance.json");
        let stats = fs::read_to_string(stats_path).expect("read stats");
        assert!(stats.contains("\"repo_runs\""));
        assert!(stats.contains("\"last_repo_ms\""));
    }

    #[test]
    fn skip_indexing_path_excludes_heavy_dirs_and_artifacts() {
        assert!(super::should_skip_indexing_path("node_modules/react/index.js"));
        assert!(super::should_skip_indexing_path("packages/api/target/debug/app"));
        assert!(super::should_skip_indexing_path("packages/web/dist/main.js"));
        assert!(super::should_skip_indexing_path("packages/web/src/types/generated.d.ts"));
        assert!(super::should_skip_indexing_path("assets/logo.png"));
        assert!(!super::should_skip_indexing_path("packages/api/src/main.rs"));
        assert!(!super::should_skip_indexing_path("docs/skills.md"));
    }

    #[test]
    fn indexing_skips_default_excluded_dirs_and_records_files_excluded() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join("src")).expect("mkdir src");
        fs::create_dir_all(repo.join("node_modules").join("react")).expect("mkdir node_modules");
        fs::write(
            repo.join("src").join("main.py"),
            "def run_app():\n    return 1\n",
        )
        .expect("write source");
        fs::write(
            repo.join("node_modules").join("react").join("index.js"),
            "export const ignored = true;\n",
        )
        .expect("write excluded file");

        let db = tmp.path().join("semantic.db");
        let idx = tmp.path().join("tantivy");
        let storage = Storage::open(&db, &idx).expect("open storage");
        let mut indexer = Indexer::new(storage);
        indexer.index_repo(&repo).expect("index repo");

        let stats_path = repo.join(".semantic").join("index_performance.json");
        let stats: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(stats_path).expect("read stats"))
                .expect("parse stats");
        assert_eq!(
            stats.get("files_excluded").and_then(|v| v.as_u64()),
            Some(1)
        );

        let symbols = indexer
            .storage
            .search_symbol_by_name("ignored", 10)
            .expect("query excluded symbol");
        assert!(
            symbols.is_empty(),
            "symbols from excluded directories should not be indexed"
        );

        let source_symbols = indexer
            .storage
            .search_symbol_by_name("run_app", 10)
            .expect("query source symbol");
        assert_eq!(source_symbols.len(), 1);
    }

    #[test]
    fn targeted_indexing_only_indexes_requested_directory() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join("src").join("auth")).expect("mkdir auth");
        fs::create_dir_all(repo.join("src").join("worker")).expect("mkdir worker");
        fs::write(
            repo.join("src").join("auth").join("session.ts"),
            "export function buildSession(){ return 1; }\n",
        )
        .expect("write auth");
        fs::write(
            repo.join("src").join("worker").join("job.ts"),
            "export function runJob(){ return 1; }\n",
        )
        .expect("write worker");

        let db = tmp.path().join("semantic.db");
        let idx = tmp.path().join("tantivy");
        let storage = Storage::open(&db, &idx).expect("open storage");
        let mut indexer = Indexer::new(storage);
        indexer
            .index_paths(&repo, &[String::from("src/auth")])
            .expect("targeted index");

        let auth_symbols = indexer
            .storage
            .search_symbol_by_name("buildSession", 10)
            .expect("query auth symbol");
        assert_eq!(auth_symbols.len(), 1);

        let worker_symbols = indexer
            .storage
            .search_symbol_by_name("runJob", 10)
            .expect("query worker symbol");
        assert!(worker_symbols.is_empty());
    }

    #[test]
    fn indexes_fixture_repo_with_exact_spans_dependencies_and_logic_nodes() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let fixture = materialize_quality_fixture("cross_stack_app").expect("fixture");
        let repo = fixture.repo_root().to_path_buf();
        let mut indexer = open_indexer(&tmp);
        indexer.index_repo(&repo).expect("index repo");

        for symbol in &fixture.manifest().symbols {
            let symbol_type = match symbol.symbol_type.as_str() {
                "class" => SymbolType::Class,
                _ => SymbolType::Function,
            };
            assert_symbol_span(
                &indexer.storage,
                &symbol.name,
                symbol_type,
                &symbol.file,
                symbol.start_line,
                symbol.end_line,
            );
        }

        let deps = indexer
            .storage
            .get_dependencies("fetchData")
            .expect("fetchData deps");
        assert!(
            deps.iter().any(|d| d.callee_symbol == "retryRequest"),
            "fetchData should depend on retryRequest"
        );

        let retry = indexer
            .storage
            .get_symbol_exact("retryRequest", SymbolType::Function)
            .expect("retryRequest lookup")
            .expect("retryRequest symbol");
        let retry_logic_nodes = indexer
            .storage
            .get_logic_nodes(retry.id.expect("retry id"))
            .expect("retry logic nodes");
        assert!(
            !retry_logic_nodes.is_empty(),
            "retryRequest should produce logic nodes"
        );

        let fetch_data = indexer
            .storage
            .get_symbol_exact("fetchData", SymbolType::Function)
            .expect("fetchData lookup")
            .expect("fetchData symbol");
        let fetch_data_nodes = indexer
            .storage
            .get_logic_nodes(fetch_data.id.expect("fetchData id"))
            .expect("fetchData logic nodes");
        assert!(
            fetch_data_nodes
                .iter()
                .any(|n| matches!(n.node_type, LogicNodeType::Conditional | LogicNodeType::Return)),
            "fetchData should include conditional/return logic nodes"
        );

        let files = indexer.storage.list_files().expect("list files");
        assert_eq!(
            files,
            vec![
                "src/api/client.ts".to_string(),
                "src/utils/retry.ts".to_string(),
                "tests/client.spec.ts".to_string()
            ]
        );
    }

    #[test]
    fn reindex_updates_symbol_spans_and_dependencies_after_file_change() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let fixture = materialize_quality_fixture("cross_stack_app").expect("fixture");
        let repo = fixture.repo_root().to_path_buf();
        let mut indexer = open_indexer(&tmp);
        indexer.index_repo(&repo).expect("index repo");

        fs::write(
            repo.join("src").join("api").join("client.ts"),
            concat!(
                "import { retryRequest } from '../utils/retry';\n",
                "import { normalize } from '../../lib/worker';\n\n",
                "export async function fetchData() {\n",
                "  const result = await retryRequest(async () => 1);\n",
                "  return normalize(result);\n",
                "}\n",
            ),
        )
        .expect("rewrite client");

        indexer.index_file(Path::new(&repo), "src/api/client.ts").expect("reindex file");

        assert_symbol_span(
            &indexer.storage,
            "fetchData",
            SymbolType::Function,
            "src/api/client.ts",
            4,
            7,
        );
        let deps = indexer
            .storage
            .get_dependencies("fetchData")
            .expect("fetchData deps");
        assert!(
            deps.iter().any(|d| d.callee_symbol == "retryRequest"),
            "updated fetchData should still depend on retryRequest"
        );
        assert!(
            deps.iter().any(|d| d.callee_symbol == "normalize"),
            "updated fetchData should now depend on normalize"
        );
    }

    #[test]
    fn deleting_file_removes_symbols_and_file_records_from_index() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let fixture = materialize_quality_fixture("cross_stack_app").expect("fixture");
        let repo = fixture.repo_root().to_path_buf();
        let mut indexer = open_indexer(&tmp);
        indexer.index_repo(&repo).expect("index repo");

        fs::remove_file(repo.join("src").join("utils").join("retry.ts")).expect("remove retry");
        indexer
            .delete_file("src/utils/retry.ts")
            .expect("delete file from index");

        let files = indexer.storage.list_files().expect("list files");
        assert!(
            !files.iter().any(|f| f == "src/utils/retry.ts"),
            "deleted file should be removed from file index"
        );
        assert!(
            indexer
                .storage
                .get_symbol_exact("retryRequest", SymbolType::Function)
                .expect("lookup retryRequest")
                .is_none(),
            "deleted source file symbols should be removed from symbol index"
        );
        assert!(
            indexer
                .storage
                .get_file_checksum("src/utils/retry.ts")
                .expect("lookup deleted file checksum")
                .is_none(),
            "deleted file metadata should be removed"
        );
    }

    #[test]
    fn renaming_a_file_reindexes_symbols_and_removes_stale_paths() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let fixture = materialize_quality_fixture("cross_stack_app").expect("fixture");
        let repo = fixture.repo_root().to_path_buf();
        let mut indexer = open_indexer(&tmp);
        indexer.index_repo(&repo).expect("index repo");

        fs::rename(
            repo.join("src").join("utils").join("retry.ts"),
            repo.join("src").join("utils").join("retry_helper.ts"),
        )
        .expect("rename retry file");
        fs::write(
            repo.join("src").join("api").join("client.ts"),
            concat!(
                "import { retryRequest } from '../utils/retry_helper';\n\n",
                "export async function fetchData() {\n",
                "  if (Math.random() > 0.5) {\n",
                "    return retryRequest(async () => 1);\n",
                "  }\n",
                "  return retryRequest(async () => 2);\n",
                "}\n",
            ),
        )
        .expect("rewrite client import");

        indexer
            .delete_file("src/utils/retry.ts")
            .expect("delete old file from index");
        indexer
            .index_file(Path::new(&repo), "src/utils/retry_helper.ts")
            .expect("index renamed file");
        indexer
            .index_file(Path::new(&repo), "src/api/client.ts")
            .expect("reindex client");

        let files = indexer.storage.list_files().expect("list files");
        assert!(
            !files.iter().any(|file| file == "src/utils/retry.ts"),
            "old file path should be gone after rename"
        );
        assert!(
            files.iter().any(|file| file == "src/utils/retry_helper.ts"),
            "new file path should be present after rename"
        );

        let retry_request = indexer
            .storage
            .get_symbol_exact("retryRequest", SymbolType::Function)
            .expect("lookup retryRequest")
            .expect("retryRequest exists");
        assert_eq!(retry_request.file, "src/utils/retry_helper.ts");

        let deps = indexer
            .storage
            .get_dependencies("fetchData")
            .expect("fetchData deps");
        assert!(
            deps.iter()
                .any(|dep| dep.callee_symbol == "retryRequest" && dep.file == "src/api/client.ts"),
            "dependency edges should still resolve after rename"
        );
        assert!(
            indexer
                .storage
                .get_file_checksum("src/utils/retry.ts")
                .expect("old checksum lookup")
                .is_none(),
            "old file metadata should be removed after rename"
        );
    }

    #[test]
    fn indexes_duplicate_symbol_names_across_workspace_roots() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let fixture = materialize_quality_fixture("workspace_duplicate_symbols").expect("fixture");
        let repo = fixture.repo_root().to_path_buf();
        let mut indexer = open_indexer(&tmp);
        indexer.index_repo(&repo).expect("index repo");

        let load_config_symbols = indexer
            .storage
            .search_symbol_by_name("loadConfig", 10)
            .expect("search loadConfig");
        assert_eq!(load_config_symbols.len(), 2);
        assert!(
            load_config_symbols
                .iter()
                .any(|symbol| symbol.file == "packages/api/src/service.ts")
        );
        assert!(
            load_config_symbols
                .iter()
                .any(|symbol| symbol.file == "packages/web/src/service.ts")
        );

        let build_client_deps = indexer
            .storage
            .get_dependencies("buildClient")
            .expect("buildClient deps");
        assert!(
            build_client_deps
                .iter()
                .any(|dep| dep.callee_symbol == "loadConfig" && dep.file == "packages/api/src/service.ts")
        );

        let render_app_deps = indexer
            .storage
            .get_dependencies("renderApp")
            .expect("renderApp deps");
        assert!(
            render_app_deps
                .iter()
                .any(|dep| dep.callee_symbol == "loadConfig" && dep.file == "packages/web/src/service.ts")
        );

        let api_load_config = load_config_symbols
            .iter()
            .find(|symbol| symbol.file == "packages/api/src/service.ts")
            .and_then(|symbol| symbol.id)
            .expect("api loadConfig id");
        let api_callers = indexer
            .storage
            .get_symbol_callers(api_load_config)
            .expect("api loadConfig callers");
        assert_eq!(api_callers.len(), 1);
        assert_eq!(api_callers[0].name, "buildClient");
        assert_eq!(api_callers[0].file, "packages/api/src/service.ts");

        let web_load_config = load_config_symbols
            .iter()
            .find(|symbol| symbol.file == "packages/web/src/service.ts")
            .and_then(|symbol| symbol.id)
            .expect("web loadConfig id");
        let web_callers = indexer
            .storage
            .get_symbol_callers(web_load_config)
            .expect("web loadConfig callers");
        assert_eq!(web_callers.len(), 1);
        assert_eq!(web_callers[0].name, "renderApp");
        assert_eq!(web_callers[0].file, "packages/web/src/service.ts");
    }

    #[test]
    fn indexes_duplicate_symbols_with_same_relative_paths_across_workspace_packages() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let fixture = materialize_quality_fixture("workspace_path_collisions").expect("fixture");
        let repo = fixture.repo_root().to_path_buf();
        let mut indexer = open_indexer(&tmp);
        indexer.index_repo(&repo).expect("index repo");

        let init_auth_symbols = indexer
            .storage
            .search_symbol_by_name("initAuth", 10)
            .expect("search initAuth");
        assert_eq!(init_auth_symbols.len(), 2);
        assert!(
            init_auth_symbols
                .iter()
                .any(|symbol| symbol.file == "packages/api/src/auth/init.ts")
        );
        assert!(
            init_auth_symbols
                .iter()
                .any(|symbol| symbol.file == "packages/worker/src/auth/init.ts")
        );

        let handle_request_deps = indexer
            .storage
            .get_dependencies("handleRequest")
            .expect("handleRequest deps");
        assert!(
            handle_request_deps.iter().any(|dep| {
                dep.callee_symbol == "initAuth" && dep.file == "packages/api/src/auth/init.ts"
            })
        );

        let run_worker_deps = indexer
            .storage
            .get_dependencies("runWorker")
            .expect("runWorker deps");
        assert!(
            run_worker_deps.iter().any(|dep| {
                dep.callee_symbol == "initAuth" && dep.file == "packages/worker/src/auth/init.ts"
            })
        );

        let api_init_auth = init_auth_symbols
            .iter()
            .find(|symbol| symbol.file == "packages/api/src/auth/init.ts")
            .and_then(|symbol| symbol.id)
            .expect("api initAuth id");
        let api_callers = indexer
            .storage
            .get_symbol_callers(api_init_auth)
            .expect("api initAuth callers");
        assert_eq!(api_callers.len(), 1);
        assert_eq!(api_callers[0].name, "handleRequest");
        assert_eq!(api_callers[0].file, "packages/api/src/auth/init.ts");

        let worker_init_auth = init_auth_symbols
            .iter()
            .find(|symbol| symbol.file == "packages/worker/src/auth/init.ts")
            .and_then(|symbol| symbol.id)
            .expect("worker initAuth id");
        let worker_callers = indexer
            .storage
            .get_symbol_callers(worker_init_auth)
            .expect("worker initAuth callers");
        assert_eq!(worker_callers.len(), 1);
        assert_eq!(worker_callers[0].name, "runWorker");
        assert_eq!(worker_callers[0].file, "packages/worker/src/auth/init.ts");
    }

    #[test]
    fn workspace_package_rename_reindexes_symbols_and_removes_stale_paths() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let fixture = materialize_quality_fixture("workspace_path_collisions").expect("fixture");
        let repo = fixture.repo_root().to_path_buf();
        let mut indexer = open_indexer(&tmp);
        indexer.index_repo(&repo).expect("index repo");

        fs::rename(
            repo.join("packages").join("worker").join("src").join("auth").join("init.ts"),
            repo.join("packages")
                .join("worker")
                .join("src")
                .join("auth")
                .join("bootstrap.ts"),
        )
        .expect("rename worker auth file");
        fs::write(
            repo.join("packages")
                .join("worker")
                .join("tests")
                .join("auth.spec.ts"),
            concat!(
                "import { runWorker } from \"../src/auth/bootstrap\";\n\n",
                "describe(\"runWorker\", () => {\n",
                "  it(\"accepts worker job ids\", () => {\n",
                "    expect(runWorker(\"job_123\")).toBeTruthy();\n",
                "  });\n",
                "});\n",
            ),
        )
        .expect("rewrite worker test import");

        indexer
            .delete_file("packages/worker/src/auth/init.ts")
            .expect("delete old worker auth file from index");
        indexer
            .index_file(&repo, "packages/worker/src/auth/bootstrap.ts")
            .expect("index renamed worker auth file");
        indexer
            .index_file(&repo, "packages/worker/tests/auth.spec.ts")
            .expect("reindex worker test");

        let files = indexer.storage.list_files().expect("list files");
        assert!(
            !files
                .iter()
                .any(|file| file == "packages/worker/src/auth/init.ts"),
            "old worker auth path should be gone after rename"
        );
        assert!(
            files
                .iter()
                .any(|file| file == "packages/worker/src/auth/bootstrap.ts"),
            "new worker auth path should be present after rename"
        );

        let init_auth_symbols = indexer
            .storage
            .search_symbol_by_name("initAuth", 10)
            .expect("search initAuth after rename");
        assert!(
            init_auth_symbols
                .iter()
                .any(|symbol| symbol.file == "packages/worker/src/auth/bootstrap.ts"),
            "renamed worker symbol should point to bootstrap.ts"
        );
        assert!(
            init_auth_symbols
                .iter()
                .all(|symbol| symbol.file != "packages/worker/src/auth/init.ts"),
            "stale worker auth path should not remain on initAuth search"
        );

        let run_worker_deps = indexer
            .storage
            .get_dependencies("runWorker")
            .expect("runWorker deps after rename");
        assert!(
            run_worker_deps.iter().any(|dep| {
                dep.callee_symbol == "initAuth"
                    && dep.file == "packages/worker/src/auth/bootstrap.ts"
            }),
            "worker dependency edges should resolve through renamed file"
        );
        assert!(
            indexer
                .storage
                .get_file_checksum("packages/worker/src/auth/init.ts")
                .expect("old worker checksum lookup")
                .is_none(),
            "old worker auth metadata should be removed after rename"
        );
    }

    #[test]
    fn indexes_workspace_with_shared_file_names_and_overlapping_symbols() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let fixture = materialize_quality_fixture("workspace_shared_file_noise").expect("fixture");
        let repo = fixture.repo_root().to_path_buf();
        let mut indexer = open_indexer(&tmp);
        indexer.index_repo(&repo).expect("index repo");

        let init_auth_symbols = indexer
            .storage
            .search_symbol_by_name("initAuth", 10)
            .expect("search initAuth");
        assert_eq!(init_auth_symbols.len(), 3);
        assert!(
            init_auth_symbols
                .iter()
                .any(|symbol| symbol.file == "packages/api/src/auth/flow.ts")
        );
        assert!(
            init_auth_symbols
                .iter()
                .any(|symbol| symbol.file == "packages/worker/src/auth/flow.ts")
        );
        assert!(
            init_auth_symbols
                .iter()
                .any(|symbol| symbol.file == "packages/admin/src/auth/flow.ts")
        );

        let admin_deps = indexer
            .storage
            .get_dependencies("openAdminPanel")
            .expect("openAdminPanel deps");
        assert!(
            admin_deps.iter().any(|dep| {
                dep.callee_symbol == "initAuth" && dep.file == "packages/admin/src/auth/flow.ts"
            })
        );

        let api_init_auth = init_auth_symbols
            .iter()
            .find(|symbol| symbol.file == "packages/api/src/auth/flow.ts")
            .and_then(|symbol| symbol.id)
            .expect("api initAuth id");
        let api_callers = indexer
            .storage
            .get_symbol_callers(api_init_auth)
            .expect("api flow callers");
        assert_eq!(api_callers.len(), 1);
        assert_eq!(api_callers[0].name, "authenticateRequest");
        assert_eq!(api_callers[0].file, "packages/api/src/auth/flow.ts");

        let worker_init_auth = init_auth_symbols
            .iter()
            .find(|symbol| symbol.file == "packages/worker/src/auth/flow.ts")
            .and_then(|symbol| symbol.id)
            .expect("worker initAuth id");
        let worker_callers = indexer
            .storage
            .get_symbol_callers(worker_init_auth)
            .expect("worker flow callers");
        assert_eq!(worker_callers.len(), 1);
        assert_eq!(worker_callers[0].name, "processJob");
        assert_eq!(worker_callers[0].file, "packages/worker/src/auth/flow.ts");

        let admin_init_auth = init_auth_symbols
            .iter()
            .find(|symbol| symbol.file == "packages/admin/src/auth/flow.ts")
            .and_then(|symbol| symbol.id)
            .expect("admin initAuth id");
        let admin_callers = indexer
            .storage
            .get_symbol_callers(admin_init_auth)
            .expect("admin flow callers");
        assert_eq!(admin_callers.len(), 1);
        assert_eq!(admin_callers[0].name, "openAdminPanel");
        assert_eq!(admin_callers[0].file, "packages/admin/src/auth/flow.ts");
    }

    #[test]
    fn indexes_cross_file_imported_duplicate_export_names() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let fixture = materialize_quality_fixture("cross_file_import_duplicates").expect("fixture");
        let repo = fixture.repo_root().to_path_buf();
        let mut indexer = open_indexer(&tmp);
        indexer.index_repo(&repo).expect("index repo");

        let load_config_symbols = indexer
            .storage
            .search_symbol_by_name("loadConfig", 10)
            .expect("search loadConfig");
        assert_eq!(load_config_symbols.len(), 2);
        assert!(
            load_config_symbols
                .iter()
                .any(|symbol| symbol.file == "src/api/loadConfig.ts")
        );
        assert!(
            load_config_symbols
                .iter()
                .any(|symbol| symbol.file == "src/web/loadConfig.ts")
        );

        let build_client = indexer
            .storage
            .get_symbol_exact("buildClient", SymbolType::Function)
            .expect("buildClient exact")
            .expect("buildClient symbol");
        let build_deps = indexer
            .storage
            .get_symbol_dependencies(build_client.id.expect("buildClient id"))
            .expect("buildClient dependencies");
        assert_eq!(build_deps.len(), 1);
        assert_eq!(build_deps[0].name, "loadConfig");
        assert_eq!(build_deps[0].file, "src/api/loadConfig.ts");

        let render_app = indexer
            .storage
            .get_symbol_exact("renderApp", SymbolType::Function)
            .expect("renderApp exact")
            .expect("renderApp symbol");
        let render_deps = indexer
            .storage
            .get_symbol_dependencies(render_app.id.expect("renderApp id"))
            .expect("renderApp dependencies");
        assert_eq!(render_deps.len(), 1);
        assert_eq!(render_deps[0].name, "loadConfig");
        assert_eq!(render_deps[0].file, "src/web/loadConfig.ts");
    }

    #[test]
    fn indexes_import_aliases_and_barrel_reexports_to_underlying_targets() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let fixture = materialize_quality_fixture("import_alias_reexports").expect("fixture");
        let repo = fixture.repo_root().to_path_buf();
        let mut indexer = open_indexer(&tmp);
        indexer.index_repo(&repo).expect("index repo");

        let build_client = indexer
            .storage
            .get_symbol_exact("buildClient", SymbolType::Function)
            .expect("buildClient exact")
            .expect("buildClient symbol");
        let build_deps = indexer
            .storage
            .get_symbol_dependencies(build_client.id.expect("buildClient id"))
            .expect("buildClient dependencies");
        assert_eq!(build_deps.len(), 1);
        assert_eq!(build_deps[0].name, "loadConfig");
        assert_eq!(build_deps[0].file, "src/api/loadConfig.ts");

        let render_app = indexer
            .storage
            .get_symbol_exact("renderApp", SymbolType::Function)
            .expect("renderApp exact")
            .expect("renderApp symbol");
        let render_deps = indexer
            .storage
            .get_symbol_dependencies(render_app.id.expect("renderApp id"))
            .expect("renderApp dependencies");
        assert_eq!(render_deps.len(), 1);
        assert_eq!(render_deps[0].name, "loadConfig");
        assert_eq!(render_deps[0].file, "src/web/loadConfig.ts");
    }

    #[test]
    fn indexes_multi_hop_export_star_to_underlying_targets() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let fixture = materialize_quality_fixture("multi_hop_export_star").expect("fixture");
        let repo = fixture.repo_root().to_path_buf();
        let mut indexer = open_indexer(&tmp);
        indexer.index_repo(&repo).expect("index repo");

        let build_api = indexer
            .storage
            .get_symbol_exact("buildApiTheme", SymbolType::Function)
            .expect("buildApiTheme exact")
            .expect("buildApiTheme symbol");
        let build_deps = indexer
            .storage
            .get_symbol_dependencies(build_api.id.expect("buildApiTheme id"))
            .expect("buildApiTheme dependencies");
        assert_eq!(build_deps.len(), 1);
        assert_eq!(build_deps[0].name, "loadThemeConfig");
        assert_eq!(build_deps[0].file, "src/api/loadThemeConfig.ts");

        let render_theme = indexer
            .storage
            .get_symbol_exact("renderTheme", SymbolType::Function)
            .expect("renderTheme exact")
            .expect("renderTheme symbol");
        let render_deps = indexer
            .storage
            .get_symbol_dependencies(render_theme.id.expect("renderTheme id"))
            .expect("renderTheme dependencies");
        assert_eq!(render_deps.len(), 1);
        assert_eq!(render_deps[0].name, "loadThemeConfig");
        assert_eq!(render_deps[0].file, "src/shared/loadThemeConfig.ts");
    }

    #[test]
    fn indexes_default_export_aliases_and_default_reexports() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let fixture = materialize_quality_fixture("default_export_aliases").expect("fixture");
        let repo = fixture.repo_root().to_path_buf();
        let mut indexer = open_indexer(&tmp);
        indexer.index_repo(&repo).expect("index repo");

        let build_api = indexer
            .storage
            .get_symbol_exact("buildApiTheme", SymbolType::Function)
            .expect("buildApiTheme exact")
            .expect("buildApiTheme symbol");
        let build_deps = indexer
            .storage
            .get_symbol_dependencies(build_api.id.expect("buildApiTheme id"))
            .expect("buildApiTheme dependencies");
        assert_eq!(build_deps.len(), 1);
        assert_eq!(build_deps[0].name, "loadThemeConfig");
        assert_eq!(build_deps[0].file, "src/api/loadThemeConfig.ts");

        let render_theme = indexer
            .storage
            .get_symbol_exact("renderTheme", SymbolType::Function)
            .expect("renderTheme exact")
            .expect("renderTheme symbol");
        let render_deps = indexer
            .storage
            .get_symbol_dependencies(render_theme.id.expect("renderTheme id"))
            .expect("renderTheme dependencies");
        assert_eq!(render_deps.len(), 1);
        assert_eq!(render_deps[0].name, "loadThemeConfig");
        assert_eq!(render_deps[0].file, "src/shared/loadThemeConfig.ts");
    }

    #[test]
    fn unsupported_anonymous_default_export_does_not_drift_to_named_duplicate() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let fixture = materialize_quality_fixture("unsupported_default_boundary").expect("fixture");
        let repo = fixture.repo_root().to_path_buf();
        let mut indexer = open_indexer(&tmp);
        indexer.index_repo(&repo).expect("index repo");

        let render_theme = indexer
            .storage
            .get_symbol_exact("renderTheme", SymbolType::Function)
            .expect("renderTheme exact")
            .expect("renderTheme symbol");
        let render_deps = indexer
            .storage
            .get_symbol_dependencies(render_theme.id.expect("renderTheme id"))
            .expect("renderTheme dependencies");
        assert!(
            render_deps.iter().all(|dep| dep.file != "src/api/loadThemeConfig.ts"),
            "unsupported anonymous default export should not drift to named duplicate"
        );
    }

    #[test]
    fn unsupported_anonymous_default_barrel_reexport_does_not_drift_to_named_duplicate() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let fixture =
            materialize_quality_fixture("unsupported_default_barrel_boundary").expect("fixture");
        let repo = fixture.repo_root().to_path_buf();
        let mut indexer = open_indexer(&tmp);
        indexer.index_repo(&repo).expect("index repo");

        let render_theme = indexer
            .storage
            .get_symbol_exact("renderTheme", SymbolType::Function)
            .expect("renderTheme exact")
            .expect("renderTheme symbol");
        let render_deps = indexer
            .storage
            .get_symbol_dependencies(render_theme.id.expect("renderTheme id"))
            .expect("renderTheme dependencies");
        assert!(
            render_deps.iter().all(|dep| dep.file != "src/api/loadThemeConfig.ts"),
            "unsupported anonymous default barrel re-export should not drift to named duplicate"
        );
    }

    #[test]
    fn unsupported_commonjs_require_does_not_drift_to_named_duplicate() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let fixture = materialize_quality_fixture("unsupported_commonjs_boundary").expect("fixture");
        let repo = fixture.repo_root().to_path_buf();
        let mut indexer = open_indexer(&tmp);
        indexer.index_repo(&repo).expect("index repo");

        let render_theme = indexer
            .storage
            .get_symbol_exact("renderTheme", SymbolType::Function)
            .expect("renderTheme exact")
            .expect("renderTheme symbol");
        let render_deps = indexer
            .storage
            .get_symbol_dependencies(render_theme.id.expect("renderTheme id"))
            .expect("renderTheme dependencies");
        assert!(
            render_deps.iter().all(|dep| dep.file != "src/api/loadThemeConfig.js"),
            "unsupported CommonJS require should not drift to named duplicate"
        );
    }

    #[test]
    fn unsupported_namespace_export_member_call_does_not_drift_to_named_duplicate() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let fixture =
            materialize_quality_fixture("unsupported_namespace_export_boundary").expect("fixture");
        let repo = fixture.repo_root().to_path_buf();
        let mut indexer = open_indexer(&tmp);
        indexer.index_repo(&repo).expect("index repo");

        let render_theme = indexer
            .storage
            .get_symbol_exact("renderTheme", SymbolType::Function)
            .expect("renderTheme exact")
            .expect("renderTheme symbol");
        let render_deps = indexer
            .storage
            .get_symbol_dependencies(render_theme.id.expect("renderTheme id"))
            .expect("renderTheme dependencies");
        assert!(
            render_deps.iter().all(|dep| dep.file != "src/api/loadThemeConfig.ts"),
            "unsupported namespace export member call should not drift to named duplicate"
        );
    }

    #[test]
    fn unsupported_commonjs_destructured_require_does_not_drift_to_named_duplicate() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let fixture =
            materialize_quality_fixture("unsupported_commonjs_destructure_boundary")
                .expect("fixture");
        let repo = fixture.repo_root().to_path_buf();
        let mut indexer = open_indexer(&tmp);
        indexer.index_repo(&repo).expect("index repo");

        let render_theme = indexer
            .storage
            .get_symbol_exact("renderTheme", SymbolType::Function)
            .expect("renderTheme exact")
            .expect("renderTheme symbol");
        let render_deps = indexer
            .storage
            .get_symbol_dependencies(render_theme.id.expect("renderTheme id"))
            .expect("renderTheme dependencies");
        assert!(
            render_deps.iter().all(|dep| dep.file != "src/api/loadThemeConfig.js"),
            "unsupported CommonJS destructured require should not drift to named duplicate"
        );
    }

    #[test]
    fn unsupported_commonjs_object_member_call_does_not_drift_to_named_duplicate() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let fixture =
            materialize_quality_fixture("unsupported_commonjs_object_boundary").expect("fixture");
        let repo = fixture.repo_root().to_path_buf();
        let mut indexer = open_indexer(&tmp);
        indexer.index_repo(&repo).expect("index repo");

        let render_theme = indexer
            .storage
            .get_symbol_exact("renderTheme", SymbolType::Function)
            .expect("renderTheme exact")
            .expect("renderTheme symbol");
        let render_deps = indexer
            .storage
            .get_symbol_dependencies(render_theme.id.expect("renderTheme id"))
            .expect("renderTheme dependencies");
        assert!(
            render_deps.iter().all(|dep| dep.file != "src/api/loadThemeConfig.js"),
            "unsupported CommonJS object member call should not drift to named duplicate"
        );
    }

    #[test]
    fn indexes_mixed_module_pattern_noise_without_duplicate_drift() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let fixture = materialize_quality_fixture("mixed_module_pattern_noise").expect("fixture");
        let repo = fixture.repo_root().to_path_buf();
        let mut indexer = open_indexer(&tmp);
        indexer.index_repo(&repo).expect("index repo");

        let build_theme = indexer
            .storage
            .get_symbol_exact("buildTheme", SymbolType::Function)
            .expect("buildTheme exact")
            .expect("buildTheme symbol");
        let build_deps = indexer
            .storage
            .get_symbol_dependencies(build_theme.id.expect("buildTheme id"))
            .expect("buildTheme dependencies");
        assert_eq!(build_deps.len(), 1);
        assert_eq!(build_deps[0].name, "loadThemeConfig");
        assert_eq!(build_deps[0].file, "src/shared/themeNamed.ts");

        let render_theme = indexer
            .storage
            .get_symbol_exact("renderTheme", SymbolType::Function)
            .expect("renderTheme exact")
            .expect("renderTheme symbol");
        let render_deps = indexer
            .storage
            .get_symbol_dependencies(render_theme.id.expect("renderTheme id"))
            .expect("renderTheme dependencies");
        assert!(
            render_deps.iter().all(|dep| dep.file != "src/api/loadThemeConfig.ts"),
            "mixed-pattern unsupported commonjs path should not drift to API duplicate"
        );
    }

    #[test]
    fn indexes_workspace_mixed_module_noise_without_cross_package_drift() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let fixture = materialize_quality_fixture("workspace_mixed_module_noise").expect("fixture");
        let repo = fixture.repo_root().to_path_buf();
        let mut indexer = open_indexer(&tmp);
        indexer.index_repo(&repo).expect("index repo");

        let init_auth_symbols = indexer
            .storage
            .search_symbol_by_name("initAuth", 10)
            .expect("search initAuth");
        assert_eq!(init_auth_symbols.len(), 2);
        assert!(init_auth_symbols
            .iter()
            .any(|symbol| symbol.file == "packages/api/src/auth/flow.ts"));
        assert!(init_auth_symbols
            .iter()
            .any(|symbol| symbol.file == "packages/worker/src/auth/flow.ts"));

        let api_init = init_auth_symbols
            .iter()
            .find(|symbol| symbol.file == "packages/api/src/auth/flow.ts")
            .and_then(|symbol| symbol.id)
            .expect("api initAuth id");
        let api_deps = indexer
            .storage
            .get_symbol_dependencies(api_init)
            .expect("api initAuth deps");
        assert_eq!(api_deps.len(), 1);
        assert_eq!(api_deps[0].file, "packages/api/src/shared/loadConfig.ts");

        let worker_init = init_auth_symbols
            .iter()
            .find(|symbol| symbol.file == "packages/worker/src/auth/flow.ts")
            .and_then(|symbol| symbol.id)
            .expect("worker initAuth id");
        let worker_deps = indexer
            .storage
            .get_symbol_dependencies(worker_init)
            .expect("worker initAuth deps");
        assert!(
            worker_deps
                .iter()
                .all(|dep| dep.file != "packages/api/src/shared/loadConfig.ts"),
            "worker commonjs path should not drift into api package"
        );
    }

    #[test]
    fn workspace_mixed_module_noise_reindexes_after_worker_commonjs_rename() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let fixture = materialize_quality_fixture("workspace_mixed_module_noise").expect("fixture");
        let repo = fixture.repo_root().to_path_buf();
        let mut indexer = open_indexer(&tmp);
        indexer.index_repo(&repo).expect("index repo");

        std::fs::rename(
            repo.join("packages")
                .join("worker")
                .join("src")
                .join("shared")
                .join("commonjsConfig.js"),
            repo.join("packages")
                .join("worker")
                .join("src")
                .join("shared")
                .join("runtimeConfig.js"),
        )
        .expect("rename worker commonjs file");
        std::fs::write(
            repo.join("packages")
                .join("worker")
                .join("src")
                .join("auth")
                .join("flow.ts"),
            concat!(
                "const configModule = require(\"../shared/runtimeConfig\");\n\n",
                "export function initAuth() {\n",
                "  return configModule.loadConfig();\n",
                "}\n",
            ),
        )
        .expect("rewrite worker flow import");

        indexer
            .delete_file("packages/worker/src/shared/commonjsConfig.js")
            .expect("delete old worker commonjs file from index");
        indexer
            .index_file(&repo, "packages/worker/src/shared/runtimeConfig.js")
            .expect("index renamed worker commonjs file");
        indexer
            .index_file(&repo, "packages/worker/src/auth/flow.ts")
            .expect("reindex worker auth flow");

        let worker_init = indexer
            .storage
            .search_symbol_by_name("initAuth", 10)
            .expect("search initAuth")
            .into_iter()
            .find(|symbol| symbol.file == "packages/worker/src/auth/flow.ts")
            .and_then(|symbol| symbol.id)
            .expect("worker initAuth id");
        let worker_deps = indexer
            .storage
            .get_symbol_dependencies(worker_init)
            .expect("worker initAuth deps after rename");
        assert!(
            worker_deps
                .iter()
                .all(|dep| dep.file != "packages/api/src/shared/loadConfig.ts"),
            "worker commonjs rename should not drift into api package"
        );
        assert!(
            indexer
                .storage
                .get_file_checksum("packages/worker/src/shared/commonjsConfig.js")
                .expect("old worker commonjs checksum lookup")
                .is_none(),
            "old worker commonjs metadata should be removed after rename"
        );
    }

    #[test]
    fn indexes_workspace_mixed_module_with_tests_without_test_config_drift() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let fixture =
            materialize_quality_fixture("workspace_mixed_module_with_tests").expect("fixture");
        let repo = fixture.repo_root().to_path_buf();
        let mut indexer = open_indexer(&tmp);
        indexer.index_repo(&repo).expect("index repo");

        let init_auth_symbols = indexer
            .storage
            .search_symbol_by_name("initAuth", 10)
            .expect("search initAuth");
        assert_eq!(init_auth_symbols.len(), 2);
        assert!(init_auth_symbols
            .iter()
            .any(|symbol| symbol.file == "packages/api/src/auth/flow.ts"));
        assert!(init_auth_symbols
            .iter()
            .any(|symbol| symbol.file == "packages/worker/src/auth/flow.ts"));

        let api_init = init_auth_symbols
            .iter()
            .find(|symbol| symbol.file == "packages/api/src/auth/flow.ts")
            .and_then(|symbol| symbol.id)
            .expect("api initAuth id");
        let api_deps = indexer
            .storage
            .get_symbol_dependencies(api_init)
            .expect("api initAuth deps");
        assert_eq!(api_deps.len(), 1);
        assert_eq!(api_deps[0].file, "packages/api/src/shared/loadConfig.ts");
    }

    #[test]
    fn workspace_mixed_module_with_tests_reindexes_after_api_load_config_rename() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let fixture =
            materialize_quality_fixture("workspace_mixed_module_with_tests").expect("fixture");
        let repo = fixture.repo_root().to_path_buf();
        let mut indexer = open_indexer(&tmp);
        indexer.index_repo(&repo).expect("index repo");

        std::fs::rename(
            repo.join("packages")
                .join("api")
                .join("src")
                .join("shared")
                .join("loadConfig.ts"),
            repo.join("packages")
                .join("api")
                .join("src")
                .join("shared")
                .join("runtimeConfig.ts"),
        )
        .expect("rename api config file");
        std::fs::write(
            repo.join("packages")
                .join("api")
                .join("src")
                .join("auth")
                .join("flow.ts"),
            concat!(
                "import { loadConfig } from \"../shared/runtimeConfig\";\n\n",
                "export function initAuth() {\n",
                "  return loadConfig();\n",
                "}\n",
            ),
        )
        .expect("rewrite api auth import");
        std::fs::write(
            repo.join("packages")
                .join("api")
                .join("tests")
                .join("auth.spec.ts"),
            concat!(
                "import { initAuth } from \"../src/auth/flow\";\n\n",
                "describe(\"api initAuth\", () => {\n",
                "  it(\"uses api config\", () => {\n",
                "    expect(initAuth()).toBeTruthy();\n",
                "  });\n",
                "});\n",
            ),
        )
        .expect("rewrite api auth test");

        indexer
            .delete_file("packages/api/src/shared/loadConfig.ts")
            .expect("delete old api config file from index");
        indexer
            .index_file(&repo, "packages/api/src/shared/runtimeConfig.ts")
            .expect("index renamed api config file");
        indexer
            .index_file(&repo, "packages/api/src/auth/flow.ts")
            .expect("reindex api auth flow");
        indexer
            .index_file(&repo, "packages/api/tests/auth.spec.ts")
            .expect("reindex api auth test");

        let api_init = indexer
            .storage
            .search_symbol_by_name("initAuth", 10)
            .expect("search initAuth")
            .into_iter()
            .find(|symbol| symbol.file == "packages/api/src/auth/flow.ts")
            .and_then(|symbol| symbol.id)
            .expect("api initAuth id");
        let api_deps = indexer
            .storage
            .get_symbol_dependencies(api_init)
            .expect("api initAuth deps after rename");
        assert!(
            api_deps
                .iter()
                .all(|dep| dep.file != "packages/api/src/shared/loadConfig.ts"),
            "api auth path should not keep stale loadConfig path after rename"
        );
        assert!(
            indexer
                .storage
                .get_file_checksum("packages/api/src/shared/loadConfig.ts")
                .expect("old api config checksum lookup")
                .is_none(),
            "old api config metadata should be removed after rename"
        );
    }

    #[test]
    fn indexes_python_workspace_noise_without_cross_package_drift() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let fixture = materialize_quality_fixture("python_workspace_noise").expect("fixture");
        let repo = fixture.repo_root().to_path_buf();
        let mut indexer = open_indexer(&tmp);
        indexer.index_repo(&repo).expect("index repo");

        let init_auth_symbols = indexer
            .storage
            .search_symbol_by_name("init_auth", 10)
            .expect("search init_auth");
        assert!(init_auth_symbols
            .iter()
            .any(|symbol| symbol.file == "packages/api/auth_flow.py"));
        assert!(init_auth_symbols
            .iter()
            .any(|symbol| symbol.file == "packages/worker/auth_flow.py"));

        let api_init = init_auth_symbols
            .iter()
            .find(|symbol| symbol.file == "packages/api/auth_flow.py")
            .and_then(|symbol| symbol.id)
            .expect("api init_auth id");
        let api_deps = indexer
            .storage
            .get_symbol_dependencies(api_init)
            .expect("api init_auth deps");
        assert_eq!(api_deps.len(), 1);
        assert_eq!(api_deps[0].file, "packages/api/config.py");
    }

    #[test]
    fn python_workspace_noise_reindexes_after_api_config_rename() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let fixture = materialize_quality_fixture("python_workspace_noise").expect("fixture");
        let repo = fixture.repo_root().to_path_buf();
        let mut indexer = open_indexer(&tmp);
        indexer.index_repo(&repo).expect("index repo");

        std::fs::rename(
            repo.join("packages").join("api").join("config.py"),
            repo.join("packages").join("api").join("runtime_config.py"),
        )
        .expect("rename api config file");
        std::fs::write(
            repo.join("packages").join("api").join("auth_flow.py"),
            concat!(
                "from runtime_config import load_config\n\n\n",
                "def init_auth():\n",
                "    return load_config()\n",
            ),
        )
        .expect("rewrite api auth import");
        std::fs::write(
            repo.join("packages").join("api").join("test_auth.py"),
            concat!(
                "from auth_flow import init_auth\n\n\n",
                "def test_init_auth_uses_api_config():\n",
                "    assert init_auth()[\"source\"] == \"api\"\n",
            ),
        )
        .expect("rewrite api auth test");

        indexer
            .delete_file("packages/api/config.py")
            .expect("delete old api config from index");
        indexer
            .index_file(&repo, "packages/api/runtime_config.py")
            .expect("index renamed api config");
        indexer
            .index_file(&repo, "packages/api/auth_flow.py")
            .expect("reindex api auth flow");
        indexer
            .index_file(&repo, "packages/api/test_auth.py")
            .expect("reindex api auth test");

        let api_init = indexer
            .storage
            .search_symbol_by_name("init_auth", 10)
            .expect("search init_auth")
            .into_iter()
            .find(|symbol| symbol.file == "packages/api/auth_flow.py")
            .and_then(|symbol| symbol.id)
            .expect("api init_auth id");
        let api_deps = indexer
            .storage
            .get_symbol_dependencies(api_init)
            .expect("api init_auth deps");
        assert_eq!(api_deps.len(), 1);
        assert_eq!(api_deps[0].file, "packages/api/runtime_config.py");
        assert!(
            api_deps
                .iter()
                .all(|dep| dep.file != "packages/api/config.py"),
            "api python dependency edges should not keep stale config path after rename"
        );
        assert!(
            indexer
                .storage
                .get_file_checksum("packages/api/config.py")
                .expect("old api config checksum lookup")
                .is_none(),
            "old api python config metadata should be removed after rename"
        );
    }

    #[test]
    fn python_workspace_noise_reindexes_after_worker_config_rename() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let fixture = materialize_quality_fixture("python_workspace_noise").expect("fixture");
        let repo = fixture.repo_root().to_path_buf();
        let mut indexer = open_indexer(&tmp);
        indexer.index_repo(&repo).expect("index repo");

        std::fs::rename(
            repo.join("packages").join("worker").join("config.py"),
            repo.join("packages").join("worker").join("runtime_config.py"),
        )
        .expect("rename worker config file");
        std::fs::write(
            repo.join("packages").join("worker").join("auth_flow.py"),
            concat!(
                "from runtime_config import load_config\n\n\n",
                "def init_auth():\n",
                "    return load_config()\n",
            ),
        )
        .expect("rewrite worker auth import");
        std::fs::write(
            repo.join("packages").join("worker").join("test_auth.py"),
            concat!(
                "from auth_flow import init_auth\n\n\n",
                "def test_init_auth_uses_worker_config():\n",
                "    assert init_auth()[\"source\"] == \"worker\"\n",
            ),
        )
        .expect("rewrite worker auth test");

        indexer
            .delete_file("packages/worker/config.py")
            .expect("delete old worker config from index");
        indexer
            .index_file(&repo, "packages/worker/runtime_config.py")
            .expect("index renamed worker config");
        indexer
            .index_file(&repo, "packages/worker/auth_flow.py")
            .expect("reindex worker auth flow");
        indexer
            .index_file(&repo, "packages/worker/test_auth.py")
            .expect("reindex worker auth test");

        let worker_init = indexer
            .storage
            .search_symbol_by_name("init_auth", 10)
            .expect("search init_auth")
            .into_iter()
            .find(|symbol| symbol.file == "packages/worker/auth_flow.py")
            .and_then(|symbol| symbol.id)
            .expect("worker init_auth id");
        let worker_deps = indexer
            .storage
            .get_symbol_dependencies(worker_init)
            .expect("worker init_auth deps");
        assert_eq!(worker_deps.len(), 1);
        assert_eq!(worker_deps[0].file, "packages/worker/runtime_config.py");
        assert!(
            worker_deps
                .iter()
                .all(|dep| dep.file != "packages/worker/config.py"),
            "worker python dependency edges should not keep stale config path after rename"
        );
        assert!(
            indexer
                .storage
                .get_file_checksum("packages/worker/config.py")
                .expect("old worker config checksum lookup")
                .is_none(),
            "old worker python config metadata should be removed after rename"
        );
    }

    #[test]
    fn workspace_mixed_module_with_tests_reindexes_after_worker_commonjs_rename() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let fixture =
            materialize_quality_fixture("workspace_mixed_module_with_tests").expect("fixture");
        let repo = fixture.repo_root().to_path_buf();
        let mut indexer = open_indexer(&tmp);
        indexer.index_repo(&repo).expect("index repo");

        std::fs::rename(
            repo.join("packages")
                .join("worker")
                .join("src")
                .join("shared")
                .join("commonjsConfig.js"),
            repo.join("packages")
                .join("worker")
                .join("src")
                .join("shared")
                .join("runtimeCommonjsConfig.js"),
        )
        .expect("rename worker commonjs file");
        std::fs::write(
            repo.join("packages")
                .join("worker")
                .join("src")
                .join("auth")
                .join("flow.ts"),
            concat!(
                "const configModule = require(\"../shared/runtimeCommonjsConfig\");\n\n",
                "export function initAuth() {\n",
                "  return configModule.loadConfig();\n",
                "}\n",
            ),
        )
        .expect("rewrite worker auth import");
        std::fs::write(
            repo.join("packages")
                .join("worker")
                .join("tests")
                .join("auth.spec.ts"),
            concat!(
                "import { initAuth } from \"../src/auth/flow\";\n\n",
                "describe(\"worker initAuth\", () => {\n",
                "  it(\"uses worker config\", () => {\n",
                "    expect(initAuth()).toBeTruthy();\n",
                "  });\n",
                "});\n",
            ),
        )
        .expect("rewrite worker auth test");

        indexer
            .delete_file("packages/worker/src/shared/commonjsConfig.js")
            .expect("delete old worker commonjs file from index");
        indexer
            .index_file(&repo, "packages/worker/src/shared/runtimeCommonjsConfig.js")
            .expect("index renamed worker commonjs file");
        indexer
            .index_file(&repo, "packages/worker/src/auth/flow.ts")
            .expect("reindex worker auth flow");
        indexer
            .index_file(&repo, "packages/worker/tests/auth.spec.ts")
            .expect("reindex worker auth test");

        let worker_init = indexer
            .storage
            .search_symbol_by_name("initAuth", 10)
            .expect("search initAuth")
            .into_iter()
            .find(|symbol| symbol.file == "packages/worker/src/auth/flow.ts")
            .and_then(|symbol| symbol.id)
            .expect("worker initAuth id");
        let worker_deps = indexer
            .storage
            .get_symbol_dependencies(worker_init)
            .expect("worker initAuth deps after rename");
        assert!(
            worker_deps
                .iter()
                .all(|dep| dep.file != "packages/api/src/shared/loadConfig.ts"),
            "worker commonjs rename should not drift into api package"
        );
        assert!(
            indexer
                .storage
                .get_file_checksum("packages/worker/src/shared/commonjsConfig.js")
                .expect("old worker commonjs checksum lookup")
                .is_none(),
            "old worker commonjs metadata should be removed after rename"
        );
    }
}
