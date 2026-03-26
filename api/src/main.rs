use anyhow::Result;
use axum::{
    extract::Query,
    extract::State,
    response::Html,
    routing::get,
    routing::patch,
    routing::post,
    Json, Router,
};
use engine::{Operation, PatchApplicationMode, RetrievalRequest, RetrievalResponse};
use indexer::Indexer;
use parking_lot::Mutex;
use retrieval::RetrievalService;
use std::collections::{HashMap, HashSet, VecDeque};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use storage::default_paths;
use telemetry::{metadata_pairs, TaskScope, TelemetrySink};
use tracing::{error, info};
use watcher::RepoWatcher;

#[derive(Clone)]
struct AppState {
    retrieval: Arc<Mutex<RetrievalService>>,
    semantic_middleware: Arc<Mutex<SemanticMiddlewareState>>,
    workspace_state: Arc<Mutex<WorkspaceState>>,
}

#[derive(Debug, Clone)]
struct WorkspaceState {
    /// When true the indexer has indexed all workspace roots and retrieval
    /// searches across all of them. When false only the primary repo root
    /// is indexed and searched.
    workspace_mode_enabled: bool,
    /// Absolute paths of all workspace project roots (excluding primary).
    workspace_roots: Vec<std::path::PathBuf>,
    primary_root: std::path::PathBuf,
}

