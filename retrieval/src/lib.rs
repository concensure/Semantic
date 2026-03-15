use anyhow::{anyhow, Result};
use budgeter::{select_with_budget, ContextBudget, ContextItem};
use engine::{
    LogicNodeRecord, Operation, RetrievalRequest, RetrievalResponse, SymbolRecord, SymbolType,
};
use planner::Planner;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

pub struct RetrievalService {
    repo_root: PathBuf,
    storage: storage::Storage,
    symbol_neighborhood_cache: Mutex<HashMap<String, CachedContext>>,
    prompt_fragment_cache: Mutex<HashMap<String, CachedPrompt>>,
    perf_stats: Mutex<PerfStats>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedContext {
    cached_at_epoch_s: u64,
    #[serde(default)]
    source_revision: u64,
    value: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedPrompt {
    cached_at_epoch_s: u64,
    source_revision: u64,
    prompt: String,
}

#[derive(Debug, Clone)]
struct TestRunResult {
    passed: bool,
    command: String,
    exit_code: Option<i32>,
    stdout: String,
    stderr: String,
}

#[derive(Debug, Default)]
struct PerfStats {
    cache_hits: usize,
    cache_misses: usize,
    cache_evictions: usize,
    symbol_cache_hits: usize,
    symbol_cache_misses: usize,
    symbol_cache_evictions: usize,
    prompt_cache_hits: usize,
    prompt_cache_misses: usize,
    prompt_cache_evictions: usize,
    op: HashMap<String, OpPerf>,
}

#[derive(Debug, Default)]
struct OpPerf {
    calls: usize,
    total_ms: u128,
    max_ms: u128,
    samples_ms: VecDeque<u128>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoTask {
    pub id: String,
    pub title: String,
    pub completed: bool,
}

#[derive(Debug, Clone)]
struct ABDevTask {
    id: &'static str,
    title: &'static str,
    feature_request: &'static str,
    semantic_query: &'static str,
    target_symbol: &'static str,
    requires_code_change: bool,
    semantic_features: Vec<&'static str>,
    context_ranges: Vec<ContextRange>,
    expected_terms: Vec<&'static str>,
}

#[derive(Debug, Clone)]
struct ContextRange {
    file: &'static str,
    start: usize,
    end: usize,
}

#[derive(Debug, Clone, Copy)]
struct ContextTuning {
    ref_limit: usize,
    raw_max_chars: usize,
    escalation_hits_threshold: usize,
    guardrail_ratio: f32,
}

#[derive(Debug, Clone, Copy)]
struct RetrievalPolicy {
    high_fanout_threshold: usize,
    low_fanout_threshold: usize,
    dense_logic_threshold: usize,
    sparse_logic_threshold: usize,
    max_dependency_radius: usize,
    max_logic_radius: usize,
    plan_token_cap: usize,
    lookup_token_cap: usize,
    edit_token_cap: usize,
    small_repo_file_threshold: usize,
    anti_bloat_small_task: bool,
    p95_latency_alert_ms: u128,
    p99_latency_alert_ms: u128,
    min_cache_hit_rate_pct: f64,
    prompt_overrun_alert_pct: f64,
    step_regression_alert_pct: f64,
}

impl Default for RetrievalPolicy {
    fn default() -> Self {
        Self {
            high_fanout_threshold: 12,
            low_fanout_threshold: 4,
            dense_logic_threshold: 24,
            sparse_logic_threshold: 8,
            max_dependency_radius: 3,
            max_logic_radius: 3,
            plan_token_cap: 3200,
            lookup_token_cap: 1800,
            edit_token_cap: 4000,
            small_repo_file_threshold: 50,
            anti_bloat_small_task: true,
            p95_latency_alert_ms: 60,
            p99_latency_alert_ms: 120,
            min_cache_hit_rate_pct: 35.0,
            prompt_overrun_alert_pct: 25.0,
            step_regression_alert_pct: 10.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MappingMode {
    FootprintFirst,
    LegacyFull,
}

impl MappingMode {
    fn parse(raw: Option<&str>) -> Self {
        match raw.unwrap_or("footprint_first").trim().to_lowercase().as_str() {
            "legacy_full" => Self::LegacyFull,
            _ => Self::FootprintFirst,
        }
    }
}

impl RetrievalService {
    pub fn new(repo_root: PathBuf, storage: storage::Storage) -> Self {
        let service = Self {
            repo_root,
            storage,
            symbol_neighborhood_cache: Mutex::new(HashMap::new()),
            prompt_fragment_cache: Mutex::new(HashMap::new()),
            perf_stats: Mutex::new(PerfStats::default()),
        };
        service.migrate_legacy_planned_context_cache();
        service
    }

    pub fn repo_root(&self) -> &Path {
        &self.repo_root
    }

    pub fn index_revision(&self) -> u64 {
        current_index_revision(&self.repo_root)
    }

    pub fn load_env(&self) {
        load_env_from_file(&self.repo_root);
    }

    fn migrate_legacy_planned_context_cache(&self) {
        let path = self
            .repo_root
            .join(".semantic")
            .join("planned_context_cache.json");
        let Ok(raw) = fs::read_to_string(&path) else {
            return;
        };
        let Ok(entries) = serde_json::from_str::<HashMap<String, CachedContext>>(&raw) else {
            return;
        };
        for (cache_key, entry) in entries {
            let value_json = serde_json::to_string(&entry.value).ok();
            let _ = self.storage.upsert_retrieval_cache_entry(&storage::RetrievalCacheEntry {
                cache_key,
                cache_kind: "planned_context".to_string(),
                value_json,
                prompt_text: None,
                cached_at_epoch_s: entry.cached_at_epoch_s,
                source_revision: entry.source_revision,
            });
        }
        let _ = fs::remove_file(path);
    }

    fn todo_tasks_path(&self) -> PathBuf {
        self.repo_root.join(".semantic").join("todo_tasks.json")
    }

    fn read_todo_tasks(&self) -> Result<Vec<TodoTask>> {
        let path = self.todo_tasks_path();
        if !path.exists() {
            return Ok(Vec::new());
        }
        let raw = fs::read_to_string(path)?;
        let tasks = serde_json::from_str::<Vec<TodoTask>>(&raw).unwrap_or_default();
        Ok(tasks)
    }

    fn write_todo_tasks(&self, tasks: &[TodoTask]) -> Result<()> {
        let path = self.todo_tasks_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let payload = serde_json::to_string_pretty(tasks)?;
        fs::write(path, payload)?;
        Ok(())
    }

    pub fn handle(&self, request: RetrievalRequest) -> Result<RetrievalResponse> {
        self.handle_with_options(request, None, None)
    }

    pub fn handle_with_options(
        &self,
        request: RetrievalRequest,
        single_file_fast_path: Option<bool>,
        include_raw_code_override: Option<bool>,
    ) -> Result<RetrievalResponse> {
        self.handle_with_options_ext(
            request,
            single_file_fast_path,
            include_raw_code_override,
            None,
            None,
        )
    }

    pub fn handle_with_options_ext(
        &self,
        request: RetrievalRequest,
        single_file_fast_path: Option<bool>,
        include_raw_code_override: Option<bool>,
        mapping_mode: Option<&str>,
        max_footprint_items: Option<usize>,
    ) -> Result<RetrievalResponse> {
        let op_name = format!("{:?}", request.operation).to_lowercase();
        let started = Instant::now();
        let operation = request.operation.clone();
        let result = match operation {
            Operation::GetRepoMap => self.get_repo_map()?,
            Operation::GetFileOutline => {
                let file = request.file.ok_or_else(|| anyhow!("file is required"))?;
                self.get_file_outline(&file)?
            }
            Operation::SearchSymbol => {
                let name = request.name.ok_or_else(|| anyhow!("name is required"))?;
                self.search_symbol(&name, request.limit.unwrap_or(20))?
            }
            Operation::GetFunction => {
                let name = request.name.ok_or_else(|| anyhow!("name is required"))?;
                self.get_symbol_span(&name, SymbolType::Function)?
            }
            Operation::GetClass => {
                let name = request.name.ok_or_else(|| anyhow!("name is required"))?;
                self.get_symbol_span(&name, SymbolType::Class)?
            }
            Operation::GetDependencies => {
                let name = request.name.ok_or_else(|| anyhow!("name is required"))?;
                self.get_dependencies(&name)?
            }
            Operation::GetCodeSpan => {
                let file = request.file.ok_or_else(|| anyhow!("file is required"))?;
                let start = request
                    .start_line
                    .ok_or_else(|| anyhow!("start_line is required"))?;
                let end = request
                    .end_line
                    .ok_or_else(|| anyhow!("end_line is required"))?;
                self.get_code_span(&file, start, end)?
            }
            Operation::GetLogicNodes => {
                let name = request.name.ok_or_else(|| anyhow!("name is required"))?;
                self.get_logic_nodes(&name)?
            }
            Operation::GetLogicNeighborhood => {
                let node_id = request
                    .node_id
                    .ok_or_else(|| anyhow!("node_id is required"))?;
                self.get_logic_neighborhood(node_id, request.radius.unwrap_or(1))?
            }
            Operation::GetLogicSpan => {
                let node_id = request
                    .node_id
                    .ok_or_else(|| anyhow!("node_id is required"))?;
                self.get_logic_span(node_id)?
            }
            Operation::GetDependencyNeighborhood => {
                let name = request.name.ok_or_else(|| anyhow!("name is required"))?;
                let radius = request
                    .radius
                    .ok_or_else(|| anyhow!("radius is required"))?;
                self.get_dependency_neighborhood(&name, radius)?
            }
            Operation::GetSymbolNeighborhood => {
                let name = request.name.ok_or_else(|| anyhow!("name is required"))?;
                let radius = request
                    .radius
                    .ok_or_else(|| anyhow!("radius is required"))?;
                self.get_symbol_neighborhood(&name, radius)?
            }
            Operation::GetReasoningContext => {
                let name = request.name.ok_or_else(|| anyhow!("name is required"))?;
                let logic_radius = request
                    .logic_radius
                    .ok_or_else(|| anyhow!("logic_radius is required"))?;
                let dependency_radius = request
                    .dependency_radius
                    .ok_or_else(|| anyhow!("dependency_radius is required"))?;
                self.get_reasoning_context(&name, logic_radius, dependency_radius)?
            }
            Operation::GetPlannedContext => {
                let query = request.query.ok_or_else(|| anyhow!("query is required"))?;
                let requested_max_tokens = request
                    .max_tokens
                    .ok_or_else(|| anyhow!("max_tokens is required"))?;
                let max_tokens =
                    clamp_tokens_for_operation(&self.repo_root, "plan", requested_max_tokens);
                self.get_planned_context(
                    &query,
                    max_tokens,
                    single_file_fast_path.unwrap_or(false),
                    include_raw_code_override,
                    None,
                    mapping_mode,
                    max_footprint_items,
                )?
            }
            Operation::GetRepoMapHierarchy => self.get_repo_map_hierarchy()?,
            Operation::GetModuleDependencies => self.get_module_dependencies()?,
            Operation::SearchSemanticSymbol => {
                let query = request
                    .query
                    .or(request.name)
                    .ok_or_else(|| anyhow!("query or name is required"))?;
                self.search_semantic_symbol(&query, request.limit.unwrap_or(20))?
            }
            Operation::GetWorkspaceReasoningContext => {
                let query = request.query.ok_or_else(|| anyhow!("query is required"))?;
                let requested_max_tokens = request
                    .max_tokens
                    .ok_or_else(|| anyhow!("max_tokens is required"))?;
                let max_tokens =
                    clamp_tokens_for_operation(&self.repo_root, "lookup", requested_max_tokens);
                self.get_workspace_reasoning_context(
                    &query,
                    max_tokens,
                    request.workspace_scope.unwrap_or_default(),
                    single_file_fast_path.unwrap_or(false),
                    include_raw_code_override,
                    mapping_mode,
                    max_footprint_items,
                )?
            }
            Operation::PlanSafeEdit => {
                let symbol = request
                    .name
                    .or(request.query)
                    .ok_or_else(|| anyhow!("name or query(symbol) is required"))?;
                let edit_description = request
                    .edit_description
                    .ok_or_else(|| anyhow!("edit_description is required"))?;
                let max_tokens = clamp_tokens_for_operation(
                    &self.repo_root,
                    "edit",
                    request.max_tokens.unwrap_or(4000),
                );
                self.plan_safe_edit(
                    &symbol,
                    &edit_description,
                    max_tokens,
                    request.patch_mode,
                    request.run_tests.unwrap_or(false),
                )?
            }
            // These operations are intercepted by the API layer before reaching the retrieval
            // service. They are listed here only to satisfy exhaustive pattern matching.
            Operation::GetControlFlowHints => {
                let symbol = request.name.or(request.query).unwrap_or_default();
                self.get_control_flow_hints(&symbol)?
            }
            Operation::GetDataFlowHints => {
                let symbol = request.name.or(request.query).unwrap_or_default();
                self.get_data_flow_hints(&symbol)?
            }
            Operation::GetControlFlowSlice => {
                let symbol = request.name.or(request.query).unwrap_or_default();
                self.get_control_flow_slice(&symbol)?
            }
            Operation::GetDataFlowSlice => {
                let symbol = request.name.or(request.query).unwrap_or_default();
                self.get_data_flow_slice(&symbol)?
            }
            Operation::GetLogicClusters => {
                let symbol = request.name.or(request.query).unwrap_or_default();
                self.get_logic_clusters(&symbol)?
            }
            Operation::GetHybridRankedContext => {
                let query = request.query.unwrap_or_default();
                let max_tokens =
                    clamp_tokens_for_operation(&self.repo_root, "lookup", request.max_tokens.unwrap_or(1400));
                self.get_hybrid_ranked_context(
                    &query,
                    max_tokens,
                    single_file_fast_path.unwrap_or(true),
                )?
            }
            Operation::GetDebugGraph => self.get_debug_graph()?,
            Operation::GetPipelineGraph => self.get_pipeline_graph()?,
            Operation::GetRootCauseCandidates => self.get_root_cause_candidates()?,
            Operation::GetTestGaps => self.get_test_gaps()?,
            Operation::GetDeploymentHistory => self.get_deployment_history()?,
            Operation::GetPerformanceStats => self.get_performance_stats(),
        };
        self.record_operation_perf(&op_name, started.elapsed().as_millis());

        Ok(RetrievalResponse { operation, result })
    }

    pub fn get_performance_stats(&self) -> serde_json::Value {
        let perf = self.perf_stats.lock().expect("perf lock");
        let op_stats: Vec<serde_json::Value> = perf
            .op
            .iter()
            .map(|(name, op)| {
                let avg_ms = if op.calls == 0 {
                    0.0
                } else {
                    op.total_ms as f64 / op.calls as f64
                };
                let mut sorted = op.samples_ms.iter().copied().collect::<Vec<_>>();
                sorted.sort_unstable();
                let p95_ms = if sorted.is_empty() {
                    0
                } else {
                    let idx = ((sorted.len() as f64) * 0.95).floor() as usize;
                    sorted[idx.min(sorted.len() - 1)]
                };
                let p99_ms = if sorted.is_empty() {
                    0
                } else {
                    let idx = ((sorted.len() as f64) * 0.99).floor() as usize;
                    sorted[idx.min(sorted.len() - 1)]
                };
                json!({
                    "operation": name,
                    "calls": op.calls,
                    "avg_ms": avg_ms,
                    "p95_ms": p95_ms,
                    "p99_ms": p99_ms,
                    "max_ms": op.max_ms,
                })
            })
            .collect();

        let cache_hits = perf.cache_hits;
        let cache_misses = perf.cache_misses;
        let cache_evictions = perf.cache_evictions;
        let symbol_cache_hits = perf.symbol_cache_hits;
        let symbol_cache_misses = perf.symbol_cache_misses;
        let symbol_cache_evictions = perf.symbol_cache_evictions;
        let prompt_cache_hits = perf.prompt_cache_hits;
        let prompt_cache_misses = perf.prompt_cache_misses;
        let prompt_cache_evictions = perf.prompt_cache_evictions;
        drop(perf);
        let policy = load_retrieval_policy(&self.repo_root);
        let cache_entries = self
            .storage
            .count_retrieval_cache_entries("planned_context")
            .unwrap_or_default();
        let symbol_cache_entries = self
            .symbol_neighborhood_cache
            .lock()
            .expect("symbol cache lock")
            .len();
        let prompt_cache_entries = self
            .prompt_fragment_cache
            .lock()
            .expect("prompt cache lock")
            .len();
        let cache_hit_rate = ratio_pct(cache_hits, cache_hits + cache_misses);

        json!({
            "targets": {
                "index_throughput_goal": "10k files under 20s (target)",
                "symbol_lookup_latency_goal": "<10ms (target)",
                "planned_context_p95_goal": "<60ms (target)"
            },
            "indexing": load_index_performance_stats(&self.repo_root),
            "cache": {
                "entries": cache_entries,
                "hits": cache_hits,
                "misses": cache_misses,
                "evictions": cache_evictions,
                "hit_rate_pct": cache_hit_rate,
            },
            "symbol_neighborhood_cache": {
                "entries": symbol_cache_entries,
                "hits": symbol_cache_hits,
                "misses": symbol_cache_misses,
                "evictions": symbol_cache_evictions,
            },
            "prompt_fragment_cache": {
                "entries": prompt_cache_entries,
                "hits": prompt_cache_hits,
                "misses": prompt_cache_misses,
                "evictions": prompt_cache_evictions,
            },
            "operations": op_stats,
            "alerts": build_observability_alerts(&op_stats, cache_hit_rate, policy)
        })
    }

    fn record_operation_perf(&self, op_name: &str, elapsed_ms: u128) {
        let mut perf = self.perf_stats.lock().expect("perf lock");
        let entry = perf.op.entry(op_name.to_string()).or_default();
        entry.calls += 1;
        entry.total_ms += elapsed_ms;
        entry.max_ms = entry.max_ms.max(elapsed_ms);
        entry.samples_ms.push_back(elapsed_ms);
        if entry.samples_ms.len() > 256 {
            entry.samples_ms.pop_front();
        }
    }

    pub fn get_patch_memory(
        &self,
        repository: Option<String>,
        symbol: Option<String>,
        model: Option<String>,
        time_range: Option<(u64, u64)>,
    ) -> Result<serde_json::Value> {
        let memory = patch_memory::PatchMemory::open(&self.repo_root)?;
        let filter = patch_memory::PatchQuery {
            repository,
            symbol,
            model,
            time_range,
        };
        let records = memory.list_records(&filter)?;
        let graph = memory.export_history_graph()?;
        Ok(json!({
            "records": records,
            "graph": graph,
        }))
    }

    pub fn get_patch_stats(
        &self,
        repository: Option<String>,
        symbol: Option<String>,
        model: Option<String>,
        time_range: Option<(u64, u64)>,
    ) -> Result<serde_json::Value> {
        let memory = patch_memory::PatchMemory::open(&self.repo_root)?;
        let filter = patch_memory::PatchQuery {
            repository,
            symbol,
            model,
            time_range,
        };
        let stats = memory.stats(&filter)?;
        Ok(json!({ "stats": stats }))
    }

    pub fn get_model_performance(
        &self,
        repository: Option<String>,
        symbol: Option<String>,
        model: Option<String>,
        time_range: Option<(u64, u64)>,
    ) -> Result<serde_json::Value> {
        let memory = patch_memory::PatchMemory::open(&self.repo_root)?;
        let filter = patch_memory::PatchQuery {
            repository,
            symbol,
            model,
            time_range,
        };
        let performance = memory.model_performance(&filter)?;
        Ok(json!({ "model_performance": performance }))
    }

    pub fn get_evolution_issues(&self, repository: &str) -> Result<serde_json::Value> {
        let code_issues =
            code_health::CodeHealthAnalyzer::analyze(&self.repo_root, repository, &self.storage)?;
        let architecture_issues =
            architecture_analysis::ArchitectureAnalyzer::analyze(&self.storage)?;
        Ok(json!({
            "repository": repository,
            "code_health_issues": code_issues,
            "architecture_issues": architecture_issues,
        }))
    }

    pub fn get_evolution_plans(&self, repository: &str) -> Result<serde_json::Value> {
        let code_issues =
            code_health::CodeHealthAnalyzer::analyze(&self.repo_root, repository, &self.storage)?;
        let architecture_issues =
            architecture_analysis::ArchitectureAnalyzer::analyze(&self.storage)?;
        let plans = improvement_planner::ImprovementPlanner::from_issues(
            &code_issues,
            &architecture_issues,
        );
        Ok(json!({
            "repository": repository,
            "plans": plans,
        }))
    }

    pub fn generate_evolution_plan(
        &self,
        repository: &str,
        dry_run: bool,
    ) -> Result<serde_json::Value> {
        let code_issues =
            code_health::CodeHealthAnalyzer::analyze(&self.repo_root, repository, &self.storage)?;
        let architecture_issues =
            architecture_analysis::ArchitectureAnalyzer::analyze(&self.storage)?;
        let plans = improvement_planner::ImprovementPlanner::from_issues(
            &code_issues,
            &architecture_issues,
        );
        let graph = evolution_graph::EvolutionGraphBuilder::from_plans(&plans);
        let simulation = evolution_graph::EvolutionGraphBuilder::simulate(&graph)?;
        let risk = estimate_evolution_risk(&self.repo_root, &self.storage, plans.len())?;

        let kg = knowledge_graph::KnowledgeGraph::open(&self.repo_root)?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or_default();
        kg.append(&knowledge_graph::KnowledgeEntry {
            timestamp: now,
            category: "evolution_plan".to_string(),
            repository: repository.to_string(),
            title: format!("Generated evolution plan with {} improvements", plans.len()),
            details: format!("dry_run={dry_run}, estimated_nodes={}", graph.nodes.len()),
        })?;

        Ok(json!({
            "repository": repository,
            "issues": {
                "code_health": code_issues,
                "architecture": architecture_issues,
            },
            "plans": plans,
            "evolution_graph": graph,
            "risk": risk,
            "estimated_changes": {
                "plan_count": plans.len(),
                "node_count": simulation.estimated_node_count,
                "patch_count": simulation.estimated_patch_count,
            },
            "simulation": if dry_run { Some(simulation) } else { None },
            "dry_run": dry_run,
        }))
    }

    pub fn get_organization_graph(&self) -> Result<serde_json::Value> {
        let graph = org_graph::OrganizationGraphBuilder::build(&self.storage)?;
        let contracts =
            api_contract_graph::APIContractGraphBuilder::scan(&self.repo_root, &self.storage)?;
        let deps = dependency_intelligence::DependencyIntelligence::analyze(&self.storage, &graph)?;
        Ok(json!({
            "organization_graph": graph,
            "api_contracts": contracts,
            "dependency_intelligence": deps,
        }))
    }

    pub fn get_service_graph(&self) -> Result<serde_json::Value> {
        let graph = service_graph::ServiceGraphBuilder::build(&self.storage)?;
        Ok(json!({ "service_graph": graph }))
    }

    pub fn plan_org_refactor(&self, origin_repo: &str) -> Result<serde_json::Value> {
        let org = org_graph::OrganizationGraphBuilder::build(&self.storage)?;
        let deps = dependency_intelligence::DependencyIntelligence::analyze(&self.storage, &org)?;
        let propagation = change_propagation::ChangePropagationEngine::predict(origin_repo, &deps);
        let plan = organization_planner::OrganizationPlanner::plan(origin_repo, &propagation);
        let multi_repo_graph =
            organization_planner::OrganizationPlanner::build_multi_repo_refactor_graph(&plan);
        let execution = organization_planner::OrganizationPlanner::execute_distributed(
            &self.repo_root,
            &plan,
            &propagation,
        )?;
        let telemetry = organization_planner::OrganizationPlanner::read_telemetry(&self.repo_root)?;
        Ok(json!({
            "plan": plan,
            "propagation": propagation,
            "multi_repo_refactor_graph": multi_repo_graph,
            "execution": execution,
            "telemetry": telemetry,
        }))
    }

    pub fn get_org_refactor_status(&self) -> Result<serde_json::Value> {
        let status = organization_planner::OrganizationPlanner::read_status(&self.repo_root)?;
        let telemetry = organization_planner::OrganizationPlanner::read_telemetry(&self.repo_root)?;
        Ok(json!({
            "org_refactor_status": status,
            "telemetry": telemetry,
        }))
    }

    pub fn debug_failure(&self, event: debug_graph::FailureEvent) -> Result<serde_json::Value> {
        let analysis =
            debug_graph::DebugGraphEngine::analyze_failure(&self.repo_root, &self.storage, event)?;
        Ok(json!({
            "debug_graph": analysis.debug_graph,
            "root_cause_candidates": analysis.candidates,
            "patch_suggestion": analysis.patch_suggestion,
            "failure_event": analysis.last_failure,
        }))
    }

    pub fn get_debug_graph(&self) -> Result<serde_json::Value> {
        let state = debug_graph::DebugGraphEngine::read_state(&self.repo_root)?;
        Ok(json!({
            "failure_event": state.last_failure,
            "debug_graph": state.debug_graph,
        }))
    }

    pub fn get_root_cause_candidates(&self) -> Result<serde_json::Value> {
        let state = debug_graph::DebugGraphEngine::read_state(&self.repo_root)?;
        Ok(json!({
            "root_cause_candidates": state.candidates,
            "patch_suggestion": state.patch_suggestion,
        }))
    }

    pub fn get_test_gaps(&self) -> Result<serde_json::Value> {
        let gaps = test_coverage::TestCoverageAnalyzer::analyze(&self.storage)?;
        Ok(json!({ "test_gaps": gaps }))
    }

    pub fn generate_tests(
        &self,
        target_symbol: &str,
        framework: &str,
    ) -> Result<serde_json::Value> {
        let symbol = self
            .storage
            .get_symbol_any(target_symbol)?
            .ok_or_else(|| anyhow!("symbol not found: {target_symbol}"))?;
        let code = read_span(
            &self.repo_root,
            &symbol.file,
            symbol.start_line,
            symbol.end_line,
        )?;
        let plan = test_planner::TestPlanner::build_plan(target_symbol, framework);
        let generated = test_planner::TestPlanner::generate_tests(&plan, framework, &code);
        Ok(json!({
            "test_plan": plan,
            "generated_tests": generated,
        }))
    }

    pub fn apply_tests(
        &self,
        repository: &str,
        target_symbol: &str,
        framework: &str,
    ) -> Result<serde_json::Value> {
        let symbol = self
            .storage
            .get_symbol_any(target_symbol)?
            .ok_or_else(|| anyhow!("symbol not found: {target_symbol}"))?;
        let code = read_span(
            &self.repo_root,
            &symbol.file,
            symbol.start_line,
            symbol.end_line,
        )?;
        let plan = test_planner::TestPlanner::build_plan(target_symbol, framework);
        let generated = test_planner::TestPlanner::generate_tests(&plan, framework, &code);
        let applied =
            test_planner::TestPlanner::apply_tests(&self.repo_root, repository, &plan, &generated)?;
        Ok(json!({
            "test_plan": plan,
            "generated_tests": generated,
            "apply_result": applied,
        }))
    }

    pub fn get_pipeline_graph(&self) -> Result<serde_json::Value> {
        let graph = pipeline_graph::PipelineIntelligence::default_graph();
        Ok(json!({ "pipeline_graph": graph }))
    }

    pub fn analyze_pipeline(
        &self,
        request: pipeline_graph::PipelineAnalysisRequest,
    ) -> Result<serde_json::Value> {
        let result = pipeline_graph::PipelineIntelligence::analyze(&self.repo_root, &request)?;
        Ok(json!({
            "pipeline_request": request,
            "analysis": result,
        }))
    }

    pub fn get_deployment_history(&self) -> Result<serde_json::Value> {
        let deployments = pipeline_graph::PipelineIntelligence::list_deployments(&self.repo_root)?;
        Ok(json!({ "deployment_history": deployments }))
    }

    pub fn seed_todo_tasks(&self, tasks: Vec<TodoTask>) -> Result<serde_json::Value> {
        self.write_todo_tasks(&tasks)?;
        Ok(json!({
            "saved": tasks.len(),
            "tasks": tasks,
        }))
    }

    pub fn get_todo_tasks(&self) -> Result<serde_json::Value> {
        let tasks = self.read_todo_tasks()?;
        Ok(json!({
            "count": tasks.len(),
            "tasks": tasks,
        }))
    }

    pub fn run_ab_test_dev(
        &self,
        feature_request: Option<&str>,
        provider: Option<String>,
        max_context_tokens: Option<usize>,
        single_file_fast_path: bool,
        autoroute_first: bool,
        scenario: Option<&str>,
    ) -> Result<serde_json::Value> {
        load_env_from_file(&self.repo_root);
        ensure_todo_ab_project(&self.repo_root)?;
        let scenario_name = scenario.unwrap_or("core");
        let tasks = if scenario_name.eq_ignore_ascii_case("extended") {
            build_todo_dev_suite_extended()
        } else {
            build_todo_dev_suite()
        };
        let requested_feature = feature_request.unwrap_or("todo app end-to-end suite");
        let policy = load_retrieval_policy(&self.repo_root);
        let max_tokens = clamp_tokens_for_operation(
            &self.repo_root,
            "plan",
            max_context_tokens.unwrap_or(1800),
        );
        let task_count = tasks.len();

        let routing_cfg =
            std::fs::read_to_string(self.repo_root.join(".semantic").join("llm_routing.toml"))
                .unwrap_or_default();
        let providers_cfg =
            std::fs::read_to_string(self.repo_root.join(".semantic").join("llm_config.toml"))
                .unwrap_or_default();
        let metrics_json =
            std::fs::read_to_string(self.repo_root.join(".semantic").join("model_metrics.json"))
                .unwrap_or_else(|_| "{}".to_string());
        let router =
            llm_router::LLMRouter::from_files(&providers_cfg, &routing_cfg, &metrics_json)?;
        let route = provider
            .clone()
            .map(|p| llm_router::RouteDecision {
                provider: p,
                endpoint: String::new(),
            })
            .or_else(|| router.route(llm_router::LLMTask::InteractiveChat));

        let provider_settings = parse_provider_settings(&providers_cfg);
        let selected_provider = provider
            .or_else(|| {
                if std::env::var("OPENAI_API_KEY")
                    .map(|v| !v.trim().is_empty())
                    .unwrap_or(false)
                {
                    Some("openai".to_string())
                } else {
                    None
                }
            })
            .or_else(|| route.as_ref().map(|r| r.provider.clone()))
            .unwrap_or_else(|| "ollama".to_string());
        let selected_model = provider_settings
            .get(&selected_provider)
            .map(|s| s.model.clone())
            .unwrap_or_else(|| "unknown".to_string());
        write_ab_test_suite_manifest(&self.repo_root, &tasks)?;

        let mut total_without = 0usize;
        let mut total_with = 0usize;
        let mut total_success_without = 0usize;
        let mut total_success_with = 0usize;
        let mut semantic_exec_success = 0usize;
        let mut validation_success = 0usize;
        let mut tests_success = 0usize;
        let mut total_steps_without = 0usize;
        let mut total_steps_with = 0usize;
        let mut step_success_without = 0usize;
        let mut step_success_with = 0usize;
        let mut task_results = Vec::new();
        let mut context_cache: HashMap<String, serde_json::Value> = HashMap::new();
        let mut context_cache_hits = 0usize;
        let mut target_match_count = 0usize;
        let mut empty_ref_tasks = 0usize;
        let mut heavy_first_tasks = 0usize;
        let mut semantic_prompt_over_control_count = 0usize;
        let mut escalation_attempts = 0usize;
        let mut escalation_guardrail_skips = 0usize;
        let mut runtime_trim_applied = 0usize;

        for task in tasks {
            let semantic_exec = self
                .plan_safe_edit(
                    task.target_symbol,
                    task.feature_request,
                    max_tokens,
                    Some(engine::PatchApplicationMode::PreviewOnly),
                    true,
                )
                .ok();
            let validation_passed = semantic_exec
                .as_ref()
                .and_then(|v| v.get("validation_result"))
                .and_then(|v| v.get("passed"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let tests_passed = semantic_exec
                .as_ref()
                .and_then(|v| v.get("test_result"))
                .and_then(|v| v.get("passed"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if semantic_exec.is_some() && validation_passed {
                semantic_exec_success += 1;
            }
            if validation_passed {
                validation_success += 1;
            }
            if tests_passed {
                tests_success += 1;
            }

            let cache_key = format!(
                "{}::{}::autoroute={}::target={}",
                task.semantic_query, max_tokens, autoroute_first, task.target_symbol
            );
            let semantic_context = if let Some(cached) = context_cache.get(&cache_key) {
                context_cache_hits += 1;
                cached.clone()
            } else {
                let fresh = if autoroute_first {
                    self.autoroute_context_for_ab(
                        task.semantic_query,
                        max_tokens,
                        single_file_fast_path,
                        Some(task.target_symbol),
                    )
                    .unwrap_or_else(|_| json!({ "context": [] }))
                } else {
                    self.get_planned_context(
                        task.semantic_query,
                        max_tokens,
                        single_file_fast_path,
                        Some(false),
                        Some(task.target_symbol),
                        Some("footprint_first"),
                        Some(120),
                    )
                    .unwrap_or_else(|_| json!({ "context": [] }))
                };
                context_cache.insert(cache_key, fresh.clone());
                fresh
            };
            let impacted_file_count = semantic_exec
                .as_ref()
                .and_then(|v| v.get("edit_plan"))
                .and_then(|v| v.get("required_context"))
                .and_then(|v| v.as_array())
                .map(|v| {
                    let mut uniq = std::collections::HashSet::new();
                    for item in v {
                        if let Some(path) = item.get("file_path").and_then(|x| x.as_str()) {
                            uniq.insert(path.to_string());
                        }
                    }
                    uniq.len()
                })
                .unwrap_or_default();
            let tuning = context_tuning_for_task(&task, impacted_file_count, max_tokens);
            let refs = build_structured_context_refs(&semantic_context, tuning.ref_limit);
            let delta_refs = refs.clone();
            if delta_refs.is_empty() {
                empty_ref_tasks += 1;
            }
            let planned_target_symbol = semantic_context
                .get("symbol")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let confidence_score = semantic_context
                .get("confidence_score")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.55) as f32;
            let confidence_band = semantic_context
                .get("confidence_band")
                .and_then(|v| v.as_str())
                .unwrap_or_else(|| confidence_band(confidence_score))
                .to_string();
            let target_match = planned_target_symbol.eq_ignore_ascii_case(task.target_symbol);
            if target_match {
                target_match_count += 1;
            }
            let raw_code_context = if task.requires_code_change {
                build_context_payload_from_edit_plan_or_fallback(
                    &self.repo_root,
                    semantic_exec.as_ref(),
                    &task.context_ranges,
                    tuning.raw_max_chars,
                )
            } else {
                String::new()
            };

            let base_prompt = format!(
                "You are editing a TypeScript todo app. Task ID: {}.\nTask: {}\nReturn: (1) exact files to edit, (2) patch outline, (3) test plan.",
                task.id, task.feature_request
            );
            let control_attachment_context = if task.requires_code_change {
                build_exact_context_payload(
                    &self.repo_root,
                    &task.context_ranges,
                    if single_file_fast_path { 1100 } else { 1800 },
                )
            } else {
                String::new()
            };
            let without_prompt = if control_attachment_context.is_empty() {
                base_prompt.clone()
            } else {
                format!(
                    "Developer-attached relevant code snippets (manual control arm):\n{}\n\nTask:\n{}",
                    control_attachment_context, base_prompt
                )
            };
            let minimal_raw_seed = semantic_context
                .get("minimal_raw_seed")
                .and_then(|v| v.get("code_span"))
                .and_then(|v| v.get("code"))
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let minimal_seed_file = semantic_context
                .get("minimal_raw_seed")
                .and_then(|v| v.get("file"))
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let target_symbol_file = self
                .storage
                .get_symbol_any(task.target_symbol)
                .ok()
                .flatten()
                .map(|s| s.file)
                .unwrap_or_default();
            let seed_target_aligned =
                !minimal_seed_file.is_empty() && minimal_seed_file == target_symbol_file;
            let mut with_prompt_light =
                self.build_light_prompt_cached(&base_prompt, &delta_refs, delta_refs.len());
            if autoroute_first
                && task.requires_code_change
                && !minimal_raw_seed.is_empty()
                && !delta_refs.is_empty()
                && seed_target_aligned
                && confidence_score < 0.75
            {
                with_prompt_light = format!(
                    "Structured context refs (delta from previous step):\n{}\n\nMinimal raw seed (autoroute):\n{}\n\nTask:\n{}",
                    serde_json::to_string_pretty(&delta_refs).unwrap_or_default(),
                    minimal_raw_seed,
                    base_prompt
                );
            }

            let result_a_attempt = call_live_llm_with_diagnostics(
                &selected_provider,
                provider_settings.get(&selected_provider),
                route.as_ref().map(|r| r.endpoint.as_str()),
                &without_prompt,
                700,
            );
            let result_a = result_a_attempt.as_ref().ok().cloned();
            let result_a_error = result_a_attempt.err();
            let (a_tokens, a_output) = result_a
                .as_ref()
                .map(|r| (r.total_tokens, r.text.clone()))
                .unwrap_or((estimate_tokens(&without_prompt), String::new()));

            let mut refs_used = delta_refs.len();
            while refs_used > 1
                && (estimate_tokens(&with_prompt_light) as f32)
                    > (a_tokens.max(1) as f32 * tuning.guardrail_ratio)
            {
                refs_used -= 1;
                with_prompt_light =
                    self.build_light_prompt_cached(&base_prompt, &delta_refs, refs_used);
            }
            while refs_used > 1
                && confidence_score < 0.75
                && estimate_tokens(&with_prompt_light) > a_tokens.max(1)
            {
                refs_used -= 1;
                with_prompt_light =
                    self.build_light_prompt_cached(&base_prompt, &delta_refs, refs_used);
                runtime_trim_applied += 1;
            }
            let with_prompt_heavy = if task.requires_code_change && !raw_code_context.is_empty() {
                format!(
                    "Structured context refs (delta from previous step):\n{}\n\nRaw code context (auto included for edit tasks):\n{}\n\nTask:\n{}",
                    serde_json::to_string_pretty(
                        &delta_refs
                            .iter()
                            .take(refs_used)
                            .cloned()
                            .collect::<Vec<_>>(),
                    )
                    .unwrap_or_default(),
                    raw_code_context,
                    base_prompt
                )
            } else {
                String::new()
            };
            let heavy_prompt_over_guardrail = !with_prompt_heavy.is_empty()
                && (estimate_tokens(&with_prompt_heavy) as f32)
                    > (a_tokens.max(1) as f32 * tuning.guardrail_ratio);
            let prefer_heavy_first = task.requires_code_change
                && refs_used == 0
                && !with_prompt_heavy.is_empty()
                && !heavy_prompt_over_guardrail
                && confidence_score < 0.50;
            if prefer_heavy_first {
                heavy_first_tasks += 1;
            }
            let with_prompt_initial = if prefer_heavy_first {
                with_prompt_heavy.clone()
            } else {
                with_prompt_light.clone()
            };

            let result_b_attempt = call_live_llm_with_diagnostics(
                &selected_provider,
                provider_settings.get(&selected_provider),
                route.as_ref().map(|r| r.endpoint.as_str()),
                &with_prompt_initial,
                700,
            );
            let result_b = result_b_attempt.as_ref().ok().cloned();
            let mut result_b_error = result_b_attempt.err();
            let mut b_live_call_final = result_b.is_some();

            let (b_tokens, b_output) = result_b
                .as_ref()
                .map(|r| (r.total_tokens, r.text.clone()))
                .unwrap_or((estimate_tokens(&with_prompt_initial), String::new()));
            let a_hits = score_expected_terms(&a_output, &task.expected_terms);
            let mut b_hits = score_expected_terms(&b_output, &task.expected_terms);
            let mut b_tokens_final = b_tokens;
            let mut b_output_final = b_output.clone();
            let mut with_prompt_final = with_prompt_initial.clone();
            let planning_prompt_chars = with_prompt_light.len();
            let editing_prompt_chars = with_prompt_heavy.len();
            let mut escalation_used = false;
            let mut guardrail_applied = false;
            let mut semantic_route = if prefer_heavy_first {
                "heavy_first".to_string()
            } else {
                "light_first".to_string()
            };

            if !prefer_heavy_first
                && task.requires_code_change
                && b_hits < tuning.escalation_hits_threshold
                && !with_prompt_heavy.is_empty()
                && confidence_score < 0.75
            {
                escalation_attempts += 1;
                escalation_used = true;
                if heavy_prompt_over_guardrail {
                    guardrail_applied = true;
                    escalation_guardrail_skips += 1;
                    semantic_route = "light_retained_guardrail".to_string();
                } else {
                    let result_b_heavy_attempt = call_live_llm_with_diagnostics(
                        &selected_provider,
                        provider_settings.get(&selected_provider),
                        route.as_ref().map(|r| r.endpoint.as_str()),
                        &with_prompt_heavy,
                        700,
                    );
                    let result_b_heavy = result_b_heavy_attempt.as_ref().ok().cloned();
                    let (heavy_tokens, heavy_output) = result_b_heavy
                        .as_ref()
                        .map(|r| (r.total_tokens, r.text.clone()))
                        .unwrap_or((estimate_tokens(&with_prompt_heavy), String::new()));
                    if result_b_heavy.is_none() {
                        result_b_error = result_b_heavy_attempt.err();
                    } else {
                        b_live_call_final = true;
                    }
                    let heavy_hits = score_expected_terms(&heavy_output, &task.expected_terms);
                    if heavy_hits >= b_hits {
                        b_hits = heavy_hits;
                        b_output_final = heavy_output;
                        with_prompt_final = with_prompt_heavy;
                        b_tokens_final = heavy_tokens;
                        semantic_route = "escalated_heavy".to_string();
                    } else {
                        semantic_route = "light_retained_after_escalation".to_string();
                    }
                }
            }
            if with_prompt_final.len() > without_prompt.len() {
                semantic_prompt_over_control_count += 1;
            }
            let task_savings_pct = if a_tokens == 0 {
                0.0
            } else {
                ((a_tokens as f32 - b_tokens_final as f32) / a_tokens as f32) * 100.0
            };
            if a_hits > 0 {
                total_success_without += 1;
            }
            if b_hits > 0 {
                total_success_with += 1;
            }

            total_without += a_tokens;
            total_with += b_tokens_final;
            let success_without = a_hits >= 2;
            let success_with = semantic_exec.is_some() && validation_passed && b_hits >= 1;
            if let Some(steps) = estimate_steps_without_semantic(success_without, &a_output) {
                total_steps_without += steps;
                step_success_without += 1;
            }
            if let Some(steps) =
                estimate_steps_with_semantic(success_with, impacted_file_count.max(1), b_hits)
            {
                total_steps_with += steps;
                step_success_with += 1;
            }
            let tracking_row = json!({
                "timestamp": current_ts(),
                "suite": "todo_app_end_to_end_v2",
                "task_id": task.id,
                "title": task.title,
                "feature_request": task.feature_request,
                "prompt_without_semantic": without_prompt.clone(),
                "prompt_with_semantic": with_prompt_final.clone(),
                "provider": selected_provider.clone(),
                "model": selected_model.clone(),
                "semantic_features": task.semantic_features.clone(),
                "semantic_query": task.semantic_query,
                "target_symbol": task.target_symbol,
                "planned_target_symbol": planned_target_symbol,
                "target_match": target_match,
                "confidence_score": confidence_score,
                "confidence_band": confidence_band,
                "autoroute_first": autoroute_first,
                "tokens_without_semantic": a_tokens,
                "tokens_with_semantic": b_tokens_final,
                "token_savings_pct": task_savings_pct,
                "live_call_error_without_semantic": result_a_error,
                "live_call_error_with_semantic": result_b_error,
                "escalation_used": escalation_used,
                "guardrail_applied": guardrail_applied,
                "seed_target_aligned": seed_target_aligned,
                "semantic_route": semantic_route,
                "light_ref_count_used": refs_used,
                "control_attachment_chars": control_attachment_context.len(),
                "semantic_prompt_chars": with_prompt_final.len(),
                "control_prompt_chars": without_prompt.len(),
                "planning_prompt_chars": planning_prompt_chars,
                "editing_prompt_chars": editing_prompt_chars,
                "reused_context_count": 0usize,
                "success_without_semantic": success_without,
                "success_with_semantic": success_with,
                "validation_passed": validation_passed,
                "tests_passed": tests_passed
            });
            append_ab_test_task_metrics(&self.repo_root, &tracking_row)?;

            task_results.push(json!({
                "task_id": task.id,
                "title": task.title,
                "target_symbol": task.target_symbol,
                "single_file_fast_path": single_file_fast_path,
                "autoroute_first": autoroute_first,
                "without_semantic": {
                    "tokens": a_tokens,
                    "live_call": result_a.is_some(),
                    "live_call_error": result_a_error,
                    "expected_term_hits": a_hits,
                    "prompt": without_prompt,
                    "output": a_output,
                },
                "with_semantic": {
                    "tokens": b_tokens_final,
                    "live_call": b_live_call_final,
                    "live_call_error": result_b_error,
                    "expected_term_hits": b_hits,
                    "prompt": with_prompt_final,
                    "output": b_output_final,
                },
                "token_savings_pct": task_savings_pct,
                "semantic_features": task.semantic_features.clone(),
                "escalation_used": escalation_used,
                "guardrail_applied": guardrail_applied,
                "semantic_route": semantic_route,
                "planned_target_symbol": planned_target_symbol,
                "target_match": target_match,
                "confidence_score": confidence_score,
                "confidence_band": confidence_band,
                "seed_target_aligned": seed_target_aligned,
                "light_ref_count_used": refs_used,
                "control_attachment_chars": control_attachment_context.len(),
                "semantic_prompt_chars": with_prompt_final.len(),
                "control_prompt_chars": without_prompt.len(),
                "planning_prompt_chars": planning_prompt_chars,
                "editing_prompt_chars": editing_prompt_chars,
                "reused_context_count": 0usize,
                "control_attachment_files": task
                    .context_ranges
                    .iter()
                    .map(|r| r.file)
                    .collect::<Vec<_>>(),
                "delta_context_ref_count": delta_refs.len(),
                "estimated_steps_without_semantic": estimate_steps_without_semantic(success_without, &a_output),
                "estimated_steps_with_semantic": estimate_steps_with_semantic(success_with, impacted_file_count.max(1), b_hits),
                "success_without_semantic": success_without,
                "success_with_semantic": success_with,
                "validation_passed": validation_passed,
                "tests_passed": tests_passed,
                "semantic_execution": semantic_exec.unwrap_or_else(|| json!({"ok": false, "error": "plan_safe_edit failed"})),
            }));
        }

        let savings_pct = if total_without == 0 {
            0.0
        } else {
            ((total_without as f32 - total_with as f32) / total_without as f32) * 100.0
        };
        let task_count_f = task_count.max(1) as f32;
        let success_without_pct = (total_success_without as f32 / task_count_f) * 100.0;
        let success_with_pct = (total_success_with as f32 / task_count_f) * 100.0;
        let semantic_exec_pct = (semantic_exec_success as f32 / task_count_f) * 100.0;
        let validation_success_pct = (validation_success as f32 / task_count_f) * 100.0;
        let tests_success_pct = (tests_success as f32 / task_count_f) * 100.0;
        let step_savings_pct = if total_steps_without == 0 {
            0.0
        } else {
            ((total_steps_without as f32 - total_steps_with as f32) / total_steps_without as f32)
                * 100.0
        };
        let target_match_pct = (target_match_count as f32 / task_count_f) * 100.0;
        let empty_ref_pct = (empty_ref_tasks as f32 / task_count_f) * 100.0;
        let heavy_first_pct = (heavy_first_tasks as f32 / task_count_f) * 100.0;
        let semantic_prompt_over_control_pct =
            (semantic_prompt_over_control_count as f32 / task_count_f) * 100.0;
        let escalation_attempt_pct = (escalation_attempts as f32 / task_count_f) * 100.0;
        let escalation_guardrail_skip_pct =
            (escalation_guardrail_skips as f32 / task_count_f) * 100.0;
        let runtime_trim_applied_pct = (runtime_trim_applied as f32 / task_count_f) * 100.0;

        append_ab_test_csv(
            &self.repo_root,
            &ABTestRow {
                timestamp: current_ts(),
                provider: selected_provider.clone(),
                symbol: format!("dev_suite_v2:todo_app_{}_tasks", task_count),
                tokens_without_project: total_without,
                tokens_with_project: total_with,
                savings_pct,
            },
        )?;

        Ok(json!({
            "scenario": "todo_app_end_to_end_v2",
            "scenario_mode": scenario_name,
            "provider": selected_provider,
            "model": selected_model,
            "feature_request": requested_feature,
            "suite_task_count": task_count,
            "suite_capabilities": ["due_date", "priority_reorder", "tags", "ui_menu_tooling"],
            "single_file_fast_path": single_file_fast_path,
            "autoroute_first": autoroute_first,
            "context_cache_hits": context_cache_hits,
            "without_project": {
                "tokens": total_without,
                "task_success_count": total_success_without,
                "task_success_pct": success_without_pct,
                "successful_step_samples": step_success_without,
                "estimated_total_steps": total_steps_without,
            },
            "with_project": {
                "tokens": total_with,
                "task_success_count": total_success_with,
                "task_success_pct": success_with_pct,
                "successful_step_samples": step_success_with,
                "estimated_total_steps": total_steps_with,
            },
            "savings_pct": savings_pct,
            "step_savings_pct": step_savings_pct,
            "primary_metric": "fewest_total_steps_to_successful_code_change",
            "semantic_execution_success_count": semantic_exec_success,
            "semantic_execution_success_pct": semantic_exec_pct,
            "validation_success_count": validation_success,
            "validation_success_pct": validation_success_pct,
            "tests_success_count": tests_success,
            "tests_success_pct": tests_success_pct,
            "gating_metrics": {
                "target_match_count": target_match_count,
                "target_match_pct": target_match_pct,
                "empty_ref_tasks": empty_ref_tasks,
                "empty_ref_pct": empty_ref_pct,
                "heavy_first_tasks": heavy_first_tasks,
                "heavy_first_pct": heavy_first_pct,
                "semantic_prompt_over_control_count": semantic_prompt_over_control_count,
                "semantic_prompt_over_control_pct": semantic_prompt_over_control_pct,
                "escalation_attempts": escalation_attempts,
                "escalation_attempt_pct": escalation_attempt_pct,
                "escalation_guardrail_skips": escalation_guardrail_skips,
                "escalation_guardrail_skip_pct": escalation_guardrail_skip_pct,
                "runtime_trim_applied": runtime_trim_applied,
                "runtime_trim_applied_pct": runtime_trim_applied_pct,
            },
            "quality_gates": {
                "primary_metric": "validated_patch_and_test_backed_task_success",
                "validated_patch_success_pct": validation_success_pct,
                "tests_success_pct": tests_success_pct,
                "step_regression_alert_pct": policy.step_regression_alert_pct,
                "token_overrun_alert_pct": policy.prompt_overrun_alert_pct,
                "regression_alert": savings_pct < -(policy.prompt_overrun_alert_pct as f32)
                    || step_savings_pct < -(policy.step_regression_alert_pct as f32),
            },
            "task_results": task_results,
        }))
    }

    pub fn get_llm_tools(&self) -> serde_json::Value {
        json!({
            "compression_policy": {
                "safe_for_semantic_retrieval": false,
                "note": "For semantic planning/edit operations, send the original uncompressed query to preserve exact symbol and line precision."
            },
            "retrieval_policy": {
                "two_stage_retrieval": true,
                "structured_refs_default": true,
                "reference_only_default": true,
                "minimal_raw_seed_via_autoroute": true,
                "mapping_mode_default": "footprint_first",
                "max_footprint_items_default": 120,
                "reuse_session_context_default": true,
                "adaptive_breadth": true,
                "single_file_fast_path": "supported in /ab_test_dev and /ide_autoroute (default=true)"
            },
            "tools": [
                {"name":"get_repo_map","operation":"GetRepoMap","purpose":"List indexed files."},
                {"name":"get_file_outline","operation":"GetFileOutline","purpose":"List symbols in a file.","required":["file"]},
                {"name":"search_symbol","operation":"SearchSymbol","purpose":"Find symbols quickly (grep-like name search).","required":["name"]},
                {"name":"get_code_span","operation":"GetCodeSpan","purpose":"Retrieve exact file lines.","required":["file","start_line","end_line"]},
                {"name":"get_logic_nodes","operation":"GetLogicNodes","purpose":"Inspect logic structure for a symbol.","required":["name"]},
                {"name":"get_control_flow_slice","operation":"GetControlFlowSlice","purpose":"Retrieve persisted control-flow edges for a symbol.","required":["name"]},
                {"name":"get_data_flow_slice","operation":"GetDataFlowSlice","purpose":"Retrieve persisted data-flow edges for a symbol.","required":["name"]},
                {"name":"get_logic_clusters","operation":"GetLogicClusters","purpose":"Retrieve clustered logic regions for a symbol.","required":["name"]},
                {"name":"get_dependency_neighborhood","operation":"GetDependencyNeighborhood","purpose":"Traverse caller/callee neighborhoods.","required":["name","radius"]},
                {"name":"get_reasoning_context","operation":"GetReasoningContext","purpose":"Fetch semantic context for planning edits.","required":["name","logic_radius","dependency_radius"]},
                {"name":"get_planned_context","operation":"GetPlannedContext","purpose":"Build context by intent and budget.","required":["query","max_tokens"],"optional":["mapping_mode","max_footprint_items","reuse_session_context"]},
                {"name":"plan_safe_edit","operation":"PlanSafeEdit","purpose":"Generate impact-aware patch preview and policy-checked edit plan.","required":["name_or_query","edit_description"]},
                {"name":"ide_autoroute","endpoint":"/ide_autoroute","purpose":"IDE-native semantic-first entrypoint that auto-selects first retrieval call.","required":["task"]},
                {"name":"performance_stats","endpoint":"/performance_stats","purpose":"Runtime hardening metrics (cache hit rate, op latency, p95)."},
                {"name":"control_flow_hints","endpoint":"/control_flow_hints","purpose":"Control-flow hints for a symbol.","required":["symbol"]},
                {"name":"data_flow_hints","endpoint":"/data_flow_hints","purpose":"Data-flow hints for a symbol.","required":["symbol"]},
                {"name":"hybrid_ranked_context","endpoint":"/hybrid_ranked_context","purpose":"Hybrid ranking (symbol+logic+dependency) for compact context.","required":["query"]}
            ],
            "workflow_recommendation": [
                "Use semantic_enabled=false for pure chat or conceptual Q&A.",
                "Use /ide_autoroute as the default first call for planning/editing workflows.",
                "Use semantic_enabled=true for planning, code edits, and execution-oriented tasks.",
                "Avoid compressed prompts for semantic operations unless original_query is also provided."
            ]
        })
    }

    pub fn get_ab_tests(&self) -> Result<serde_json::Value> {
        let rows = read_ab_test_csv(&self.repo_root)?;
        Ok(json!({ "rows": rows }))
    }

    fn get_repo_map(&self) -> Result<serde_json::Value> {
        let files = self.storage.list_files()?;
        Ok(json!({ "files": files }))
    }

    fn get_file_outline(&self, file: &str) -> Result<serde_json::Value> {
        let symbols = self.storage.file_outline(file)?;
        Ok(json!({ "file": file, "symbols": symbols }))
    }

    fn get_repo_map_hierarchy(&self) -> Result<serde_json::Value> {
        let modules = self.storage.list_modules()?;
        let mut out = Vec::new();
        for module in modules {
            let module_id = module.id.unwrap_or_default();
            let files = self.storage.list_module_files(module_id)?;
            let mut file_entries = Vec::new();
            for file in files {
                let symbols = self.storage.file_outline(&file.file_path)?;
                file_entries.push(json!({
                    "file": file.file_path,
                    "symbols": symbols,
                }));
            }
            out.push(json!({
                "module": module.name,
                "path": module.path,
                "files": file_entries,
            }));
        }
        Ok(json!({ "modules": out }))
    }

    fn get_module_dependencies(&self) -> Result<serde_json::Value> {
        let deps = self.storage.list_named_module_dependencies()?;
        let edges: Vec<serde_json::Value> = deps
            .into_iter()
            .map(|(from, to)| json!({ "from": from, "to": to }))
            .collect();
        Ok(json!({ "edges": edges }))
    }

    fn search_symbol(&self, name: &str, limit: usize) -> Result<serde_json::Value> {
        let hits = self.storage.tantivy_search(name, limit)?;
        let fallback = self.storage.search_symbol_by_name(name, limit)?;
        Ok(json!({
            "query": name,
            "tantivy_hits": hits,
            "fallback": fallback,
        }))
    }

    fn search_semantic_symbol(&self, query: &str, limit: usize) -> Result<serde_json::Value> {
        let lexical = self.storage.search_symbol_by_name(query, limit)?;
        if !lexical.is_empty() {
            return Ok(json!({
                "query": query,
                "strategy": "lexical",
                "results": lexical
            }));
        }
        let semantic = semantic_search::SemanticSearcher::search(&self.storage, query, limit)?;
        Ok(json!({
            "query": query,
            "strategy": "semantic_fallback",
            "results": semantic
        }))
    }

    fn get_symbol_span(&self, name: &str, symbol_type: SymbolType) -> Result<serde_json::Value> {
        let sym = self
            .storage
            .get_symbol_exact(name, symbol_type)?
            .ok_or_else(|| anyhow!("symbol not found: {name}"))?;

        let code = read_span(&self.repo_root, &sym.file, sym.start_line, sym.end_line)?;
        Ok(json!({
            "name": sym.name,
            "file": sym.file,
            "start_line": sym.start_line,
            "end_line": sym.end_line,
            "code": code,
        }))
    }

    fn get_dependencies(&self, name: &str) -> Result<serde_json::Value> {
        let deps = self.storage.get_dependencies(name)?;
        Ok(json!({ "name": name, "dependencies": deps }))
    }

    fn get_code_span(
        &self,
        file: &str,
        start_line: u32,
        end_line: u32,
    ) -> Result<serde_json::Value> {
        let code = read_span(&self.repo_root, file, start_line, end_line)?;
        Ok(json!({
            "file": file,
            "start_line": start_line,
            "end_line": end_line,
            "code": code,
        }))
    }

    fn get_logic_nodes(&self, symbol_name: &str) -> Result<serde_json::Value> {
        let symbol = self
            .storage
            .get_symbol_any(symbol_name)?
            .ok_or_else(|| anyhow!("symbol not found: {symbol_name}"))?;
        let symbol_id = symbol.id.ok_or_else(|| anyhow!("symbol id missing"))?;
        let nodes = self.storage.get_logic_nodes(symbol_id)?;

        Ok(json!({
            "symbol": symbol.name,
            "symbol_id": symbol_id,
            "nodes": nodes,
        }))
    }

    fn get_logic_neighborhood(&self, node_id: i64, radius: usize) -> Result<serde_json::Value> {
        let nodes = self.storage.get_logic_neighbors(node_id, radius)?;
        Ok(json!({
            "node_id": node_id,
            "radius": radius,
            "nodes": nodes,
        }))
    }

    fn get_logic_span(&self, node_id: i64) -> Result<serde_json::Value> {
        let node = self
            .storage
            .get_logic_node(node_id)?
            .ok_or_else(|| anyhow!("logic node not found: {node_id}"))?;
        let file = self
            .storage
            .get_logic_node_file(node_id)?
            .ok_or_else(|| anyhow!("logic node file not found: {node_id}"))?;

        let start_line = node.start_line as u32;
        let end_line = node.end_line as u32;
        let code = read_span(&self.repo_root, &file, start_line, end_line)?;
        Ok(json!({
            "node_id": node_id,
            "type": node.node_type,
            "file": file,
            "start_line": start_line,
            "end_line": end_line,
            "code": code,
        }))
    }

    fn get_dependency_neighborhood(
        &self,
        symbol_name: &str,
        radius: usize,
    ) -> Result<serde_json::Value> {
        let symbol = self
            .storage
            .get_symbol_any(symbol_name)?
            .ok_or_else(|| anyhow!("symbol not found: {symbol_name}"))?;
        let symbol_id = symbol.id.ok_or_else(|| anyhow!("symbol id missing"))?;

        let mut neighbors = self.storage.get_dependency_neighbors(symbol_id, radius)?;
        neighbors.retain(|s| s.id != Some(symbol_id));
        sort_symbols(&mut neighbors);

        Ok(json!({
            "symbol": symbol,
            "radius": radius,
            "neighbors": neighbors,
        }))
    }

    fn get_symbol_neighborhood(
        &self,
        symbol_name: &str,
        radius: usize,
    ) -> Result<serde_json::Value> {
        let key = format!("symbol_neighborhood::{symbol_name}::{radius}");
        if let Some(cached) = self.try_get_symbol_cache(&key, 1800) {
            let mut perf = self.perf_stats.lock().expect("perf lock");
            perf.symbol_cache_hits += 1;
            return Ok(cached);
        }
        {
            let mut perf = self.perf_stats.lock().expect("perf lock");
            perf.symbol_cache_misses += 1;
        }
        let symbol = self
            .storage
            .get_symbol_any(symbol_name)?
            .ok_or_else(|| anyhow!("symbol not found: {symbol_name}"))?;
        let symbol_id = symbol.id.ok_or_else(|| anyhow!("symbol id missing"))?;

        let mut logic_nodes = self.storage.get_logic_nodes(symbol_id)?;
        sort_logic_nodes(&mut logic_nodes);

        let mut dependencies = self.storage.get_dependency_neighbors(symbol_id, radius)?;
        dependencies.retain(|s| s.id != Some(symbol_id));
        sort_symbols(&mut dependencies);

        let output = json!({
            "symbol": symbol,
            "logic_nodes": logic_nodes,
            "dependency_neighbors": dependencies,
            "order": ["symbol", "logic_nodes", "dependency_neighbors"],
        });
        self.store_symbol_cache(key, output.clone(), 512);
        Ok(output)
    }

    fn get_reasoning_context(
        &self,
        symbol_name: &str,
        logic_radius: usize,
        dependency_radius: usize,
    ) -> Result<serde_json::Value> {
        let symbol = self
            .storage
            .get_symbol_any(symbol_name)?
            .ok_or_else(|| anyhow!("symbol not found: {symbol_name}"))?;
        let symbol_id = symbol.id.ok_or_else(|| anyhow!("symbol id missing"))?;

        let mut seed_nodes = self.storage.get_logic_nodes(symbol_id)?;
        sort_logic_nodes(&mut seed_nodes);

        let mut logic_context = Vec::new();
        for node in &seed_nodes {
            if let Some(node_id) = node.id {
                let mut neighborhood = self.storage.get_logic_neighbors(node_id, logic_radius)?;
                logic_context.append(&mut neighborhood);
            }
        }
        logic_context.sort_by_key(|n| (n.id.unwrap_or_default(), n.start_line, n.end_line));
        logic_context.dedup_by_key(|n| n.id.unwrap_or_default());
        sort_logic_nodes(&mut logic_context);

        let mut dependency_symbols = self
            .storage
            .get_dependency_neighbors(symbol_id, dependency_radius)?;
        dependency_symbols.retain(|s| s.id != Some(symbol_id));
        sort_symbols(&mut dependency_symbols);

        let mut logic_spans = Vec::new();
        for node in &logic_context {
            if let Some(node_id) = node.id {
                if let Some(file) = self.storage.get_logic_node_file(node_id)? {
                    let code = read_span(
                        &self.repo_root,
                        &file,
                        node.start_line as u32,
                        node.end_line as u32,
                    )?;
                    logic_spans.push(json!({
                        "node_id": node_id,
                        "type": node.node_type,
                        "file": file,
                        "start_line": node.start_line,
                        "end_line": node.end_line,
                        "code": code,
                    }));
                }
            }
        }
        logic_spans.sort_by(|a, b| {
            let af = a.get("file").and_then(|v| v.as_str()).unwrap_or_default();
            let bf = b.get("file").and_then(|v| v.as_str()).unwrap_or_default();
            let as_line = a
                .get("start_line")
                .and_then(|v| v.as_u64())
                .unwrap_or_default();
            let bs_line = b
                .get("start_line")
                .and_then(|v| v.as_u64())
                .unwrap_or_default();
            af.cmp(bf).then_with(|| as_line.cmp(&bs_line))
        });

        let mut dependency_spans = Vec::new();
        for dep in &dependency_symbols {
            let code = read_span(&self.repo_root, &dep.file, dep.start_line, dep.end_line)?;
            dependency_spans.push(json!({
                "symbol": dep,
                "code": code,
            }));
        }
        dependency_spans.sort_by(|a, b| {
            let af = a
                .get("symbol")
                .and_then(|v| v.get("file"))
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let bf = b
                .get("symbol")
                .and_then(|v| v.get("file"))
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let as_line = a
                .get("symbol")
                .and_then(|v| v.get("start_line"))
                .and_then(|v| v.as_u64())
                .unwrap_or_default();
            let bs_line = b
                .get("symbol")
                .and_then(|v| v.get("start_line"))
                .and_then(|v| v.as_u64())
                .unwrap_or_default();
            af.cmp(bf).then_with(|| as_line.cmp(&bs_line))
        });

        Ok(json!({
            "symbol": symbol,
            "logic_nodes": logic_context,
            "logic_spans": logic_spans,
            "dependencies": dependency_symbols,
            "dependency_spans": dependency_spans,
            "order": ["symbol", "logic_nodes", "dependencies"],
            "logic_radius": logic_radius,
            "dependency_radius": dependency_radius,
        }))
    }

    fn get_planned_context(
        &self,
        query: &str,
        max_tokens: usize,
        single_file_fast_path: bool,
        include_raw_code_override: Option<bool>,
        preferred_symbol: Option<&str>,
        mapping_mode: Option<&str>,
        max_footprint_items: Option<usize>,
    ) -> Result<serde_json::Value> {
        let policy = load_retrieval_policy(&self.repo_root);
        let include_raw_code = include_raw_code_override.unwrap_or(false);
        let preferred_symbol_key = preferred_symbol.unwrap_or("");
        let mapping_mode_key = mapping_mode.unwrap_or("footprint_first");
        let footprint_limit = max_footprint_items.unwrap_or(120);
        let cache_key = format!(
            "planned::{query}::{max_tokens}::{single_file_fast_path}::{include_raw_code}::{preferred_symbol_key}::{mapping_mode_key}::{footprint_limit}"
        );
        if let Some(cached) = self.try_get_cached_context(&cache_key, 3600) {
            let mut perf = self.perf_stats.lock().expect("perf lock");
            perf.cache_hits += 1;
            return Ok(cached);
        }
        {
            let mut perf = self.perf_stats.lock().expect("perf lock");
            perf.cache_misses += 1;
        }

        let symbols = self.storage.list_symbols()?;
        let mut symbol_names: Vec<String> = symbols.into_iter().map(|s| s.name).collect();
        symbol_names.sort();
        symbol_names.dedup();
        let mode = MappingMode::parse(mapping_mode);
        let mut candidate_files: Vec<String> = Vec::new();
        let mut candidate_symbols: Vec<String> = Vec::new();
        if mode == MappingMode::FootprintFirst {
            let footprint = self.build_project_footprint(footprint_limit)?;
            let (files, symbols) = build_task_candidate_set(query, &footprint, 18, 64);
            candidate_files = files;
            candidate_symbols = symbols;
            if !candidate_symbols.is_empty() {
                symbol_names = candidate_symbols.clone();
            }
        }

        let module_records = self.storage.list_modules()?;
        let mut file_to_module = std::collections::HashMap::new();
        for module in &module_records {
            let module_id = module.id.unwrap_or_default();
            for mf in self.storage.list_module_files(module_id)? {
                file_to_module.insert(mf.file_path, module.name.clone());
            }
        }
        let named_module_deps = self.storage.list_named_module_dependencies()?;
        let mut symbol_to_module = std::collections::HashMap::new();
        for symbol in self.storage.list_symbols()? {
            if let Some(module_name) = file_to_module.get(&symbol.file) {
                symbol_to_module.insert(symbol.name, module_name.clone());
            }
        }

        let planner = Planner::new();
        let intent = planner.detect_intent(query);
        let plan = planner
            .build_plan_with_modules_and_hint(
                query,
                &symbol_names,
                &symbol_to_module,
                &named_module_deps,
                preferred_symbol,
            )
            .ok_or_else(|| anyhow!("unable to determine target symbol from query"))?;

        let target_symbol = self
            .storage
            .get_symbol_any(&plan.target_symbol)?
            .ok_or_else(|| anyhow!("symbol not found: {}", plan.target_symbol))?;
        let target_id = target_symbol
            .id
            .ok_or_else(|| anyhow!("target symbol id missing"))?;
        let (effective_logic_radius, effective_dependency_radius, breadth) = self
            .optimize_retrieval_breadth(
                target_id,
                plan.logic_radius,
                plan.dependency_radius,
                max_tokens,
            )?;
        let confidence_score = self.estimate_context_confidence(
            query,
            &plan.target_symbol,
            &candidate_symbols,
            preferred_symbol,
        );
        let confidence_band = confidence_band(confidence_score);

        if single_file_fast_path {
            let target_code = if include_raw_code {
                read_span(
                    &self.repo_root,
                    &target_symbol.file,
                    target_symbol.start_line,
                    target_symbol.end_line,
                )
                .unwrap_or_default()
            } else {
                String::new()
            };

            let output = json!({
                "symbol": plan.target_symbol,
                "intent": format!("{intent:?}").to_lowercase(),
                "plan": plan,
                "effective_breadth": breadth,
                "small_repo_mode": self.storage.list_files()?.len() < policy.small_repo_file_threshold,
                "single_file_fast_path": true,
                "retrieval_strategy": "single_file_fast_path",
                "mapping_mode": mapping_mode_key,
                "context_phase": if mode == MappingMode::FootprintFirst { "footprint_stage_a_targeted_stage_b_conditional_stage_c" } else { "legacy_full" },
                "candidate_files": candidate_files,
                "candidate_symbols": candidate_symbols,
                "confidence_score": confidence_score,
                "confidence_band": confidence_band,
                "include_raw_code": include_raw_code,
                "context": [{
                    "file": target_symbol.file,
                    "module": file_to_module
                        .get(&target_symbol.file)
                        .cloned()
                        .unwrap_or_else(|| "unknown".to_string()),
                    "start": target_symbol.start_line,
                    "end": target_symbol.end_line,
                    "priority": 0,
                    "raw_included": include_raw_code,
                    "code": target_code
                }],
                "cache": { "hit": false }
            });
            self.store_cached_context(cache_key, output.clone(), 1024);
            return Ok(output);
        }

        let mut context_items = Vec::new();
        let estimated_text = |start: usize, end: usize| -> String {
            let span_lines = end.saturating_sub(start).saturating_add(1);
            let approx_chars = (span_lines.saturating_mul(14)).clamp(80, 1600);
            "x".repeat(approx_chars)
        };
        context_items.push(ContextItem {
            file_path: target_symbol.file.clone(),
            module_name: file_to_module
                .get(&target_symbol.file)
                .cloned()
                .unwrap_or_else(|| "unknown".to_string()),
            module_rank: module_rank_for_file(
                &file_to_module,
                &plan.scoped_modules,
                &target_symbol.file,
            ),
            start_line: target_symbol.start_line as usize,
            end_line: target_symbol.end_line as usize,
            priority: 0,
            text: estimated_text(
                target_symbol.start_line as usize,
                target_symbol.end_line as usize,
            ),
        });

        let mut logic_nodes = self.storage.get_logic_nodes(target_id)?;
        sort_logic_nodes(&mut logic_nodes);
        let mut logic_context = Vec::new();
        for node in &logic_nodes {
            if let Some(node_id) = node.id {
                logic_context.append(
                    &mut self
                        .storage
                        .get_logic_neighbors(node_id, effective_logic_radius)?,
                );
            }
        }
        logic_context.sort_by_key(|n| (n.id.unwrap_or_default(), n.start_line, n.end_line));
        logic_context.dedup_by_key(|n| n.id.unwrap_or_default());
        sort_logic_nodes(&mut logic_context);

        for node in logic_context {
            if let Some(node_id) = node.id {
                if let Some(file) = self.storage.get_logic_node_file(node_id)? {
                    context_items.push(ContextItem {
                        file_path: file.clone(),
                        module_name: file_to_module
                            .get(&file)
                            .cloned()
                            .unwrap_or_else(|| "unknown".to_string()),
                        module_rank: module_rank_for_file(
                            &file_to_module,
                            &plan.scoped_modules,
                            &file,
                        ),
                        start_line: node.start_line,
                        end_line: node.end_line,
                        priority: 1,
                        text: estimated_text(node.start_line, node.end_line),
                    });
                }
            }
        }

        let direct_dependencies = self.storage.get_symbol_dependencies(target_id)?;
        let mut direct_ids = HashSet::new();
        for dep in &direct_dependencies {
            if let Some(dep_id) = dep.id {
                direct_ids.insert(dep_id);
                context_items.push(ContextItem {
                    file_path: dep.file.clone(),
                    module_name: file_to_module
                        .get(&dep.file)
                        .cloned()
                        .unwrap_or_else(|| "unknown".to_string()),
                    module_rank: module_rank_for_file(
                        &file_to_module,
                        &plan.scoped_modules,
                        &dep.file,
                    ),
                    start_line: dep.start_line as usize,
                    end_line: dep.end_line as usize,
                    priority: 2,
                    text: estimated_text(dep.start_line as usize, dep.end_line as usize),
                });
            }
        }

        let neighbors = collect_dependency_neighbors(
            &self.storage,
            target_id,
            effective_dependency_radius,
            plan.include_callers,
        )?;
        for dep in neighbors {
            if let Some(dep_id) = dep.id {
                if dep_id == target_id || direct_ids.contains(&dep_id) {
                    continue;
                }
                context_items.push(ContextItem {
                    file_path: dep.file.clone(),
                    module_name: file_to_module
                        .get(&dep.file)
                        .cloned()
                        .unwrap_or_else(|| "unknown".to_string()),
                    module_rank: module_rank_for_file(
                        &file_to_module,
                        &plan.scoped_modules,
                        &dep.file,
                    ),
                    start_line: dep.start_line as usize,
                    end_line: dep.end_line as usize,
                    priority: 3,
                    text: estimated_text(dep.start_line as usize, dep.end_line as usize),
                });
            }
        }

        context_items.sort_by(|a, b| {
            a.priority
                .cmp(&b.priority)
                .then_with(|| a.module_rank.cmp(&b.module_rank))
                .then_with(|| a.file_path.cmp(&b.file_path))
                .then_with(|| a.start_line.cmp(&b.start_line))
                .then_with(|| a.end_line.cmp(&b.end_line))
        });
        context_items.dedup_by(|a, b| {
            a.file_path == b.file_path
                && a.start_line == b.start_line
                && a.end_line == b.end_line
                && a.priority == b.priority
        });

        let file_count = self.storage.list_files()?.len();
        let budget = ContextBudget {
            max_tokens,
            reserved_prompt: 1000,
        };
        let selected = select_with_budget(context_items, &budget);

        let mut raw_budget_chars = max_tokens.saturating_mul(4).saturating_sub(1600).max(800);
        if policy.anti_bloat_small_task && single_file_fast_path && file_count < policy.small_repo_file_threshold {
            raw_budget_chars = raw_budget_chars.min(900);
        }
        let assembled: Vec<serde_json::Value> = selected
            .into_iter()
            .map(|item| {
                let mut code = String::new();
                let mut raw_included = false;
                if include_raw_code && raw_budget_chars > 0 {
                    if let Ok(raw) = read_span(
                        &self.repo_root,
                        &item.file_path,
                        item.start_line as u32,
                        item.end_line as u32,
                    ) {
                        let limited = if raw.chars().count() > raw_budget_chars {
                            let kept: String = raw.chars().take(raw_budget_chars).collect();
                            format!("{kept}...")
                        } else {
                            raw
                        };
                        raw_budget_chars = raw_budget_chars.saturating_sub(limited.len());
                        code = limited;
                        raw_included = !code.is_empty();
                    }
                }
                json!({
                    "file": item.file_path,
                    "module": item.module_name,
                    "start": item.start_line,
                    "end": item.end_line,
                    "priority": item.priority,
                    "raw_included": raw_included,
                    "code": code,
                })
            })
            .collect();

        let output = json!({
            "symbol": plan.target_symbol,
            "intent": format!("{intent:?}").to_lowercase(),
            "plan": plan,
            "effective_breadth": breadth,
            "mapping_mode": mapping_mode_key,
            "context_phase": if mode == MappingMode::FootprintFirst { "footprint_stage_a_targeted_stage_b_conditional_stage_c" } else { "legacy_full" },
            "candidate_files": candidate_files,
            "candidate_symbols": candidate_symbols,
            "confidence_score": confidence_score,
            "confidence_band": confidence_band,
            "small_repo_mode": file_count < policy.small_repo_file_threshold,
            "retrieval_strategy": "two_stage_rank_then_span_fetch",
            "include_raw_code": include_raw_code,
            "context": assembled,
            "cache": { "hit": false }
        });
        self.store_cached_context(cache_key, output.clone(), 1024);
        Ok(output)
    }

    fn build_project_footprint(&self, max_items: usize) -> Result<Vec<serde_json::Value>> {
        let modules = self.storage.list_modules()?;
        let mut file_to_module: HashMap<String, String> = HashMap::new();
        for module in modules {
            let module_id = module.id.unwrap_or_default();
            for mf in self.storage.list_module_files(module_id)? {
                file_to_module.insert(mf.file_path, module.name.clone());
            }
        }
        let files = self.storage.list_files()?;
        let symbols = self.storage.list_symbols()?;
        let mut symbols_by_file: HashMap<String, Vec<String>> = HashMap::new();
        for sym in symbols {
            symbols_by_file
                .entry(sym.file)
                .or_default()
                .push(sym.name.clone());
        }
        let mut footprint = Vec::new();
        for file in files.into_iter().take(max_items) {
            let top_symbols = symbols_by_file
                .get(&file)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .take(8)
                .collect::<Vec<_>>();
            let objective = infer_file_objective(&file, &top_symbols);
            footprint.push(json!({
                "file": file,
                "module": file_to_module.get(&file).cloned().unwrap_or_else(|| "unknown".to_string()),
                "objective": objective,
                "top_symbols": top_symbols
            }));
        }
        Ok(footprint)
    }

    fn estimate_context_confidence(
        &self,
        query: &str,
        target_symbol: &str,
        candidate_symbols: &[String],
        preferred_symbol: Option<&str>,
    ) -> f32 {
        let q_norm = normalize_query_tokens(query);
        let target_norm = normalize_query_tokens(target_symbol);
        let mut score = 0.0f32;
        if !target_norm.is_empty() && q_norm.contains(&target_norm) {
            score += 0.35;
        }
        if let Ok(lexical) = self.storage.search_symbol_by_name(query, 8) {
            if lexical
                .iter()
                .any(|s| s.name.eq_ignore_ascii_case(target_symbol))
            {
                score += 0.2;
            }
        }
        if candidate_symbols
            .iter()
            .any(|s| s.eq_ignore_ascii_case(target_symbol))
        {
            score += 0.2;
        }
        if let Ok(symbol) = self.storage.get_symbol_any(target_symbol) {
            if let Some(sym) = symbol {
                if let Some(id) = sym.id {
                    if self
                        .storage
                        .get_dependency_neighbors(id, 1)
                        .map(|v| !v.is_empty())
                        .unwrap_or(false)
                    {
                        score += 0.15;
                    }
                }
            }
        }
        if preferred_symbol
            .map(|p| p.eq_ignore_ascii_case(target_symbol))
            .unwrap_or(false)
        {
            score += 0.1;
        }
        score.clamp(0.0, 1.0)
    }

    fn get_workspace_reasoning_context(
        &self,
        query: &str,
        max_tokens: usize,
        workspace_scope: Vec<String>,
        single_file_fast_path: bool,
        include_raw_code_override: Option<bool>,
        mapping_mode: Option<&str>,
        max_footprint_items: Option<usize>,
    ) -> Result<serde_json::Value> {
        let mut repositories = self.storage.list_repositories()?;
        if !workspace_scope.is_empty() {
            repositories.retain(|r| workspace_scope.iter().any(|s| s == &r.name || s == &r.path));
        }
        let planned = self.get_planned_context(
            query,
            max_tokens,
            single_file_fast_path,
            include_raw_code_override,
            None,
            mapping_mode,
            max_footprint_items,
        )?;
        Ok(json!({
            "workspace_repositories": repositories,
            "workspace_scope": workspace_scope,
            "context": planned,
        }))
    }

    fn autoroute_context_for_ab(
        &self,
        task_query: &str,
        max_tokens: usize,
        single_file_fast_path: bool,
        preferred_symbol: Option<&str>,
    ) -> Result<serde_json::Value> {
        let mut planned = self.get_planned_context(
            task_query,
            max_tokens,
            single_file_fast_path,
            Some(false),
            preferred_symbol,
            Some("footprint_first"),
            Some(120),
        )?;
        let seed_ctx = preferred_symbol
            .and_then(|s| self.storage.get_symbol_any(s).ok().flatten())
            .map(|sym| {
                json!({
                    "file": sym.file,
                    "start": sym.start_line,
                    "end": sym.end_line,
                })
            })
            .or_else(|| {
                planned
                    .get("context")
                    .and_then(|v| v.as_array())
                    .and_then(|arr| arr.first())
                    .cloned()
            });
        if let Some(first_ctx) = seed_ctx {
            let file = first_ctx
                .get("file")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let start = first_ctx
                .get("start")
                .and_then(|v| v.as_u64())
                .unwrap_or_default() as u32;
            let end = first_ctx
                .get("end")
                .and_then(|v| v.as_u64())
                .unwrap_or_default() as u32;
            if !file.is_empty() && start > 0 && end >= start {
                let clipped_end = start.saturating_add(40).min(end);
                if let Ok(span) = self.get_code_span(file, start, clipped_end) {
                    if let Some(obj) = planned.as_object_mut() {
                        obj.insert(
                            "minimal_raw_seed".to_string(),
                            json!({
                                "file": file,
                                "start": start,
                                "end": clipped_end,
                                "code_span": span
                            }),
                        );
                    }
                }
            }
        }
        Ok(planned)
    }

    pub fn get_control_flow_hints(&self, symbol: &str) -> Result<serde_json::Value> {
        let slice = self.get_control_flow_slice(symbol)?;
        let branch_like = slice
            .get("control_flow_edges")
            .and_then(|v| v.as_array())
            .map(|edges| {
                edges.iter().filter(|edge| {
                    edge.get("kind")
                        .and_then(|v| v.as_str())
                        .map(|kind| kind == "Branch")
                        .unwrap_or(false)
                }).count()
            })
            .unwrap_or_default();
        let loop_like = slice
            .get("control_flow_edges")
            .and_then(|v| v.as_array())
            .map(|edges| {
                edges.iter().filter(|edge| {
                    edge.get("kind")
                        .and_then(|v| v.as_str())
                        .map(|kind| kind == "LoopBack")
                        .unwrap_or(false)
                }).count()
            })
            .unwrap_or_default();
        let mut out = slice;
        if let Some(obj) = out.as_object_mut() {
            obj.insert(
                "metrics".to_string(),
                json!({
                    "branch_points": branch_like,
                    "loop_points": loop_like,
                }),
            );
        }
        Ok(out)
    }

    pub fn get_control_flow_slice(&self, symbol: &str) -> Result<serde_json::Value> {
        let sym = self
            .storage
            .get_symbol_any(symbol)?
            .ok_or_else(|| anyhow!("symbol not found: {symbol}"))?;
        let symbol_id = sym.id.ok_or_else(|| anyhow!("symbol id missing"))?;
        let mut nodes = self.storage.get_logic_nodes(symbol_id)?;
        sort_logic_nodes(&mut nodes);
        let edges = self.storage.get_control_flow_edges(symbol_id)?;

        Ok(json!({
            "symbol": sym.name,
            "file": sym.file,
            "control_flow_nodes": nodes,
            "control_flow_edges": edges,
        }))
    }

    pub fn get_data_flow_hints(&self, symbol: &str) -> Result<serde_json::Value> {
        let slice = self.get_data_flow_slice(symbol)?;
        let mut identifier_freq: HashMap<String, usize> = HashMap::new();
        if let Some(edges) = slice.get("data_flow_edges").and_then(|v| v.as_array()) {
            for edge in edges {
                if let Some(name) = edge.get("variable_name").and_then(|v| v.as_str()) {
                    *identifier_freq.entry(name.to_string()).or_insert(0) += 1;
                }
            }
        }
        let mut top_identifiers = identifier_freq.into_iter().collect::<Vec<_>>();
        top_identifiers.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        top_identifiers.truncate(10);
        let mut out = slice;
        let assignments = out
            .get("data_flow_edges")
            .and_then(|v| v.as_array())
            .map(|edges| edges.iter().filter(|e| e.get("kind").and_then(|v| v.as_str()) == Some("AssignmentToUse")).count())
            .unwrap_or_default();
        let returns = out
            .get("data_flow_edges")
            .and_then(|v| v.as_array())
            .map(|edges| edges.iter().filter(|e| e.get("kind").and_then(|v| v.as_str()) == Some("AssignmentToReturn")).count())
            .unwrap_or_default();
        let calls = out
            .get("data_flow_edges")
            .and_then(|v| v.as_array())
            .map(|edges| edges.iter().filter(|e| e.get("kind").and_then(|v| v.as_str()) == Some("CallResult")).count())
            .unwrap_or_default();
        if let Some(obj) = out.as_object_mut() {
            obj.insert(
                "data_flow_hints".to_string(),
                json!({
                    "assignments": assignments,
                    "calls": calls,
                    "returns": returns,
                    "top_identifiers": top_identifiers,
                }),
            );
        }
        Ok(out)
    }

    pub fn get_data_flow_slice(&self, symbol: &str) -> Result<serde_json::Value> {
        let sym = self
            .storage
            .get_symbol_any(symbol)?
            .ok_or_else(|| anyhow!("symbol not found: {symbol}"))?;
        let symbol_id = sym.id.ok_or_else(|| anyhow!("symbol id missing"))?;
        let mut nodes = self.storage.get_logic_nodes(symbol_id)?;
        sort_logic_nodes(&mut nodes);
        let edges = self.storage.get_data_flow_edges(symbol_id)?;

        Ok(json!({
            "symbol": sym.name,
            "file": sym.file,
            "logic_nodes": nodes,
            "data_flow_edges": edges,
        }))
    }

    pub fn get_logic_clusters(&self, symbol: &str) -> Result<serde_json::Value> {
        let sym = self
            .storage
            .get_symbol_any(symbol)?
            .ok_or_else(|| anyhow!("symbol not found: {symbol}"))?;
        let symbol_id = sym.id.ok_or_else(|| anyhow!("symbol id missing"))?;
        let clusters = self.storage.get_logic_clusters(symbol_id)?;
        Ok(json!({
            "symbol": sym.name,
            "file": sym.file,
            "logic_clusters": clusters,
        }))
    }

    pub fn get_hybrid_ranked_context(
        &self,
        query: &str,
        max_tokens: usize,
        single_file_fast_path: bool,
    ) -> Result<serde_json::Value> {
        let planned =
            self.get_planned_context(
                query,
                max_tokens,
                single_file_fast_path,
                Some(false),
                None,
                Some("footprint_first"),
                Some(120),
            )?;
        let symbol = planned
            .get("symbol")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        if symbol.is_empty() {
            return Ok(
                json!({ "query": query, "ranked_context": [], "strategy": "hybrid_ranked_context" }),
            );
        }

        let control = self
            .get_control_flow_slice(&symbol)
            .unwrap_or_else(|_| json!({}));
        let data = self
            .get_data_flow_slice(&symbol)
            .unwrap_or_else(|_| json!({}));
        let clusters = self
            .get_logic_clusters(&symbol)
            .unwrap_or_else(|_| json!({}));

        let graph_focus_files: HashSet<String> = clusters
            .get("logic_clusters")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
            .filter_map(|_| control.get("file").and_then(|v| v.as_str()).map(|s| s.to_string()))
            .collect();
        let control_edge_count = control
            .get("control_flow_edges")
            .and_then(|v| v.as_array())
            .map(|v| v.len() as i64)
            .unwrap_or_default();
        let data_edge_count = data
            .get("data_flow_edges")
            .and_then(|v| v.as_array())
            .map(|v| v.len() as i64)
            .unwrap_or_default();
        let cluster_count = clusters
            .get("logic_clusters")
            .and_then(|v| v.as_array())
            .map(|v| v.len() as i64)
            .unwrap_or_default();

        let mut ranked_context = planned
            .get("context")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        for item in &mut ranked_context {
            let mut score = 100i64;
            let priority = item.get("priority").and_then(|v| v.as_i64()).unwrap_or(3);
            score -= priority * 10;
            let raw_included = item
                .get("raw_included")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if raw_included {
                score += 5;
            }
            if let Some(file) = item.get("file").and_then(|v| v.as_str()) {
                if graph_focus_files.contains(file) {
                    score += 12;
                }
            }
            score += (control_edge_count.min(20)) / 4;
            score += (data_edge_count.min(20)) / 5;
            score += cluster_count.min(6);
            if let Some(obj) = item.as_object_mut() {
                obj.insert("hybrid_score".to_string(), json!(score));
            }
        }
        ranked_context.sort_by(|a, b| {
            let ascore = a
                .get("hybrid_score")
                .and_then(|v| v.as_i64())
                .unwrap_or_default();
            let bscore = b
                .get("hybrid_score")
                .and_then(|v| v.as_i64())
                .unwrap_or_default();
            bscore.cmp(&ascore)
        });

        Ok(json!({
            "query": query,
            "symbol": symbol,
            "strategy": "hybrid_ranked_context",
            "control_flow_hints": self.get_control_flow_hints(&symbol).unwrap_or_else(|_| json!({})),
            "data_flow_hints": self.get_data_flow_hints(&symbol).unwrap_or_else(|_| json!({})),
            "logic_clusters": clusters.get("logic_clusters").cloned().unwrap_or_else(|| json!([])),
            "graph_rank_signals": {
                "control_flow_edges": control_edge_count,
                "data_flow_edges": data_edge_count,
                "logic_clusters": cluster_count,
            },
            "ranked_context": ranked_context,
        }))
    }

    fn try_get_cached_context(&self, key: &str, ttl_seconds: u64) -> Option<serde_json::Value> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or_default();
        let current_revision = current_index_revision(&self.repo_root);
        let Some(entry) = self
            .storage
            .get_retrieval_cache_entry(key, "planned_context")
            .ok()
            .flatten()
        else {
            return None;
        };
        if now.saturating_sub(entry.cached_at_epoch_s) > ttl_seconds
            || entry.source_revision != current_revision
        {
            let _ = self
                .storage
                .delete_retrieval_cache_entry(key, "planned_context");
            let mut perf = self.perf_stats.lock().expect("perf lock");
            perf.cache_evictions += 1;
            return None;
        }
        let mut value = entry
            .value_json
            .as_deref()
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())?;
        if let Some(obj) = value.as_object_mut() {
            obj.insert("cache".to_string(), json!({ "hit": true }));
        }
        Some(value)
    }

    fn store_cached_context(&self, key: String, value: serde_json::Value, max_entries: usize) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or_default();
        let source_revision = current_index_revision(&self.repo_root);
        let serialized = serde_json::to_string(&value).ok();
        let _ = self.storage.upsert_retrieval_cache_entry(&storage::RetrievalCacheEntry {
            cache_key: key,
            cache_kind: "planned_context".to_string(),
            value_json: serialized,
            prompt_text: None,
            cached_at_epoch_s: now,
            source_revision,
        });
        let evicted = self
            .storage
            .prune_retrieval_cache_kind("planned_context", max_entries)
            .unwrap_or_default();
        if evicted > 0 {
            let mut perf = self.perf_stats.lock().expect("perf lock");
            perf.cache_evictions += evicted;
        }
    }

    fn try_get_symbol_cache(&self, key: &str, ttl_seconds: u64) -> Option<serde_json::Value> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or_default();
        let current_revision = current_index_revision(&self.repo_root);
        let mut cache = self
            .symbol_neighborhood_cache
            .lock()
            .expect("symbol cache lock");
        let Some(entry) = cache.get_mut(key) else {
            return None;
        };
        if now.saturating_sub(entry.cached_at_epoch_s) > ttl_seconds
            || entry.source_revision != current_revision
        {
            cache.remove(key);
            drop(cache);
            let mut perf = self.perf_stats.lock().expect("perf lock");
            perf.symbol_cache_evictions += 1;
            return None;
        }
        Some(entry.value.clone())
    }

    fn store_symbol_cache(&self, key: String, value: serde_json::Value, max_entries: usize) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or_default();
        let source_revision = current_index_revision(&self.repo_root);
        let mut cache = self
            .symbol_neighborhood_cache
            .lock()
            .expect("symbol cache lock");
        let mut evicted = false;
        if cache.len() >= max_entries {
            if let Some(oldest_key) = cache
                .iter()
                .min_by_key(|(_, v)| v.cached_at_epoch_s)
                .map(|(k, _)| k.clone())
            {
                cache.remove(&oldest_key);
                evicted = true;
            }
        }
        cache.insert(
            key,
            CachedContext {
                cached_at_epoch_s: now,
                source_revision,
                value,
            },
        );
        drop(cache);
        if evicted {
            let mut perf = self.perf_stats.lock().expect("perf lock");
            perf.symbol_cache_evictions += 1;
        }
    }

    fn build_light_prompt_cached(
        &self,
        base_prompt: &str,
        refs: &[serde_json::Value],
        count: usize,
    ) -> String {
        let refs_key = serde_json::to_string(&refs.iter().take(count).collect::<Vec<_>>())
            .unwrap_or_default();
        let key = format!("v1::{count}::{refs_key}::{base_prompt}");
        if let Some(prompt) = self.try_get_prompt_fragment(&key, 1800) {
            let mut perf = self.perf_stats.lock().expect("perf lock");
            perf.prompt_cache_hits += 1;
            return prompt;
        }
        {
            let mut perf = self.perf_stats.lock().expect("perf lock");
            perf.prompt_cache_misses += 1;
        }
        let prompt = build_light_prompt_from_refs(base_prompt, refs, count);
        self.store_prompt_fragment(key, prompt.clone(), 512);
        prompt
    }

    fn try_get_prompt_fragment(&self, key: &str, ttl_seconds: u64) -> Option<String> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or_default();
        let current_revision = current_index_revision(&self.repo_root);
        let mut cache = self
            .prompt_fragment_cache
            .lock()
            .expect("prompt cache lock");
        let Some(entry) = cache.get_mut(key) else {
            return None;
        };
        if now.saturating_sub(entry.cached_at_epoch_s) > ttl_seconds
            || entry.source_revision != current_revision
        {
            cache.remove(key);
            let mut perf = self.perf_stats.lock().expect("perf lock");
            perf.prompt_cache_evictions += 1;
            return None;
        }
        Some(entry.prompt.clone())
    }

    fn store_prompt_fragment(&self, key: String, prompt: String, max_entries: usize) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or_default();
        let source_revision = current_index_revision(&self.repo_root);
        let mut cache = self
            .prompt_fragment_cache
            .lock()
            .expect("prompt cache lock");
        let mut evicted = false;
        if cache.len() >= max_entries {
            if let Some(oldest_key) = cache
                .iter()
                .min_by_key(|(_, v)| v.cached_at_epoch_s)
                .map(|(k, _)| k.clone())
            {
                cache.remove(&oldest_key);
                evicted = true;
            }
        }
        cache.insert(
            key,
            CachedPrompt {
                cached_at_epoch_s: now,
                source_revision,
                prompt,
            },
        );
        if evicted {
            let mut perf = self.perf_stats.lock().expect("perf lock");
            perf.prompt_cache_evictions += 1;
        }
    }

    fn optimize_retrieval_breadth(
        &self,
        target_id: i64,
        base_logic_radius: usize,
        base_dependency_radius: usize,
        max_tokens: usize,
    ) -> Result<(usize, usize, serde_json::Value)> {
        let policy = load_retrieval_policy(&self.repo_root);
        let direct_deps = self.storage.get_symbol_dependencies(target_id)?.len();
        let callers = self.storage.get_symbol_callers(target_id)?.len();
        let logic_nodes = self.storage.get_logic_nodes(target_id)?.len();
        let fanout = direct_deps.saturating_add(callers);

        let mut logic_radius = base_logic_radius.max(1);
        let mut dependency_radius = base_dependency_radius.max(1);

        if max_tokens <= 1200 {
            logic_radius = 1;
            dependency_radius = 1;
        }
        if fanout > policy.high_fanout_threshold {
            dependency_radius = dependency_radius.min(1);
        } else if fanout < policy.low_fanout_threshold && max_tokens > 2600 {
            dependency_radius = dependency_radius.max(2).min(policy.max_dependency_radius);
        }
        if logic_nodes > policy.dense_logic_threshold {
            logic_radius = logic_radius.min(1);
        } else if logic_nodes < policy.sparse_logic_threshold && max_tokens > 2600 {
            logic_radius = logic_radius.max(2).min(policy.max_logic_radius);
        }

        Ok((
            logic_radius,
            dependency_radius,
            json!({
                "base_logic_radius": base_logic_radius,
                "base_dependency_radius": base_dependency_radius,
                "logic_radius": logic_radius,
                "dependency_radius": dependency_radius,
                "fanout": fanout,
                "direct_dependencies": direct_deps,
                "callers": callers,
                "logic_nodes": logic_nodes,
                "max_tokens": max_tokens,
                "policy": "adaptive_case_by_case",
                "policy_thresholds": {
                    "high_fanout_threshold": policy.high_fanout_threshold,
                    "low_fanout_threshold": policy.low_fanout_threshold,
                    "dense_logic_threshold": policy.dense_logic_threshold,
                    "sparse_logic_threshold": policy.sparse_logic_threshold,
                }
            }),
        ))
    }

    fn plan_safe_edit(
        &self,
        symbol: &str,
        edit_description: &str,
        max_tokens: usize,
        patch_mode: Option<engine::PatchApplicationMode>,
        run_tests: bool,
    ) -> Result<serde_json::Value> {
        load_env_from_file(&self.repo_root);
        let impact = impact_analysis::ImpactAnalyzer::analyze(&self.storage, symbol)?;
        let patch_memory = patch_memory::PatchMemory::open(&self.repo_root)?;
        let plan = safe_edit_planner::SafeEditPlanner::plan_with_memory(
            &self.storage,
            symbol,
            edit_description,
            &patch_memory,
        )?;

        let policy_config =
            std::fs::read_to_string(self.repo_root.join(".semantic").join("policies.toml"))
                .unwrap_or_default();
        let policies = policy_engine::PolicyEngine::from_toml(&policy_config)?;
        policies.validate_edit_plan(&plan)?;

        let routing_cfg =
            std::fs::read_to_string(self.repo_root.join(".semantic").join("llm_routing.toml"))
                .unwrap_or_default();
        let providers_cfg =
            std::fs::read_to_string(self.repo_root.join(".semantic").join("llm_config.toml"))
                .unwrap_or_default();
        let metrics_json =
            std::fs::read_to_string(self.repo_root.join(".semantic").join("model_metrics.json"))
                .unwrap_or_else(|_| "{}".to_string());
        let history_perf = patch_memory.model_performance(&patch_memory::PatchQuery::default())?;
        let merged_metrics = merge_metrics_json(&metrics_json, &history_perf);
        let router =
            llm_router::LLMRouter::from_files(&providers_cfg, &routing_cfg, &merged_metrics)?;
        let route = router.route(llm_router::LLMTask::CodeExecution);
        let provider_settings = parse_provider_settings(&providers_cfg);
        let live_llm_result = route.as_ref().and_then(|selected| {
            let prompt = format!(
                "You are editing symbol '{}'.\nFailure/Task: {}\nReturn concise fix guidance.",
                symbol, edit_description
            );
            call_live_llm(
                &selected.provider,
                provider_settings.get(&selected.provider),
                Some(&selected.endpoint),
                &prompt,
                512,
            )
        });

        let file_path = plan
            .required_context
            .first()
            .map(|c| c.file_path.clone())
            .unwrap_or_default();
        let patch = patch_engine::PatchEngine::generate_ast_patch(
            &file_path,
            &plan.target_symbol,
            engine::ASTTransformation::ReplaceFunctionBody,
        );
        let preview_diff = match &patch.representation {
            engine::PatchRepresentation::ASTTransform(ast_edit) => {
                patch_engine::PatchEngine::ast_to_diff(&patch.file_path, ast_edit)
            }
            engine::PatchRepresentation::UnifiedDiff(diff) => diff.clone(),
        };
        let existing_code = if file_path.is_empty() {
            String::new()
        } else {
            fs::read_to_string(self.repo_root.join(&file_path)).unwrap_or_default()
        };
        let validation_result =
            patch_engine::PatchEngine::validate_patch(&file_path, &patch, &existing_code);
        let validation_passed = validation_result.is_ok();
        let test_result = if run_tests {
            run_repo_tests(&self.repo_root)
        } else {
            None
        };
        let tests_passed = test_result
            .as_ref()
            .map(|result| result.passed)
            .unwrap_or(false);

        let application_mode = patch_mode.unwrap_or(engine::PatchApplicationMode::Confirm);
        let validation_cfg =
            std::fs::read_to_string(self.repo_root.join(".semantic").join("validation.toml"))
                .unwrap_or_default();

        let (provider, model_used) = route
            .as_ref()
            .map(|r| {
                let model = provider_settings
                    .get(&r.provider)
                    .map(|s| s.model.clone())
                    .unwrap_or_else(|| r.provider.clone());
                (r.provider.clone(), model)
            })
            .unwrap_or_else(|| ("unknown".to_string(), "unknown".to_string()));
        let ast_transform = match &patch.representation {
            engine::PatchRepresentation::ASTTransform(ast_edit) => {
                Some(ast_edit.transformation.clone())
            }
            engine::PatchRepresentation::UnifiedDiff(_) => None,
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or_default();
        let record = engine::PatchRecord {
            patch_id: patch_memory::PatchMemory::new_record_id(),
            timestamp: now,
            repository: self.repo_root.to_string_lossy().to_string(),
            file_path: patch.file_path.clone(),
            target_symbol: plan.target_symbol.clone(),
            edit_type: plan.edit_type.clone(),
            model_used,
            provider,
            diff: preview_diff.clone(),
            ast_transform,
            impacted_symbols: plan.impacted_symbols.clone(),
            approved_by_user: matches!(application_mode, engine::PatchApplicationMode::AutoApply),
            validation_passed,
            tests_passed,
            rollback_occurred: false,
            rollback_reason: None,
        };
        patch_memory.append_record(&record)?;

        Ok(json!({
            "impact_report": impact,
            "edit_plan": plan,
            "route": route.map(|r| json!({"provider": r.provider, "endpoint": r.endpoint})),
            "patch_preview": {
                "file_path": patch.file_path,
                "representation": patch.representation,
                "diff": preview_diff,
            },
            "live_llm": live_llm_result.map(|r| json!({
                "model": r.model,
                "total_tokens": r.total_tokens,
                "response_text": r.text,
            })),
            "patch_application_mode": application_mode,
            "run_tests": run_tests,
            "max_tokens": max_tokens,
            "validation_config": validation_cfg,
            "validation_result": {
                "passed": validation_passed,
                "error": validation_result.err().map(|err| err.to_string())
            },
            "test_result": test_result.map(|result| json!({
                "passed": result.passed,
                "command": result.command,
                "exit_code": result.exit_code,
                "stdout": truncate_chars(&result.stdout, 600),
                "stderr": truncate_chars(&result.stderr, 600),
            })),
            "patch_record_id": record.patch_id,
        }))
    }
}

fn collect_dependency_neighbors(
    storage: &storage::Storage,
    symbol_id: i64,
    radius: usize,
    include_callers: bool,
) -> Result<Vec<SymbolRecord>> {
    let mut queue = VecDeque::new();
    let mut visited = HashSet::new();
    queue.push_back((symbol_id, 0usize));
    visited.insert(symbol_id);

    let mut out = Vec::new();
    while let Some((current, depth)) = queue.pop_front() {
        if let Some(symbol) = storage.get_symbol_by_id(current)? {
            out.push(symbol);
        }
        if depth >= radius {
            continue;
        }

        let mut neighbors = storage.get_symbol_dependencies(current)?;
        if include_callers {
            neighbors.extend(storage.get_symbol_callers(current)?);
        }
        sort_symbols(&mut neighbors);
        neighbors.dedup_by_key(|s| s.id.unwrap_or_default());

        for symbol in neighbors {
            if let Some(next_id) = symbol.id {
                if visited.insert(next_id) {
                    queue.push_back((next_id, depth + 1));
                }
            }
        }
    }

    sort_symbols(&mut out);
    Ok(out)
}

fn sort_symbols(symbols: &mut [SymbolRecord]) {
    symbols.sort_by(|a, b| {
        a.file
            .cmp(&b.file)
            .then_with(|| a.start_line.cmp(&b.start_line))
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.id.unwrap_or_default().cmp(&b.id.unwrap_or_default()))
    });
}

fn sort_logic_nodes(nodes: &mut [LogicNodeRecord]) {
    nodes.sort_by_key(|n| (n.start_line, n.end_line, n.id.unwrap_or_default()));
}

#[derive(Debug, Clone)]
struct ProviderSetting {
    model: String,
    api_key_env: Option<String>,
}

#[derive(Debug, Clone)]
struct LLMCallResult {
    model: String,
    total_tokens: usize,
    text: String,
}

#[derive(Debug)]
struct ABTestRow {
    timestamp: u64,
    provider: String,
    symbol: String,
    tokens_without_project: usize,
    tokens_with_project: usize,
    savings_pct: f32,
}

fn parse_provider_settings(config: &str) -> HashMap<String, ProviderSetting> {
    let mut out = HashMap::new();
    let mut current_provider: Option<String> = None;
    let mut current_model: Option<String> = None;
    let mut current_api_key_env: Option<String> = None;

    let flush = |provider: &Option<String>,
                 model: &Option<String>,
                 api_key_env: &Option<String>,
                 out_map: &mut HashMap<String, ProviderSetting>| {
        if let Some(p) = provider {
            out_map.insert(
                p.clone(),
                ProviderSetting {
                    model: model.clone().unwrap_or_else(|| "gpt-4o-mini".to_string()),
                    api_key_env: api_key_env.clone(),
                },
            );
        }
    };

    for raw in config.lines() {
        let line = raw.trim();
        if line.starts_with('[') && line.ends_with(']') {
            flush(
                &current_provider,
                &current_model,
                &current_api_key_env,
                &mut out,
            );
            current_model = None;
            current_api_key_env = None;
            if line.starts_with("[provider_settings.") && line.ends_with(']') {
                let name = line
                    .trim_start_matches("[provider_settings.")
                    .trim_end_matches(']')
                    .to_string();
                current_provider = Some(name);
            } else {
                current_provider = None;
            }
            continue;
        }
        if current_provider.is_none() {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            let key = key.trim();
            let value = parse_toml_inline_value(value);
            if key == "model" {
                current_model = Some(value);
            } else if key == "api_key_env" {
                current_api_key_env = Some(value);
            }
        }
    }
    flush(
        &current_provider,
        &current_model,
        &current_api_key_env,
        &mut out,
    );
    out
}

fn parse_toml_inline_value(raw: &str) -> String {
    let mut in_quotes = false;
    let mut escaped = false;
    let mut out = String::new();
    for ch in raw.trim().chars() {
        if escaped {
            out.push(ch);
            escaped = false;
            continue;
        }
        if ch == '\\' && in_quotes {
            out.push(ch);
            escaped = true;
            continue;
        }
        if ch == '"' {
            in_quotes = !in_quotes;
            out.push(ch);
            continue;
        }
        if ch == '#' && !in_quotes {
            break;
        }
        out.push(ch);
    }
    out.trim().trim_matches('"').trim().to_string()
}

fn load_env_from_file(repo_root: &Path) {
    let env_path = repo_root.join(".semantic").join(".env");
    if !env_path.exists() {
        return;
    }
    let content = fs::read_to_string(env_path).unwrap_or_default();
    for raw in content.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            // Handle UTF-8 BOM in the first line key name (common on Windows PowerShell writes).
            let key = k.trim().trim_start_matches('\u{feff}');
            let value = v.trim().trim_matches('"');
            let should_set = match std::env::var(key) {
                Ok(existing) => existing.trim().is_empty(),
                Err(_) => true,
            };
            if should_set {
                unsafe {
                    std::env::set_var(key, value);
                }
            }
        }
    }
}

