use crate::config::ensure_semantic_config;
#[cfg(feature = "rust-support")]
use crate::config::load_rust_support_config;
use crate::session::SemanticMiddlewareState;
use anyhow::Result;
use fs2::FileExt;
use indexer::Indexer;
use knowledge_graph::KnowledgeGraph;
use llm_router::LLMRouter;
use parking_lot::Mutex;
use parser::SupportedLanguage;
use retrieval::RetrievalService;
use serde::Deserialize;
use serde_json::json;
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use storage::default_paths;
use tracing::{info, warn};
use walkdir::WalkDir;
use watcher::RepoWatcher;

const STATUS_REPO_SCAN_TTL: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy)]
pub enum BootstrapIndexPolicy {
    ReuseExistingOrCreate,
    ForceRefresh,
    Skip,
}

#[derive(Debug, Clone, Copy)]
pub struct RuntimeOptions {
    pub start_watcher: bool,
    pub ensure_config: bool,
    pub bootstrap_index_policy: BootstrapIndexPolicy,
}

impl Default for RuntimeOptions {
    fn default() -> Self {
        Self {
            start_watcher: false,
            ensure_config: true,
            bootstrap_index_policy: BootstrapIndexPolicy::ReuseExistingOrCreate,
        }
    }
}

#[derive(Debug, Clone)]
pub struct WorkspaceState {
    pub workspace_mode_enabled: bool,
    pub workspace_roots: Vec<PathBuf>,
    pub primary_root: PathBuf,
}

impl WorkspaceState {
    pub fn load(primary_root: &Path) -> Self {
        let config_path = primary_root.join(".semantic").join("workspace.toml");
        let mut roots = Vec::new();
        if let Ok(raw) = std::fs::read_to_string(&config_path) {
            let mut in_paths = false;
            for line in raw.lines() {
                let trimmed = line.trim();
                if trimmed == "paths = [" || trimmed == "paths=[" {
                    in_paths = true;
                    continue;
                }
                if in_paths {
                    if trimmed == "]" {
                        break;
                    }
                    let path = trimmed.trim_matches(',').trim().trim_matches('"');
                    if path.is_empty() {
                        continue;
                    }
                    let resolved = if Path::new(path).is_absolute() {
                        PathBuf::from(path)
                    } else {
                        primary_root.join(path)
                    };
                    if let Ok(canonical) = resolved.canonicalize() {
                        roots.push(canonical);
                    }
                }
            }
        }

        Self {
            workspace_mode_enabled: false,
            workspace_roots: roots,
            primary_root: primary_root.to_path_buf(),
        }
    }
}

struct AppRuntimeInner {
    repo_root: PathBuf,
    retrieval: Arc<Mutex<RetrievalService>>,
    indexer: Arc<Mutex<Indexer>>,
    watcher: Mutex<Option<RepoWatcher>>,
    semantic_middleware: Arc<Mutex<SemanticMiddlewareState>>,
    workspace_state: Arc<Mutex<WorkspaceState>>,
    knowledge_graph: Arc<Mutex<KnowledgeGraph>>,
    bootstrap_index_action: &'static str,
    status_cache: Mutex<StatusCache>,
}

#[derive(Clone)]
pub struct AppRuntime {
    inner: Arc<AppRuntimeInner>,
}

#[derive(Debug, Default)]
struct StatusCache {
    repo_scan: Option<TimedRepoScan>,
    indexed_snapshot: Option<IndexedStatusSnapshot>,
}

#[derive(Debug, Clone)]
struct TimedRepoScan {
    computed_at: Instant,
    summary: RepoSourceBoundarySummary,
}

#[derive(Debug, Clone)]
struct IndexedStatusSnapshot {
    index_revision: u64,
    index_available: bool,
    indexed_file_count: usize,
    indexed_path_hints: Vec<String>,
    index_region_status: &'static str,
    indexed_region_hints: Vec<String>,
}

