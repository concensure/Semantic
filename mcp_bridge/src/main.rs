use anyhow::Result;
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
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
        .route("/health", get(health))
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

    let (method, endpoint) = map_tool(&call.tool)?;
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
            .json(&call.input)
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

fn map_tool(name: &str) -> Result<(&'static str, &'static str), (StatusCode, String)> {
    match name {
        "debug_failure" => Ok(("POST", "/debug_failure")),
        "debug_graph" => Ok(("GET", "/debug_graph")),
        "root_cause_candidates" => Ok(("GET", "/root_cause_candidates")),
        "test_gaps" => Ok(("GET", "/test_gaps")),
        "generate_tests" => Ok(("POST", "/generate_tests")),
        "apply_tests" => Ok(("POST", "/apply_tests")),
        "pipeline_graph" => Ok(("GET", "/pipeline_graph")),
        "analyze_pipeline" => Ok(("POST", "/analyze_pipeline")),
        "deployment_history" => Ok(("GET", "/deployment_history")),
        "ab_test" => Ok(("POST", "/ab_test")),
        _ => Err((StatusCode::BAD_REQUEST, format!("unknown tool '{name}'"))),
    }
}