fn call_live_llm(
    provider: &str,
    provider_setting: Option<&ProviderSetting>,
    routed_endpoint: Option<&str>,
    prompt: &str,
    max_tokens: usize,
) -> Option<LLMCallResult> {
    call_live_llm_with_diagnostics(
        provider,
        provider_setting,
        routed_endpoint,
        prompt,
        max_tokens,
    )
    .ok()
}

fn call_live_llm_with_diagnostics(
    provider: &str,
    provider_setting: Option<&ProviderSetting>,
    routed_endpoint: Option<&str>,
    prompt: &str,
    max_tokens: usize,
) -> std::result::Result<LLMCallResult, String> {
    let setting = provider_setting.cloned().unwrap_or(ProviderSetting {
        model: default_model_for_provider(provider).to_string(),
        api_key_env: default_key_env_for_provider(provider).map(|s| s.to_string()),
    });
    let endpoint = routed_endpoint
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| default_endpoint_for_provider(provider).to_string());

    if provider == "ollama" {
        return call_ollama(&endpoint, &setting.model, prompt);
    }

    let api_key_env = setting
        .api_key_env
        .clone()
        .ok_or_else(|| format!("provider '{provider}' missing api_key_env setting"))?;
    let api_key =
        std::env::var(&api_key_env).map_err(|_| format!("missing env var: {api_key_env}"))?;
    if api_key.trim().is_empty() {
        return Err(format!("empty env var: {api_key_env}"));
    }

    if provider == "anthropic" {
        return call_anthropic(&endpoint, &api_key, &setting.model, prompt, max_tokens);
    }
    if provider == "gemini" {
        return call_gemini(&endpoint, &api_key, &setting.model, prompt, max_tokens);
    }
    call_openai_family(&endpoint, &api_key, &setting.model, prompt, max_tokens)
}

