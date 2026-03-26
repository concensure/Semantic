use anyhow::Result;
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::Html,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::sync::Arc;

#[derive(Clone)]
struct AppState {
    semantic_base_url: String,
    bridge_token: String,
    client: reqwest::Client,
}

#[derive(Debug, Deserialize)]
struct MCPToolCall {
    tool: String,
    input: serde_json::Value,
}

#[derive(Debug, Serialize)]
struct MCPToolResult {
    ok: bool,
    tool: String,
    result: serde_json::Value,
}

#[tokio::main]
async fn main() -> Result<()> {
    let semantic_base_url =
        std::env::var("SEMANTIC_BASE_URL").unwrap_or_else(|_| "http://127.0.0.1:4317".to_string());
    let bridge_token = std::env::var("MCP_BRIDGE_TOKEN").unwrap_or_else(|_| "change-me".to_string());

    let state = Arc::new(AppState {
        semantic_base_url,
        bridge_token,
        client: reqwest::Client::new(),
    });

    let app = Router::new()
        .route("/", get(index))
        .route("/health", get(health))
        .route("/mcp/tools", get(list_tools))
        .route("/mcp/tools/call", post(call_tool))
        .with_state(state);

    let addr: SocketAddr = "127.0.0.1:4321".parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({"status":"ok","service":"mcp_bridge"}))
}

async fn index() -> Html<String> {
    Html(
        "<html><body><h3>mcp_bridge running</h3><p>Use /health, /mcp/tools, /mcp/tools/call</p></body></html>"
            .to_string(),
    )
}