impl AppRuntime {
    pub fn bootstrap(repo_root: PathBuf, options: RuntimeOptions) -> Result<Self> {
        if options.ensure_config {
            ensure_semantic_config(&repo_root)?;
        }

        let _index_lock = RepoIndexLock::acquire(&repo_root)?;
        let (db_path, tantivy_path) = default_paths(&repo_root);
        let index_storage = storage::Storage::open(&db_path, &tantivy_path)?;
        let has_existing_index = !index_storage.list_files()?.is_empty();
        let mut indexer = Indexer::new(index_storage);
        let bootstrap_index_action = match options.bootstrap_index_policy {
            BootstrapIndexPolicy::ForceRefresh => {
                indexer.index_repo(&repo_root)?;
                "refresh_full"
            }
            BootstrapIndexPolicy::Skip => "skip_bootstrap_refresh",
            BootstrapIndexPolicy::ReuseExistingOrCreate => {
                if has_existing_index {
                    "reuse_existing"
                } else {
                    indexer.index_repo(&repo_root)?;
                    "bootstrap_full"
                }
            }
        };
        let indexer = Arc::new(Mutex::new(indexer));

        let retrieval_storage = storage::Storage::open(&db_path, &tantivy_path)?;
        let retrieval = Arc::new(Mutex::new(RetrievalService::new(
            repo_root.clone(),
            retrieval_storage,
        )));

        let knowledge_graph = load_knowledge_graph(&repo_root);

        let runtime = Self {
            inner: Arc::new(AppRuntimeInner {
                repo_root: repo_root.clone(),
                retrieval,
                indexer,
                watcher: Mutex::new(None),
                semantic_middleware: Arc::new(Mutex::new(SemanticMiddlewareState::default())),
                workspace_state: Arc::new(Mutex::new(WorkspaceState::load(&repo_root))),
                knowledge_graph,
                bootstrap_index_action,
                status_cache: Mutex::new(StatusCache::default()),
            }),
        };

        if options.start_watcher {
            runtime.ensure_watcher_started()?;
        }

        Ok(runtime)
    }

    pub fn repo_root(&self) -> &Path {
        &self.inner.repo_root
    }

    pub fn retrieval(&self) -> Arc<Mutex<RetrievalService>> {
        self.inner.retrieval.clone()
    }

    pub fn indexer(&self) -> Arc<Mutex<Indexer>> {
        self.inner.indexer.clone()
    }

    pub fn middleware(&self) -> Arc<Mutex<SemanticMiddlewareState>> {
        self.inner.semantic_middleware.clone()
    }

    pub fn workspace_state(&self) -> Arc<Mutex<WorkspaceState>> {
        self.inner.workspace_state.clone()
    }

    pub fn llm_router(&self) -> Option<Arc<LLMRouter>> {
        crate::improvement_loop::refresh_model_metrics_from_feedback(self.repo_root()).ok();
        load_llm_router(&self.inner.repo_root)
    }

    pub fn knowledge_graph(&self) -> Arc<Mutex<KnowledgeGraph>> {
        self.inner.knowledge_graph.clone()
    }

    pub fn ensure_watcher_started(&self) -> Result<()> {
        let mut guard = self.inner.watcher.lock();
        if guard.is_none() {
            *guard = Some(RepoWatcher::start(
                self.inner.repo_root.clone(),
                self.inner.indexer.clone(),
            )?);
            info!("semantic watcher started");
        }
        Ok(())
    }

    pub fn watcher_running(&self) -> bool {
        self.inner.watcher.lock().is_some()
    }

    pub(crate) fn indexed_unsupported_path_fallback(
        &self,
        indexed_files: &[String],
        target: &str,
    ) -> Option<serde_json::Value> {
        let normalized = target.trim().replace('\\', "/").trim_matches('/').to_string();
        if normalized.is_empty() {
            return None;
        }
        let resolved = resolve_indexed_target_alias(indexed_files, &normalized)
            .unwrap_or_else(|| normalized.clone());
        let indexed_match = indexed_files.iter().any(|item| item == &resolved);
        let absolute = self.repo_root().join(&resolved);
        if !absolute.is_file() {
            return None;
        }
        let raw = fs::read_to_string(absolute).ok()?;
        let total_lines = raw.lines().count().max(1);
        let preview_lines = raw.lines().take(160).collect::<Vec<_>>();
        let preview_line_count = preview_lines.len().max(1);
        let mut code = preview_lines.join("\n");
        let truncated = total_lines > preview_line_count;
        if code.len() > 12_000 {
            code.truncate(12_000);
        }
        let file_brief = self.retrieval().lock().get_file_brief(&resolved).ok();
        let index_readiness = if indexed_match {
            "target_ready"
        } else {
            "filesystem_fallback"
        };
        let index_recovery_mode = if indexed_match {
            "none"
        } else {
            "parser_unsupported_filesystem_fallback"
        };
        let index_coverage = if indexed_match {
            "indexed_target"
        } else {
            "filesystem_fallback"
        };
        Some(json!({
            "message": "returned raw file preview for parser-unsupported target",
            "content_kind": "code",
            "path_target_fallback": true,
            "fallback_kind": "unsupported_indexed_file_preview",
            "parser_target_support": "unsupported",
            "file_brief": file_brief,
            "index_readiness": index_readiness,
            "index_recovery_mode": index_recovery_mode,
            "index_recovery_target_kind": "file",
            "index_coverage": index_coverage,
            "index_coverage_target": resolved,
            "context": [{
                "file": target.trim().replace('\\', "/").trim_matches('/'),
                "start": 1,
                "end": preview_line_count,
                "code": code,
                "source": "unsupported_indexed_file_preview"
            }],
            "code_span": {
                "file": target.trim().replace('\\', "/").trim_matches('/'),
                "start_line": 1,
                "end_line": preview_line_count,
                "code": code
            },
            "preview_truncated": truncated,
            "preview_line_count": preview_line_count,
            "total_line_count": total_lines,
        }))
    }