impl WorkspaceState {
    /// Read workspace roots from `.semantic/workspace.toml` in the primary root.
    /// Format (one path per line under [roots]):
    ///   [roots]
    ///   paths = [
    ///     "../Agenton",
    ///     "../CLAIR lazy skill loading",
    ///   ]
    fn load(primary_root: &std::path::Path) -> Self {
        let config_path = primary_root.join(".semantic").join("workspace.toml");
        let mut roots: Vec<std::path::PathBuf> = Vec::new();
        if let Ok(raw) = std::fs::read_to_string(&config_path) {
            let mut in_paths = false;
            for line in raw.lines() {
                let t = line.trim();
                if t == "paths = [" || t == "paths=[" { in_paths = true; continue; }
                if in_paths {
                    if t == "]" { break; }
                    let p = t.trim_matches(',').trim().trim_matches('"');
                    if p.is_empty() { continue; }
                    let resolved = if std::path::Path::new(p).is_absolute() {
                        std::path::PathBuf::from(p)
                    } else {
                        primary_root.join(p)
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

struct SemanticMiddlewareState {
    semantic_first_enabled: bool,
    sessions: HashMap<String, SessionContextState>,
}

#[derive(Debug, Clone)]
struct SessionContextState {
    last_seen_epoch_s: u64,
    index_revision: u64,
    accepted_refs: HashSet<String>,
    accepted_order: VecDeque<String>,
    last_target_symbols: VecDeque<String>,
    intent_symbol_cache: HashMap<String, String>,
}

impl Default for SemanticMiddlewareState {
    fn default() -> Self {
        Self {
            semantic_first_enabled: true,
            sessions: HashMap::new(),
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter("info")
        .with_target(false)
        .compact()
        .init();

    let repo_root = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or(std::env::current_dir()?);

    let (db_path, tantivy_path) = default_paths(&repo_root);

    let index_storage = storage::Storage::open(&db_path, &tantivy_path)?;
    let mut indexer = Indexer::new(index_storage);
    indexer.index_repo(&repo_root)?;

    let retrieval_storage = storage::Storage::open(&db_path, &tantivy_path)?;
    let retrieval_service = Arc::new(Mutex::new(RetrievalService::new(repo_root.clone(), retrieval_storage)));

    let shared_indexer = Arc::new(Mutex::new(indexer));
    let _watcher = RepoWatcher::start(repo_root.clone(), shared_indexer)?;

    let workspace_state = WorkspaceState::load(&repo_root);

    let state = AppState {
        retrieval: retrieval_service,
        semantic_middleware: Arc::new(Mutex::new(SemanticMiddlewareState::default())),
        workspace_state: Arc::new(Mutex::new(workspace_state)),
    };

    let app = Router::new()
        .route("/health", get(health))
        .route("/retrieve", post(retrieve))
        .route("/llm_tools", get(get_llm_tools))
        .route("/semantic_ui", get(get_semantic_ui))
        .route("/ide_autoroute", post(ide_autoroute))
        .route("/performance_stats", get(get_performance_stats))
        .route("/control_flow_hints", get(get_control_flow_hints))
        .route("/data_flow_hints", get(get_data_flow_hints))
        .route("/hybrid_ranked_context", post(get_hybrid_ranked_context))
        .route("/semantic_middleware", get(get_semantic_middleware))
        .route("/semantic_middleware", post(update_semantic_middleware))
        .route("/edit", patch(edit))
        .route("/organization_graph", get(get_organization_graph))
        .route("/service_graph", get(get_service_graph))
        .route("/plan_org_refactor", post(plan_org_refactor))
        .route("/org_refactor_status", get(get_org_refactor_status))
        .route("/refactor_status", get(get_refactor_status))
        .route("/evolution_issues", get(get_evolution_issues))
        .route("/evolution_plans", get(get_evolution_plans))
        .route("/generate_evolution_plan", post(generate_evolution_plan))
        .route("/patch_memory", get(get_patch_memory))
        .route("/patch_stats", get(get_patch_stats))
        .route("/model_performance", get(get_model_performance))
        .route("/debug_failure", post(debug_failure))
        .route("/debug_graph", get(get_debug_graph))
        .route("/root_cause_candidates", get(get_root_cause_candidates))
        .route("/test_gaps", get(get_test_gaps))
        .route("/generate_tests", post(generate_tests))
        .route("/apply_tests", post(apply_tests))
        .route("/pipeline_graph", get(get_pipeline_graph))
        .route("/analyze_pipeline", post(analyze_pipeline))
        .route("/deployment_history", get(get_deployment_history))
        .route("/todo/seed", post(seed_todo_tasks))
        .route("/todo/tasks", get(get_todo_tasks))
        .route("/ab_test_dev", get(get_ab_tests))
        .route("/ab_test_dev", post(run_ab_test_dev))
        .route("/env_check", get(get_env_check))
        .route("/mcp_settings_ui", get(get_mcp_settings_ui))
        .route("/mcp_settings_update", post(update_mcp_settings))
        .route("/project_summary", get(get_project_summary))
        .with_state(state);

    let addr: SocketAddr = "127.0.0.1:4317".parse()?;
    info!("API listening on {addr}");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({"status": "ok"}))
}

const SESSION_TTL_SECS: u64 = 20 * 60;
const MAX_SESSION_CONTEXT_ENTRIES: usize = 200;

fn now_epoch_s() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

fn next_task_scope(telemetry: &TelemetrySink, session_id: Option<&str>, route_id: &str) -> TaskScope {
    TaskScope {
        session_id: session_id.map(|v| v.to_string()),
        task_id: telemetry.next_event_id("task"),
        route_id: route_id.to_string(),
    }
}

fn emit_task_started(
    telemetry: &TelemetrySink,
    scope: &TaskScope,
    category: &str,
    metadata: serde_json::Value,
) {
    let mut event = telemetry.event(Some(scope), "task_started", "api", Some(category));
    event.status = Some("started".to_string());
    event.metadata = telemetry.sanitize_metadata(metadata);
    telemetry.emit(event);
}

fn emit_task_finished(
    telemetry: &TelemetrySink,
    scope: &TaskScope,
    category: &str,
    response: &serde_json::Value,
) {
    let ok = response.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
    let mut event = telemetry.event(
        Some(scope),
        if ok { "task_completed" } else { "task_failed" },
        "api",
        Some(category),
    );
    event.status = Some(if ok { "ok" } else { "error" }.to_string());
    event.error_code = response
        .get("error")
        .and_then(|v| v.as_str())
        .map(|v| v.to_string());
    event.metadata = telemetry.sanitize_metadata(metadata_pairs([
        ("route_id", serde_json::json!(scope.route_id)),
        ("response", response.clone()),
    ]));
    telemetry.emit(event);
}

fn touch_or_create_session<'a>(
    middleware: &'a mut SemanticMiddlewareState,
    session_id: &str,
    index_revision: u64,
) -> &'a mut SessionContextState {
    let now = now_epoch_s();
    middleware
        .sessions
        .retain(|_, v| now.saturating_sub(v.last_seen_epoch_s) <= SESSION_TTL_SECS);
    let entry = middleware
        .sessions
        .entry(session_id.to_string())
        .or_insert_with(|| SessionContextState {
            last_seen_epoch_s: now,
            index_revision,
            accepted_refs: HashSet::new(),
            accepted_order: VecDeque::new(),
            last_target_symbols: VecDeque::new(),
            intent_symbol_cache: HashMap::new(),
        });
    if entry.index_revision != index_revision {
        entry.index_revision = index_revision;
        entry.accepted_refs.clear();
        entry.accepted_order.clear();
        entry.last_target_symbols.clear();
        entry.intent_symbol_cache.clear();
    }
    entry.last_seen_epoch_s = now;
    entry
}

fn apply_session_context_reuse(
    result: &mut serde_json::Value,
    session: &mut SessionContextState,
) -> usize {
    let Some(context) = result.get_mut("context").and_then(|v| v.as_array_mut()) else {
        return 0;
    };
    let mut filtered = Vec::new();
    let mut reused = 0usize;
    for item in context.drain(..) {
        let file = item
            .get("file")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let start = item.get("start").and_then(|v| v.as_u64()).unwrap_or_default();
        let end = item.get("end").and_then(|v| v.as_u64()).unwrap_or_default();
        let key = format!("{file}:{start}-{end}");
        if file.is_empty() || start == 0 || end == 0 {
            filtered.push(item);
            continue;
        }
        if session.accepted_refs.contains(&key) {
            reused += 1;
            continue;
        }
        session.accepted_refs.insert(key.clone());
        session.accepted_order.push_back(key);
        while session.accepted_order.len() > MAX_SESSION_CONTEXT_ENTRIES {
            if let Some(old) = session.accepted_order.pop_front() {
                session.accepted_refs.remove(&old);
            }
        }
        filtered.push(item);
    }
    *context = filtered;
    reused
}

async fn retrieve(
    State(state): State<AppState>,
    Json(body): Json<RetrieveRequestBody>,
) -> Json<serde_json::Value> {
    let telemetry = state.retrieval.lock().telemetry();
    let scope = next_task_scope(&telemetry, body.session_id.as_deref(), "retrieve");
    emit_task_started(
        &telemetry,
        &scope,
        "tool_routing",
        metadata_pairs([
            ("operation", serde_json::json!(body.request.operation)),
            ("session_id", serde_json::json!(body.session_id)),
            ("single_file_fast_path", serde_json::json!(body.single_file_fast_path)),
            ("reference_only", serde_json::json!(body.reference_only)),
            ("mapping_mode", serde_json::json!(body.mapping_mode)),
            ("max_footprint_items", serde_json::json!(body.max_footprint_items)),
        ]),
    );

    let response = telemetry.with_task_scope(scope.clone(), || {
        // Dispatch unified operations that were previously separate endpoints
        match &body.request.operation {
            Operation::GetControlFlowHints => {
                let symbol = body.request.name.clone()
                    .or_else(|| body.request.query.clone())
                    .unwrap_or_default();
                return Json(match state.retrieval.lock().get_control_flow_hints(&symbol) {
                    Ok(result) => serde_json::json!({"ok": true, "operation": "get_control_flow_hints", "result": result}),
                    Err(err) => serde_json::json!({"ok": false, "error": err.to_string()}),
                });
            }
            Operation::GetDataFlowHints => {
                let symbol = body.request.name.clone()
                    .or_else(|| body.request.query.clone())
                    .unwrap_or_default();
                return Json(match state.retrieval.lock().get_data_flow_hints(&symbol) {
                    Ok(result) => serde_json::json!({"ok": true, "operation": "get_data_flow_hints", "result": result}),
                    Err(err) => serde_json::json!({"ok": false, "error": err.to_string()}),
                });
            }
            Operation::GetHybridRankedContext => {
                let query = body.request.query.clone().unwrap_or_default();
                let max_tokens = body.request.max_tokens.unwrap_or(1400);
                let single_file_fast_path = body.single_file_fast_path.unwrap_or(true);
                return Json(match state.retrieval.lock().get_hybrid_ranked_context(&query, max_tokens, single_file_fast_path) {
                    Ok(result) => serde_json::json!({"ok": true, "operation": "get_hybrid_ranked_context", "result": result}),
                    Err(err) => serde_json::json!({"ok": false, "error": err.to_string()}),
                });
            }
            Operation::GetDebugGraph => {
                return Json(match state.retrieval.lock().get_debug_graph() {
                    Ok(result) => serde_json::json!({"ok": true, "operation": "get_debug_graph", "result": result}),
                    Err(err) => serde_json::json!({"ok": false, "error": err.to_string()}),
                });
            }
            Operation::GetPipelineGraph => {
                return Json(match state.retrieval.lock().get_pipeline_graph() {
                    Ok(result) => serde_json::json!({"ok": true, "operation": "get_pipeline_graph", "result": result}),
                    Err(err) => serde_json::json!({"ok": false, "error": err.to_string()}),
                });
            }
            Operation::GetRootCauseCandidates => {
                return Json(match state.retrieval.lock().get_root_cause_candidates() {
                    Ok(result) => serde_json::json!({"ok": true, "operation": "get_root_cause_candidates", "result": result}),
                    Err(err) => serde_json::json!({"ok": false, "error": err.to_string()}),
                });
            }
            Operation::GetTestGaps => {
                return Json(match state.retrieval.lock().get_test_gaps() {
                    Ok(result) => serde_json::json!({"ok": true, "operation": "get_test_gaps", "result": result}),
                    Err(err) => serde_json::json!({"ok": false, "error": err.to_string()}),
                });
            }
            Operation::GetDeploymentHistory => {
                return Json(match state.retrieval.lock().get_deployment_history() {
                    Ok(result) => serde_json::json!({"ok": true, "operation": "get_deployment_history", "result": result}),
                    Err(err) => serde_json::json!({"ok": false, "error": err.to_string()}),
                });
            }
            Operation::GetPerformanceStats => {
                return Json(serde_json::json!({
                    "ok": true,
                    "operation": "get_performance_stats",
                    "result": state.retrieval.lock().get_performance_stats()
                }));
            }
            Operation::GetProjectSummary => {
                let max_tokens = body.request.max_tokens.unwrap_or(800);
                let result = state.retrieval.lock().with_storage(|storage| {
                    project_summariser::ProjectSummariser::new(storage).build(max_tokens)
                });
                return Json(match result {
                    Ok(doc) => serde_json::json!({
                        "ok": true,
                        "operation": "get_project_summary",
                        "token_estimate": doc.token_estimate,
                        "summary": doc.to_json(),
                        "summary_text": doc.summary_text,
                    }),
                    Err(err) => serde_json::json!({"ok": false, "error": err.to_string()}),
                });
            }
            _ => {}
        }

        if body.semantic_enabled == Some(false) {
            return Json(serde_json::json!({
                "ok": true,
                "semantic_enabled": false,
                "skipped": true,
                "message": "Semantic layer disabled for this request."
            }));
        }

        let mut request = body.request;
        if body.input_compressed == Some(true) && should_block_compressed_semantic(&request.operation) {
            if let Some(original_query) = body.original_query {
                if request.query.is_none() {
                    request.query = Some(original_query.clone());
                }
                if request.name.is_none() {
                    request.name = Some(original_query);
                }
            } else {
                return Json(serde_json::json!({
                    "ok": false,
                    "error": "input_compressed=true can reduce semantic retrieval precision. Send original_query or disable compression for semantic operations."
                }));
            }
        }

        let query_for_session = request.query.clone();
        let result = state
            .retrieval
            .lock()
            .handle_with_options_ext(
                request,
                body.single_file_fast_path,
                Some(!body.reference_only.unwrap_or(true)),
                body.mapping_mode.as_deref(),
                body.max_footprint_items,
            );
        match result {
            Ok(mut response) => {
                let mut reused_context_count = 0usize;
                if let Some(session_id) = body.session_id.as_ref() {
                    let index_revision = state.retrieval.lock().index_revision();
                    let mut middleware = state.semantic_middleware.lock();
                    let session = touch_or_create_session(&mut middleware, session_id, index_revision);
                    if body.reuse_session_context.unwrap_or(true) {
                        reused_context_count = apply_session_context_reuse(&mut response.result, session);
                    }
                    if let Some(symbol) = response.result.get("symbol").and_then(|v| v.as_str()) {
                        session.last_target_symbols.push_back(symbol.to_string());
                        while session.last_target_symbols.len() > 32 {
                            session.last_target_symbols.pop_front();
                        }
                    }
                    if let Some(query) = query_for_session.as_ref() {
                        if let Some(symbol) = response.result.get("symbol").and_then(|v| v.as_str()) {
                            session
                                .intent_symbol_cache
                                .insert(query.to_lowercase(), symbol.to_string());
                        }
                    }
                }
                if let Some(obj) = response.result.as_object_mut() {
                    obj.insert("reused_context_count".to_string(), serde_json::json!(reused_context_count));
                }
                Json(success(response))
            }
            Err(err) => {
                error!("retrieval failed: {err}");
                Json(serde_json::json!({
                    "ok": false,
                    "error": err.to_string(),
                }))
            }
        }
    });
    emit_task_finished(&telemetry, &scope, "tool_routing", &response.0);
    response
}

#[derive(Debug, serde::Deserialize)]
struct RetrieveRequestBody {
    #[serde(flatten)]
    request: RetrievalRequest,
    semantic_enabled: Option<bool>,
    input_compressed: Option<bool>,
    original_query: Option<String>,
    single_file_fast_path: Option<bool>,
    reference_only: Option<bool>,
    mapping_mode: Option<String>,
    max_footprint_items: Option<usize>,
    reuse_session_context: Option<bool>,
    session_id: Option<String>,
}

fn should_block_compressed_semantic(operation: &Operation) -> bool {
    matches!(
        operation,
        Operation::SearchSymbol
            | Operation::GetFunction
            | Operation::GetClass
            | Operation::GetDependencies
            | Operation::GetLogicNodes
            | Operation::GetDependencyNeighborhood
            | Operation::GetSymbolNeighborhood
            | Operation::GetReasoningContext
            | Operation::GetPlannedContext
            | Operation::SearchSemanticSymbol
            | Operation::GetWorkspaceReasoningContext
            | Operation::PlanSafeEdit
    )
}

#[derive(Debug, serde::Deserialize)]
struct IdeAutoRouteRequest {
    /// Optional free-text task description (used for context retrieval / intent detection).
    task: Option<String>,
    /// Action-oriented dispatch: "debug_failure" | "generate_tests" | "apply_tests" | "analyze_pipeline"
    action: Option<String>,
    /// Payload for action-oriented calls (replaces per-action request bodies).
    action_input: Option<serde_json::Value>,
    session_id: Option<String>,
    max_tokens: Option<usize>,
    single_file_fast_path: Option<bool>,
    reference_only: Option<bool>,
    mapping_mode: Option<String>,
    max_footprint_items: Option<usize>,
    reuse_session_context: Option<bool>,
    auto_minimal_raw: Option<bool>,
    include_summary: Option<bool>,
}

async fn ide_autoroute(
    State(state): State<AppState>,
    Json(body): Json<IdeAutoRouteRequest>,
) -> Json<serde_json::Value> {
    let telemetry = state.retrieval.lock().telemetry();
    let scope = next_task_scope(&telemetry, body.session_id.as_deref(), "ide_autoroute");
    emit_task_started(
        &telemetry,
        &scope,
        "tool_routing",
        metadata_pairs([
            ("task", serde_json::json!(body.task)),
            ("action", serde_json::json!(body.action)),
            ("session_id", serde_json::json!(body.session_id)),
            ("max_tokens", serde_json::json!(body.max_tokens)),
        ]),
    );
    let response = telemetry.with_task_scope(scope.clone(), || {
    // Action-oriented dispatch: handle structured actions before intent-based routing
    if let Some(ref action) = body.action {
        let input = body.action_input.clone().unwrap_or_else(|| serde_json::json!({}));
        return match action.as_str() {
            "debug_failure" => {
                let event_id = input.get("event_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let repository = input.get("repository").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let timestamp = input.get("timestamp").and_then(|v| v.as_u64()).unwrap_or(0);
                let failure_type = input.get("failure_type").and_then(|v| v.as_str()).unwrap_or("runtime_exception").to_string();
                let stack_trace: Vec<String> = input.get("stack_trace")
                    .and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect())
                    .unwrap_or_default();
                let error_message = input.get("error_message").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let event = debug_graph::FailureEvent {
                    event_id,
                    repository,
                    timestamp,
                    failure_type: parse_failure_type(&failure_type),
                    stack_trace,
                    error_message,
                };
                match state.retrieval.lock().debug_failure(event) {
                    Ok(result) => Json(serde_json::json!({"ok": true, "action": "debug_failure", "result": result})),
                    Err(err) => Json(serde_json::json!({"ok": false, "action": "debug_failure", "error": err.to_string()})),
                }
            }
            "generate_tests" => {
                let target_symbol = input.get("target_symbol").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let framework = input.get("framework").and_then(|v| v.as_str()).unwrap_or("rust-test").to_string();
                match state.retrieval.lock().generate_tests(&target_symbol, &framework) {
                    Ok(result) => Json(serde_json::json!({"ok": true, "action": "generate_tests", "result": result})),
                    Err(err) => Json(serde_json::json!({"ok": false, "action": "generate_tests", "error": err.to_string()})),
                }
            }
            "apply_tests" => {
                let repository = input.get("repository").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let target_symbol = input.get("target_symbol").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let framework = input.get("framework").and_then(|v| v.as_str()).unwrap_or("rust-test").to_string();
                match state.retrieval.lock().apply_tests(&repository, &target_symbol, &framework) {
                    Ok(result) => Json(serde_json::json!({"ok": true, "action": "apply_tests", "result": result})),
                    Err(err) => Json(serde_json::json!({"ok": false, "action": "apply_tests", "error": err.to_string()})),
                }
            }
            "analyze_pipeline" => {
                let failure_stage = input.get("failure_stage").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let failure_message = input.get("failure_message").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let request = pipeline_graph::PipelineAnalysisRequest { failure_stage, failure_message };
                match state.retrieval.lock().analyze_pipeline(request) {
                    Ok(result) => Json(serde_json::json!({"ok": true, "action": "analyze_pipeline", "result": result})),
                    Err(err) => Json(serde_json::json!({"ok": false, "action": "analyze_pipeline", "error": err.to_string()})),
                }
            }
            "llm_tools" => Json(serde_json::json!({
                "ok": true,
                "action": "llm_tools",
                "result": state.retrieval.lock().get_llm_tools()
            })),
            "semantic_middleware_get" => {
                let mut middleware = state.semantic_middleware.lock();
                let now = now_epoch_s();
                middleware
                    .sessions
                    .retain(|_, v| now.saturating_sub(v.last_seen_epoch_s) <= SESSION_TTL_SECS);
                Json(serde_json::json!({
                    "ok": true,
                    "action": "semantic_middleware_get",
                    "result": {
                        "semantic_first_enabled": middleware.semantic_first_enabled,
                        "tracked_sessions": middleware.sessions.len()
                    }
                }))
            }
            "semantic_middleware_set" => {
                let enabled = input.get("semantic_first_enabled").and_then(|v| v.as_bool()).unwrap_or(true);
                let mut middleware = state.semantic_middleware.lock();
                middleware.semantic_first_enabled = enabled;
                if !enabled {
                    middleware.sessions.clear();
                }
                Json(serde_json::json!({
                    "ok": true,
                    "action": "semantic_middleware_set",
                    "result": {
                        "semantic_first_enabled": middleware.semantic_first_enabled,
                        "tracked_sessions": middleware.sessions.len()
                    }
                }))
            }
            "patch_memory" => {
                let repository = input.get("repository").and_then(|v| v.as_str()).map(|s| s.to_string());
                let symbol = input.get("symbol").and_then(|v| v.as_str()).map(|s| s.to_string());
                let model = input.get("model").and_then(|v| v.as_str()).map(|s| s.to_string());
                let time_range = parse_time_range(&input.get("time_range").and_then(|v| v.as_str()).map(|s| s.to_string()));
                match state.retrieval.lock().get_patch_memory(repository, symbol, model, time_range) {
                    Ok(result) => Json(serde_json::json!({"ok": true, "action": "patch_memory", "result": result})),
                    Err(err) => Json(serde_json::json!({"ok": false, "action": "patch_memory", "error": err.to_string()})),
                }
            }
            "patch_stats" => {
                let repository = input.get("repository").and_then(|v| v.as_str()).map(|s| s.to_string());
                let symbol = input.get("symbol").and_then(|v| v.as_str()).map(|s| s.to_string());
                let model = input.get("model").and_then(|v| v.as_str()).map(|s| s.to_string());
                let time_range = parse_time_range(&input.get("time_range").and_then(|v| v.as_str()).map(|s| s.to_string()));
                match state.retrieval.lock().get_patch_stats(repository, symbol, model, time_range) {
                    Ok(result) => Json(serde_json::json!({"ok": true, "action": "patch_stats", "result": result})),
                    Err(err) => Json(serde_json::json!({"ok": false, "action": "patch_stats", "error": err.to_string()})),
                }
            }
            "model_performance" => {
                let repository = input.get("repository").and_then(|v| v.as_str()).map(|s| s.to_string());
                let symbol = input.get("symbol").and_then(|v| v.as_str()).map(|s| s.to_string());
                let model = input.get("model").and_then(|v| v.as_str()).map(|s| s.to_string());
                let time_range = parse_time_range(&input.get("time_range").and_then(|v| v.as_str()).map(|s| s.to_string()));
                match state.retrieval.lock().get_model_performance(repository, symbol, model, time_range) {
                    Ok(result) => Json(serde_json::json!({"ok": true, "action": "model_performance", "result": result})),
                    Err(err) => Json(serde_json::json!({"ok": false, "action": "model_performance", "error": err.to_string()})),
                }
            }
            "organization_graph" => match state.retrieval.lock().get_organization_graph() {
                Ok(result) => Json(serde_json::json!({"ok": true, "action": "organization_graph", "result": result})),
                Err(err) => Json(serde_json::json!({"ok": false, "action": "organization_graph", "error": err.to_string()})),
            },
            "service_graph" => match state.retrieval.lock().get_service_graph() {
                Ok(result) => Json(serde_json::json!({"ok": true, "action": "service_graph", "result": result})),
                Err(err) => Json(serde_json::json!({"ok": false, "action": "service_graph", "error": err.to_string()})),
            },
            "plan_org_refactor" => {
                let origin_repo = input.get("origin_repo").and_then(|v| v.as_str()).unwrap_or("").to_string();
                match state.retrieval.lock().plan_org_refactor(&origin_repo) {
                    Ok(result) => Json(serde_json::json!({"ok": true, "action": "plan_org_refactor", "result": result})),
                    Err(err) => Json(serde_json::json!({"ok": false, "action": "plan_org_refactor", "error": err.to_string()})),
                }
            }
            "org_refactor_status" => match state.retrieval.lock().get_org_refactor_status() {
                Ok(result) => Json(serde_json::json!({"ok": true, "action": "org_refactor_status", "result": result})),
                Err(err) => Json(serde_json::json!({"ok": false, "action": "org_refactor_status", "error": err.to_string()})),
            },
            "refactor_status" => {
                let repo_root = state.retrieval.lock().repo_root().to_path_buf();
                match refactor_graph::RefactorExecutor::status(&repo_root) {
                    Ok(result) => Json(serde_json::json!({"ok": true, "action": "refactor_status", "result": result})),
                    Err(err) => Json(serde_json::json!({"ok": false, "action": "refactor_status", "error": err.to_string()})),
                }
            }
            "evolution_issues" => {
                let repository = input.get("repository").and_then(|v| v.as_str()).unwrap_or("default").to_string();
                match state.retrieval.lock().get_evolution_issues(&repository) {
                    Ok(result) => Json(serde_json::json!({"ok": true, "action": "evolution_issues", "result": result})),
                    Err(err) => Json(serde_json::json!({"ok": false, "action": "evolution_issues", "error": err.to_string()})),
                }
            }
            "evolution_plans" => {
                let repository = input.get("repository").and_then(|v| v.as_str()).unwrap_or("default").to_string();
                match state.retrieval.lock().get_evolution_plans(&repository) {
                    Ok(result) => Json(serde_json::json!({"ok": true, "action": "evolution_plans", "result": result})),
                    Err(err) => Json(serde_json::json!({"ok": false, "action": "evolution_plans", "error": err.to_string()})),
                }
            }
            "generate_evolution_plan" => {
                let repository = input.get("repository").and_then(|v| v.as_str()).unwrap_or("default").to_string();
                let dry_run = input.get("dry_run").and_then(|v| v.as_bool()).unwrap_or(true);
                match state.retrieval.lock().generate_evolution_plan(&repository, dry_run) {
                    Ok(result) => Json(serde_json::json!({"ok": true, "action": "generate_evolution_plan", "result": result})),
                    Err(err) => Json(serde_json::json!({"ok": false, "action": "generate_evolution_plan", "error": err.to_string()})),
                }
            }
            "todo_seed" => {
                let tasks = input.get("tasks")
                    .and_then(|v| serde_json::from_value::<Vec<retrieval::TodoTask>>(v.clone()).ok())
                    .unwrap_or_default();
                match state.retrieval.lock().seed_todo_tasks(tasks) {
                    Ok(result) => Json(serde_json::json!({"ok": true, "action": "todo_seed", "result": result})),
                    Err(err) => Json(serde_json::json!({"ok": false, "action": "todo_seed", "error": err.to_string()})),
                }
            }
            "todo_tasks" => match state.retrieval.lock().get_todo_tasks() {
                Ok(result) => Json(serde_json::json!({"ok": true, "action": "todo_tasks", "result": result})),
                Err(err) => Json(serde_json::json!({"ok": false, "action": "todo_tasks", "error": err.to_string()})),
            },
            "ab_test_dev" => {
                let feature_request = input.get("feature_request").and_then(|v| v.as_str()).map(|s| s.to_string());
                let provider = input.get("provider").and_then(|v| v.as_str()).map(|s| s.to_string());
                let max_context_tokens = input.get("max_context_tokens").and_then(|v| v.as_u64()).map(|v| v as usize);
                let single_file_fast_path = input.get("single_file_fast_path").and_then(|v| v.as_bool()).unwrap_or(true);
                let autoroute_first = input.get("autoroute_first").and_then(|v| v.as_bool()).unwrap_or(true);
                let scenario = input.get("scenario").and_then(|v| v.as_str()).map(|s| s.to_string());
                match state.retrieval.lock().run_ab_test_dev(
                    feature_request.as_deref(),
                    provider,
                    max_context_tokens,
                    single_file_fast_path,
                    autoroute_first,
                    scenario.as_deref(),
                ) {
                    Ok(result) => Json(serde_json::json!({"ok": true, "action": "ab_test_dev", "result": result})),
                    Err(err) => Json(serde_json::json!({"ok": false, "action": "ab_test_dev", "error": err.to_string()})),
                }
            }
            "ab_test_dev_results" => match state.retrieval.lock().get_ab_tests() {
                Ok(result) => Json(serde_json::json!({"ok": true, "action": "ab_test_dev_results", "result": result})),
                Err(err) => Json(serde_json::json!({"ok": false, "action": "ab_test_dev_results", "error": err.to_string()})),
            },
            "env_check" => {
                let repo_root = state.retrieval.lock().repo_root().to_path_buf();
                state.retrieval.lock().load_env();
                let env_path = repo_root.join(".semantic").join(".env");
                let env_exists = env_path.exists();
                let openai_set = std::env::var("OPENAI_API_KEY").map(|v| !v.trim().is_empty()).unwrap_or(false);
                let anthropic_set = std::env::var("ANTHROPIC_API_KEY").map(|v| !v.trim().is_empty()).unwrap_or(false);
                let openrouter_set = std::env::var("OPENROUTER_API_KEY").map(|v| !v.trim().is_empty()).unwrap_or(false);
                Json(serde_json::json!({
                    "ok": true,
                    "action": "env_check",
                    "result": {
                        "repo_root": repo_root,
                        "env_path": env_path,
                        "env_exists": env_exists,
                        "env": {
                            "OPENAI_API_KEY": openai_set,
                            "ANTHROPIC_API_KEY": anthropic_set,
                            "OPENROUTER_API_KEY": openrouter_set
                        }
                    }
                }))
            }
            "workspace_mode_get" => {
                let ws = state.workspace_state.lock();
                Json(serde_json::json!({
                    "ok": true,
                    "action": "workspace_mode_get",
                    "result": {
                        "workspace_mode_enabled": ws.workspace_mode_enabled,
                        "workspace_roots": ws.workspace_roots,
                        "primary_root": ws.primary_root,
                        "note": "Toggle with action=workspace_mode_set, action_input={\"enabled\": true|false}"
                    }
                }))
            }
            "workspace_mode_set" => {
                let enabled = input.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false);
                let mut ws = state.workspace_state.lock();
                let changed = ws.workspace_mode_enabled != enabled;
                ws.workspace_mode_enabled = enabled;
                let roots = ws.workspace_roots.clone();
                let primary = ws.primary_root.clone();
                drop(ws);
                if changed && enabled {
                    // Re-index all workspace roots in a background thread so the
                    // HTTP response returns immediately.
                    let retrieval = state.retrieval.clone();
                    let roots_for_closure = roots.clone();
                    let primary_for_closure = primary.clone();
                    let index_storage = {
                        let (db_path, tantivy_path) = storage::default_paths(&primary);
                        storage::Storage::open(&db_path, &tantivy_path)
                    };
                    match index_storage {
                        Ok(index_storage) => {
                            tokio::task::spawn_blocking(move || {
                                let mut indexer = indexer::Indexer::new(index_storage);
                                if let Err(e) = indexer.index_workspace(&primary_for_closure, &roots_for_closure) {
                                    tracing::error!("workspace index failed: {e}");
                                } else {
                                    tracing::info!("workspace index complete: {} extra roots", roots_for_closure.len());
                                }
                                // Invalidate retrieval cache after re-index.
                                let _ = retrieval.lock().index_revision();
                            });
                        }
                        Err(e) => {
                            return Json(serde_json::json!({
                                "ok": false,
                                "action": "workspace_mode_set",
                                "error": format!("failed to open storage for workspace index: {e}")
                            }));
                        }
                    }
                } else if changed && !enabled {
                    // Re-index only the primary root to remove workspace files.
                    let retrieval = state.retrieval.clone();
                    let index_storage = {
                        let (db_path, tantivy_path) = storage::default_paths(&primary);
                        storage::Storage::open(&db_path, &tantivy_path)
                    };
                    match index_storage {
                        Ok(index_storage) => {
                            tokio::task::spawn_blocking(move || {
                                let mut indexer = indexer::Indexer::new(index_storage);
                                if let Err(e) = indexer.index_repo(&primary) {
                                    tracing::error!("primary re-index failed: {e}");
                                } else {
                                    tracing::info!("reverted to primary-only index");
                                }
                                let _ = retrieval.lock().index_revision();
                            });
                        }
                        Err(e) => {
                            return Json(serde_json::json!({
                                "ok": false,
                                "action": "workspace_mode_set",
                                "error": format!("failed to open storage for primary re-index: {e}")
                            }));
                        }
                    }
                }
                Json(serde_json::json!({
                    "ok": true,
                    "action": "workspace_mode_set",
                    "result": {
                        "workspace_mode_enabled": enabled,
                        "indexing_triggered": changed,
                        "workspace_roots": roots,
                        "note": if enabled {
                            "Workspace indexing started in background. Retrieval will cover all projects once complete."
                        } else {
                            "Reverted to primary-root-only indexing. Re-index started in background."
                        }
                    }
                }))
            }
            unknown => Json(serde_json::json!({"ok": false, "error": format!("unknown action '{unknown}'")})),
        };
    }

    let task = body.task.clone().unwrap_or_default();
    let intent = detect_ide_intent(&task);
    let max_tokens = body.max_tokens.unwrap_or(1400);
    let single_file_fast_path = body.single_file_fast_path.unwrap_or(true);
    let reference_only = body.reference_only.unwrap_or(true);
    let mapping_mode = body.mapping_mode.clone();
    let max_footprint_items = body.max_footprint_items;
    let reuse_session_context = body.reuse_session_context.unwrap_or(true);
    let include_summary = body.include_summary.unwrap_or(false);
    let auto_minimal_raw = body.auto_minimal_raw.unwrap_or(true);

    let planned_request = RetrievalRequest {
        operation: Operation::GetPlannedContext,
        name: None,
        query: Some(task.clone()),
        file: None,
        start_line: None,
        end_line: None,
        max_tokens: Some(max_tokens),
        workspace_scope: None,
        limit: None,
        node_id: None,
        radius: None,
        logic_radius: None,
        dependency_radius: None,
        edit_description: None,
        patch_mode: None,
        run_tests: None,
        workspace_mode: None,
    };

    let planned = state
        .retrieval
        .lock()
        .handle_with_options_ext(
            planned_request,
            Some(single_file_fast_path),
            Some(!reference_only),
            mapping_mode.as_deref(),
            max_footprint_items,
        );

    let (selected_tool, mut result) = match planned {
        Ok(r) => ("get_planned_context", r.result),
        Err(_) => {
            let fallback = RetrievalRequest {
                operation: Operation::SearchSemanticSymbol,
                name: None,
                query: Some(task.clone()),
                file: None,
                start_line: None,
                end_line: None,
                max_tokens: None,
                workspace_scope: None,
                limit: Some(8),
                node_id: None,
                radius: None,
                logic_radius: None,
                dependency_radius: None,
                edit_description: None,
                patch_mode: None,
                run_tests: None,
                workspace_mode: None,
            };
            match state.retrieval.lock().handle(fallback) {
                Ok(r) => ("search_semantic_symbol", r.result),
                Err(err) => {
                    return Json(serde_json::json!({
                        "ok": false,
                        "intent": intent,
                        "selected_tool": "none",
                        "error": err.to_string()
                    }));
                }
            }
        }
    };

    let mut reused_context_count = 0usize;
    if let Some(session_id) = body.session_id {
        let index_revision = state.retrieval.lock().index_revision();
        let mut middleware = state.semantic_middleware.lock();
        let session = touch_or_create_session(&mut middleware, &session_id, index_revision);
        if reuse_session_context {
            reused_context_count = apply_session_context_reuse(&mut result, session);
        }
        if let Some(symbol) = result.get("symbol").and_then(|v| v.as_str()) {
            session.last_target_symbols.push_back(symbol.to_string());
            while session.last_target_symbols.len() > 32 {
                session.last_target_symbols.pop_front();
            }
            session
                .intent_symbol_cache
                .insert(task.to_lowercase(), symbol.to_string());
        }
    }

    let confidence_score = result
        .get("confidence_score")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.55);
    if reference_only && auto_minimal_raw {
        if (0.50..0.75).contains(&confidence_score) {
            if let Some(seed) = build_minimal_raw_seed(&result, &state) {
                if let Some(obj) = result.as_object_mut() {
                    obj.insert("minimal_raw_seed".to_string(), seed);
                }
            }
        } else if confidence_score < 0.50 {
            if let Some(raw) = build_low_confidence_raw_context(&result, &state, 2) {
                if let Some(obj) = result.as_object_mut() {
                    obj.insert("low_confidence_raw_context".to_string(), raw);
                }
            }
        }
    }

    Json(serde_json::json!({
        "ok": true,
        "intent": intent,
        "selected_tool": selected_tool,
        "single_file_fast_path": single_file_fast_path,
        "reference_only": reference_only,
        "mapping_mode": mapping_mode.unwrap_or_else(|| "footprint_first".to_string()),
        "reuse_session_context": reuse_session_context,
        "reused_context_count": reused_context_count,
        "result": result,
        "project_summary": if include_summary {
            state.retrieval.lock().with_storage(|storage| {
                project_summariser::ProjectSummariser::new(storage)
                    .build(800)
                    .ok()
                    .map(|doc| serde_json::json!({
                        "summary_text": doc.summary_text,
                        "token_estimate": doc.token_estimate,
                    }))
            })
        } else {
            None
        }
    }))
    });
    emit_task_finished(&telemetry, &scope, "tool_routing", &response.0);
    response
}

fn detect_ide_intent(task: &str) -> &'static str {
    let t = task.to_lowercase();
    if t.contains("fix") || t.contains("bug") || t.contains("error") {
        "debug"
    } else if t.contains("refactor") || t.contains("rewrite") || t.contains("optimize") {
        "refactor"
    } else if t.contains("add") || t.contains("implement") || t.contains("change") {
        "implement"
    } else {
        "understand"
    }
}

fn build_minimal_raw_seed(
    planned_result: &serde_json::Value,
    state: &AppState,
) -> Option<serde_json::Value> {
    let first = planned_result.get("context")?.as_array()?.first()?;
    let file = first.get("file")?.as_str()?.to_string();
    let start = first.get("start")?.as_u64()? as u32;
    let end = first.get("end")?.as_u64()? as u32;
    let clipped_end = start.saturating_add(40).min(end);

    let req = RetrievalRequest {
        operation: Operation::GetCodeSpan,
        name: None,
        query: None,
        file: Some(file.clone()),
        start_line: Some(start),
        end_line: Some(clipped_end),
        max_tokens: None,
        workspace_scope: None,
        limit: None,
        node_id: None,
        radius: None,
        logic_radius: None,
        dependency_radius: None,
        edit_description: None,
        patch_mode: None,
        run_tests: None,
        workspace_mode: None,
    };
    let result = state.retrieval.lock().handle(req).ok()?;
    Some(serde_json::json!({
        "file": file,
        "start": start,
        "end": clipped_end,
        "code_span": result.result
    }))
}

fn build_low_confidence_raw_context(
    planned_result: &serde_json::Value,
    state: &AppState,
    max_items: usize,
) -> Option<serde_json::Value> {
    let ctx = planned_result.get("context")?.as_array()?;
    let mut out = Vec::new();
    for item in ctx.iter().take(max_items) {
        let file = item.get("file")?.as_str()?.to_string();
        let start = item.get("start")?.as_u64()? as u32;
        let end = item.get("end")?.as_u64()? as u32;
        let clipped_end = start.saturating_add(40).min(end);
        let req = RetrievalRequest {
            operation: Operation::GetCodeSpan,
            name: None,
            query: None,
            file: Some(file.clone()),
            start_line: Some(start),
            end_line: Some(clipped_end),
            max_tokens: None,
            workspace_scope: None,
            limit: None,
            node_id: None,
            radius: None,
            logic_radius: None,
            dependency_radius: None,
            edit_description: None,
            patch_mode: None,
            run_tests: None,
            workspace_mode: None,
        };
        let res = state.retrieval.lock().handle(req).ok()?;
        out.push(serde_json::json!({
            "file": file,
            "start": start,
            "end": clipped_end,
            "code_span": res.result
        }));
    }
    Some(serde_json::json!(out))
}

async fn get_performance_stats(State(state): State<AppState>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "ok": true,
        "result": state.retrieval.lock().get_performance_stats()
    }))
}