fn call_openai_family(
    endpoint: &str,
    api_key: &str,
    model: &str,
    prompt: &str,
    max_tokens: usize,
) -> std::result::Result<LLMCallResult, String> {
    let url = format!("{}/chat/completions", endpoint.trim_end_matches('/'));
    let client = reqwest::blocking::Client::new();
    let response = client
        .post(url)
        .header("Authorization", format!("Bearer {api_key}"))
        .header("Content-Type", "application/json")
        .json(&json!({
            "model": model,
            "messages": [{"role":"user","content": prompt}],
            "max_tokens": max_tokens
        }))
        .send()
        .map_err(|e| format!("openai-family network error: {e}"))?;
    if !response.status().is_success() {
        return Err(format!("openai-family http status: {}", response.status()));
    }
    let value: serde_json::Value = response
        .json()
        .map_err(|e| format!("openai-family invalid json: {e}"))?;
    let text = value
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|arr| arr.first())
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let tokens = value
        .get("usage")
        .and_then(|u| u.get("total_tokens"))
        .and_then(|v| v.as_u64())
        .map(|v| v as usize)
        .unwrap_or_else(|| estimate_tokens(&(prompt.to_string() + &text)));
    Ok(LLMCallResult {
        model: model.to_string(),
        total_tokens: tokens,
        text,
    })
}