    pub fn status_json(&self) -> serde_json::Value {
        let retrieval = self.inner.retrieval.lock();
        let workspace = self.inner.workspace_state.lock();
        let index_revision = retrieval.index_revision();
        let indexed_snapshot = self.cached_indexed_status(&retrieval, index_revision);
        let repo_scan = self.cached_repo_source_boundary();
        let rust_status = rust_support_status(self.repo_root());
        let indexing_mode = "full_with_default_excludes";
        let indexing_completeness = "source_focused";
        serde_json::json!({
            "ok": true,
            "repo_root": self.inner.repo_root,
            "index_revision": index_revision,
            "index_available": indexed_snapshot.index_available,
            "indexed_file_count": indexed_snapshot.indexed_file_count,
            "indexed_path_hints": indexed_snapshot.indexed_path_hints,
            "index_region_status": indexed_snapshot.index_region_status,
            "indexed_region_hints": indexed_snapshot.indexed_region_hints,
            "supported_languages": supported_languages(self.repo_root()),
            "rust_support": {
                "compiled": rust_status.compiled,
                "enabled": rust_status.enabled,
                "status": rust_status.status,
                "small_project_mode": rust_status.small_project_mode,
            },
            "repo_supported_source_file_count": repo_scan.supported_source_file_count,
            "repo_unsupported_source_file_count": repo_scan.unsupported_source_file_count,
            "repo_unsupported_source_path_hints": repo_scan.unsupported_source_path_hints,
            "watcher_running": self.watcher_running(),
            "bootstrap_index_action": self.inner.bootstrap_index_action,
            "indexing_mode": indexing_mode,
            "indexing_completeness": indexing_completeness,
            "workspace_mode_enabled": workspace.workspace_mode_enabled,
            "workspace_roots": workspace.workspace_roots,
            "llm_router_configured": self.llm_router().is_some(),
            "performance": retrieval.get_performance_stats(),
        })
    }

    fn cached_repo_source_boundary(&self) -> RepoSourceBoundarySummary {
        {
            let cache = self.inner.status_cache.lock();
            if let Some(cached) = cache.repo_scan.as_ref() {
                if cached.computed_at.elapsed() < STATUS_REPO_SCAN_TTL {
                    return cached.summary.clone();
                }
            }
        }

        let summary = summarize_repo_source_boundary(&self.inner.repo_root);
        let mut cache = self.inner.status_cache.lock();
        cache.repo_scan = Some(TimedRepoScan {
            computed_at: Instant::now(),
            summary: summary.clone(),
        });
        summary
    }

    fn cached_indexed_status(
        &self,
        retrieval: &RetrievalService,
        index_revision: u64,
    ) -> IndexedStatusSnapshot {
        {
            let cache = self.inner.status_cache.lock();
            if let Some(cached) = cache.indexed_snapshot.as_ref() {
                if cached.index_revision == index_revision {
                    return cached.clone();
                }
            }
        }

        let indexed_files = retrieval
            .with_storage(|storage| storage.list_files())
            .unwrap_or_default();
        let index_manifest = load_index_coverage_manifest(&self.inner.repo_root);
        let indexed_file_count = indexed_files.len();
        let index_available = indexed_file_count > 0;
        let snapshot = IndexedStatusSnapshot {
            index_revision,
            index_available,
            indexed_file_count,
            indexed_path_hints: summarize_indexed_path_hints(&indexed_files),
            index_region_status: index_region_status(
                index_available,
                index_manifest.as_ref().map(|m| m.coverage_mode.as_str()),
            ),
            indexed_region_hints: index_manifest
                .as_ref()
                .map(|m| m.targeted_paths.clone())
                .unwrap_or_default(),
        };
        let mut cache = self.inner.status_cache.lock();
        cache.indexed_snapshot = Some(snapshot.clone());
        snapshot
    }
}

struct RepoIndexLock {
    file: std::fs::File,
}

impl RepoIndexLock {
    fn acquire(repo_root: &Path) -> Result<Self> {
        let lock_path = repo_root.join(".semantic").join("index.lock");
        if let Some(parent) = lock_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(&lock_path)?;
        file.lock_exclusive()?;
        Ok(Self { file })
    }
}

impl Drop for RepoIndexLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