#[derive(Debug, serde::Deserialize)]
struct SymbolHintsQuery {
    symbol: String,
}

async fn get_control_flow_hints(
    State(state): State<AppState>,
    Query(query): Query<SymbolHintsQuery>,
) -> Json<serde_json::Value> {
    match state.retrieval.lock().get_control_flow_hints(&query.symbol) {
        Ok(result) => Json(serde_json::json!({"ok": true, "result": result})),
        Err(err) => Json(serde_json::json!({"ok": false, "error": err.to_string()})),
    }
}

async fn get_data_flow_hints(
    State(state): State<AppState>,
    Query(query): Query<SymbolHintsQuery>,
) -> Json<serde_json::Value> {
    match state.retrieval.lock().get_data_flow_hints(&query.symbol) {
        Ok(result) => Json(serde_json::json!({"ok": true, "result": result})),
        Err(err) => Json(serde_json::json!({"ok": false, "error": err.to_string()})),
    }
}

#[derive(Debug, serde::Deserialize)]
struct HybridContextRequest {
    query: String,
    max_tokens: Option<usize>,
    single_file_fast_path: Option<bool>,
}

async fn get_hybrid_ranked_context(
    State(state): State<AppState>,
    Json(body): Json<HybridContextRequest>,
) -> Json<serde_json::Value> {
    match state.retrieval.lock().get_hybrid_ranked_context(
        &body.query,
        body.max_tokens.unwrap_or(1400),
        body.single_file_fast_path.unwrap_or(true),
    ) {
        Ok(result) => Json(serde_json::json!({"ok": true, "result": result})),
        Err(err) => Json(serde_json::json!({"ok": false, "error": err.to_string()})),
    }
}