fn call_anthropic(
    endpoint: &str,
    api_key: &str,
    model: &str,
    prompt: &str,
    max_tokens: usize,
) -> std::result::Result<LLMCallResult, String> {
    let url = format!("{}/messages", endpoint.trim_end_matches('/'));
    let client = reqwest::blocking::Client::new();
    let response = client
        .post(url)
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .header("Content-Type", "application/json")
        .json(&json!({
            "model": model,
            "max_tokens": max_tokens,
            "messages": [{"role":"user","content": prompt}]
        }))
        .send()
        .map_err(|e| format!("anthropic network error: {e}"))?;
    if !response.status().is_success() {
        return Err(format!("anthropic http status: {}", response.status()));
    }
    let value: serde_json::Value = response
        .json()
        .map_err(|e| format!("anthropic invalid json: {e}"))?;
    let text = value
        .get("content")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .and_then(|v| v.get("text"))
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let tokens = value
        .get("usage")
        .and_then(|u| u.get("input_tokens"))
        .and_then(|v| v.as_u64())
        .map(|v| v as usize + estimate_tokens(&text))
        .unwrap_or_else(|| estimate_tokens(&(prompt.to_string() + &text)));
    Ok(LLMCallResult {
        model: model.to_string(),
        total_tokens: tokens,
        text,
    })
}