pub(crate) fn summarize_indexed_path_hints(files: &[String]) -> Vec<String> {
    let mut counts = std::collections::BTreeMap::<String, usize>::new();
    for file in files {
        if is_internal_index_summary_path(file) {
            continue;
        }
        let hint = Path::new(file)
            .parent()
            .and_then(|parent| parent.to_str())
            .map(|path| path.replace('\\', "/"))
            .filter(|path| !path.is_empty())
            .unwrap_or_else(|| file.clone());
        *counts.entry(hint).or_default() += 1;
    }

    let mut ranked = counts.into_iter().collect::<Vec<_>>();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    ranked.into_iter().take(8).map(|(path, _)| path).collect()
}

fn is_internal_index_summary_path(path: &str) -> bool {
    let normalized = path.replace('\\', "/");
    let components = normalized
        .split('/')
        .filter(|component| !component.is_empty())
        .collect::<Vec<_>>();

    if components
        .iter()
        .any(|component| matches!(*component, ".semantic" | ".claude"))
    {
        return true;
    }

    let has_fixture_segment = components.iter().any(|component| {
        component.eq_ignore_ascii_case("fixture")
            || component.eq_ignore_ascii_case("fixtures")
            || component.eq_ignore_ascii_case("test_fixture")
            || component.eq_ignore_ascii_case("test_fixtures")
    });
    let has_worktree_segment = components.iter().any(|component| {
        component.eq_ignore_ascii_case("worktree") || component.eq_ignore_ascii_case("worktrees")
    });

    has_fixture_segment && has_worktree_segment
}

pub(crate) fn summarize_index_recovery_delta(
    before: &[String],
    after: &[String],
) -> (usize, Vec<String>) {
    let before_set = before.iter().cloned().collect::<std::collections::BTreeSet<_>>();
    let mut added = after
        .iter()
        .filter(|path| !before_set.contains(*path))
        .cloned()
        .collect::<Vec<_>>();
    added.sort();
    let added_file_count = added.len();
    let changed_files = added
        .iter()
        .filter(|path| !is_internal_index_summary_path(path))
        .take(8)
        .cloned()
        .collect::<Vec<_>>();
    (added_file_count, changed_files)
}

pub(crate) fn parser_support_for_target_path(target: &str) -> &'static str {
    let normalized = target.trim().replace('\\', "/").trim_matches('/').to_string();
    if normalized.is_empty() {
        return "unknown";
    }
    let file_like = normalized
        .rsplit('/')
        .next()
        .map(|segment| segment.contains('.'))
        .unwrap_or(false);
    if !file_like {
        return "unknown";
    }
    if SupportedLanguage::from_path(&normalized).is_some() {
        "supported"
    } else {
        "unsupported"
    }
}

#[derive(Debug, Clone, Copy)]
struct RustSupportStatus {
    compiled: bool,
    enabled: bool,
    status: &'static str,
    small_project_mode: bool,
}

fn supported_languages(repo_root: &Path) -> Vec<&'static str> {
    #[cfg(not(feature = "rust-support"))]
    let _ = repo_root;
    #[cfg(feature = "rust-support")]
    let rust = rust_support_status(repo_root);
    SupportedLanguage::ALL
        .iter()
        .filter_map(|language| match language {
            SupportedLanguage::Python => Some("python"),
            SupportedLanguage::JavaScript => Some("javascript"),
            SupportedLanguage::TypeScript => Some("typescript"),
            #[cfg(feature = "rust-support")]
            SupportedLanguage::Rust if rust.enabled => Some("rust"),
            #[cfg(feature = "rust-support")]
            SupportedLanguage::Rust => None,
        })
        .collect()
}

fn rust_support_status(repo_root: &Path) -> RustSupportStatus {
    #[cfg(feature = "rust-support")]
    {
        let config = load_rust_support_config(repo_root);
        RustSupportStatus {
            compiled: true,
            enabled: config.enabled,
            status: if config.enabled {
                "enabled"
            } else {
                "compiled_disabled"
            },
            small_project_mode: config.small_project_mode,
        }
    }
    #[cfg(not(feature = "rust-support"))]
    {
        let _ = repo_root;
        RustSupportStatus {
            compiled: false,
            enabled: false,
            status: "unsupported",
            small_project_mode: true,
        }
    }
}

pub(crate) fn resolve_indexed_target_alias(
    indexed_files: &[String],
    target: &str,
) -> Option<String> {
    let normalized = target.trim().replace('\\', "/").trim_matches('/').to_string();
    if normalized.is_empty() {
        return None;
    }
    if indexed_files.iter().any(|item| item == &normalized) {
        return Some(normalized);
    }
    let (stem, extension) = normalized.rsplit_once('.')?;
    let candidates = match extension {
        "ts" | "tsx" | "js" | "jsx" => [
            format!("{stem}.ts"),
            format!("{stem}.tsx"),
            format!("{stem}.js"),
            format!("{stem}.jsx"),
        ]
        .into_iter()
        .filter(|candidate| candidate != &normalized)
        .collect::<Vec<_>>(),
        _ => return None,
    };
    candidates
        .into_iter()
        .find(|candidate| indexed_files.iter().any(|item| item == candidate))
}