async fn get_llm_tools(State(state): State<AppState>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "ok": true,
        "result": state.retrieval.lock().get_llm_tools(),
    }))
}

async fn get_semantic_ui() -> Html<String> {
    Html(
        r#"<!doctype html>
<html><head><meta charset="utf-8"/><title>Semantic Layer UI</title>
<style>
body{font-family:Segoe UI,Arial,sans-serif;max-width:980px;margin:24px auto;padding:0 16px}
textarea,input,select{width:100%;margin:6px 0 12px 0;padding:8px}
button{padding:8px 12px;margin-right:8px}
pre{background:#f4f4f4;padding:12px;overflow:auto;white-space:pre-wrap}
</style>
</head><body>
<h2>Semantic Layer Controls</h2>
<p>Use this UI to toggle semantic retrieval and run operations without writing raw HTTP manually.</p>
<label>Operation</label>
<select id="operation">
  <option>GetPlannedContext</option>
  <option>SearchSymbol</option>
  <option>GetCodeSpan</option>
  <option>GetLogicNodes</option>
  <option>GetControlFlowSlice</option>
  <option>GetDataFlowSlice</option>
  <option>GetLogicClusters</option>
  <option>GetHybridRankedContext</option>
  <option>GetPerformanceStats</option>
  <option>PlanSafeEdit</option>
</select>
<label>Name / Symbol</label><input id="name" placeholder="retryRequest"/>
<label>Query</label><input id="query" placeholder="todo app add due date"/>
<label>File</label><input id="file" placeholder="test_repo/todo_app/src/taskStore.ts"/>
<label>Start line</label><input id="start" value="1"/>
<label>End line</label><input id="end" value="40"/>
<label>Edit description</label><input id="edit" placeholder="Add due date validation"/>
<label>Session id (optional, required only when semantic middleware semantic-first is enabled)</label><input id="session_id" placeholder="dev-session-1"/>
<label><input type="checkbox" id="semantic_enabled" checked/> Semantic enabled</label><br/>
<label><input type="checkbox" id="reference_only" checked/> Reference-only mode (default)</label><br/>
<label><input type="checkbox" id="single_file_fast_path"/> Single-file fast path</label><br/>
<label>Mapping mode</label>
<select id="mapping_mode">
  <option value="footprint_first">footprint_first (default)</option>
  <option value="legacy_full">legacy_full</option>
</select>
<label>Max footprint items</label><input id="max_footprint_items" value="120"/>
<label><input type="checkbox" id="reuse_session_context" checked/> Reuse session context</label><br/>
<label><input type="checkbox" id="compressed"/> Input compressed</label><br/>
<label>Original query (required when compressed semantic is used)</label><input id="original_query" placeholder="original user query"/>
<button onclick="loadTools()">GET /llm_tools</button>
<button onclick="sendRetrieve()">POST /retrieve</button>
<pre id="out"></pre>
<script>
async function loadTools(){
  const r=await fetch('/llm_tools'); const j=await r.json();
  document.getElementById('out').textContent=JSON.stringify(j,null,2);
}
async function sendRetrieve(){
  const body={
    operation:document.getElementById('operation').value,
    name:document.getElementById('name').value||null,
    query:document.getElementById('query').value||null,
    file:document.getElementById('file').value||null,
    start_line:Number(document.getElementById('start').value)||null,
    end_line:Number(document.getElementById('end').value)||null,
    max_tokens:1800,
    limit:20,
    logic_radius:1,
    dependency_radius:1,
    edit_description:document.getElementById('edit').value||null,
    semantic_enabled:document.getElementById('semantic_enabled').checked,
    reference_only:document.getElementById('reference_only').checked,
    single_file_fast_path:document.getElementById('single_file_fast_path').checked,
    mapping_mode:document.getElementById('mapping_mode').value||'footprint_first',
    max_footprint_items:Number(document.getElementById('max_footprint_items').value)||120,
    reuse_session_context:document.getElementById('reuse_session_context').checked,
    input_compressed:document.getElementById('compressed').checked,
    original_query:document.getElementById('original_query').value||null,
    session_id:document.getElementById('session_id').value||null
  };
  const r=await fetch('/retrieve',{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify(body)});
  const j=await r.json();
  document.getElementById('out').textContent=JSON.stringify(j,null,2);
}
</script>
</body></html>"#.to_string(),
    )
}

#[derive(serde::Deserialize)]
struct EditRequestBody {
    symbol: String,
    edit: String,
    patch_mode: Option<PatchApplicationMode>,
    run_tests: Option<bool>,
    max_tokens: Option<usize>,
    session_id: Option<String>,
}

async fn edit(
    State(state): State<AppState>,
    Json(body): Json<EditRequestBody>,
) -> Json<serde_json::Value> {
    let telemetry = state.retrieval.lock().telemetry();
    let scope = next_task_scope(&telemetry, body.session_id.as_deref(), "edit");
    emit_task_started(
        &telemetry,
        &scope,
        "code_generation",
        metadata_pairs([
            ("symbol", serde_json::json!(body.symbol)),
            ("session_id", serde_json::json!(body.session_id)),
            ("max_tokens", serde_json::json!(body.max_tokens)),
            ("run_tests", serde_json::json!(body.run_tests)),
        ]),
    );
    let response = telemetry.with_task_scope(scope.clone(), || {
        {
            let mut middleware = state.semantic_middleware.lock();
            let now = now_epoch_s();
            middleware
                .sessions
                .retain(|_, v| now.saturating_sub(v.last_seen_epoch_s) <= SESSION_TTL_SECS);
            if middleware.semantic_first_enabled {
                let session_id = body.session_id.as_deref().unwrap_or_default();
                if session_id.is_empty() || !middleware.sessions.contains_key(session_id) {
                    return Json(serde_json::json!({
                        "ok": false,
                        "error": "semantic-first middleware blocked edit: call /retrieve first with a session_id, then retry /edit with the same session_id."
                    }));
                }
            }
        }

        let request = RetrievalRequest {
            operation: Operation::PlanSafeEdit,
            name: Some(body.symbol),
            query: None,
            file: None,
            start_line: None,
            end_line: None,
            max_tokens: Some(body.max_tokens.unwrap_or(4000)),
            workspace_scope: None,
            limit: None,
            node_id: None,
            radius: None,
            logic_radius: None,
            dependency_radius: None,
            edit_description: Some(body.edit),
            patch_mode: body.patch_mode,
            run_tests: body.run_tests,
            workspace_mode: None,
        };
        let result = state.retrieval.lock().handle(request);
        match result {
            Ok(response) => Json(success(response)),
            Err(err) => Json(serde_json::json!({"ok": false, "error": err.to_string()})),
        }
    });
    emit_task_finished(&telemetry, &scope, "code_generation", &response.0);
    response
}

#[derive(Debug, serde::Deserialize)]
struct SemanticMiddlewareUpdate {
    semantic_first_enabled: bool,
}

async fn get_semantic_middleware(State(state): State<AppState>) -> Json<serde_json::Value> {
    let mut middleware = state.semantic_middleware.lock();
    let now = now_epoch_s();
    middleware
        .sessions
        .retain(|_, v| now.saturating_sub(v.last_seen_epoch_s) <= SESSION_TTL_SECS);
    Json(serde_json::json!({
        "ok": true,
        "semantic_first_enabled": middleware.semantic_first_enabled,
        "tracked_sessions": middleware.sessions.len()
    }))
}

async fn update_semantic_middleware(
    State(state): State<AppState>,
    Json(body): Json<SemanticMiddlewareUpdate>,
) -> Json<serde_json::Value> {
    let mut middleware = state.semantic_middleware.lock();
    middleware.semantic_first_enabled = body.semantic_first_enabled;
    if !body.semantic_first_enabled {
        middleware.sessions.clear();
    }
    Json(serde_json::json!({
        "ok": true,
        "semantic_first_enabled": middleware.semantic_first_enabled,
        "tracked_sessions": middleware.sessions.len()
    }))
}

#[derive(Debug, serde::Deserialize)]
struct PatchMemoryQuery {
    repository: Option<String>,
    symbol: Option<String>,
    model: Option<String>,
    time_range: Option<String>,
}

fn parse_time_range(input: &Option<String>) -> Option<(u64, u64)> {
    let value = input.as_ref()?;
    let mut parts = value.split(',');
    let from = parts.next()?.trim().parse::<u64>().ok()?;
    let to = parts.next()?.trim().parse::<u64>().ok()?;
    Some((from, to))
}

async fn get_patch_memory(
    State(state): State<AppState>,
    Query(query): Query<PatchMemoryQuery>,
) -> Json<serde_json::Value> {
    match state.retrieval.lock().get_patch_memory(
        query.repository,
        query.symbol,
        query.model,
        parse_time_range(&query.time_range),
    ) {
        Ok(result) => Json(serde_json::json!({"ok": true, "result": result})),
        Err(err) => Json(serde_json::json!({"ok": false, "error": err.to_string()})),
    }
}

async fn get_patch_stats(
    State(state): State<AppState>,
    Query(query): Query<PatchMemoryQuery>,
) -> Json<serde_json::Value> {
    match state.retrieval.lock().get_patch_stats(
        query.repository,
        query.symbol,
        query.model,
        parse_time_range(&query.time_range),
    ) {
        Ok(result) => Json(serde_json::json!({"ok": true, "result": result})),
        Err(err) => Json(serde_json::json!({"ok": false, "error": err.to_string()})),
    }
}

async fn get_model_performance(
    State(state): State<AppState>,
    Query(query): Query<PatchMemoryQuery>,
) -> Json<serde_json::Value> {
    match state.retrieval.lock().get_model_performance(
        query.repository,
        query.symbol,
        query.model,
        parse_time_range(&query.time_range),
    ) {
        Ok(result) => Json(serde_json::json!({"ok": true, "result": result})),
        Err(err) => Json(serde_json::json!({"ok": false, "error": err.to_string()})),
    }
}

async fn get_refactor_status(State(state): State<AppState>) -> Json<serde_json::Value> {
    let repo_root = state.retrieval.lock().repo_root().to_path_buf();
    match refactor_graph::RefactorExecutor::status(&repo_root) {
        Ok(status) => Json(serde_json::json!({"ok": true, "result": status})),
        Err(err) => Json(serde_json::json!({"ok": false, "error": err.to_string()})),
    }
}

#[derive(Debug, serde::Deserialize)]
struct EvolutionQuery {
    repository: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct EvolutionPlanRequest {
    repository: String,
    dry_run: Option<bool>,
}

async fn get_evolution_issues(
    State(state): State<AppState>,
    Query(query): Query<EvolutionQuery>,
) -> Json<serde_json::Value> {
    let repository = query.repository.unwrap_or_else(|| "default".to_string());
    match state.retrieval.lock().get_evolution_issues(&repository) {
        Ok(result) => Json(serde_json::json!({"ok": true, "result": result})),
        Err(err) => Json(serde_json::json!({"ok": false, "error": err.to_string()})),
    }
}

async fn get_evolution_plans(
    State(state): State<AppState>,
    Query(query): Query<EvolutionQuery>,
) -> Json<serde_json::Value> {
    let repository = query.repository.unwrap_or_else(|| "default".to_string());
    match state.retrieval.lock().get_evolution_plans(&repository) {
        Ok(result) => Json(serde_json::json!({"ok": true, "result": result})),
        Err(err) => Json(serde_json::json!({"ok": false, "error": err.to_string()})),
    }
}

async fn generate_evolution_plan(
    State(state): State<AppState>,
    Json(body): Json<EvolutionPlanRequest>,
) -> Json<serde_json::Value> {
    match state
        .retrieval
        .lock()
        .generate_evolution_plan(&body.repository, body.dry_run.unwrap_or(true))
    {
        Ok(result) => Json(serde_json::json!({"ok": true, "result": result})),
        Err(err) => Json(serde_json::json!({"ok": false, "error": err.to_string()})),
    }
}

async fn get_organization_graph(State(state): State<AppState>) -> Json<serde_json::Value> {
    match state.retrieval.lock().get_organization_graph() {
        Ok(result) => Json(serde_json::json!({"ok": true, "result": result})),
        Err(err) => Json(serde_json::json!({"ok": false, "error": err.to_string()})),
    }
}

async fn get_service_graph(State(state): State<AppState>) -> Json<serde_json::Value> {
    match state.retrieval.lock().get_service_graph() {
        Ok(result) => Json(serde_json::json!({"ok": true, "result": result})),
        Err(err) => Json(serde_json::json!({"ok": false, "error": err.to_string()})),
    }
}

#[derive(Debug, serde::Deserialize)]
struct OrgRefactorRequest {
    origin_repo: String,
}

async fn plan_org_refactor(
    State(state): State<AppState>,
    Json(body): Json<OrgRefactorRequest>,
) -> Json<serde_json::Value> {
    match state.retrieval.lock().plan_org_refactor(&body.origin_repo) {
        Ok(result) => Json(serde_json::json!({"ok": true, "result": result})),
        Err(err) => Json(serde_json::json!({"ok": false, "error": err.to_string()})),
    }
}

async fn get_org_refactor_status(State(state): State<AppState>) -> Json<serde_json::Value> {
    match state.retrieval.lock().get_org_refactor_status() {
        Ok(result) => Json(serde_json::json!({"ok": true, "result": result})),
        Err(err) => Json(serde_json::json!({"ok": false, "error": err.to_string()})),
    }
}

#[derive(Debug, serde::Deserialize)]
struct DebugFailureRequest {
    event_id: String,
    repository: String,
    timestamp: u64,
    failure_type: String,
    stack_trace: Vec<String>,
    error_message: String,
}

fn parse_failure_type(value: &str) -> debug_graph::FailureType {
    match value {
        "test_failure" => debug_graph::FailureType::TestFailure,
        "runtime_exception" => debug_graph::FailureType::RuntimeException,
        "build_failure" => debug_graph::FailureType::BuildFailure,
        "integration_failure" => debug_graph::FailureType::IntegrationFailure,
        _ => debug_graph::FailureType::RuntimeException,
    }
}

async fn debug_failure(
    State(state): State<AppState>,
    Json(body): Json<DebugFailureRequest>,
) -> Json<serde_json::Value> {
    let event = debug_graph::FailureEvent {
        event_id: body.event_id,
        repository: body.repository,
        timestamp: body.timestamp,
        failure_type: parse_failure_type(&body.failure_type),
        stack_trace: body.stack_trace,
        error_message: body.error_message,
    };
    match state.retrieval.lock().debug_failure(event) {
        Ok(result) => Json(serde_json::json!({"ok": true, "result": result})),
        Err(err) => Json(serde_json::json!({"ok": false, "error": err.to_string()})),
    }
}

async fn get_debug_graph(State(state): State<AppState>) -> Json<serde_json::Value> {
    match state.retrieval.lock().get_debug_graph() {
        Ok(result) => Json(serde_json::json!({"ok": true, "result": result})),
        Err(err) => Json(serde_json::json!({"ok": false, "error": err.to_string()})),
    }
}

async fn get_root_cause_candidates(State(state): State<AppState>) -> Json<serde_json::Value> {
    match state.retrieval.lock().get_root_cause_candidates() {
        Ok(result) => Json(serde_json::json!({"ok": true, "result": result})),
        Err(err) => Json(serde_json::json!({"ok": false, "error": err.to_string()})),
    }
}

async fn get_test_gaps(State(state): State<AppState>) -> Json<serde_json::Value> {
    match state.retrieval.lock().get_test_gaps() {
        Ok(result) => Json(serde_json::json!({"ok": true, "result": result})),
        Err(err) => Json(serde_json::json!({"ok": false, "error": err.to_string()})),
    }
}

#[derive(Debug, serde::Deserialize)]
struct GenerateTestsRequest {
    target_symbol: String,
    framework: Option<String>,
}

async fn generate_tests(
    State(state): State<AppState>,
    Json(body): Json<GenerateTestsRequest>,
) -> Json<serde_json::Value> {
    match state
        .retrieval
        .lock()
        .generate_tests(&body.target_symbol, &body.framework.unwrap_or_else(|| "rust-test".to_string()))
    {
        Ok(result) => Json(serde_json::json!({"ok": true, "result": result})),
        Err(err) => Json(serde_json::json!({"ok": false, "error": err.to_string()})),
    }
}

#[derive(Debug, serde::Deserialize)]
struct ApplyTestsRequest {
    repository: String,
    target_symbol: String,
    framework: Option<String>,
}

async fn apply_tests(
    State(state): State<AppState>,
    Json(body): Json<ApplyTestsRequest>,
) -> Json<serde_json::Value> {
    match state.retrieval.lock().apply_tests(
        &body.repository,
        &body.target_symbol,
        &body.framework.unwrap_or_else(|| "rust-test".to_string()),
    ) {
        Ok(result) => Json(serde_json::json!({"ok": true, "result": result})),
        Err(err) => Json(serde_json::json!({"ok": false, "error": err.to_string()})),
    }
}

async fn get_pipeline_graph(State(state): State<AppState>) -> Json<serde_json::Value> {
    match state.retrieval.lock().get_pipeline_graph() {
        Ok(result) => Json(serde_json::json!({"ok": true, "result": result})),
        Err(err) => Json(serde_json::json!({"ok": false, "error": err.to_string()})),
    }
}

#[derive(Debug, serde::Deserialize)]
struct AnalyzePipelineRequest {
    failure_stage: String,
    failure_message: String,
}

async fn analyze_pipeline(
    State(state): State<AppState>,
    Json(body): Json<AnalyzePipelineRequest>,
) -> Json<serde_json::Value> {
    let request = pipeline_graph::PipelineAnalysisRequest {
        failure_stage: body.failure_stage,
        failure_message: body.failure_message,
    };
    match state.retrieval.lock().analyze_pipeline(request) {
        Ok(result) => Json(serde_json::json!({"ok": true, "result": result})),
        Err(err) => Json(serde_json::json!({"ok": false, "error": err.to_string()})),
    }
}

async fn get_deployment_history(State(state): State<AppState>) -> Json<serde_json::Value> {
    match state.retrieval.lock().get_deployment_history() {
        Ok(result) => Json(serde_json::json!({"ok": true, "result": result})),
        Err(err) => Json(serde_json::json!({"ok": false, "error": err.to_string()})),
    }
}

async fn get_ab_tests(State(state): State<AppState>) -> Json<serde_json::Value> {
    match state.retrieval.lock().get_ab_tests() {
        Ok(result) => Json(serde_json::json!({"ok": true, "result": result})),
        Err(err) => Json(serde_json::json!({"ok": false, "error": err.to_string()})),
    }
}

async fn get_env_check(State(state): State<AppState>) -> Json<serde_json::Value> {
    let repo_root = state.retrieval.lock().repo_root().to_path_buf();
    state.retrieval.lock().load_env();
    let env_path = repo_root.join(".semantic").join(".env");
    let env_exists = env_path.exists();
    let openai_set = std::env::var("OPENAI_API_KEY")
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false);
    let anthropic_set = std::env::var("ANTHROPIC_API_KEY")
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false);
    let openrouter_set = std::env::var("OPENROUTER_API_KEY")
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false);
    Json(serde_json::json!({
        "ok": true,
        "repo_root": repo_root,
        "env_path": env_path,
        "env_exists": env_exists,
        "env": {
            "OPENAI_API_KEY": openai_set,
            "ANTHROPIC_API_KEY": anthropic_set,
            "OPENROUTER_API_KEY": openrouter_set
        }
    }))
}


