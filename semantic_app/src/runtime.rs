use crate::config::ensure_semantic_config;
use crate::session::SemanticMiddlewareState;
use anyhow::Result;
use fs2::FileExt;
use indexer::Indexer;
use knowledge_graph::KnowledgeGraph;
use llm_router::LLMRouter;
use parking_lot::Mutex;
use retrieval::RetrievalService;
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use storage::default_paths;
use tracing::{info, warn};
use watcher::RepoWatcher;

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
    llm_router: Option<Arc<LLMRouter>>,
    knowledge_graph: Arc<Mutex<KnowledgeGraph>>,
    bootstrap_index_action: &'static str,
}

#[derive(Clone)]
pub struct AppRuntime {
    inner: Arc<AppRuntimeInner>,
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

        let llm_router = load_llm_router(&repo_root);
        let knowledge_graph = load_knowledge_graph(&repo_root);

        let runtime = Self {
            inner: Arc::new(AppRuntimeInner {
                repo_root: repo_root.clone(),
                retrieval,
                indexer,
                watcher: Mutex::new(None),
                semantic_middleware: Arc::new(Mutex::new(SemanticMiddlewareState::default())),
                workspace_state: Arc::new(Mutex::new(WorkspaceState::load(&repo_root))),
                llm_router,
                knowledge_graph,
                bootstrap_index_action,
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
        self.inner.llm_router.clone()
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

    pub fn status_json(&self) -> serde_json::Value {
        let retrieval = self.inner.retrieval.lock();
        let workspace = self.inner.workspace_state.lock();
        let indexed_file_count = retrieval.indexed_file_count();
        let indexed_path_hints = retrieval
            .with_storage(|storage| storage.list_files())
            .map(|files| summarize_indexed_path_hints(&files))
            .unwrap_or_default();
        let index_available = indexed_file_count > 0;
        let indexing_mode = "full_with_default_excludes";
        let indexing_completeness = "source_focused";
        serde_json::json!({
            "ok": true,
            "repo_root": self.inner.repo_root,
            "index_revision": retrieval.index_revision(),
            "index_available": index_available,
            "indexed_file_count": indexed_file_count,
            "indexed_path_hints": indexed_path_hints,
            "watcher_running": self.watcher_running(),
            "bootstrap_index_action": self.inner.bootstrap_index_action,
            "indexing_mode": indexing_mode,
            "indexing_completeness": indexing_completeness,
            "workspace_mode_enabled": workspace.workspace_mode_enabled,
            "workspace_roots": workspace.workspace_roots,
            "llm_router_configured": self.inner.llm_router.is_some(),
            "performance": retrieval.get_performance_stats(),
        })
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

fn summarize_indexed_path_hints(files: &[String]) -> Vec<String> {
    let mut counts = std::collections::BTreeMap::<String, usize>::new();
    for file in files {
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

#[cfg(test)]
mod tests {
    use super::{AppRuntime, BootstrapIndexPolicy, RuntimeOptions};
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