#[derive(Debug, Clone, Default)]
pub(crate) struct RepoSourceBoundarySummary {
    pub(crate) supported_source_file_count: usize,
    pub(crate) unsupported_source_file_count: usize,
    pub(crate) unsupported_source_path_hints: Vec<String>,
    pub(crate) unsupported_source_files: Vec<String>,
}

pub(crate) fn summarize_repo_source_boundary(repo_root: &Path) -> RepoSourceBoundarySummary {
    let mut summary = RepoSourceBoundarySummary::default();
    let rust = rust_support_status(repo_root);
    for entry in WalkDir::new(repo_root).sort_by_file_name().into_iter().filter_map(|entry| entry.ok()) {
        if !entry.file_type().is_file() {
            continue;
        }
        let Ok(relative) = entry.path().strip_prefix(repo_root) else {
            continue;
        };
        let relative = relative.to_string_lossy().replace('\\', "/");
        if should_skip_status_source_scan_path(&relative) {
            continue;
        }
        if SupportedLanguage::from_path(&relative).is_some()
            && (!relative.ends_with(".rs") || rust.enabled)
        {
            summary.supported_source_file_count += 1;
            continue;
        }
        if is_probably_unsupported_source_file(&relative) {
            summary.unsupported_source_file_count += 1;
            summary.unsupported_source_files.push(relative);
        }
    }
    summary.unsupported_source_files.sort();
    summary.unsupported_source_path_hints = summary
        .unsupported_source_files
        .iter()
        .take(8)
        .cloned()
        .collect();
    summary
}

pub(crate) fn should_skip_status_source_scan_path(relative_path: &str) -> bool {
    let normalized = relative_path.replace('\\', "/");
    let lower = normalized.to_ascii_lowercase();
    let heavy_dirs = [
        ".venv/",
        "venv/",
        "env/",
        "__pycache__/",
        "site-packages/",
        ".claude/",
        ".semantic/",
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
    if heavy_dirs
        .iter()
        .any(|dir| lower.starts_with(dir) || lower.contains(&format!("/{dir}")))
    {
        return true;
    }
    let heavy_suffixes = [".log", ".lock", ".min.js", ".d.ts", ".png", ".pdf", ".exe"];
    heavy_suffixes.iter().any(|suffix| lower.ends_with(suffix))
}

fn is_probably_unsupported_source_file(relative_path: &str) -> bool {
    let normalized = relative_path.replace('\\', "/");
    let lower = normalized.to_ascii_lowercase();
    let source_like_suffixes = [
        ".c", ".cc", ".cpp", ".cs", ".go", ".java", ".kt", ".kts", ".lua", ".mjs", ".cjs",
        ".php", ".rb", ".rs", ".scala", ".sh", ".swift",
    ];
    if source_like_suffixes
        .iter()
        .any(|suffix| lower.ends_with(suffix))
    {
        return true;
    }
    let file_name = Path::new(&normalized)
        .file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.to_ascii_lowercase());
    matches!(
        file_name.as_deref(),
        Some("dockerfile") | Some("makefile")
    )
}

#[derive(Debug, Clone, Deserialize, Default)]
pub(crate) struct IndexCoverageManifest {
    #[serde(default)]
    pub coverage_mode: String,
    #[serde(default)]
    pub targeted_paths: Vec<String>,
}