#[derive(Debug, serde::Deserialize)]
struct SeedTodoRequest {
    tasks: Vec<retrieval::TodoTask>,
}

async fn seed_todo_tasks(
    State(state): State<AppState>,
    Json(body): Json<SeedTodoRequest>,
) -> Json<serde_json::Value> {
    match state.retrieval.lock().seed_todo_tasks(body.tasks) {
        Ok(result) => Json(serde_json::json!({"ok": true, "result": result})),
        Err(err) => Json(serde_json::json!({"ok": false, "error": err.to_string()})),
    }
}

async fn get_todo_tasks(State(state): State<AppState>) -> Json<serde_json::Value> {
    match state.retrieval.lock().get_todo_tasks() {
        Ok(result) => Json(serde_json::json!({"ok": true, "result": result})),
        Err(err) => Json(serde_json::json!({"ok": false, "error": err.to_string()})),
    }
}
#[derive(Debug, serde::Deserialize)]
struct ABTestDevRequest {
    feature_request: Option<String>,
    provider: Option<String>,
    max_context_tokens: Option<usize>,
    single_file_fast_path: Option<bool>,
    autoroute_first: Option<bool>,
    scenario: Option<String>,
}

async fn run_ab_test_dev(
    State(state): State<AppState>,
    Json(body): Json<ABTestDevRequest>,
) -> Json<serde_json::Value> {
    let feature_request = body.feature_request;
    let provider = body.provider;
    let max_context_tokens = body.max_context_tokens;
    let single_file_fast_path = body.single_file_fast_path.unwrap_or(true);
    let autoroute_first = body.autoroute_first.unwrap_or(true);
    let scenario = body.scenario;
    let retrieval = state.retrieval.clone();
    let result = tokio::task::spawn_blocking(move || {
        retrieval
            .lock()
            .run_ab_test_dev(
                feature_request.as_deref(),
                provider,
                max_context_tokens,
                single_file_fast_path,
                autoroute_first,
                scenario.as_deref(),
            )
    })
    .await;

    match result {
        Ok(Ok(result)) => Json(serde_json::json!({"ok": true, "result": result})),
        Ok(Err(err)) => Json(serde_json::json!({"ok": false, "error": err.to_string()})),
        Err(err) => Json(serde_json::json!({"ok": false, "error": err.to_string()})),
    }
}

