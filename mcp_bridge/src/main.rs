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

#[derive(Debug, Serialize)]
struct MCPToolSpec {
    name: &'static str,
    method: &'static str,
    endpoint: &'static str,
    notes: Option<&'static str>,
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
    let url = format!("{}{}", state.semantic_base_url, endpoint);
    let response = match method {
        "GET" => state
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?,
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
    let tools: Vec<MCPToolSpec> = vec![
        MCPToolSpec { name: "llm_tools", method: "GET", endpoint: "/llm_tools", notes: None },
        MCPToolSpec { name: "debug_failure", method: "POST", endpoint: "/debug_failure", notes: None },
        MCPToolSpec { name: "debug_graph", method: "GET", endpoint: "/debug_graph", notes: None },
        MCPToolSpec { name: "root_cause_candidates", method: "GET", endpoint: "/root_cause_candidates", notes: None },
        MCPToolSpec { name: "test_gaps", method: "GET", endpoint: "/test_gaps", notes: None },
        MCPToolSpec { name: "generate_tests", method: "POST", endpoint: "/generate_tests", notes: None },
        MCPToolSpec { name: "apply_tests", method: "POST", endpoint: "/apply_tests", notes: None },
        MCPToolSpec { name: "pipeline_graph", method: "GET", endpoint: "/pipeline_graph", notes: None },
        MCPToolSpec { name: "analyze_pipeline", method: "POST", endpoint: "/analyze_pipeline", notes: None },
        MCPToolSpec { name: "deployment_history", method: "GET", endpoint: "/deployment_history", notes: None },
        MCPToolSpec { name: "ab_test_dev", method: "POST", endpoint: "/ab_test_dev", notes: Some("accepts single_file_fast_path=true|false") },
        MCPToolSpec { name: "ab_test_dev_results", method: "GET", endpoint: "/ab_test_dev", notes: None },
        MCPToolSpec { name: "get_repo_map", method: "POST", endpoint: "/retrieve", notes: Some("supports single_file_fast_path") },
        MCPToolSpec { name: "get_file_outline", method: "POST", endpoint: "/retrieve", notes: Some("supports single_file_fast_path") },
        MCPToolSpec { name: "search_symbol", method: "POST", endpoint: "/retrieve", notes: Some("supports single_file_fast_path") },
        MCPToolSpec { name: "get_code_span", method: "POST", endpoint: "/retrieve", notes: Some("supports single_file_fast_path") },
        MCPToolSpec { name: "get_logic_nodes", method: "POST", endpoint: "/retrieve", notes: Some("supports single_file_fast_path") },
        MCPToolSpec { name: "get_dependency_neighborhood", method: "POST", endpoint: "/retrieve", notes: Some("supports single_file_fast_path") },
        MCPToolSpec { name: "get_reasoning_context", method: "POST", endpoint: "/retrieve", notes: Some("supports single_file_fast_path") },
        MCPToolSpec { name: "get_planned_context", method: "POST", endpoint: "/retrieve", notes: Some("supports single_file_fast_path") },
        MCPToolSpec { name: "plan_safe_edit", method: "POST", endpoint: "/retrieve", notes: Some("supports single_file_fast_path") },
    ];
    Json(serde_json::json!({"ok": true, "tools": tools}))
}

fn resolve_tool_request(
    name: &str,
    input: serde_json::Value,
) -> Result<(&'static str, &'static str, Option<serde_json::Value>), (StatusCode, String)> {
    if let Ok((method, endpoint)) = map_tool(name) {
        return Ok((method, endpoint, Some(input)));
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
        "ab_test_dev" => Ok(("POST", "/ab_test_dev")),
        "ab_test_dev_results" => Ok(("GET", "/ab_test_dev")),
        _ => Err((StatusCode::BAD_REQUEST, format!("unknown tool '{name}'"))),
    }
}

fn map_retrieve_operation(name: &str) -> Option<&'static str> {
    match name {
        "get_repo_map" => Some("GetRepoMap"),
        "get_file_outline" => Some("GetFileOutline"),
        "search_symbol" => Some("SearchSymbol"),
        "get_code_span" => Some("GetCodeSpan"),
        "get_logic_nodes" => Some("GetLogicNodes"),
        "get_dependency_neighborhood" => Some("GetDependencyNeighborhood"),
        "get_reasoning_context" => Some("GetReasoningContext"),
        "get_planned_context" => Some("GetPlannedContext"),
        "plan_safe_edit" => Some("PlanSafeEdit"),
        _ => None,
    }
}
