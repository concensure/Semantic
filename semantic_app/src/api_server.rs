use crate::models::{
    ABTestDevRequest, AnalyzePipelineRequest, ApplyTestsRequest, DebugFailureRequest,
    EditRequestBody, EvolutionPlanRequest, EvolutionQuery, GenerateTestsRequest,
    HybridContextRequest, MCPSettingsUpdate, OrgRefactorRequest, PatchMemoryQuery,
    ProjectSummaryQuery, RetrieveRequestBody, SeedTodoRequest, SemanticMiddlewareUpdate,
    SymbolHintsQuery,
};
use crate::runtime::AppRuntime;
use anyhow::Result;
use axum::{
    extract::{Query, State},
    response::Html,
    routing::{get, patch, post},
    Json, Router,
};
use std::net::SocketAddr;

pub fn build_router(runtime: AppRuntime) -> Router {
    Router::new()
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
        .with_state(runtime)
}

pub async fn serve(runtime: AppRuntime, addr: SocketAddr) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, build_router(runtime)).await?;
    Ok(())
}

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({"status": "ok"}))
}

async fn retrieve(
    State(runtime): State<AppRuntime>,
    Json(body): Json<RetrieveRequestBody>,
) -> Json<serde_json::Value> {
    Json(runtime.handle_retrieve(body))
}

async fn ide_autoroute(
    State(runtime): State<AppRuntime>,
    Json(body): Json<crate::models::IdeAutoRouteRequest>,
) -> Json<serde_json::Value> {
    Json(runtime.handle_autoroute(body))
}

async fn edit(
    State(runtime): State<AppRuntime>,
    Json(body): Json<EditRequestBody>,
) -> Json<serde_json::Value> {
    Json(runtime.handle_edit(body))
}

async fn get_semantic_middleware(State(runtime): State<AppRuntime>) -> Json<serde_json::Value> {
    Json(runtime.get_semantic_middleware())
}

async fn update_semantic_middleware(
    State(runtime): State<AppRuntime>,
    Json(body): Json<SemanticMiddlewareUpdate>,
) -> Json<serde_json::Value> {
    Json(runtime.update_semantic_middleware(body))
}

async fn get_patch_memory(
    State(runtime): State<AppRuntime>,
    Query(query): Query<PatchMemoryQuery>,
) -> Json<serde_json::Value> {
    Json(runtime.get_patch_memory(query))
}

async fn get_patch_stats(
    State(runtime): State<AppRuntime>,
    Query(query): Query<PatchMemoryQuery>,
) -> Json<serde_json::Value> {
    Json(runtime.get_patch_stats(query))
}

async fn get_model_performance(
    State(runtime): State<AppRuntime>,
    Query(query): Query<PatchMemoryQuery>,
) -> Json<serde_json::Value> {
    Json(runtime.get_model_performance(query))
}

async fn get_refactor_status(State(runtime): State<AppRuntime>) -> Json<serde_json::Value> {
    Json(runtime.get_refactor_status())
}

async fn get_evolution_issues(
    State(runtime): State<AppRuntime>,
    Query(query): Query<EvolutionQuery>,
) -> Json<serde_json::Value> {
    Json(runtime.get_evolution_issues(query))
}

async fn get_evolution_plans(
    State(runtime): State<AppRuntime>,
    Query(query): Query<EvolutionQuery>,
) -> Json<serde_json::Value> {
    Json(runtime.get_evolution_plans(query))
}

async fn generate_evolution_plan(
    State(runtime): State<AppRuntime>,
    Json(body): Json<EvolutionPlanRequest>,
) -> Json<serde_json::Value> {
    Json(runtime.generate_evolution_plan(body))
}

async fn get_organization_graph(State(runtime): State<AppRuntime>) -> Json<serde_json::Value> {
    Json(runtime.get_organization_graph())
}

async fn get_service_graph(State(runtime): State<AppRuntime>) -> Json<serde_json::Value> {
    Json(runtime.get_service_graph())
}

async fn plan_org_refactor(
    State(runtime): State<AppRuntime>,
    Json(body): Json<OrgRefactorRequest>,
) -> Json<serde_json::Value> {
    Json(runtime.plan_org_refactor(body))
}

async fn get_org_refactor_status(State(runtime): State<AppRuntime>) -> Json<serde_json::Value> {
    Json(runtime.get_org_refactor_status())
}

async fn debug_failure(
    State(runtime): State<AppRuntime>,
    Json(body): Json<DebugFailureRequest>,
) -> Json<serde_json::Value> {
    Json(runtime.debug_failure(body))
}

async fn get_debug_graph(State(runtime): State<AppRuntime>) -> Json<serde_json::Value> {
    Json(runtime.get_debug_graph())
}