async fn get_mcp_settings_ui(State(state): State<AppState>) -> Html<String> {
    let repo_root = state.retrieval.lock().repo_root().to_path_buf();
    let llm_cfg = std::fs::read_to_string(repo_root.join(".semantic").join("llm_config.toml"))
        .unwrap_or_default();
    let routing_cfg = std::fs::read_to_string(repo_root.join(".semantic").join("llm_routing.toml"))
        .unwrap_or_default();
    let metrics = std::fs::read_to_string(repo_root.join(".semantic").join("model_metrics.json"))
        .unwrap_or_else(|_| "{}".to_string());
    let env_content = std::fs::read_to_string(repo_root.join(".semantic").join(".env"))
        .unwrap_or_default();

    Html(format!(
        r#"<html><head><title>MCP Settings</title></head><body>
<h2>MCP / LLM Settings</h2>
<form method="post" action="/mcp_settings_update">
<label>llm_config.toml</label><br/>
<textarea name="llm_config" rows="16" cols="120">{}</textarea><br/><br/>
<label>llm_routing.toml</label><br/>
<textarea name="llm_routing" rows="12" cols="120">{}</textarea><br/><br/>
<label>model_metrics.json</label><br/>
<textarea name="model_metrics" rows="10" cols="120">{}</textarea><br/><br/>
<label>.env (API keys)</label><br/>
<textarea name="env_file" rows="10" cols="120">{}</textarea><br/><br/>
<label><input type="checkbox" name="enable_ollama" value="true"/>Enable Ollama</label><br/><br/>
<button type="submit">Save</button>
</form>
</body></html>"#,
        html_escape(&llm_cfg),
        html_escape(&routing_cfg),
        html_escape(&metrics),
        html_escape(&env_content)
    ))
}