pub(crate) fn load_index_coverage_manifest(repo_root: &Path) -> Option<IndexCoverageManifest> {
    let path = repo_root.join(".semantic").join("index_manifest.json");
    let raw = fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

pub(crate) fn index_region_status(
    index_available: bool,
    coverage_mode: Option<&str>,
) -> &'static str {
    if !index_available {
        "unindexed"
    } else {
        match coverage_mode {
            Some("full") => "fully_indexed",
            Some("targeted") => "targeted_partial",
            _ => "indexed_unknown_scope",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        index_region_status, parser_support_for_target_path, resolve_indexed_target_alias,
        summarize_index_recovery_delta, summarize_indexed_path_hints,
        summarize_repo_source_boundary, AppRuntime, BootstrapIndexPolicy, RuntimeOptions,
    };
    use std::fs;

    #[test]
    fn bootstrap_creates_index_when_repo_is_unindexed() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join("src")).expect("mkdir src");
        fs::write(repo.join("src").join("main.py"), "def run_app():\n    return 1\n")
            .expect("write source");

        let runtime = AppRuntime::bootstrap(
            repo.clone(),
            RuntimeOptions {
                start_watcher: false,
                ensure_config: true,
                bootstrap_index_policy: BootstrapIndexPolicy::ReuseExistingOrCreate,
            },
        )
        .expect("bootstrap runtime");
        let status = runtime.status_json();
        assert_eq!(
            status
                .get("bootstrap_index_action")
                .and_then(|v| v.as_str()),
            Some("bootstrap_full")
        );
        assert_eq!(
            status.get("index_region_status").and_then(|v| v.as_str()),
            Some("fully_indexed")
        );
        assert_eq!(status.get("index_available").and_then(|v| v.as_bool()), Some(true));
    }

    #[test]
    fn bootstrap_reuses_existing_index_by_default() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join("src")).expect("mkdir src");
        fs::write(repo.join("src").join("main.py"), "def run_app():\n    return 1\n")
            .expect("write source");

        let first = AppRuntime::bootstrap(
            repo.clone(),
            RuntimeOptions {
                start_watcher: false,
                ensure_config: true,
                bootstrap_index_policy: BootstrapIndexPolicy::ReuseExistingOrCreate,
            },
        )
        .expect("first bootstrap");
        assert_eq!(
            first.status_json()
                .get("bootstrap_index_action")
                .and_then(|v| v.as_str()),
            Some("bootstrap_full")
        );
        drop(first);

        let second = AppRuntime::bootstrap(
            repo.clone(),
            RuntimeOptions {
                start_watcher: false,
                ensure_config: true,
                bootstrap_index_policy: BootstrapIndexPolicy::ReuseExistingOrCreate,
            },
        )
        .expect("second bootstrap");
        assert_eq!(
            second
                .status_json()
                .get("bootstrap_index_action")
                .and_then(|v| v.as_str()),
            Some("reuse_existing")
        );
        assert_eq!(
            second
                .status_json()
                .get("index_region_status")
                .and_then(|v| v.as_str()),
            Some("fully_indexed")
        );
        assert_eq!(
            second
                .status_json()
                .get("indexed_path_hints")
                .and_then(|v| v.as_array())
                .map(|items| items.iter().filter_map(|item| item.as_str()).collect::<Vec<_>>()),
            Some(vec!["src"])
        );
    }

    #[test]
    fn bootstrap_skip_keeps_unindexed_repo_unindexed() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join("src")).expect("mkdir src");
        fs::write(repo.join("src").join("main.py"), "def run_app():\n    return 1\n")
            .expect("write source");

        let runtime = AppRuntime::bootstrap(
            repo.clone(),
            RuntimeOptions {
                start_watcher: false,
                ensure_config: true,
                bootstrap_index_policy: BootstrapIndexPolicy::Skip,
            },
        )
        .expect("bootstrap runtime");
        let status = runtime.status_json();
        assert_eq!(
            status
                .get("bootstrap_index_action")
                .and_then(|v| v.as_str()),
            Some("skip_bootstrap_refresh")
        );
        assert_eq!(status.get("index_available").and_then(|v| v.as_bool()), Some(false));
        assert_eq!(status.get("indexed_file_count").and_then(|v| v.as_u64()), Some(0));
        assert_eq!(
            status.get("index_region_status").and_then(|v| v.as_str()),
            Some("unindexed")
        );
    }

    #[test]
    fn summarize_index_recovery_delta_reports_new_files_only() {
        let before = vec!["src/auth/session.ts".to_string()];
        let after = vec![
            "src/auth/session.ts".to_string(),
            "src/worker/job.ts".to_string(),
            "src/worker/queue.ts".to_string(),
        ];

        let (count, files) = summarize_index_recovery_delta(&before, &after);
        assert_eq!(count, 2);
        assert_eq!(
            files,
            vec![
                "src/worker/job.ts".to_string(),
                "src/worker/queue.ts".to_string()
            ]
        );
    }

    #[test]
    fn summarize_indexed_path_hints_filters_internal_runtime_paths() {
        let hints = summarize_indexed_path_hints(&[
            "src/auth/session.ts".to_string(),
            "src/auth/token.ts".to_string(),
            ".semantic/index_manifest.json".to_string(),
            ".claude/worktrees/task-123/src/generated.ts".to_string(),
            "tests/fixtures/worktrees/tmp/src/fixture-only.ts".to_string(),
            "packages/api/src/server.ts".to_string(),
        ]);

        assert_eq!(hints, vec!["src/auth", "packages/api/src"]);
    }

    #[test]
    fn summarize_index_recovery_delta_keeps_true_count_but_filters_internal_samples() {
        let before = vec!["src/auth/session.ts".to_string()];
        let after = vec![
            "src/auth/session.ts".to_string(),
            ".semantic/index_manifest.json".to_string(),
            ".claude/worktrees/task-123/src/generated.ts".to_string(),
            "src/worker/job.ts".to_string(),
        ];

        let (count, files) = summarize_index_recovery_delta(&before, &after);
        assert_eq!(count, 3);
        assert_eq!(files, vec!["src/worker/job.ts".to_string()]);
    }

    #[test]
    fn summarize_repo_source_boundary_surfaces_supported_and_unsupported_counts() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join("src")).expect("mkdir src");
        fs::create_dir_all(repo.join("scripts")).expect("mkdir scripts");
        fs::create_dir_all(repo.join(".venv").join("Lib").join("site-packages"))
            .expect("mkdir venv");
        fs::create_dir_all(repo.join(".claude").join("worktrees").join("agent")).expect("mkdir claude");
        fs::create_dir_all(repo.join("node_modules").join("left-pad")).expect("mkdir node_modules");
        fs::write(repo.join("src").join("main.ts"), "export const main = 1;\n")
            .expect("write ts");
        fs::write(repo.join("src").join("main.rs"), "fn main() {}\n").expect("write rs");
        fs::write(repo.join("scripts").join("build.sh"), "echo ok\n").expect("write sh");
        fs::write(
            repo.join(".venv")
                .join("Lib")
                .join("site-packages")
                .join("vendor.py"),
            "print('vendor')\n",
        )
        .expect("write venv py");
        fs::write(
            repo.join(".claude").join("worktrees").join("agent").join("main.rs"),
            "fn main() {}\n",
        )
        .expect("write claude rs");
        fs::write(
            repo.join("node_modules").join("left-pad").join("index.js"),
            "module.exports = 1;\n",
        )
        .expect("write vendor js");

        let summary = summarize_repo_source_boundary(&repo);
        assert_eq!(summary.supported_source_file_count, 1);
        assert_eq!(summary.unsupported_source_file_count, 2);
        assert_eq!(
            summary.unsupported_source_path_hints,
            vec!["scripts/build.sh".to_string(), "src/main.rs".to_string()]
        );
    }

    #[test]
    fn parser_support_for_target_path_distinguishes_supported_unsupported_and_unknown() {
        assert_eq!(parser_support_for_target_path("src/app.ts"), "supported");
        assert_eq!(parser_support_for_target_path("src/main.rs"), "unsupported");
        assert_eq!(parser_support_for_target_path("src/worker"), "unknown");
    }

    #[test]
    fn resolve_indexed_target_alias_prefers_indexed_sibling_extension() {
        let indexed = vec![
            "src/app.tsx".to_string(),
            "src/auth/session.ts".to_string(),
        ];
        assert_eq!(
            resolve_indexed_target_alias(&indexed, "src/app.ts"),
            Some("src/app.tsx".to_string())
        );
        assert_eq!(
            resolve_indexed_target_alias(&indexed, "src/auth/session.ts"),
            Some("src/auth/session.ts".to_string())
        );
        assert_eq!(resolve_indexed_target_alias(&indexed, "src/main.rs"), None);
    }

    #[test]
    fn index_region_status_distinguishes_full_targeted_and_unindexed() {
        assert_eq!(index_region_status(false, Some("full")), "unindexed");
        assert_eq!(index_region_status(true, Some("full")), "fully_indexed");
        assert_eq!(
            index_region_status(true, Some("targeted")),
            "targeted_partial"
        );
        assert_eq!(index_region_status(true, None), "indexed_unknown_scope");
    }

    #[test]
    fn targeted_indexing_sets_targeted_partial_region_status() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join("src").join("auth")).expect("mkdir auth");
        fs::write(
            repo.join("src").join("auth").join("session.ts"),
            "export function buildSession(){ return 1; }\n",
        )
        .expect("write source");

        let runtime = AppRuntime::bootstrap(
            repo.clone(),
            RuntimeOptions {
                start_watcher: false,
                ensure_config: true,
                bootstrap_index_policy: BootstrapIndexPolicy::Skip,
            },
        )
        .expect("bootstrap runtime");
        runtime
            .indexer()
            .lock()
            .index_paths(runtime.repo_root(), &[String::from("src/auth")])
            .expect("targeted index");

        let status = runtime.status_json();
        assert_eq!(
            status.get("index_region_status").and_then(|v| v.as_str()),
            Some("targeted_partial")
        );
        assert!(
            status
                .get("indexed_region_hints")
                .and_then(|v| v.as_array())
                .map(|items| items.iter().any(|item| item.as_str() == Some("src/auth")))
                .unwrap_or(false)
        );
        assert_eq!(
            status
                .get("supported_languages")
                .and_then(|v| v.as_array())
                .map(|items| items.iter().filter_map(|item| item.as_str()).collect::<Vec<_>>()),
            Some(vec!["python", "javascript", "typescript"])
        );
        assert_eq!(
            status
                .get("repo_supported_source_file_count")
                .and_then(|v| v.as_u64()),
            Some(1)
        );
        assert_eq!(
            status
                .get("repo_unsupported_source_file_count")
                .and_then(|v| v.as_u64()),
            Some(0)
        );
    }

    #[cfg(feature = "rust-support")]
    #[test]
    fn status_reports_compiled_but_disabled_rust_support() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join("src")).expect("mkdir src");
        fs::write(repo.join("src").join("main.rs"), "fn main() {}\n").expect("write rust");

        let runtime = AppRuntime::bootstrap(
            repo.clone(),
            RuntimeOptions {
                start_watcher: false,
                ensure_config: true,
                bootstrap_index_policy: BootstrapIndexPolicy::Skip,
            },
        )
        .expect("bootstrap runtime");
        let status = runtime.status_json();
        assert_eq!(
            status
                .get("rust_support")
                .and_then(|v| v.get("status"))
                .and_then(|v| v.as_str()),
            Some("compiled_disabled")
        );
        assert_eq!(
            status
                .get("supported_languages")
                .and_then(|v| v.as_array())
                .map(|items| items.iter().filter_map(|item| item.as_str()).collect::<Vec<_>>()),
            Some(vec!["python", "javascript", "typescript"])
        );
        assert_eq!(
            status
                .get("repo_unsupported_source_file_count")
                .and_then(|v| v.as_u64()),
            Some(1)
        );
    }

    #[cfg(feature = "rust-support")]
    #[test]
    fn status_reports_enabled_rust_support_when_configured() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join("src")).expect("mkdir src");
        fs::write(repo.join("src").join("main.rs"), "fn main() {}\n").expect("write rust");
        fs::create_dir_all(repo.join(".semantic")).expect("mkdir .semantic");
        fs::write(
            repo.join(".semantic").join("rust.toml"),
            "enabled = true\nsmall_project_mode = true\n",
        )
        .expect("write rust config");

        let runtime = AppRuntime::bootstrap(
            repo.clone(),
            RuntimeOptions {
                start_watcher: false,
                ensure_config: false,
                bootstrap_index_policy: BootstrapIndexPolicy::Skip,
            },
        )
        .expect("bootstrap runtime");
        let status = runtime.status_json();
        assert_eq!(
            status
                .get("rust_support")
                .and_then(|v| v.get("status"))
                .and_then(|v| v.as_str()),
            Some("enabled")
        );
        assert!(
            status
                .get("supported_languages")
                .and_then(|v| v.as_array())
                .map(|items| items.iter().any(|item| item.as_str() == Some("rust")))
                .unwrap_or(false)
        );
        assert_eq!(
            status
                .get("repo_supported_source_file_count")
                .and_then(|v| v.as_u64()),
            Some(1)
        );
    }
}

