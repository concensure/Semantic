use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SymbolType {
    Function,
    Class,
    Import,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolRecord {
    pub id: Option<i64>,
    pub repo_id: i64,
    pub name: String,
    pub symbol_type: SymbolType,
    pub file: String,
    pub start_line: u32,
    pub end_line: u32,
    pub language: String,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DependencyRecord {
    pub id: Option<i64>,
    pub repo_id: i64,
    pub caller_symbol: String,
    pub callee_symbol: String,
    pub file: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum LogicNodeType {
    Loop,
    Conditional,
    Try,
    Catch,
    Finally,
    Return,
    Call,
    Await,
    Assignment,
    Throw,
    Switch,
    Case,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogicNodeRecord {
    pub id: Option<i64>,
    pub symbol_id: i64,
    pub node_type: LogicNodeType,
    pub start_line: usize,
    pub end_line: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogicEdgeRecord {
    pub id: Option<i64>,
    pub from_node_id: i64,
    pub to_node_id: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleRecord {
    pub id: Option<i64>,
    pub name: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleFile {
    pub module_id: i64,
    pub file_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleDependency {
    pub from_module: i64,
    pub to_module: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workspace {
    pub repositories: Vec<RepositoryRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepositoryRecord {
    pub id: Option<i64>,
    pub name: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoDependency {
    pub from_repo: i64,
    pub to_repo: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignatureImpact {
    pub callers: Vec<String>,
    pub implementors: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImpactReport {
    pub changed_symbol: String,
    pub impacted_symbols: Vec<String>,
    pub impacted_files: Vec<String>,
    pub impacted_tests: Vec<String>,
    pub signature_impact: Option<SignatureImpact>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum EditType {
    ModifyLogic,
    ChangeSignature,
    RefactorFunction,
    RenameSymbol,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum PatchApplicationMode {
    Confirm,
    AutoApply,
    PreviewOnly,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditContextItem {
    pub file_path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub priority: u8,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditPlan {
    pub target_symbol: String,
    pub edit_type: EditType,
    pub impacted_symbols: Vec<String>,
    pub required_context: Vec<EditContextItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ASTTransformation {
    ReplaceFunctionBody,
    RenameSymbol,
    ChangeSignature,
    InsertNode,
    DeleteNode,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ASTEdit {
    pub target_symbol: String,
    pub transformation: ASTTransformation,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PatchRepresentation {
    UnifiedDiff(String),
    ASTTransform(ASTEdit),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodePatch {
    pub file_path: String,
    pub representation: PatchRepresentation,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SafeEditRequest {
    pub symbol: String,
    pub edit_description: String,
    pub max_tokens: usize,
    pub patch_mode: Option<PatchApplicationMode>,
    pub run_tests: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatchRecord {
    pub patch_id: String,
    pub timestamp: u64,
    pub repository: String,
    pub file_path: String,
    pub target_symbol: String,
    pub edit_type: EditType,
    pub model_used: String,
    pub provider: String,
    pub diff: String,
    pub ast_transform: Option<ASTTransformation>,
    pub impacted_symbols: Vec<String>,
    pub approved_by_user: bool,
    pub validation_passed: bool,
    pub tests_passed: bool,
    pub rollback_occurred: bool,
    pub rollback_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelPerformance {
    pub model: String,
    pub success_rate: f32,
    pub avg_latency_ms: u32,
    pub avg_cost: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditRiskScore {
    pub edit_type: EditType,
    pub risk: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvolutionRisk {
    pub risk_score: f32,
    pub reasoning: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileRecord {
    pub id: Option<i64>,
    pub path: String,
    pub language: String,
    pub checksum: String,
    pub indexed_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParsedFile {
    pub file: String,
    pub language: String,
    pub symbols: Vec<SymbolRecord>,
    pub dependencies: Vec<DependencyRecord>,
    pub logic_nodes: Vec<LogicNodeRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetrievalRequest {
    pub operation: Operation,
    pub name: Option<String>,
    pub query: Option<String>,
    pub file: Option<String>,
    pub start_line: Option<u32>,
    pub end_line: Option<u32>,
    pub max_tokens: Option<usize>,
    pub workspace_scope: Option<Vec<String>>,
    pub limit: Option<usize>,
    pub node_id: Option<i64>,
    pub radius: Option<usize>,
    pub logic_radius: Option<usize>,
    pub dependency_radius: Option<usize>,
    pub edit_description: Option<String>,
    pub patch_mode: Option<PatchApplicationMode>,
    pub run_tests: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    GetRepoMap,
    GetFileOutline,
    SearchSymbol,
    GetFunction,
    GetClass,
    GetDependencies,
    GetCodeSpan,
    GetLogicNodes,
    GetLogicNeighborhood,
    GetLogicSpan,
    GetDependencyNeighborhood,
    GetSymbolNeighborhood,
    GetReasoningContext,
    GetPlannedContext,
    GetRepoMapHierarchy,
    GetModuleDependencies,
    SearchSemanticSymbol,
    GetWorkspaceReasoningContext,
    PlanSafeEdit,
    // Unified retrieve operations (previously separate endpoints)
    GetControlFlowHints,
    GetDataFlowHints,
    GetHybridRankedContext,
    GetDebugGraph,
    GetPipelineGraph,
    GetRootCauseCandidates,
    GetTestGaps,
    GetDeploymentHistory,
    GetPerformanceStats,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetrievalResponse {
    pub operation: Operation,
    pub result: serde_json::Value,
}
