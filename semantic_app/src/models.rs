use engine::{PatchApplicationMode, RetrievalRequest};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RetrieveRequestBody {
    #[serde(flatten)]
    pub request: RetrievalRequest,
    pub semantic_enabled: Option<bool>,
    pub input_compressed: Option<bool>,
    pub original_query: Option<String>,
    pub single_file_fast_path: Option<bool>,
    pub reference_only: Option<bool>,
    pub mapping_mode: Option<String>,
    pub max_footprint_items: Option<usize>,
    pub reuse_session_context: Option<bool>,
    pub session_id: Option<String>,
    pub raw_expansion_mode: Option<String>,
    pub auto_index_target: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct IdeAutoRouteRequest {
    pub task: Option<String>,
    pub action: Option<String>,
    pub action_input: Option<serde_json::Value>,
    pub session_id: Option<String>,
    pub max_tokens: Option<usize>,
    pub single_file_fast_path: Option<bool>,
    pub reference_only: Option<bool>,
    pub mapping_mode: Option<String>,
    pub max_footprint_items: Option<usize>,
    pub reuse_session_context: Option<bool>,
    pub auto_minimal_raw: Option<bool>,
    pub include_summary: Option<bool>,
    pub raw_expansion_mode: Option<String>,
    pub auto_index_target: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct EditRequestBody {
    pub symbol: String,
    pub edit: String,
    pub patch_mode: Option<PatchApplicationMode>,
    pub run_tests: Option<bool>,
    pub max_tokens: Option<usize>,
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HybridContextRequest {
    pub query: String,
    pub max_tokens: Option<usize>,
    pub single_file_fast_path: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SymbolHintsQuery {
    pub symbol: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PatchMemoryQuery {
    pub repository: Option<String>,
    pub symbol: Option<String>,
    pub model: Option<String>,
    pub time_range: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct EvolutionQuery {
    pub repository: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct EvolutionPlanRequest {
    pub repository: String,
    pub dry_run: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OrgRefactorRequest {
    pub origin_repo: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DebugFailureRequest {
    pub event_id: String,
    pub repository: String,
    pub timestamp: u64,
    pub failure_type: String,
    pub stack_trace: Vec<String>,
    pub error_message: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GenerateTestsRequest {
    pub target_symbol: String,
    pub framework: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ApplyTestsRequest {
    pub repository: String,
    pub target_symbol: String,
    pub framework: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AnalyzePipelineRequest {
    pub failure_stage: String,
    pub failure_message: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SeedTodoRequest {
    pub tasks: Vec<retrieval::TodoTask>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ABTestDevRequest {
    pub feature_request: Option<String>,
    pub provider: Option<String>,
    pub max_context_tokens: Option<usize>,
    pub single_file_fast_path: Option<bool>,
    pub autoroute_first: Option<bool>,
    pub scenario: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SemanticMiddlewareUpdate {
    pub semantic_first_enabled: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MCPSettingsUpdate {
    pub llm_config: String,
    pub llm_routing: String,
    pub model_metrics: String,
    pub enable_ollama: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProjectSummaryQuery {
    pub max_tokens: Option<usize>,
    pub format: Option<String>,
}
