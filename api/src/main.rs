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
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use storage::default_paths;
use tracing::{error, info};
use watcher::RepoWatcher;

#[derive(Clone)]
struct AppState {
    retrieval: Arc<Mutex<RetrievalService>>,
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
    let _watcher = RepoWatcher::start(repo_root, shared_indexer)?;

    let state = AppState {
        retrieval: retrieval_service,
    };

    let app = Router::new()
        .route("/health", get(health))
        .route("/retrieve", post(retrieve))
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
        .route("/ab_test", post(run_ab_test))
        .route("/mcp_settings_ui", get(get_mcp_settings_ui))
        .route("/mcp_settings_update", post(update_mcp_settings))
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

async fn retrieve(
    State(state): State<AppState>,
    Json(request): Json<RetrievalRequest>,
) -> Json<serde_json::Value> {
    let result = state.retrieval.lock().handle(request);
    match result {
        Ok(response) => Json(success(response)),
        Err(err) => {
            error!("retrieval failed: {err}");
            Json(serde_json::json!({
                "ok": false,
                "error": err.to_string(),
            }))
        }
    }
}

#[derive(serde::Deserialize)]
struct EditRequestBody {
    symbol: String,
    edit: String,
    patch_mode: Option<PatchApplicationMode>,
    run_tests: Option<bool>,
    max_tokens: Option<usize>,
}

async fn edit(
    State(state): State<AppState>,
    Json(body): Json<EditRequestBody>,
) -> Json<serde_json::Value> {
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
    };
    let result = state.retrieval.lock().handle(request);
    match result {
        Ok(response) => Json(success(response)),
        Err(err) => Json(serde_json::json!({"ok": false, "error": err.to_string()})),
    }
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

#[derive(Debug, serde::Deserialize)]
struct ABTestRequest {
    prompt: String,
    symbol: Option<String>,
    provider: Option<String>,
}

async fn run_ab_test(
    State(state): State<AppState>,
    Json(body): Json<ABTestRequest>,
) -> Json<serde_json::Value> {
    match state
        .retrieval
        .lock()
        .run_ab_test(&body.prompt, body.symbol, body.provider)
    {
        Ok(result) => Json(serde_json::json!({"ok": true, "result": result})),
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

fn success(response: RetrievalResponse) -> serde_json::Value {
    serde_json::json!({
        "ok": true,
        "operation": response.operation,
        "result": response.result,
    })
}