#[derive(Debug, serde::Deserialize)]
struct MCPSettingsUpdate {
    llm_config: String,
    llm_routing: String,
    model_metrics: String,
    env_file: String,
    enable_ollama: Option<String>,
}

async fn update_mcp_settings(
    State(state): State<AppState>,
    axum::extract::Form(body): axum::extract::Form<MCPSettingsUpdate>,
) -> Html<String> {
    let repo_root = state.retrieval.lock().repo_root().to_path_buf();
    let semantic_dir = repo_root.join(".semantic");
    let _ = std::fs::create_dir_all(&semantic_dir);

    let mut llm_config = body.llm_config;
    if body.enable_ollama.is_some() && !llm_config.contains("ollama") {
        llm_config.push_str(
            "\n[providers]\nollama = \"http://127.0.0.1:11434\"\n\n[provider_settings.ollama]\nmodel = \"llama3.1:8b\"\n",
        );
    }

    let _ = std::fs::write(semantic_dir.join("llm_config.toml"), llm_config);
    let _ = std::fs::write(semantic_dir.join("llm_routing.toml"), body.llm_routing);
    let _ = std::fs::write(semantic_dir.join("model_metrics.json"), body.model_metrics);
    let _ = std::fs::write(semantic_dir.join(".env"), body.env_file);

    Html("<html><body><h3>Saved.</h3><a href=\"/mcp_settings_ui\">Back</a></body></html>".to_string())
}