async fn get_root_cause_candidates(State(runtime): State<AppRuntime>) -> Json<serde_json::Value> {
    Json(runtime.get_root_cause_candidates())
}

async fn get_test_gaps(State(runtime): State<AppRuntime>) -> Json<serde_json::Value> {
    Json(runtime.get_test_gaps())
}

async fn generate_tests(
    State(runtime): State<AppRuntime>,
    Json(body): Json<GenerateTestsRequest>,
) -> Json<serde_json::Value> {
    Json(runtime.generate_tests(body))
}

async fn apply_tests(
    State(runtime): State<AppRuntime>,
    Json(body): Json<ApplyTestsRequest>,
) -> Json<serde_json::Value> {
    Json(runtime.apply_tests(body))
}

async fn get_pipeline_graph(State(runtime): State<AppRuntime>) -> Json<serde_json::Value> {
    Json(runtime.get_pipeline_graph())
}

async fn analyze_pipeline(
    State(runtime): State<AppRuntime>,
    Json(body): Json<AnalyzePipelineRequest>,
) -> Json<serde_json::Value> {
    Json(runtime.analyze_pipeline(body))
}

async fn get_deployment_history(State(runtime): State<AppRuntime>) -> Json<serde_json::Value> {
    Json(runtime.get_deployment_history())
}

async fn get_ab_tests(State(runtime): State<AppRuntime>) -> Json<serde_json::Value> {
    Json(runtime.get_ab_tests())
}

async fn get_env_check(State(runtime): State<AppRuntime>) -> Json<serde_json::Value> {
    Json(runtime.get_env_check())
}

async fn seed_todo_tasks(
    State(runtime): State<AppRuntime>,
    Json(body): Json<SeedTodoRequest>,
) -> Json<serde_json::Value> {
    Json(runtime.seed_todo_tasks(body))
}

async fn get_todo_tasks(State(runtime): State<AppRuntime>) -> Json<serde_json::Value> {
    Json(runtime.get_todo_tasks())
}

async fn run_ab_test_dev(
    State(runtime): State<AppRuntime>,
    Json(body): Json<ABTestDevRequest>,
) -> Json<serde_json::Value> {
    Json(runtime.run_ab_test_dev(body))
}

async fn get_mcp_settings_ui(State(runtime): State<AppRuntime>) -> Html<String> {
    Html(runtime.get_mcp_settings_ui())
}

async fn update_mcp_settings(
    State(runtime): State<AppRuntime>,
    axum::extract::Form(body): axum::extract::Form<MCPSettingsUpdate>,
) -> Html<String> {
    Html(runtime.update_mcp_settings(body))
}

async fn get_project_summary(
    State(runtime): State<AppRuntime>,
    Query(query): Query<ProjectSummaryQuery>,
) -> Json<serde_json::Value> {
    Json(runtime.get_project_summary(query))
}

async fn get_performance_stats(State(runtime): State<AppRuntime>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "ok": true,
        "result": runtime.retrieval().lock().get_performance_stats()
    }))
}

async fn get_control_flow_hints(
    State(runtime): State<AppRuntime>,
    Query(query): Query<SymbolHintsQuery>,
) -> Json<serde_json::Value> {
    Json(runtime.get_control_flow_hints(query))
}

async fn get_data_flow_hints(
    State(runtime): State<AppRuntime>,
    Query(query): Query<SymbolHintsQuery>,
) -> Json<serde_json::Value> {
    Json(runtime.get_data_flow_hints(query))
}

async fn get_hybrid_ranked_context(
    State(runtime): State<AppRuntime>,
    Json(body): Json<HybridContextRequest>,
) -> Json<serde_json::Value> {
    Json(runtime.get_hybrid_ranked_context(body))
}

async fn get_llm_tools(State(runtime): State<AppRuntime>) -> Json<serde_json::Value> {
    Json(runtime.get_llm_tools())
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
<label>Session id</label><input id="session_id" placeholder="dev-session-1"/>
<label><input type="checkbox" id="semantic_enabled" checked/> Semantic enabled</label><br/>
<label><input type="checkbox" id="reference_only" checked/> Reference-only mode</label><br/>
<label><input type="checkbox" id="single_file_fast_path"/> Single-file fast path</label><br/>
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
    edit_description:document.getElementById('edit').value||null,
    semantic_enabled:document.getElementById('semantic_enabled').checked,
    reference_only:document.getElementById('reference_only').checked,
    single_file_fast_path:document.getElementById('single_file_fast_path').checked,
    session_id:document.getElementById('session_id').value||null
  };
  const r=await fetch('/retrieve',{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify(body)});
  const j=await r.json();
  document.getElementById('out').textContent=JSON.stringify(j,null,2);
}
</script>
</body></html>"#
            .to_string(),
    )
}