fn load_llm_router(repo_root: &Path) -> Option<Arc<LLMRouter>> {
    let sem = repo_root.join(".semantic");
    let providers_path = if sem.join("llm_providers.toml").exists() {
        sem.join("llm_providers.toml")
    } else {
        sem.join("llm_config.toml")
    };
    let routing_path = sem.join("llm_routing.toml");
    let metrics_path = if sem.join("llm_metrics.json").exists() {
        sem.join("llm_metrics.json")
    } else {
        sem.join("model_metrics.json")
    };
    let result = (|| -> anyhow::Result<LLMRouter> {
        let providers_toml = std::fs::read_to_string(&providers_path)?;
        let routing_toml = std::fs::read_to_string(&routing_path)?;
        let metrics_json = std::fs::read_to_string(&metrics_path)?;
        LLMRouter::from_files(&providers_toml, &routing_toml, &metrics_json)
    })();
    match result {
        Ok(router) => Some(Arc::new(router)),
        Err(err) => {
            warn!("LLM router not available (graceful degradation): {err}");
            None
        }
    }
}

fn load_knowledge_graph(repo_root: &Path) -> Arc<Mutex<KnowledgeGraph>> {
    match KnowledgeGraph::open(repo_root) {
        Ok(graph) => Arc::new(Mutex::new(graph)),
        Err(err) => {
            warn!("KnowledgeGraph init failed (graceful degradation): {err}");
            let tmp = std::env::temp_dir().join("semantic_kg_fallback");
            let fallback = KnowledgeGraph::open(&tmp)
                .expect("KnowledgeGraph fallback in temp dir must succeed");
            Arc::new(Mutex::new(fallback))
        }
    }
}