fn html_escape(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[derive(Debug, serde::Deserialize)]
struct ProjectSummaryQuery {
    max_tokens: Option<usize>,
    format: Option<String>,
}

async fn get_project_summary(
    State(state): State<AppState>,
    Query(query): Query<ProjectSummaryQuery>,
) -> Json<serde_json::Value> {
    let max_tokens = query.max_tokens.unwrap_or(800);
    let result = state.retrieval.lock().with_storage(|storage| {
        project_summariser::ProjectSummariser::new(storage).build(max_tokens)
    });
    match result {
        Ok(doc) => {
            let telemetry = state.retrieval.lock().telemetry();
            let mut event = telemetry.event(None, "project_summary_built", "api", Some("planning"));
            event.metadata = telemetry.sanitize_metadata(serde_json::json!({
                "token_estimate": doc.token_estimate,
                "file_count": doc.file_count,
                "module_count": doc.module_count,
                "cache_hit": false,
            }));
            telemetry.emit(event);
            let want_markdown = query.format.as_deref() == Some("markdown");
            Json(serde_json::json!({
                "ok": true,
                "token_estimate": doc.token_estimate,
                "summary": doc.to_json(),
                "summary_text": if want_markdown { doc.summary_text } else { String::new() },
            }))
        }
        Err(err) => Json(serde_json::json!({"ok": false, "error": err.to_string()})),
    }
}

fn success(response: RetrievalResponse) -> serde_json::Value {
    serde_json::json!({
        "ok": true,
        "operation": response.operation,
        "result": response.result,
    })
}

