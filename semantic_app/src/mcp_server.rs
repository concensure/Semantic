use crate::runtime::AppRuntime;
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
struct MCPState {
    runtime: AppRuntime,
    bridge_token: String,
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

pub async fn serve(runtime: AppRuntime, bridge_token: String, addr: SocketAddr) -> Result<()> {
    let state = Arc::new(MCPState {
        runtime,
        bridge_token,
    });
    let app = Router::new()
        .route("/", get(index))
        .route("/health", get(health))
        .route("/mcp/tools", get(list_tools))
        .route("/mcp/tools/call", post(call_tool))
        .with_state(state);
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
    State(state): State<Arc<MCPState>>,
    headers: HeaderMap,
    Json(call): Json<MCPToolCall>,
) -> Result<Json<MCPToolResult>, (StatusCode, String)> {
    let auth = headers
        .get("x-mcp-token")
        .and_then(|header| header.to_str().ok())
        .unwrap_or_default();
    if auth != state.bridge_token {
        return Err((StatusCode::UNAUTHORIZED, "invalid bridge token".to_string()));
    }
    let result = resolve_tool_request(&state.runtime, &call.tool, call.input)?;
    Ok(Json(MCPToolResult {
        ok: result.get("ok").and_then(|v| v.as_bool()).unwrap_or(false),
        tool: call.tool,
        result,
    }))
}

async fn list_tools() -> Json<serde_json::Value> {
    Json(serde_json::json!({"ok": true, "tools": [
        {
            "name": "retrieve",
            "method": "POST",
            "endpoint": "/retrieve",
            "description": "Targeted retrieval and analysis routed through the shared application layer."
        },
        {
            "name": "ide_autoroute",
            "method": "POST",
            "endpoint": "/ide_autoroute",
            "description": "Intent routing and action dispatch routed through the shared application layer."
        }
    ]}))
}

fn resolve_tool_request(
    runtime: &AppRuntime,
    name: &str,
    input: serde_json::Value,
) -> Result<serde_json::Value, (StatusCode, String)> {
    if name == "retrieve" {
        let body = serde_json::from_value(input)
            .map_err(|err| (StatusCode::BAD_REQUEST, err.to_string()))?;
        return Ok(runtime.handle_retrieve(body));
    }
    if let Some(operation) = map_retrieve_operation(name) {
        let mut payload = input;
        let Some(obj) = payload.as_object_mut() else {
            return Err((
                StatusCode::BAD_REQUEST,
                "retrieve payload must be an object".to_string(),
            ));
        };
        obj.insert("operation".to_string(), serde_json::json!(operation));
        obj.entry("semantic_enabled".to_string())
            .or_insert(serde_json::json!(true));
        let body = serde_json::from_value(payload)
            .map_err(|err| (StatusCode::BAD_REQUEST, err.to_string()))?;
        return Ok(runtime.handle_retrieve(body));
    }
    if let Some(action) = map_ide_action(name) {
        return Ok(
            runtime.handle_autoroute(crate::models::IdeAutoRouteRequest {
                task: None,
                action: Some(action.to_string()),
                action_input: Some(input),
                session_id: None,
                max_tokens: None,
                single_file_fast_path: None,
                reference_only: None,
                mapping_mode: None,
                max_footprint_items: None,
                reuse_session_context: None,
                auto_minimal_raw: None,
                include_summary: None,
                raw_expansion_mode: None,
                auto_index_target: None,
            }),
        );
    }
    if name == "ide_autoroute" {
        let body = serde_json::from_value(input)
            .map_err(|err| (StatusCode::BAD_REQUEST, err.to_string()))?;
        return Ok(runtime.handle_autoroute(body));
    }
    Err((StatusCode::BAD_REQUEST, format!("unknown tool '{name}'")))
}

fn map_retrieve_operation(name: &str) -> Option<&'static str> {
    match name {
        "retrieve" => None,
        "get_repo_map" => Some("GetRepoMap"),
        "get_directory_brief" => Some("GetDirectoryBrief"),
        "get_file_outline" => Some("GetFileOutline"),
        "get_file_brief" => Some("GetFileBrief"),
        "search_symbol" => Some("SearchSymbol"),
        "get_symbol_brief" => Some("GetSymbolBrief"),
        "get_code_span" => Some("GetCodeSpan"),
        "get_logic_nodes" => Some("GetLogicNodes"),
        "get_control_flow_slice" => Some("GetControlFlowSlice"),
        "get_data_flow_slice" => Some("GetDataFlowSlice"),
        "get_logic_clusters" => Some("GetLogicClusters"),
        "get_dependency_neighborhood" => Some("GetDependencyNeighborhood"),
        "get_reasoning_context" => Some("GetReasoningContext"),
        "get_planned_context" => Some("GetPlannedContext"),
        "plan_safe_edit" => Some("PlanSafeEdit"),
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
        "get_section_brief" => Some("GetSectionBrief"),
        "GetKnowledgeGraph" | "get_knowledge_graph" => Some("GetKnowledgeGraph"),
        "AppendKnowledge" | "append_knowledge" => Some("AppendKnowledge"),
        "GetChangePropagation" | "get_change_propagation" => Some("GetChangePropagation"),
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
    use super::{map_ide_action, map_retrieve_operation};

    #[test]
    fn maps_legacy_retrieve_tools() {
        assert_eq!(
            map_retrieve_operation("get_directory_brief"),
            Some("GetDirectoryBrief")
        );
        assert_eq!(
            map_retrieve_operation("get_file_brief"),
            Some("GetFileBrief")
        );
        assert_eq!(
            map_retrieve_operation("get_symbol_brief"),
            Some("GetSymbolBrief")
        );
        assert_eq!(
            map_retrieve_operation("get_section_brief"),
            Some("GetSectionBrief")
        );
        assert_eq!(
            map_retrieve_operation("get_control_flow_slice"),
            Some("GetControlFlowSlice")
        );
    }

    #[test]
    fn maps_legacy_route_actions() {
        assert_eq!(map_ide_action("patch_memory"), Some("patch_memory"));
    }
}