fn call_gemini(
    endpoint: &str,
    api_key: &str,
    model: &str,
    prompt: &str,
    max_tokens: usize,
) -> std::result::Result<LLMCallResult, String> {
    let base = endpoint.trim_end_matches('/');
    let url = format!(
        "{}/v1beta/models/{}:generateContent?key={}",
        base, model, api_key
    );
    let client = reqwest::blocking::Client::new();
    let response = client
        .post(url)
        .header("Content-Type", "application/json")
        .json(&json!({
            "contents": [{"parts": [{"text": prompt}]}],
            "generationConfig": {"maxOutputTokens": max_tokens}
        }))
        .send()
        .map_err(|e| format!("gemini network error: {e}"))?;
    if !response.status().is_success() {
        return Err(format!("gemini http status: {}", response.status()));
    }
    let value: serde_json::Value = response
        .json()
        .map_err(|e| format!("gemini invalid json: {e}"))?;
    let text = value
        .get("candidates")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .and_then(|v| v.get("content"))
        .and_then(|v| v.get("parts"))
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .and_then(|v| v.get("text"))
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let tokens = value
        .get("usageMetadata")
        .and_then(|u| u.get("totalTokenCount"))
        .and_then(|v| v.as_u64())
        .map(|v| v as usize)
        .unwrap_or_else(|| estimate_tokens(&(prompt.to_string() + &text)));
    Ok(LLMCallResult {
        model: model.to_string(),
        total_tokens: tokens,
        text,
    })
}