async fn call_tool(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(call): Json<MCPToolCall>,
) -> Result<Json<MCPToolResult>, (StatusCode, String)> {
    let auth = headers
        .get("x-mcp-token")
        .and_then(|h| h.to_str().ok())
        .unwrap_or_default();
    if auth != state.bridge_token {
        return Err((StatusCode::UNAUTHORIZED, "invalid bridge token".to_string()));
    }

    let (method, endpoint, body) = resolve_tool_request(&call.tool, call.input)?;
    let mut url = format!("{}{}", state.semantic_base_url, endpoint);
    let response = match method {
        "GET" => {
            if let Some(serde_json::Value::Object(map)) = body.as_ref() {
                let params: Vec<(String, String)> = map
                    .iter()
                    .map(|(k, v)| {
                        let sval = v
                            .as_str()
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| v.to_string());
                        (k.clone(), sval)
                    })
                    .collect();
                if !params.is_empty() {
                    let query = params
                        .iter()
                        .map(|(k, v)| {
                            format!(
                                "{}={}",
                                urlencoding::encode(k),
                                urlencoding::encode(v)
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("&");
                    if !query.is_empty() {
                        url = format!("{url}?{query}");
                    }
                }
            }
            state
                .client
                .get(&url)
                .send()
                .await
                .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?
        }
        "POST" => state
            .client
            .post(&url)
            .json(&body.unwrap_or_else(|| serde_json::json!({})))
            .send()
            .await
            .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?,
        _ => return Err((StatusCode::BAD_REQUEST, "unsupported method".to_string())),
    };

    let status = response.status();
    let json = response
        .json::<serde_json::Value>()
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
    if !status.is_success() {
        return Err((StatusCode::BAD_GATEWAY, json.to_string()));
    }

    Ok(Json(MCPToolResult {
        ok: true,
        tool: call.tool,
        result: json,
    }))
}

async fn list_tools() -> Json<serde_json::Value> {
    // Exactly two primary tools. Legacy aliases are routed but not advertised
    // to keep the tool-list token cost low (~120 tokens vs ~1400 previously).
    let primary = serde_json::json!([
        {
            "name": "retrieve",
            "method": "POST",
            "endpoint": "/retrieve",
            "description": "All retrieval and graph operations. Required: `operation` (string). Key operations: GetRepoMap, GetFileOutline (file), SearchSymbol (name), GetCodeSpan (file,start_line,end_line), GetPlannedContext (query,max_tokens), GetReasoningContext (name,logic_radius,dependency_radius), PlanSafeEdit (name,edit_description), GetHybridRankedContext (query), GetPerformanceStats, GetProjectSummary (max_tokens?,format?). Optional: session_id, single_file_fast_path, reference_only, mapping_mode, workspace_mode (bool)."
        },
        {
            "name": "ide_autoroute",
            "method": "POST",
            "endpoint": "/ide_autoroute",
            "description": "Two modes. (1) Intent: pass `task` string — auto-retrieves context. Optional: session_id, max_tokens, single_file_fast_path, reference_only. (2) Action: pass `action` + `action_input`. Actions: debug_failure, generate_tests, apply_tests, analyze_pipeline, patch_memory, patch_stats, model_performance, refactor_status, evolution_issues, evolution_plans, generate_evolution_plan, organization_graph, service_graph, plan_org_refactor, semantic_middleware_get, semantic_middleware_set, workspace_mode_get, workspace_mode_set (enabled:bool), env_check, ab_test_dev, llm_tools."
        }
    ]);
    Json(serde_json::json!({"ok": true, "tools": primary}))
}

fn resolve_tool_request(
    name: &str,
    input: serde_json::Value,
) -> Result<(&'static str, &'static str, Option<serde_json::Value>), (StatusCode, String)> {
    // Primary tool: "retrieve" — pass input directly to /retrieve (caller supplies `operation`)
    if name == "retrieve" {
        let mut payload = match input {
            serde_json::Value::Object(map) => serde_json::Value::Object(map),
            _ => serde_json::json!({}),
        };
        if let Some(obj) = payload.as_object_mut() {
            obj.entry("semantic_enabled".to_string())
                .or_insert(serde_json::json!(true));
        }
        return Ok(("POST", "/retrieve", Some(payload)));
    }
    if let Some(operation) = map_retrieve_operation(name) {
        let mut payload = match input {
            serde_json::Value::Object(map) => serde_json::Value::Object(map),
            _ => serde_json::json!({}),
        };
        if let Some(obj) = payload.as_object_mut() {
            obj.insert("operation".to_string(), serde_json::json!(operation));
            // Ensure semantic layer is used for retrieve-backed tools unless caller overrides it.
            obj.entry("semantic_enabled".to_string())
                .or_insert(serde_json::json!(true));
        }
        return Ok(("POST", "/retrieve", Some(payload)));
    }
    if let Some(action) = map_ide_action(name) {
        return Ok((
            "POST",
            "/ide_autoroute",
            Some(serde_json::json!({
                "action": action,
                "action_input": input
            })),
        ));
    }
    if let Ok((method, endpoint)) = map_tool(name) {
        return Ok((method, endpoint, Some(input)));
    }
    Err((StatusCode::BAD_REQUEST, format!("unknown tool '{name}'")))
}

fn map_tool(name: &str) -> Result<(&'static str, &'static str), (StatusCode, String)> {
    match name {
        "llm_tools" => Ok(("GET", "/llm_tools")),
        "debug_failure" => Ok(("POST", "/debug_failure")),
        "debug_graph" => Ok(("GET", "/debug_graph")),
        "root_cause_candidates" => Ok(("GET", "/root_cause_candidates")),
        "test_gaps" => Ok(("GET", "/test_gaps")),
        "generate_tests" => Ok(("POST", "/generate_tests")),
        "apply_tests" => Ok(("POST", "/apply_tests")),
        "pipeline_graph" => Ok(("GET", "/pipeline_graph")),
        "analyze_pipeline" => Ok(("POST", "/analyze_pipeline")),
        "deployment_history" => Ok(("GET", "/deployment_history")),
        "performance_stats" => Ok(("GET", "/performance_stats")),
        "ide_autoroute" => Ok(("POST", "/ide_autoroute")),
        "semantic_first" => Ok(("POST", "/ide_autoroute")),
        "control_flow_hints" => Ok(("GET", "/control_flow_hints")),
        "data_flow_hints" => Ok(("GET", "/data_flow_hints")),
        "hybrid_ranked_context" => Ok(("POST", "/hybrid_ranked_context")),
        "ab_test_dev" => Ok(("POST", "/ab_test_dev")),
        "ab_test_dev_results" => Ok(("GET", "/ab_test_dev")),
        _ => Err((StatusCode::BAD_REQUEST, format!("unknown tool '{name}'"))),
    }
}

fn map_retrieve_operation(name: &str) -> Option<&'static str> {
    match name {
        // Primary tool: caller passes operation directly — pass through as-is
        "retrieve" => None, // handled by pass-through in resolve_tool_request
        // Legacy named tools
        "get_repo_map" => Some("GetRepoMap"),
        "get_file_outline" => Some("GetFileOutline"),
        "search_symbol" => Some("SearchSymbol"),
        "get_code_span" => Some("GetCodeSpan"),
        "get_logic_nodes" => Some("GetLogicNodes"),
        "get_control_flow_slice" => Some("GetControlFlowSlice"),
        "get_data_flow_slice" => Some("GetDataFlowSlice"),
        "get_logic_clusters" => Some("GetLogicClusters"),
        "get_dependency_neighborhood" => Some("GetDependencyNeighborhood"),
        "get_reasoning_context" => Some("GetReasoningContext"),
        "get_planned_context" => Some("GetPlannedContext"),
        "plan_safe_edit" => Some("PlanSafeEdit"),
        // New unified operations (also accessible via legacy names)
        "get_control_flow_hints" => Some("GetControlFlowHints"),
        "get_data_flow_hints" => Some("GetDataFlowHints"),
        "get_hybrid_ranked_context" => Some("GetHybridRankedContext"),
        "get_debug_graph" => Some("GetDebugGraph"),
        "get_pipeline_graph" => Some("GetPipelineGraph"),
        "get_root_cause_candidates" => Some("GetRootCauseCandidates"),
        "get_test_gaps" => Some("GetTestGaps"),
        "get_deployment_history" => Some("GetDeploymentHistory"),
        "get_performance_stats" => Some("GetPerformanceStats"),
        "get_project_summary" => Some("GetProjectSummary"),
        _ => None,
    }
}

fn map_ide_action(name: &str) -> Option<&'static str> {
    match name {
        "debug_failure" => Some("debug_failure"),
        "generate_tests" => Some("generate_tests"),
        "apply_tests" => Some("apply_tests"),
        "analyze_pipeline" => Some("analyze_pipeline"),
        "llm_tools" => Some("llm_tools"),
        "patch_memory" => Some("patch_memory"),
        "patch_stats" => Some("patch_stats"),
        "model_performance" => Some("model_performance"),
        "organization_graph" => Some("organization_graph"),
        "service_graph" => Some("service_graph"),
        "plan_org_refactor" => Some("plan_org_refactor"),
        "org_refactor_status" => Some("org_refactor_status"),
        "refactor_status" => Some("refactor_status"),
        "evolution_issues" => Some("evolution_issues"),
        "evolution_plans" => Some("evolution_plans"),
        "generate_evolution_plan" => Some("generate_evolution_plan"),
        "todo_seed" => Some("todo_seed"),
        "todo_tasks" => Some("todo_tasks"),
        "ab_test_dev" => Some("ab_test_dev"),
        "ab_test_dev_results" => Some("ab_test_dev_results"),
        "semantic_middleware" => Some("semantic_middleware_get"),
        "env_check" => Some("env_check"),
        "workspace_mode_get" => Some("workspace_mode_get"),
        "workspace_mode_set" => Some("workspace_mode_set"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::resolve_tool_request;
    use serde_json::json;

    #[test]
    fn routes_graph_legacy_tool_through_retrieve() {
        let (_, endpoint, payload) =
            resolve_tool_request("get_control_flow_slice", json!({"name": "retryRequest"}))
                .expect("resolve");
        assert_eq!(endpoint, "/retrieve");
        let payload = payload.expect("payload");
        assert_eq!(
            payload.get("operation").and_then(|v| v.as_str()),
            Some("GetControlFlowSlice")
        );
    }

    #[test]
    fn routes_patch_memory_legacy_tool_through_ide_autoroute() {
        let (_, endpoint, payload) =
            resolve_tool_request("patch_memory", json!({"symbol": "retryRequest"}))
                .expect("resolve");
        assert_eq!(endpoint, "/ide_autoroute");
        let payload = payload.expect("payload");
        assert_eq!(payload.get("action").and_then(|v| v.as_str()), Some("patch_memory"));
    }
}