fn call_ollama(
    endpoint: &str,
    model: &str,
    prompt: &str,
) -> std::result::Result<LLMCallResult, String> {
    let url = format!("{}/api/generate", endpoint.trim_end_matches('/'));
    let client = reqwest::blocking::Client::new();
    let response = client
        .post(url)
        .header("Content-Type", "application/json")
        .json(&json!({
            "model": model,
            "prompt": prompt,
            "stream": false
        }))
        .send()
        .map_err(|e| format!("ollama network error: {e}"))?;
    if !response.status().is_success() {
        return Err(format!("ollama http status: {}", response.status()));
    }
    let value: serde_json::Value = response
        .json()
        .map_err(|e| format!("ollama invalid json: {e}"))?;
    let text = value
        .get("response")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    Ok(LLMCallResult {
        model: model.to_string(),
        total_tokens: estimate_tokens(&(prompt.to_string() + &text)),
        text,
    })
}

fn default_endpoint_for_provider(provider: &str) -> &'static str {
    match provider {
        "anthropic" => "https://api.anthropic.com/v1",
        "gemini" => "https://generativelanguage.googleapis.com",
        "openrouter" => "https://openrouter.ai/api/v1",
        "together" => "https://api.together.xyz/v1",
        "ollama" => "http://127.0.0.1:11434",
        _ => "https://api.openai.com/v1",
    }
}

fn default_model_for_provider(provider: &str) -> &'static str {
    match provider {
        "anthropic" => "claude-3-5-sonnet-latest",
        "gemini" => "gemini-1.5-pro",
        "openrouter" => "openai/gpt-4o-mini",
        "together" => "meta-llama/Meta-Llama-3.1-70B-Instruct-Turbo",
        "ollama" => "llama3.1:8b",
        _ => "gpt-4o-mini",
    }
}

fn default_key_env_for_provider(provider: &str) -> Option<&'static str> {
    match provider {
        "anthropic" => Some("ANTHROPIC_API_KEY"),
        "gemini" => Some("GEMINI_API_KEY"),
        "openrouter" => Some("OPENROUTER_API_KEY"),
        "together" => Some("TOGETHER_API_KEY"),
        "ollama" => None,
        _ => Some("OPENAI_API_KEY"),
    }
}

fn estimate_tokens(text: &str) -> usize {
    ((text.len() as f32) / 4.0).ceil() as usize
}

fn build_context_payload(planned_context: &serde_json::Value, max_chars: usize) -> String {
    let mut out = String::new();
    let Some(items) = planned_context.get("context").and_then(|v| v.as_array()) else {
        return out;
    };

    for item in items {
        if out.len() >= max_chars {
            break;
        }
        let file = item
            .get("file")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let start = item
            .get("start")
            .and_then(|v| v.as_u64())
            .unwrap_or_default();
        let end = item.get("end").and_then(|v| v.as_u64()).unwrap_or_default();
        let code = item
            .get("code")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let trimmed = if code.len() > 700 {
            format!("{}...", &code[..700])
        } else {
            code.to_string()
        };
        let block = format!("File: {file}:{start}-{end}\n{trimmed}\n\n");
        if out.len() + block.len() > max_chars {
            break;
        }
        out.push_str(&block);
    }
    out
}

fn build_exact_context_payload(
    repo_root: &Path,
    ranges: &[ContextRange],
    max_chars: usize,
) -> String {
    let mut out = String::new();
    for range in ranges {
        if out.len() >= max_chars {
            break;
        }
        let mut path = repo_root.join(range.file);
        if !path.exists() {
            path = repo_root.join("test_repo").join(range.file);
        }
        let Ok(raw) = fs::read_to_string(path) else {
            continue;
        };
        let lines: Vec<&str> = raw.lines().collect();
        if lines.is_empty() {
            continue;
        }
        let start = range.start.max(1).min(lines.len());
        let end = range.end.max(start).min(lines.len());
        let snippet = lines[start - 1..end].join("\n");
        let block = format!("File: {}:{}-{}\n{}\n\n", range.file, start, end, snippet);
        if out.len() + block.len() > max_chars {
            break;
        }
        out.push_str(&block);
    }
    out
}

fn build_context_payload_from_edit_plan_or_fallback(
    repo_root: &Path,
    semantic_exec: Option<&serde_json::Value>,
    fallback_ranges: &[ContextRange],
    max_chars: usize,
) -> String {
    if let Some(exec) = semantic_exec {
        if let Some(items) = exec
            .get("edit_plan")
            .and_then(|v| v.get("required_context"))
            .and_then(|v| v.as_array())
        {
            let mut out = String::new();
            for item in items.iter().take(3) {
                if out.len() >= max_chars {
                    break;
                }
                let file = item
                    .get("file_path")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                let start = item
                    .get("start_line")
                    .and_then(|v| v.as_u64())
                    .unwrap_or_default() as usize;
                let end = item
                    .get("end_line")
                    .and_then(|v| v.as_u64())
                    .unwrap_or_default() as usize;
                if file.is_empty() || start == 0 || end == 0 {
                    continue;
                }
                let mut path = repo_root.join(file);
                if !path.exists() {
                    path = repo_root.join("test_repo").join(file);
                }
                let Ok(raw) = fs::read_to_string(path) else {
                    continue;
                };
                let lines: Vec<&str> = raw.lines().collect();
                if lines.is_empty() {
                    continue;
                }
                let clamped_start = start.max(1).min(lines.len());
                let clamped_end = end.max(clamped_start).min(lines.len());
                let snippet = lines[clamped_start - 1..clamped_end].join("\n");
                let block = format!(
                    "File: {}:{}-{}\n{}\n\n",
                    file, clamped_start, clamped_end, snippet
                );
                if block.is_empty() {
                    continue;
                }
                if out.len() + block.len() > max_chars {
                    break;
                }
                out.push_str(&block);
            }
            if !out.is_empty() {
                return out;
            }
        }
    }
    build_exact_context_payload(repo_root, fallback_ranges, max_chars)
}

fn build_structured_context_refs(
    planned_context: &serde_json::Value,
    max_items: usize,
) -> Vec<serde_json::Value> {
    planned_context
        .get("context")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .take(max_items)
                .map(|item| {
                    json!({
                        "file": item.get("file").and_then(|v| v.as_str()).unwrap_or_default(),
                        "module": item.get("module").and_then(|v| v.as_str()).unwrap_or_default(),
                        "start": item.get("start").and_then(|v| v.as_u64()).unwrap_or_default(),
                        "end": item.get("end").and_then(|v| v.as_u64()).unwrap_or_default(),
                        "priority": item.get("priority").and_then(|v| v.as_u64()).unwrap_or_default(),
                        "raw_included": item.get("raw_included").and_then(|v| v.as_bool()).unwrap_or(false)
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn normalize_query_tokens(input: &str) -> String {
    let mut out = String::new();
    let mut prev_space = true;
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_space = false;
        } else if !prev_space {
            out.push(' ');
            prev_space = true;
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn infer_file_objective(path: &str, top_symbols: &[String]) -> String {
    let p = path.to_lowercase();
    if p.contains("test") || p.contains("spec") {
        return "tests".to_string();
    }
    if p.contains("api") || p.contains("server") || p.contains("route") {
        return "api_surface".to_string();
    }
    if p.contains("store") || p.contains("repo") || p.contains("db") {
        return "data_layer".to_string();
    }
    if p.contains("ui") || p.contains("component") || p.contains("view") {
        return "ui_layer".to_string();
    }
    if top_symbols
        .iter()
        .any(|s| s.to_lowercase().contains("render") || s.to_lowercase().contains("component"))
    {
        return "ui_layer".to_string();
    }
    "application_logic".to_string()
}

fn build_task_candidate_set(
    query: &str,
    footprint: &[serde_json::Value],
    max_files: usize,
    max_symbols: usize,
) -> (Vec<String>, Vec<String>) {
    let query_norm = normalize_query_tokens(query);
    let tokens = query_norm
        .split_whitespace()
        .filter(|t| t.len() >= 2)
        .collect::<Vec<_>>();
    let mut scored: Vec<(i64, String, Vec<String>)> = Vec::new();
    for item in footprint {
        let file = item
            .get("file")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let objective = item
            .get("objective")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let symbols = item
            .get("top_symbols")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if file.is_empty() {
            continue;
        }
        let mut score = 0i64;
        for token in &tokens {
            if file.to_lowercase().contains(token) {
                score += 8;
            }
            if objective.to_lowercase().contains(token) {
                score += 6;
            }
            if symbols
                .iter()
                .any(|s| normalize_query_tokens(s).contains(token))
            {
                score += 10;
            }
        }
        if score > 0 {
            scored.push((score, file, symbols));
        }
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let mut files = Vec::new();
    let mut symbols = Vec::new();
    let mut seen_files = HashSet::new();
    let mut seen_symbols = HashSet::new();
    for (_, file, syms) in scored {
        if files.len() >= max_files {
            break;
        }
        if seen_files.insert(file.clone()) {
            files.push(file);
        }
        for sym in syms {
            if symbols.len() >= max_symbols {
                break;
            }
            if seen_symbols.insert(sym.to_lowercase()) {
                symbols.push(sym);
            }
        }
    }
    (files, symbols)
}

fn confidence_band(score: f32) -> &'static str {
    if score >= 0.75 {
        "high"
    } else if score >= 0.50 {
        "medium"
    } else {
        "low"
    }
}

fn context_tuning_for_task(
    task: &ABDevTask,
    impacted_file_count: usize,
    max_context_tokens: usize,
) -> ContextTuning {
    let unique_files = task
        .context_ranges
        .iter()
        .map(|r| r.file)
        .collect::<HashSet<_>>()
        .len();
    let cross_file = unique_files > 1 || impacted_file_count > 1;
    let dynamic_raw_cap = (max_context_tokens.saturating_mul(3)).clamp(500, 1800);

    if !task.requires_code_change {
        ContextTuning {
            ref_limit: 6,
            raw_max_chars: 0,
            escalation_hits_threshold: 1,
            guardrail_ratio: 1.2,
        }
    } else if cross_file {
        ContextTuning {
            ref_limit: 8,
            raw_max_chars: dynamic_raw_cap.min(1600),
            escalation_hits_threshold: 2,
            guardrail_ratio: 2.0,
        }
    } else {
        ContextTuning {
            ref_limit: 5,
            raw_max_chars: dynamic_raw_cap.min(900),
            escalation_hits_threshold: 1,
            guardrail_ratio: 1.5,
        }
    }
}

fn build_light_prompt_from_refs(
    base_prompt: &str,
    refs: &[serde_json::Value],
    count: usize,
) -> String {
    if refs.is_empty() || count == 0 {
        return base_prompt.to_string();
    }
    let subset: Vec<serde_json::Value> = refs.iter().take(count).cloned().collect();
    let refs_text = serde_json::to_string_pretty(&subset).unwrap_or_default();
    format!(
        "Structured context refs (delta from previous step):\n{}\n\nTask:\n{}",
        refs_text, base_prompt
    )
}

fn score_expected_terms(output: &str, terms: &[&str]) -> usize {
    let output_lc = output.to_lowercase();
    terms
        .iter()
        .filter(|t| output_lc.contains(&t.to_lowercase()))
        .count()
}

fn estimate_steps_without_semantic(success: bool, output: &str) -> Option<usize> {
    if !success {
        return None;
    }
    let file_mentions = output.matches(".ts").count() + output.matches(".tsx").count();
    Some(4 + file_mentions.min(3))
}

fn estimate_steps_with_semantic(
    success: bool,
    impacted_file_count: usize,
    signal_hits: usize,
) -> Option<usize> {
    if !success {
        return None;
    }
    // 1) retrieve planned context, 2) generate patch, 3) run validation/tests, + impacted files.
    Some(3 + impacted_file_count.min(3) + usize::from(signal_hits == 0))
}

fn ensure_todo_ab_project(repo_root: &Path) -> Result<()> {
    let base = if repo_root.join("test_repo").exists() {
        repo_root.join("test_repo").join("todo_app").join("src")
    } else {
        repo_root.join("todo_app").join("src")
    };
    fs::create_dir_all(&base)?;

    let files = [
        (base.join("types.ts"), TODO_TYPES_TS),
        (base.join("taskStore.ts"), TODO_TASK_STORE_TS),
        (base.join("taskService.ts"), TODO_TASK_SERVICE_TS),
        (base.join("menu.tsx"), TODO_MENU_TSX),
        (base.join("app.tsx"), TODO_APP_TSX),
    ];

    for (path, content) in files {
        if !path.exists() {
            fs::write(path, content)?;
        }
    }
    Ok(())
}

fn write_ab_test_suite_manifest(repo_root: &Path, tasks: &[ABDevTask]) -> Result<()> {
    let path = if repo_root.join("test_repo").exists() {
        repo_root
            .join("test_repo")
            .join("todo_app")
            .join("ab_test_suite_tasks.json")
    } else {
        repo_root.join("todo_app").join("ab_test_suite_tasks.json")
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let payload: Vec<serde_json::Value> = tasks
        .iter()
        .map(|t| {
            json!({
                "task_id": t.id,
                "title": t.title,
                "feature_request": t.feature_request,
                "semantic_query": t.semantic_query,
                "target_symbol": t.target_symbol,
                "requires_code_change": t.requires_code_change,
                "semantic_features": t.semantic_features,
            })
        })
        .collect();
    fs::write(path, serde_json::to_string_pretty(&payload)?)?;
    Ok(())
}

fn append_ab_test_task_metrics(repo_root: &Path, row: &serde_json::Value) -> Result<()> {
    let path = repo_root
        .join(".semantic")
        .join("ab_test_dev_task_metrics.jsonl");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(file, "{}", row)?;
    Ok(())
}

fn build_todo_dev_suite() -> Vec<ABDevTask> {
    vec![
        ABDevTask {
            id: "T01",
            title: "Add due date while creating tasks",
            feature_request: "Update createTask and addTask so new tasks can include dueDate and validation.",
            semantic_query: "todo app create task due date validation",
            target_symbol: "createTask",
            requires_code_change: true,
            semantic_features: vec!["GetPlannedContext", "PlanSafeEdit", "GetCodeSpan"],
            context_ranges: vec![
                ContextRange { file: "todo_app/src/taskStore.ts", start: 5, end: 23 },
                ContextRange { file: "todo_app/src/taskService.ts", start: 15, end: 21 },
            ],
            expected_terms: vec!["createTask", "dueDate", "validation", "taskStore.ts"],
        },
        ABDevTask {
            id: "T02",
            title: "Support due date edits",
            feature_request: "Implement setDueDate to update existing tasks and reject invalid dates.",
            semantic_query: "todo app set due date edit task",
            target_symbol: "setDueDate",
            requires_code_change: true,
            semantic_features: vec!["GetPlannedContext", "GetLogicNodes", "PlanSafeEdit"],
            context_ranges: vec![
                ContextRange { file: "todo_app/src/taskStore.ts", start: 36, end: 41 },
                ContextRange { file: "todo_app/src/taskService.ts", start: 31, end: 33 },
            ],
            expected_terms: vec!["setDueDate", "ISO", "taskStore.ts"],
        },
        ABDevTask {
            id: "T03",
            title: "Reorder by priority",
            feature_request: "Replace reorderPriority with deterministic HIGH/MEDIUM/LOW ordering and stable ordering by id.",
            semantic_query: "todo app reorder tasks by priority stable sort",
            target_symbol: "reorderPriority",
            requires_code_change: true,
            semantic_features: vec!["GetPlannedContext", "GetDependencyNeighborhood", "PlanSafeEdit"],
            context_ranges: vec![ContextRange { file: "todo_app/src/taskStore.ts", start: 50, end: 52 }],
            expected_terms: vec!["reorderPriority", "HIGH", "MEDIUM", "LOW"],
        },
        ABDevTask {
            id: "T04",
            title: "Add tags on create",
            feature_request: "Allow addTask to receive tags and normalize tags to lowercase unique values.",
            semantic_query: "todo app add task tags normalize lowercase unique",
            target_symbol: "addTask",
            requires_code_change: true,
            semantic_features: vec!["SearchSymbol", "GetPlannedContext", "PlanSafeEdit"],
            context_ranges: vec![
                ContextRange { file: "todo_app/src/taskService.ts", start: 15, end: 21 },
                ContextRange { file: "todo_app/src/taskStore.ts", start: 5, end: 23 },
            ],
            expected_terms: vec!["addTask", "tags", "normalize", "lowercase"],
        },
        ABDevTask {
            id: "T05",
            title: "Add tag mutation endpoint",
            feature_request: "Implement addTag and removeTag in store with dedupe behavior and not-found safety.",
            semantic_query: "todo app addTag removeTag dedupe",
            target_symbol: "addTag",
            requires_code_change: true,
            semantic_features: vec!["GetPlannedContext", "GetReasoningContext", "PlanSafeEdit"],
            context_ranges: vec![ContextRange { file: "todo_app/src/taskStore.ts", start: 54, end: 67 }],
            expected_terms: vec!["addTag", "removeTag", "dedupe", "task id"],
        },
        ABDevTask {
            id: "T06",
            title: "Filter by tag",
            feature_request: "Fix filterByTag so it matches case-insensitive tags and excludes completed tasks by default.",
            semantic_query: "todo app filter by tag case insensitive incomplete",
            target_symbol: "filterByTag",
            requires_code_change: true,
            semantic_features: vec!["GetPlannedContext", "GetLogicNodes", "PlanSafeEdit"],
            context_ranges: vec![ContextRange { file: "todo_app/src/taskStore.ts", start: 69, end: 76 }],
            expected_terms: vec!["filterByTag", "case-insensitive", "completed"],
        },
        ABDevTask {
            id: "T07",
            title: "Overdue task detection",
            feature_request: "Implement listOverdueTasks using dueDate and current time injection for testability.",
            semantic_query: "todo app overdue tasks due date current time injection",
            target_symbol: "listOverdueTasks",
            requires_code_change: true,
            semantic_features: vec!["GetPlannedContext", "GetCodeSpan", "PlanSafeEdit"],
            context_ranges: vec![
                ContextRange { file: "todo_app/src/taskStore.ts", start: 73, end: 76 },
                ContextRange { file: "todo_app/src/types.ts", start: 1, end: 10 },
            ],
            expected_terms: vec!["listOverdueTasks", "dueDate", "Date.now", "inject"],
        },
        ABDevTask {
            id: "T08",
            title: "Propagate priority updates",
            feature_request: "Wire taskService.updatePriority to call store.setPriority and return updated task.",
            semantic_query: "todo app update priority service to store",
            target_symbol: "updatePriority",
            requires_code_change: true,
            semantic_features: vec!["GetPlannedContext", "GetDependencyNeighborhood", "PlanSafeEdit"],
            context_ranges: vec![
                ContextRange { file: "todo_app/src/taskService.ts", start: 27, end: 33 },
                ContextRange { file: "todo_app/src/taskStore.ts", start: 43, end: 48 },
            ],
            expected_terms: vec!["updatePriority", "setPriority", "taskService.ts"],
        },
        ABDevTask {
            id: "T09",
            title: "Task list sorting",
            feature_request: "Update listTasks to sort by priority then dueDate then createdAt.",
            semantic_query: "todo app list tasks sort priority due date created",
            target_symbol: "listTasks",
            requires_code_change: true,
            semantic_features: vec!["GetPlannedContext", "GetReasoningContext", "PlanSafeEdit"],
            context_ranges: vec![ContextRange { file: "todo_app/src/taskStore.ts", start: 25, end: 52 }],
            expected_terms: vec!["listTasks", "sort", "priority", "dueDate"],
        },
        ABDevTask {
            id: "T10",
            title: "End-to-end acceptance checklist",
            feature_request: "Create a test checklist for add due date, reorder priority, and tag flows with edge cases.",
            semantic_query: "todo app e2e test checklist due date priority tags",
            target_symbol: "addTask",
            requires_code_change: false,
            semantic_features: vec!["GetPlannedContext", "GetWorkspaceReasoningContext", "PlanSafeEdit"],
            context_ranges: vec![
                ContextRange { file: "todo_app/src/taskService.ts", start: 15, end: 57 },
                ContextRange { file: "todo_app/src/taskStore.ts", start: 25, end: 76 },
            ],
            expected_terms: vec!["test", "due date", "priority", "tags"],
        },
        ABDevTask {
            id: "T11",
            title: "UI integration: tools menu",
            feature_request: "Add a Tools menu section for due-date filter and tag quick-actions, and wire it into app.tsx navigation.",
            semantic_query: "todo app ui tools menu integrate actions app navigation",
            target_symbol: "TaskMenu",
            requires_code_change: true,
            semantic_features: vec!["GetPlannedContext", "GetDependencyNeighborhood", "PlanSafeEdit"],
            context_ranges: vec![
                ContextRange { file: "todo_app/src/menu.tsx", start: 1, end: 23 },
                ContextRange { file: "todo_app/src/app.tsx", start: 1, end: 11 },
            ],
            expected_terms: vec!["menu", "tools", "app.tsx", "tag", "due date"],
        },
    ]
}

fn build_todo_dev_suite_extended() -> Vec<ABDevTask> {
    let mut tasks = build_todo_dev_suite();
    tasks.extend([
        ABDevTask {
            id: "X12",
            title: "Cross-file workflow: add and render due-date badge",
            feature_request: "Update store, service, and UI rendering path to support due-date badge visibility and sorting interactions.",
            semantic_query: "todo app cross file due date badge ui service store",
            target_symbol: "renderAppHome",
            requires_code_change: true,
            semantic_features: vec!["GetPlannedContext", "GetReasoningContext", "PlanSafeEdit"],
            context_ranges: vec![
                ContextRange { file: "todo_app/src/taskStore.ts", start: 25, end: 76 },
                ContextRange { file: "todo_app/src/taskService.ts", start: 1, end: 57 },
                ContextRange { file: "todo_app/src/app.tsx", start: 1, end: 11 },
            ],
            expected_terms: vec!["due date", "renderAppHome", "taskStore.ts", "taskService.ts"],
        },
        ABDevTask {
            id: "R13",
            title: "Repeated workflow: optimize follow-up edit path",
            feature_request: "After implementing tag normalization, apply a follow-up refinement to tag filtering without reloading unrelated context.",
            semantic_query: "todo app repeated workflow tag normalization follow up filter refinement",
            target_symbol: "filterByTag",
            requires_code_change: true,
            semantic_features: vec!["GetPlannedContext", "GetDependencyNeighborhood", "PlanSafeEdit"],
            context_ranges: vec![
                ContextRange { file: "todo_app/src/taskStore.ts", start: 54, end: 76 },
            ],
            expected_terms: vec!["filterByTag", "tags", "normalization", "reuse"],
        },
        ABDevTask {
            id: "M14",
            title: "Medium footprint planning scenario",
            feature_request: "Produce an implementation plan that touches multiple modules with bounded context expansion for due-date, priority, and tags.",
            semantic_query: "todo app medium scenario multi module planning bounded context",
            target_symbol: "addTask",
            requires_code_change: false,
            semantic_features: vec!["GetPlannedContext", "GetWorkspaceReasoningContext", "PlanSafeEdit"],
            context_ranges: vec![
                ContextRange { file: "todo_app/src/taskService.ts", start: 1, end: 57 },
                ContextRange { file: "todo_app/src/taskStore.ts", start: 1, end: 76 },
                ContextRange { file: "todo_app/src/menu.tsx", start: 1, end: 23 },
            ],
            expected_terms: vec!["plan", "priority", "due date", "tags"],
        },
        ABDevTask {
            id: "L15",
            title: "Large footprint planning scenario",
            feature_request: "Generate a rollout checklist for scaling semantic retrieval behavior across a larger codebase while preserving edit precision.",
            semantic_query: "semantic retrieval large project rollout checklist precision token savings",
            target_symbol: "TaskMenu",
            requires_code_change: false,
            semantic_features: vec!["GetPlannedContext", "GetRepoMapHierarchy", "PlanSafeEdit"],
            context_ranges: vec![
                ContextRange { file: "todo_app/src/menu.tsx", start: 1, end: 23 },
                ContextRange { file: "todo_app/src/app.tsx", start: 1, end: 11 },
            ],
            expected_terms: vec!["rollout", "retrieval", "precision", "token"],
        },
    ]);
    tasks
}

const TODO_TYPES_TS: &str = r#"export type TaskPriority = "HIGH" | "MEDIUM" | "LOW";

export interface Task {
  id: string;
  title: string;
  completed: boolean;
  priority: TaskPriority;
  dueDate?: string;
  tags: string[];
  createdAt: string;
}
"#;

const TODO_TASK_STORE_TS: &str = r#"import { Task, TaskPriority } from "./types";

const tasks: Task[] = [];

export function createTask(input: {
  id: string;
  title: string;
  priority?: TaskPriority;
  dueDate?: string;
  tags?: string[];
}): Task {
  const task: Task = {
    id: input.id,
    title: input.title.trim(),
    completed: false,
    priority: input.priority ?? "MEDIUM",
    dueDate: input.dueDate,
    tags: input.tags ?? [],
    createdAt: new Date().toISOString(),
  };
  tasks.push(task);
  return task;
}

export function listTasks(): Task[] {
  return [...tasks];
}

export function completeTask(id: string): Task | undefined {
  const task = tasks.find((t) => t.id === id);
  if (!task) return undefined;
  task.completed = true;
  return task;
}

export function setDueDate(id: string, dueDate: string): Task | undefined {
  const task = tasks.find((t) => t.id === id);
  if (!task) return undefined;
  task.dueDate = dueDate;
  return task;
}

export function setPriority(id: string, priority: TaskPriority): Task | undefined {
  const task = tasks.find((t) => t.id === id);
  if (!task) return undefined;
  task.priority = priority;
  return task;
}

export function reorderPriority(): Task[] {
  return [...tasks].sort((a, b) => a.priority.localeCompare(b.priority));
}

export function addTag(id: string, tag: string): Task | undefined {
  const task = tasks.find((t) => t.id === id);
  if (!task) return undefined;
  task.tags.push(tag);
  return task;
}

export function removeTag(id: string, tag: string): Task | undefined {
  const task = tasks.find((t) => t.id === id);
  if (!task) return undefined;
  task.tags = task.tags.filter((t) => t !== tag);
  return task;
}

export function filterByTag(tag: string): Task[] {
  return tasks.filter((t) => t.tags.includes(tag));
}

export function listOverdueTasks(nowIso: string = new Date().toISOString()): Task[] {
  const now = Date.parse(nowIso);
  return tasks.filter((t) => !!t.dueDate && Date.parse(t.dueDate!) < now);
}
"#;

const TODO_TASK_SERVICE_TS: &str = r#"import {
  addTag,
  completeTask,
  createTask,
  filterByTag,
  listOverdueTasks,
  listTasks,
  removeTag,
  reorderPriority,
  setDueDate,
  setPriority,
} from "./taskStore";
import { Task, TaskPriority } from "./types";

export function addTask(title: string, priority: TaskPriority = "MEDIUM"): Task {
  return createTask({
    id: `task-${Math.random().toString(16).slice(2)}`,
    title,
    priority,
  });
}

export function finishTask(id: string): Task | undefined {
  return completeTask(id);
}

export function updatePriority(id: string, priority: TaskPriority): Task | undefined {
  return setPriority(id, priority);
}

export function updateDueDate(id: string, dueDate: string): Task | undefined {
  return setDueDate(id, dueDate);
}

export function attachTag(id: string, tag: string): Task | undefined {
  return addTag(id, tag);
}

export function detachTag(id: string, tag: string): Task | undefined {
  return removeTag(id, tag);
}

export function getTasksByTag(tag: string): Task[] {
  return filterByTag(tag);
}

export function getOrderedTasks(): Task[] {
  return reorderPriority();
}

export function getOverdueTasks(nowIso?: string): Task[] {
  return listOverdueTasks(nowIso);
}

export function allTasks(): Task[] {
  return listTasks();
}
"#;

const TODO_MENU_TSX: &str = r#"export type ToolAction = {
  id: string;
  label: string;
  kind: "due_date" | "tag";
};

const tools: ToolAction[] = [
  { id: "due-today", label: "Due Today", kind: "due_date" },
  { id: "tag-bug", label: "Tag: bug", kind: "tag" },
];

export function registerTool(action: ToolAction): ToolAction[] {
  tools.push(action);
  return [...tools];
}

export function listToolActions(): ToolAction[] {
  return [...tools];
}

export function TaskMenu(): string {
  return tools.map((t) => t.label).join(" | ");
}
"#;

const TODO_APP_TSX: &str = r#"import { TaskMenu, listToolActions } from "./menu";
import { allTasks } from "./taskService";

export function renderAppHome(): string {
  const taskCount = allTasks().length;
  return `Tasks(${taskCount}) :: ${TaskMenu()}`;
}

export function getToolsPanel(): string[] {
  return listToolActions().map((a) => `${a.kind}:${a.label}`);
}
"#;

fn append_ab_test_csv(repo_root: &Path, row: &ABTestRow) -> Result<()> {
    let path = repo_root.join(".semantic").join("ab_test_results.csv");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    if file.metadata()?.len() == 0 {
        writeln!(
            file,
            "timestamp,provider,symbol,tokens_without_project,tokens_with_project,savings_pct"
        )?;
    }
    writeln!(
        file,
        "{},{},{},{},{},{:.2}",
        row.timestamp,
        row.provider,
        row.symbol,
        row.tokens_without_project,
        row.tokens_with_project,
        row.savings_pct
    )?;
    Ok(())
}

fn read_ab_test_csv(repo_root: &Path) -> Result<Vec<serde_json::Value>> {
    let path = repo_root.join(".semantic").join("ab_test_results.csv");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content = fs::read_to_string(path)?;
    let mut rows = Vec::new();
    for (idx, raw) in content.lines().enumerate() {
        if idx == 0 {
            continue;
        }
        let parts: Vec<&str> = raw.split(',').collect();
        if parts.len() < 6 {
            continue;
        }
        rows.push(json!({
            "timestamp": parts[0].trim().parse::<u64>().unwrap_or_default(),
            "provider": parts[1].trim(),
            "symbol": parts[2].trim(),
            "tokens_without_project": parts[3].trim().parse::<usize>().unwrap_or_default(),
            "tokens_with_project": parts[4].trim().parse::<usize>().unwrap_or_default(),
            "savings_pct": parts[5].trim().parse::<f32>().unwrap_or_default(),
        }));
    }
    Ok(rows)
}

fn current_ts() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

fn merge_metrics_json(base_metrics_json: &str, performance: &[engine::ModelPerformance]) -> String {
    let mut base: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str::<serde_json::Value>(base_metrics_json)
            .ok()
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default();

    for perf in performance {
        let entry = base.entry(perf.model.clone()).or_insert_with(|| json!({}));
        if let Some(obj) = entry.as_object_mut() {
            obj.insert("success_rate".to_string(), json!(perf.success_rate));
            if !obj.contains_key("latency_ms") {
                obj.insert("latency_ms".to_string(), json!(perf.avg_latency_ms));
            }
            if !obj.contains_key("token_cost") {
                obj.insert("token_cost".to_string(), json!(perf.avg_cost));
            }
        }
    }

    serde_json::to_string(&base).unwrap_or_else(|_| "{}".to_string())
}

fn estimate_evolution_risk(
    repo_root: &Path,
    storage: &storage::Storage,
    impacted_plan_count: usize,
) -> Result<engine::EvolutionRisk> {
    let memory = patch_memory::PatchMemory::open(repo_root)?;
    let stats = memory.stats(&patch_memory::PatchQuery::default())?;
    let failure_rate = 1.0f32 - stats.success_rate;
    let impacted_files = impacted_plan_count as f32;
    let file_count = storage.list_files()?.len();
    let test_files = storage
        .list_files()?
        .into_iter()
        .filter(|f| {
            let l = f.to_lowercase();
            l.contains("test") || l.contains("spec")
        })
        .count();
    let coverage_signal = if file_count == 0 {
        0.0
    } else {
        1.0 - (test_files as f32 / file_count as f32)
    };

    let risk_score =
        (failure_rate * 0.5) + ((impacted_files / 100.0).min(1.0) * 0.3) + (coverage_signal * 0.2);
    Ok(engine::EvolutionRisk {
        risk_score,
        reasoning: format!(
            "failure_rate={:.2}, impacted_plans={}, low_test_signal={:.2}",
            failure_rate, impacted_plan_count, coverage_signal
        ),
    })
}

fn module_rank_for_file(
    file_to_module: &std::collections::HashMap<String, String>,
    scoped_modules: &[String],
    file_path: &str,
) -> u8 {
    let Some(module_name) = file_to_module.get(file_path) else {
        return 2;
    };
    if let Some(pos) = scoped_modules.iter().position(|m| m == module_name) {
        if pos == 0 {
            0
        } else {
            1
        }
    } else {
        2
    }
}

fn current_index_revision(repo_root: &Path) -> u64 {
    let (db_path, _) = storage::default_paths(repo_root);
    std::fs::metadata(db_path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

fn load_index_performance_stats(repo_root: &Path) -> serde_json::Value {
    let path = repo_root.join(".semantic").join("index_performance.json");
    let Ok(raw) = fs::read_to_string(path) else {
        return json!({
            "status": "unavailable",
            "note": "index performance stats have not been written yet"
        });
    };
    serde_json::from_str(&raw).unwrap_or_else(|_| {
        json!({
            "status": "invalid",
            "note": "index performance stats could not be parsed"
        })
    })
}

fn load_retrieval_policy(repo_root: &Path) -> RetrievalPolicy {
    let path = repo_root.join(".semantic").join("retrieval_policy.toml");
    let Ok(raw) = fs::read_to_string(path) else {
        return RetrievalPolicy::default();
    };

    let mut policy = RetrievalPolicy::default();
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('[') {
            continue;
        }
        let Some((key, value)) = trimmed.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim().trim_matches('"');
        match key {
            "high_fanout_threshold" => assign_usize(value, &mut policy.high_fanout_threshold),
            "low_fanout_threshold" => assign_usize(value, &mut policy.low_fanout_threshold),
            "dense_logic_threshold" => assign_usize(value, &mut policy.dense_logic_threshold),
            "sparse_logic_threshold" => assign_usize(value, &mut policy.sparse_logic_threshold),
            "max_dependency_radius" => assign_usize(value, &mut policy.max_dependency_radius),
            "max_logic_radius" => assign_usize(value, &mut policy.max_logic_radius),
            "plan_token_cap" => assign_usize(value, &mut policy.plan_token_cap),
            "lookup_token_cap" => assign_usize(value, &mut policy.lookup_token_cap),
            "edit_token_cap" => assign_usize(value, &mut policy.edit_token_cap),
            "small_repo_file_threshold" => {
                assign_usize(value, &mut policy.small_repo_file_threshold)
            }
            "anti_bloat_small_task" => assign_bool(value, &mut policy.anti_bloat_small_task),
            "p95_latency_alert_ms" => assign_u128(value, &mut policy.p95_latency_alert_ms),
            "p99_latency_alert_ms" => assign_u128(value, &mut policy.p99_latency_alert_ms),
            "min_cache_hit_rate_pct" => assign_f64(value, &mut policy.min_cache_hit_rate_pct),
            "prompt_overrun_alert_pct" => assign_f64(value, &mut policy.prompt_overrun_alert_pct),
            "step_regression_alert_pct" => assign_f64(value, &mut policy.step_regression_alert_pct),
            _ => {}
        }
    }
    policy
}

fn clamp_tokens_for_operation(repo_root: &Path, operation_kind: &str, requested: usize) -> usize {
    let policy = load_retrieval_policy(repo_root);
    match operation_kind {
        "plan" => requested.min(policy.plan_token_cap),
        "edit" => requested.min(policy.edit_token_cap),
        _ => requested.min(policy.lookup_token_cap),
    }
}

fn ratio_pct(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        (numerator as f64 / denominator as f64) * 100.0
    }
}

fn build_observability_alerts(
    op_stats: &[serde_json::Value],
    cache_hit_rate_pct: f64,
    policy: RetrievalPolicy,
) -> serde_json::Value {
    let mut alerts = Vec::new();
    if cache_hit_rate_pct > 0.0 && cache_hit_rate_pct < policy.min_cache_hit_rate_pct {
        alerts.push(json!({
            "severity": "warning",
            "kind": "cache_hit_rate",
            "message": format!("planned-context cache hit rate {:.1}% is below threshold {:.1}%", cache_hit_rate_pct, policy.min_cache_hit_rate_pct)
        }));
    }
    for op in op_stats {
        let name = op.get("operation").and_then(|v| v.as_str()).unwrap_or("unknown");
        let p95 = op.get("p95_ms").and_then(|v| v.as_u64()).unwrap_or_default() as u128;
        let p99 = op.get("p99_ms").and_then(|v| v.as_u64()).unwrap_or_default() as u128;
        if p95 > policy.p95_latency_alert_ms {
            alerts.push(json!({
                "severity": "warning",
                "kind": "latency_p95",
                "operation": name,
                "message": format!("p95 {}ms exceeds threshold {}ms", p95, policy.p95_latency_alert_ms)
            }));
        }
        if p99 > policy.p99_latency_alert_ms {
            alerts.push(json!({
                "severity": "warning",
                "kind": "latency_p99",
                "operation": name,
                "message": format!("p99 {}ms exceeds threshold {}ms", p99, policy.p99_latency_alert_ms)
            }));
        }
    }
    json!({
        "thresholds": {
            "p95_latency_alert_ms": policy.p95_latency_alert_ms,
            "p99_latency_alert_ms": policy.p99_latency_alert_ms,
            "min_cache_hit_rate_pct": policy.min_cache_hit_rate_pct,
        },
        "items": alerts
    })
}

fn assign_usize(value: &str, out: &mut usize) {
    if let Ok(parsed) = value.parse::<usize>() {
        *out = parsed;
    }
}

fn assign_u128(value: &str, out: &mut u128) {
    if let Ok(parsed) = value.parse::<u128>() {
        *out = parsed;
    }
}

fn assign_f64(value: &str, out: &mut f64) {
    if let Ok(parsed) = value.parse::<f64>() {
        *out = parsed;
    }
}

fn assign_bool(value: &str, out: &mut bool) {
    if let Ok(parsed) = value.parse::<bool>() {
        *out = parsed;
    }
}

fn run_repo_tests(repo_root: &Path) -> Option<TestRunResult> {
    if repo_root.join("Cargo.toml").exists() {
        return run_command(
            repo_root,
            if cfg!(windows) { "cargo.exe" } else { "cargo" },
            &["test", "--lib", "--quiet"],
        );
    }
    if repo_root.join("package.json").exists() {
        return run_command(
            repo_root,
            if cfg!(windows) { "npm.cmd" } else { "npm" },
            &["test", "--", "--runInBand"],
        );
    }
    None
}

fn run_command(repo_root: &Path, command: &str, args: &[&str]) -> Option<TestRunResult> {
    let output = std::process::Command::new(command)
        .args(args)
        .current_dir(repo_root)
        .output()
        .ok()?;
    Some(TestRunResult {
        passed: output.status.success(),
        command: format!("{command} {}", args.join(" ")).trim().to_string(),
        exit_code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    })
}

fn truncate_chars(input: &str, max_chars: usize) -> String {
    if input.chars().count() <= max_chars {
        input.to_string()
    } else {
        let kept: String = input.chars().take(max_chars).collect();
        format!("{kept}...")
    }
}

fn read_span(
    repo_root: &Path,
    relative_file: &str,
    start_line: u32,
    end_line: u32,
) -> Result<String> {
    let full_path = repo_root.join(relative_file);
    let content = fs::read_to_string(full_path)?;
    let mut out = Vec::new();

    for (idx, line) in content.lines().enumerate() {
        let line_no = idx as u32 + 1;
        if line_no >= start_line && line_no <= end_line {
            out.push(line.to_string());
        }
    }

    Ok(out.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::RetrievalService;
    use engine::{
        LogicNodeRecord, LogicNodeType, Operation, RetrievalRequest, SymbolRecord, SymbolType,
    };
    use std::fs;
    use storage::Storage;

    #[test]
    fn returns_code_span_for_function() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join("src")).expect("mkdir src");
        fs::write(
            repo.join("src").join("client.ts"),
            "function retryRequest(){\n  return 1;\n}\n",
        )
        .expect("write file");

        let db = tmp.path().join("semantic.db");
        let idx = tmp.path().join("tantivy");
        let mut storage = Storage::open(&db, &idx).expect("open storage");
        storage
            .upsert_file("src/client.ts", "typescript", "x")
            .expect("upsert file");
        let ids = storage
            .insert_symbols(&[SymbolRecord {
                id: None,
                repo_id: 0,
                name: "retryRequest".into(),
                symbol_type: SymbolType::Function,
                file: "src/client.ts".into(),
                start_line: 1,
                end_line: 3,
                language: "typescript".into(),
                summary: "Function retryRequest".into(),
            }])
            .expect("insert symbol");
        storage
            .insert_logic_nodes(
                ids[0],
                &[LogicNodeRecord {
                    id: None,
                    symbol_id: ids[0],
                    node_type: LogicNodeType::Return,
                    start_line: 2,
                    end_line: 2,
                    semantic_label: "result_exit".into(),
                }],
            )
            .expect("insert logic nodes");

        let service = RetrievalService::new(repo, storage);
        let resp = service
            .handle(RetrievalRequest {
                operation: Operation::GetFunction,
                name: Some("retryRequest".into()),
                query: None,
                file: None,
                start_line: None,
                end_line: None,
                max_tokens: None,
                limit: None,
                node_id: None,
                radius: None,
                logic_radius: None,
                dependency_radius: None,
                workspace_scope: None,
                edit_description: None,
                patch_mode: None,
                run_tests: None,
            })
            .expect("get function");

        let code = resp
            .result
            .get("code")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        assert!(code.contains("retryRequest"));
    }

    #[test]
    fn returns_logic_nodes() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join("src")).expect("mkdir src");
        fs::write(
            repo.join("src").join("client.ts"),
            "function retryRequest(){\n  if (x) { return 1; }\n  return 2;\n}\n",
        )
        .expect("write file");

        let db = tmp.path().join("semantic.db");
        let idx = tmp.path().join("tantivy");
        let mut storage = Storage::open(&db, &idx).expect("open storage");
        storage
            .replace_file_index(
                0,
                "src/client.ts",
                "typescript",
                "x",
                &[SymbolRecord {
                    id: None,
                    repo_id: 0,
                    name: "retryRequest".into(),
                    symbol_type: SymbolType::Function,
                    file: "src/client.ts".into(),
                    start_line: 1,
                    end_line: 4,
                    language: "typescript".into(),
                    summary: "Function retryRequest".into(),
                }],
                &[],
                &[
                    LogicNodeRecord {
                        id: None,
                        symbol_id: 1,
                        node_type: LogicNodeType::Conditional,
                        start_line: 2,
                        end_line: 2,
                        semantic_label: "branch_decision".into(),
                    },
                    LogicNodeRecord {
                        id: None,
                        symbol_id: 1,
                        node_type: LogicNodeType::Return,
                        start_line: 3,
                        end_line: 3,
                        semantic_label: "result_exit".into(),
                    },
                ],
                &[],
                &[],
                &[],
            )
            .expect("replace index");

        let service = RetrievalService::new(repo, storage);
        let resp = service
            .handle(RetrievalRequest {
                operation: Operation::GetLogicNodes,
                name: Some("retryRequest".into()),
                query: None,
                file: None,
                start_line: None,
                end_line: None,
                max_tokens: None,
                limit: None,
                node_id: None,
                radius: None,
                logic_radius: None,
                dependency_radius: None,
                workspace_scope: None,
                edit_description: None,
                patch_mode: None,
                run_tests: None,
            })
            .expect("logic nodes");

        let nodes = resp
            .result
            .get("nodes")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        assert_eq!(nodes.len(), 2);
    }

    #[test]
    fn returns_reasoning_context_with_deterministic_groups() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join("src")).expect("mkdir src");
        fs::write(
            repo.join("src").join("flow.ts"),
            "async function a(){ if(true){ throw new Error('x') } await b(); return c(); }\nfunction b(){ return 1; }\nfunction c(){ return b(); }\n",
        )
        .expect("write file");

        let db = tmp.path().join("semantic.db");
        let idx = tmp.path().join("tantivy");
        let mut storage = Storage::open(&db, &idx).expect("open storage");
        storage
            .replace_file_index(
                0,
                "src/flow.ts",
                "typescript",
                "x",
                &[
                    SymbolRecord {
                        id: None,
                        repo_id: 0,
                        name: "a".into(),
                        symbol_type: SymbolType::Function,
                        file: "src/flow.ts".into(),
                        start_line: 1,
                        end_line: 1,
                        language: "typescript".into(),
                        summary: "Function a".into(),
                    },
                    SymbolRecord {
                        id: None,
                        repo_id: 0,
                        name: "b".into(),
                        symbol_type: SymbolType::Function,
                        file: "src/flow.ts".into(),
                        start_line: 2,
                        end_line: 2,
                        language: "typescript".into(),
                        summary: "Function b".into(),
                    },
                    SymbolRecord {
                        id: None,
                        repo_id: 0,
                        name: "c".into(),
                        symbol_type: SymbolType::Function,
                        file: "src/flow.ts".into(),
                        start_line: 3,
                        end_line: 3,
                        language: "typescript".into(),
                        summary: "Function c".into(),
                    },
                ],
                &[
                    engine::DependencyRecord {
                        id: None,
                        repo_id: 0,
                        caller_symbol: "a".into(),
                        callee_symbol: "b".into(),
                        file: "src/flow.ts".into(),
                    },
                    engine::DependencyRecord {
                        id: None,
                        repo_id: 0,
                        caller_symbol: "a".into(),
                        callee_symbol: "c".into(),
                        file: "src/flow.ts".into(),
                    },
                    engine::DependencyRecord {
                        id: None,
                        repo_id: 0,
                        caller_symbol: "c".into(),
                        callee_symbol: "b".into(),
                        file: "src/flow.ts".into(),
                    },
                ],
                &[
                    LogicNodeRecord {
                        id: None,
                        symbol_id: 1,
                        node_type: LogicNodeType::Conditional,
                        start_line: 1,
                        end_line: 1,
                        semantic_label: "branch_decision".into(),
                    },
                    LogicNodeRecord {
                        id: None,
                        symbol_id: 1,
                        node_type: LogicNodeType::Throw,
                        start_line: 1,
                        end_line: 1,
                        semantic_label: "error_exit".into(),
                    },
                    LogicNodeRecord {
                        id: None,
                        symbol_id: 1,
                        node_type: LogicNodeType::Await,
                        start_line: 1,
                        end_line: 1,
                        semantic_label: "async_wait".into(),
                    },
                    LogicNodeRecord {
                        id: None,
                        symbol_id: 1,
                        node_type: LogicNodeType::Return,
                        start_line: 1,
                        end_line: 1,
                        semantic_label: "result_exit".into(),
                    },
                ],
                &[],
                &[],
                &[],
            )
            .expect("replace index");

        let service = RetrievalService::new(repo, storage);
        let resp = service
            .handle(RetrievalRequest {
                operation: Operation::GetReasoningContext,
                name: Some("a".into()),
                query: None,
                file: None,
                start_line: None,
                end_line: None,
                max_tokens: None,
                limit: None,
                node_id: None,
                radius: None,
                logic_radius: Some(1),
                dependency_radius: Some(2),
                workspace_scope: None,
                edit_description: None,
                patch_mode: None,
                run_tests: None,
            })
            .expect("reasoning context");

        let order = resp
            .result
            .get("order")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        assert_eq!(order.len(), 3);

        let deps = resp
            .result
            .get("dependencies")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        assert!(!deps.is_empty());
    }

    #[test]
    fn returns_dependency_and_symbol_neighborhood() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join("src")).expect("mkdir src");
        fs::write(
            repo.join("src").join("flow.ts"),
            "function a(){ return b(); }\nfunction b(){ return c(); }\nfunction c(){ return 1; }\n",
        )
        .expect("write file");

        let db = tmp.path().join("semantic.db");
        let idx = tmp.path().join("tantivy");
        let mut storage = Storage::open(&db, &idx).expect("open storage");
        storage
            .replace_file_index(
                0,
                "src/flow.ts",
                "typescript",
                "x",
                &[
                    SymbolRecord {
                        id: None,
                        repo_id: 0,
                        name: "a".into(),
                        symbol_type: SymbolType::Function,
                        file: "src/flow.ts".into(),
                        start_line: 1,
                        end_line: 1,
                        language: "typescript".into(),
                        summary: "Function a".into(),
                    },
                    SymbolRecord {
                        id: None,
                        repo_id: 0,
                        name: "b".into(),
                        symbol_type: SymbolType::Function,
                        file: "src/flow.ts".into(),
                        start_line: 2,
                        end_line: 2,
                        language: "typescript".into(),
                        summary: "Function b".into(),
                    },
                    SymbolRecord {
                        id: None,
                        repo_id: 0,
                        name: "c".into(),
                        symbol_type: SymbolType::Function,
                        file: "src/flow.ts".into(),
                        start_line: 3,
                        end_line: 3,
                        language: "typescript".into(),
                        summary: "Function c".into(),
                    },
                ],
                &[
                    engine::DependencyRecord {
                        id: None,
                        repo_id: 0,
                        caller_symbol: "a".into(),
                        callee_symbol: "b".into(),
                        file: "src/flow.ts".into(),
                    },
                    engine::DependencyRecord {
                        id: None,
                        repo_id: 0,
                        caller_symbol: "b".into(),
                        callee_symbol: "c".into(),
                        file: "src/flow.ts".into(),
                    },
                ],
                &[LogicNodeRecord {
                    id: None,
                    symbol_id: 1,
                    node_type: LogicNodeType::Return,
                    start_line: 1,
                    end_line: 1,
                    semantic_label: "result_exit".into(),
                }],
                &[],
                &[],
                &[],
            )
            .expect("replace index");

        let service = RetrievalService::new(repo, storage);
        let deps_resp = service
            .handle(RetrievalRequest {
                operation: Operation::GetDependencyNeighborhood,
                name: Some("a".into()),
                query: None,
                file: None,
                start_line: None,
                end_line: None,
                max_tokens: None,
                limit: None,
                node_id: None,
                radius: Some(2),
                logic_radius: None,
                dependency_radius: None,
                workspace_scope: None,
                edit_description: None,
                patch_mode: None,
                run_tests: None,
            })
            .expect("dependency neighborhood");
        let deps = deps_resp
            .result
            .get("neighbors")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        assert_eq!(deps.len(), 2);

        let symbol_resp = service
            .handle(RetrievalRequest {
                operation: Operation::GetSymbolNeighborhood,
                name: Some("a".into()),
                query: None,
                file: None,
                start_line: None,
                end_line: None,
                max_tokens: None,
                limit: None,
                node_id: None,
                radius: Some(2),
                logic_radius: None,
                dependency_radius: None,
                workspace_scope: None,
                edit_description: None,
                patch_mode: None,
                run_tests: None,
            })
            .expect("symbol neighborhood");
        let order = symbol_resp
            .result
            .get("order")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        assert_eq!(order.len(), 3);
    }

    #[test]
    fn planned_context_skips_budget_in_small_repo() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join("src")).expect("mkdir src");
        fs::write(
            repo.join("src").join("client.ts"),
            "function fetchData(){ return retryRequest(); }\nfunction retryRequest(){ return 1; }\n",
        )
        .expect("write file");

        let db = tmp.path().join("semantic.db");
        let idx = tmp.path().join("tantivy");
        let mut storage = Storage::open(&db, &idx).expect("open storage");
        storage
            .replace_file_index(
                0,
                "src/client.ts",
                "typescript",
                "x",
                &[
                    SymbolRecord {
                        id: None,
                        repo_id: 0,
                        name: "fetchData".into(),
                        symbol_type: SymbolType::Function,
                        file: "src/client.ts".into(),
                        start_line: 1,
                        end_line: 1,
                        language: "typescript".into(),
                        summary: "Function fetchData".into(),
                    },
                    SymbolRecord {
                        id: None,
                        repo_id: 0,
                        name: "retryRequest".into(),
                        symbol_type: SymbolType::Function,
                        file: "src/client.ts".into(),
                        start_line: 2,
                        end_line: 2,
                        language: "typescript".into(),
                        summary: "Function retryRequest".into(),
                    },
                ],
                &[engine::DependencyRecord {
                    id: None,
                    repo_id: 0,
                    caller_symbol: "fetchData".into(),
                    callee_symbol: "retryRequest".into(),
                    file: "src/client.ts".into(),
                }],
                &[LogicNodeRecord {
                    id: None,
                    symbol_id: 1,
                    node_type: LogicNodeType::Return,
                    start_line: 1,
                    end_line: 1,
                    semantic_label: "result_exit".into(),
                }],
                &[],
                &[],
                &[],
            )
            .expect("replace index");

        let service = RetrievalService::new(repo, storage);
        let resp = service
            .handle(RetrievalRequest {
                operation: Operation::GetPlannedContext,
                name: None,
                query: Some("fix retry logic in fetchData".into()),
                file: None,
                start_line: None,
                end_line: None,
                max_tokens: Some(20),
                limit: None,
                node_id: None,
                radius: None,
                logic_radius: None,
                dependency_radius: None,
                workspace_scope: None,
                edit_description: None,
                patch_mode: None,
                run_tests: None,
            })
            .expect("planned context");

        assert_eq!(
            resp.result
                .get("small_repo_mode")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            true
        );
        let context = resp
            .result
            .get("context")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        assert!(context.len() <= 1);
    }

    #[test]
    fn planned_context_applies_budget_in_large_repo() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join("src")).expect("mkdir src");
        fs::write(
            repo.join("src").join("client.ts"),
            "function fetchData(){ return retryRequest(); }\nfunction retryRequest(){ return 1; }\n",
        )
        .expect("write file");

        let db = tmp.path().join("semantic.db");
        let idx = tmp.path().join("tantivy");
        let mut storage = Storage::open(&db, &idx).expect("open storage");
        storage
            .replace_file_index(
                0,
                "src/client.ts",
                "typescript",
                "x",
                &[
                    SymbolRecord {
                        id: None,
                        repo_id: 0,
                        name: "fetchData".into(),
                        symbol_type: SymbolType::Function,
                        file: "src/client.ts".into(),
                        start_line: 1,
                        end_line: 1,
                        language: "typescript".into(),
                        summary: "Function fetchData".into(),
                    },
                    SymbolRecord {
                        id: None,
                        repo_id: 0,
                        name: "retryRequest".into(),
                        symbol_type: SymbolType::Function,
                        file: "src/client.ts".into(),
                        start_line: 2,
                        end_line: 2,
                        language: "typescript".into(),
                        summary: "Function retryRequest".into(),
                    },
                ],
                &[engine::DependencyRecord {
                    id: None,
                    repo_id: 0,
                    caller_symbol: "fetchData".into(),
                    callee_symbol: "retryRequest".into(),
                    file: "src/client.ts".into(),
                }],
                &[LogicNodeRecord {
                    id: None,
                    symbol_id: 1,
                    node_type: LogicNodeType::Return,
                    start_line: 1,
                    end_line: 1,
                    semantic_label: "result_exit".into(),
                }],
                &[],
                &[],
                &[],
            )
            .expect("replace index");

        for i in 0..60 {
            storage
                .upsert_file(&format!("src/dummy_{i}.ts"), "typescript", "z")
                .expect("upsert file");
        }

        let service = RetrievalService::new(repo, storage);
        let resp = service
            .handle(RetrievalRequest {
                operation: Operation::GetPlannedContext,
                name: None,
                query: Some("refactor fetchData".into()),
                file: None,
                start_line: None,
                end_line: None,
                max_tokens: Some(1004),
                limit: None,
                node_id: None,
                radius: None,
                logic_radius: None,
                dependency_radius: None,
                workspace_scope: None,
                edit_description: None,
                patch_mode: None,
                run_tests: None,
            })
            .expect("planned context");

        assert_eq!(
            resp.result
                .get("small_repo_mode")
                .and_then(|v| v.as_bool())
                .unwrap_or(true),
            false
        );
        let context = resp
            .result
            .get("context")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        assert!(context.len() <= 1);
    }

    #[test]
    fn returns_repo_map_hierarchy_and_module_dependencies() {
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
        let mut storage = Storage::open(&db, &idx).expect("open storage");
        storage
            .replace_file_index(
                0,
                "src/utils/retry.ts",
                "typescript",
                "x1",
                &[SymbolRecord {
                    id: None,
                    repo_id: 0,
                    name: "retryRequest".into(),
                    symbol_type: SymbolType::Function,
                    file: "src/utils/retry.ts".into(),
                    start_line: 1,
                    end_line: 1,
                    language: "typescript".into(),
                    summary: "Function retryRequest".into(),
                }],
                &[],
                &[],
                &[],
                &[],
                &[],
            )
            .expect("replace retry");
        storage
            .replace_file_index(
                0,
                "src/api/client.ts",
                "typescript",
                "x2",
                &[SymbolRecord {
                    id: None,
                    repo_id: 0,
                    name: "fetchData".into(),
                    symbol_type: SymbolType::Function,
                    file: "src/api/client.ts".into(),
                    start_line: 2,
                    end_line: 2,
                    language: "typescript".into(),
                    summary: "Function fetchData".into(),
                }],
                &[engine::DependencyRecord {
                    id: None,
                    repo_id: 0,
                    caller_symbol: "fetchData".into(),
                    callee_symbol: "retryRequest".into(),
                    file: "src/api/client.ts".into(),
                }],
                &[],
                &[],
                &[],
                &[],
            )
            .expect("replace client");

        storage.clear_module_graph().expect("clear modules");
        let api_id = storage.insert_module("api", "src/api").expect("insert api");
        let util_id = storage
            .insert_module("utils", "src/utils")
            .expect("insert utils");
        storage
            .insert_module_file(api_id, "src/api/client.ts")
            .expect("insert api file");
        storage
            .insert_module_file(util_id, "src/utils/retry.ts")
            .expect("insert util file");
        storage
            .insert_module_dependency(api_id, util_id)
            .expect("insert module dep");

        let service = RetrievalService::new(repo, storage);
        let hierarchy = service
            .handle(RetrievalRequest {
                operation: Operation::GetRepoMapHierarchy,
                name: None,
                query: None,
                file: None,
                start_line: None,
                end_line: None,
                max_tokens: None,
                limit: None,
                node_id: None,
                radius: None,
                logic_radius: None,
                dependency_radius: None,
                workspace_scope: None,
                edit_description: None,
                patch_mode: None,
                run_tests: None,
            })
            .expect("repo hierarchy");
        let modules = hierarchy
            .result
            .get("modules")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        assert!(modules.len() >= 2);

        let module_deps = service
            .handle(RetrievalRequest {
                operation: Operation::GetModuleDependencies,
                name: None,
                query: None,
                file: None,
                start_line: None,
                end_line: None,
                max_tokens: None,
                limit: None,
                node_id: None,
                radius: None,
                logic_radius: None,
                dependency_radius: None,
                workspace_scope: None,
                edit_description: None,
                patch_mode: None,
                run_tests: None,
            })
            .expect("module dependencies");
        let edges = module_deps
            .result
            .get("edges")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        assert!(edges.iter().any(|e| {
            e.get("from").and_then(|v| v.as_str()) == Some("api")
                && e.get("to").and_then(|v| v.as_str()) == Some("utils")
        }));
    }

    #[test]
    fn semantic_symbol_search_fallback_works() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join("src")).expect("mkdir src");
        fs::write(
            repo.join("src").join("retry.ts"),
            "function retryRequest(){ return 1; }\n",
        )
        .expect("write file");

        let db = tmp.path().join("semantic.db");
        let idx = tmp.path().join("tantivy");
        let mut storage = Storage::open(&db, &idx).expect("open storage");
        storage
            .replace_file_index(
                0,
                "src/retry.ts",
                "typescript",
                "x",
                &[SymbolRecord {
                    id: None,
                    repo_id: 0,
                    name: "retryRequest".into(),
                    symbol_type: SymbolType::Function,
                    file: "src/retry.ts".into(),
                    start_line: 1,
                    end_line: 1,
                    language: "typescript".into(),
                    summary: "Function retryRequest".into(),
                }],
                &[],
                &[],
                &[],
                &[],
                &[],
            )
            .expect("replace");

        let service = RetrievalService::new(repo, storage);
        let resp = service
            .handle(RetrievalRequest {
                operation: Operation::SearchSemanticSymbol,
                name: None,
                query: Some("retry operation".into()),
                file: None,
                start_line: None,
                end_line: None,
                max_tokens: None,
                workspace_scope: None,
                limit: Some(5),
                node_id: None,
                radius: None,
                logic_radius: None,
                dependency_radius: None,
                edit_description: None,
                patch_mode: None,
                run_tests: None,
            })
            .expect("semantic search");
        let results = resp
            .result
            .get("results")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        assert!(!results.is_empty());
    }

    #[test]
    fn workspace_reasoning_context_returns_repositories() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join("src")).expect("mkdir src");
        fs::write(
            repo.join("src").join("a.ts"),
            "function fetchData(){ return 1; }\n",
        )
        .expect("write file");

        let db = tmp.path().join("semantic.db");
        let idx = tmp.path().join("tantivy");
        let mut storage = Storage::open(&db, &idx).expect("open storage");
        storage
            .register_repository("repoA", "/workspace/repoA")
            .expect("register repo");
        storage
            .replace_file_index(
                1,
                "src/a.ts",
                "typescript",
                "x",
                &[SymbolRecord {
                    id: None,
                    repo_id: 1,
                    name: "fetchData".into(),
                    symbol_type: SymbolType::Function,
                    file: "src/a.ts".into(),
                    start_line: 1,
                    end_line: 1,
                    language: "typescript".into(),
                    summary: "Function fetchData".into(),
                }],
                &[],
                &[LogicNodeRecord {
                    id: None,
                    symbol_id: 1,
                    node_type: LogicNodeType::Return,
                    start_line: 1,
                    end_line: 1,
                    semantic_label: "result_exit".into(),
                }],
                &[],
                &[],
                &[],
            )
            .expect("replace");

        let service = RetrievalService::new(repo, storage);
        let resp = service
            .handle(RetrievalRequest {
                operation: Operation::GetWorkspaceReasoningContext,
                name: None,
                query: Some("explain fetchData".into()),
                file: None,
                start_line: None,
                end_line: None,
                max_tokens: Some(1500),
                workspace_scope: Some(vec!["repoA".into()]),
                limit: None,
                node_id: None,
                radius: None,
                logic_radius: None,
                dependency_radius: None,
                edit_description: None,
                patch_mode: None,
                run_tests: None,
            })
            .expect("workspace context");
        let repos = resp
            .result
            .get("workspace_repositories")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        assert!(!repos.is_empty());
    }
}
