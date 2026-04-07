use crate::config::ensure_semantic_config;
use crate::models::{IdeAutoRouteRequest, RetrieveRequestBody};
use crate::{api_server, mcp_server, AppRuntime, BootstrapIndexPolicy, RuntimeOptions};
use anyhow::{anyhow, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use engine::{Operation, RetrievalRequest};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "semantic-cli")]
#[command(about = "CLI-first Semantic runtime")]
struct Cli {
    #[arg(long)]
    repo: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Retrieve(RetrieveArgs),
    Route(RouteArgs),
    Index(IndexArgs),
    Status(OutputArgs),
    Edit(EditArgs),
    Serve(ServeArgs),
    Config(ConfigArgs),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum OutputMode {
    Json,
    Text,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum VerificationThreshold {
    NeedsReview,
    HighConfidence,
}

#[derive(Args)]
struct OutputArgs {
    #[arg(long, value_enum, default_value_t = OutputMode::Text)]
    output: OutputMode,
    #[arg(long)]
    compact: bool,
    #[arg(long)]
    verbose: bool,
    #[arg(long)]
    quality: bool,
}

#[derive(Args)]
struct RetrieveArgs {
    #[command(flatten)]
    output: OutputArgs,
    #[arg(long)]
    op: String,
    #[arg(long)]
    name: Option<String>,
    #[arg(long)]
    query: Option<String>,
    #[arg(long)]
    file: Option<String>,
    #[arg(long)]
    start_line: Option<u32>,
    #[arg(long)]
    end_line: Option<u32>,
    #[arg(long)]
    max_tokens: Option<usize>,
    #[arg(long)]
    limit: Option<usize>,
    #[arg(long)]
    session_id: Option<String>,
    #[arg(long)]
    path: Option<String>,
    #[arg(long)]
    reference_only: bool,
    #[arg(long)]
    single_file_fast_path: bool,
    #[arg(long)]
    raw_expansion_mode: Option<String>,
    #[arg(long)]
    auto_index_target: bool,
    #[arg(long)]
    heading: Option<String>,
}

#[derive(Args)]
struct RouteArgs {
    #[command(flatten)]
    output: OutputArgs,
    #[arg(long)]
    task: String,
    #[arg(long)]
    session_id: Option<String>,
    #[arg(long)]
    max_tokens: Option<usize>,
    #[arg(long)]
    include_summary: bool,
    #[arg(long)]
    raw_expansion_mode: Option<String>,
    #[arg(long)]
    auto_index_target: bool,
    #[arg(long)]
    require_high_confidence: bool,
    #[arg(long, value_enum)]
    min_verification: Option<VerificationThreshold>,
    #[arg(long)]
    require_mutation_ready: bool,
}

#[derive(Args)]
struct IndexArgs {
    #[arg(long)]
    watch: bool,
    #[arg(long)]
    workspace: bool,
    #[arg(long)]
    path: Vec<String>,
}

#[derive(Args)]
struct EditArgs {
    #[command(flatten)]
    output: OutputArgs,
    #[arg(long)]
    symbol: String,
    #[arg(long)]
    edit: String,
    #[arg(long)]
    session_id: Option<String>,
    #[arg(long)]
    max_tokens: Option<usize>,
    #[arg(long)]
    run_tests: bool,
}

#[derive(Args)]
struct ServeArgs {
    #[command(subcommand)]
    command: ServeCommand,
}

#[derive(Subcommand)]
enum ServeCommand {
    Api {
        #[arg(long, default_value = "127.0.0.1:4317")]
        addr: SocketAddr,
    },
    Mcp {
        #[arg(long, default_value = "127.0.0.1:4321")]
        addr: SocketAddr,
        #[arg(long, default_value = "semantic-local-token")]
        token: String,
    },
}

#[derive(Args)]
struct ConfigArgs {
    #[command(subcommand)]
    command: ConfigCommand,
}

#[derive(Subcommand)]
enum ConfigCommand {
    Init,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct QualityTrendSnapshot {
    timestamp_unix_ms: u64,
    status: String,
    health: String,
    #[serde(default)]
    latency_health: String,
    #[serde(default)]
    graph_drift_health: String,
    #[serde(default)]
    diagnosis: String,
    #[serde(default)]
    action_recommendation: String,
    #[serde(default)]
    action_priority: String,
    #[serde(default)]
    triage_path: String,
    #[serde(default)]
    action_target: Option<String>,
    #[serde(default)]
    action_checklist: Vec<String>,
    #[serde(default)]
    action_commands: Vec<String>,
    #[serde(default)]
    action_primary_command: Option<String>,
    #[serde(default)]
    action_command_categories: Vec<String>,
    #[serde(default)]
    action_primary_command_category: Option<String>,
    #[serde(default)]
    action_source_artifacts: Vec<String>,
    #[serde(default)]
    latency_hotspot: Option<String>,
    #[serde(default)]
    graph_drift_hotspot: Option<String>,
    #[serde(default)]
    latency_hotspot_bucket_id: Option<String>,
    #[serde(default)]
    graph_drift_hotspot_bucket_id: Option<String>,
    #[serde(default)]
    summary_lookup_hint: Option<String>,
    #[serde(default)]
    summary_lookup_scope: Option<String>,
    #[serde(default)]
    latency_severity: Option<String>,
    #[serde(default)]
    latency_severity_reason: Option<String>,
    #[serde(default)]
    latency_score: f64,
    #[serde(default)]
    latency_score_delta_vs_trailing: f64,
    #[serde(default)]
    latency_score_direction: Option<String>,
    regression_count: usize,
    threshold_failure_count: usize,
    fixture_count: usize,
    #[serde(default)]
    leading_graph_drift: Option<String>,
    #[serde(default)]
    leading_graph_drift_fixture: Option<String>,
    #[serde(default)]
    graph_drift_severity: Option<String>,
    #[serde(default)]
    graph_drift_severity_reason: Option<String>,
    #[serde(default)]
    graph_drift_score: f64,
    #[serde(default)]
    graph_drift_score_delta_vs_trailing: f64,
    #[serde(default)]
    graph_drift_score_direction: Option<String>,
    #[serde(default)]
    graph_drift_trend: Option<String>,
    #[serde(default)]
    graph_drift_fixture_trend: Option<String>,
    #[serde(default)]
    top_worsening_graph_drift_fixture: Option<String>,
    #[serde(default)]
    leading_graph_drift_delta_vs_trailing_pp: f64,
    #[serde(default)]
    mutation_scope_incomplete_rate: f64,
    retrieval_avg_latency_ms: f64,
    retrieval_p95_latency_ms: f64,
    route_avg_latency_ms: f64,
    route_p95_latency_ms: f64,
    retrieval_avg_latency_delta_vs_trailing: f64,
    retrieval_p95_latency_delta_vs_trailing: f64,
    route_avg_latency_delta_vs_trailing: f64,
    route_p95_latency_delta_vs_trailing: f64,
}

pub async fn run_cli() -> Result<()> {
    let cli = Cli::parse();
    let repo_root = cli.repo.unwrap_or(std::env::current_dir()?);
    if let Command::Config(args) = cli.command {
        match args.command {
            ConfigCommand::Init => {
                ensure_semantic_config(&repo_root)?;
                println!("initialized {}", repo_root.join(".semantic").display());
            }
        }
        return Ok(());
    }
    if let Command::Status(output) = &cli.command {
        if output.quality {
            print_output(&load_quality_status(&repo_root)?, output);
            return Ok(());
        }
    }

    let runtime_options = match &cli.command {
        Command::Index(_) | Command::Status(_) => RuntimeOptions {
            start_watcher: false,
            ensure_config: true,
            bootstrap_index_policy: BootstrapIndexPolicy::Skip,
        },
        _ => RuntimeOptions {
            start_watcher: false,
            ensure_config: true,
            bootstrap_index_policy: BootstrapIndexPolicy::ReuseExistingOrCreate,
        },
    };
    let runtime = AppRuntime::bootstrap(
        repo_root.clone(),
        runtime_options,
    )?;

    match cli.command {
        Command::Retrieve(args) => {
            let operation = parse_operation(&args.op)?;
            let value = runtime.handle_retrieve(RetrieveRequestBody {
                request: RetrievalRequest {
                    operation,
                    name: args.name,
                    query: args.query,
                    file: args.file,
                    path: args.path,
                    start_line: args.start_line,
                    end_line: args.end_line,
                    max_tokens: args.max_tokens,
                    limit: args.limit,
                    heading: args.heading,
                    ..Default::default()
                },
                semantic_enabled: Some(true),
                input_compressed: None,
                original_query: None,
                single_file_fast_path: Some(args.single_file_fast_path),
                reference_only: Some(args.reference_only),
                mapping_mode: None,
                max_footprint_items: None,
                reuse_session_context: Some(true),
                session_id: args.session_id,
                raw_expansion_mode: args.raw_expansion_mode,
                auto_index_target: Some(args.auto_index_target),
            });
            print_output(&value, &args.output);
        }
        Command::Route(args) => {
            let mut value = runtime.handle_autoroute(IdeAutoRouteRequest {
                task: Some(args.task),
                action: None,
                action_input: None,
                session_id: args.session_id,
                max_tokens: args.max_tokens,
                single_file_fast_path: Some(true),
                reference_only: None,
                mapping_mode: None,
                max_footprint_items: None,
                reuse_session_context: Some(true),
                auto_minimal_raw: Some(true),
                include_summary: Some(args.include_summary),
                raw_expansion_mode: args.raw_expansion_mode,
                auto_index_target: Some(args.auto_index_target),
            });
            let verification_threshold = if args.require_high_confidence {
                Some(VerificationThreshold::HighConfidence)
            } else {
                args.min_verification
            };
            if let Some(threshold) = verification_threshold {
                if let Some(obj) = value.as_object_mut() {
                    obj.insert(
                        "verification_threshold".to_string(),
                        serde_json::json!(verification_threshold_label(threshold)),
                    );
                }
            }
            if args.require_mutation_ready {
                if let Some(obj) = value.as_object_mut() {
                    obj.insert("mutation_gate".to_string(), serde_json::json!("ready"));
                }
            }
            print_output(&value, &args.output);
            if let Some(threshold) = verification_threshold {
                ensure_route_verification_at_least(&value, threshold)?;
            }
            if args.require_mutation_ready {
                ensure_route_mutation_ready(&value)?;
            }
        }
        Command::Index(args) => {
            if args.workspace && !args.path.is_empty() {
                return Err(anyhow!(
                    "--workspace and --path cannot be combined; targeted indexing is repo-local"
                ));
            }
            if args.workspace {
                let workspace = runtime.workspace_state();
                let ws = workspace.lock().clone();
                runtime
                    .indexer()
                    .lock()
                    .index_workspace(&ws.primary_root, &ws.workspace_roots)?;
            } else if !args.path.is_empty() {
                runtime
                    .indexer()
                    .lock()
                    .index_paths(runtime.repo_root(), &args.path)?;
            } else {
                runtime.indexer().lock().index_repo(runtime.repo_root())?;
            }
            if args.watch {
                runtime.ensure_watcher_started()?;
                println!("watcher running for {}", runtime.repo_root().display());
                tokio::signal::ctrl_c().await?;
            } else {
                if !args.path.is_empty() {
                    println!(
                        "targeted index refreshed for {} ({})",
                        runtime.repo_root().display(),
                        args.path.join(", ")
                    );
                } else {
                    println!("index refreshed for {}", runtime.repo_root().display());
                }
            }
        }
        Command::Status(output) => {
            let value = runtime.status_json();
            print_output(&value, &output);
        }
        Command::Edit(args) => {
            let value = runtime.handle_edit(crate::models::EditRequestBody {
                symbol: args.symbol,
                edit: args.edit,
                patch_mode: None,
                run_tests: Some(args.run_tests),
                max_tokens: args.max_tokens,
                session_id: args.session_id,
            });
            print_output(&value, &args.output);
        }
        Command::Serve(args) => match args.command {
            ServeCommand::Api { addr } => {
                let runtime = AppRuntime::bootstrap(
                    repo_root,
                    RuntimeOptions {
                        start_watcher: true,
                        ensure_config: true,
                        bootstrap_index_policy: BootstrapIndexPolicy::ReuseExistingOrCreate,
                    },
                )?;
                api_server::serve(runtime, addr).await?;
            }
            ServeCommand::Mcp { addr, token } => {
                let runtime = AppRuntime::bootstrap(
                    repo_root,
                    RuntimeOptions {
                        start_watcher: true,
                        ensure_config: true,
                        bootstrap_index_policy: BootstrapIndexPolicy::ReuseExistingOrCreate,
                    },
                )?;
                mcp_server::serve(runtime, token, addr).await?;
            }
        },
        Command::Config(_) => unreachable!("config command handled before runtime bootstrap"),
    }

    Ok(())
}

fn load_quality_status(repo_root: &std::path::Path) -> Result<serde_json::Value> {
    let snapshot_path = repo_root
        .join("docs")
        .join("doc_ignore")
        .join("quality_report_trend_snapshot.json");
    let raw = std::fs::read_to_string(&snapshot_path).map_err(|error| {
        anyhow!(
            "quality snapshot not found at {}: {error}",
            snapshot_path.display()
        )
    })?;
    let snapshot: QualityTrendSnapshot = serde_json::from_str(&raw).map_err(|error| {
        anyhow!(
            "quality snapshot at {} is invalid: {error}",
            snapshot_path.display()
        )
    })?;
    Ok(serde_json::json!({
        "kind": "quality_status",
        "snapshot_path": snapshot_path.display().to_string(),
        "status": snapshot.status,
        "health": snapshot.health,
        "latency_health": snapshot.latency_health,
        "graph_drift_health": snapshot.graph_drift_health,
        "diagnosis": snapshot.diagnosis,
        "action_recommendation": snapshot.action_recommendation,
        "action_priority": snapshot.action_priority,
        "triage_path": snapshot.triage_path,
        "action_target": snapshot.action_target,
        "action_checklist": snapshot.action_checklist,
        "action_commands": snapshot.action_commands,
        "action_primary_command": snapshot.action_primary_command,
        "action_command_categories": snapshot.action_command_categories,
        "action_primary_command_category": snapshot.action_primary_command_category,
        "action_source_artifacts": snapshot.action_source_artifacts,
        "latency_hotspot": snapshot.latency_hotspot,
        "graph_drift_hotspot": snapshot.graph_drift_hotspot,
        "latency_hotspot_bucket_id": snapshot.latency_hotspot_bucket_id,
        "graph_drift_hotspot_bucket_id": snapshot.graph_drift_hotspot_bucket_id,
        "summary_lookup_hint": snapshot.summary_lookup_hint,
        "summary_lookup_scope": snapshot.summary_lookup_scope,
        "latency_severity": snapshot.latency_severity,
        "latency_severity_reason": snapshot.latency_severity_reason,
        "latency_score": snapshot.latency_score,
        "latency_score_delta_vs_trailing": snapshot.latency_score_delta_vs_trailing,
        "latency_score_direction": snapshot.latency_score_direction,
        "regression_count": snapshot.regression_count,
        "threshold_failure_count": snapshot.threshold_failure_count,
        "fixture_count": snapshot.fixture_count,
        "leading_graph_drift": snapshot.leading_graph_drift,
        "leading_graph_drift_fixture": snapshot.leading_graph_drift_fixture,
        "graph_drift_severity": snapshot.graph_drift_severity,
        "graph_drift_severity_reason": snapshot.graph_drift_severity_reason,
        "graph_drift_score": snapshot.graph_drift_score,
        "graph_drift_score_delta_vs_trailing": snapshot.graph_drift_score_delta_vs_trailing,
        "graph_drift_score_direction": snapshot.graph_drift_score_direction,
        "graph_drift_trend": snapshot.graph_drift_trend,
        "graph_drift_fixture_trend": snapshot.graph_drift_fixture_trend,
        "top_worsening_graph_drift_fixture": snapshot.top_worsening_graph_drift_fixture,
        "leading_graph_drift_delta_vs_trailing_pp": snapshot.leading_graph_drift_delta_vs_trailing_pp,
        "mutation_scope_incomplete_rate": snapshot.mutation_scope_incomplete_rate,
        "retrieval": {
            "avg_latency_ms": snapshot.retrieval_avg_latency_ms,
            "p95_latency_ms": snapshot.retrieval_p95_latency_ms,
            "avg_latency_delta_vs_trailing": snapshot.retrieval_avg_latency_delta_vs_trailing,
            "p95_latency_delta_vs_trailing": snapshot.retrieval_p95_latency_delta_vs_trailing,
        },
        "route": {
            "avg_latency_ms": snapshot.route_avg_latency_ms,
            "p95_latency_ms": snapshot.route_p95_latency_ms,
            "avg_latency_delta_vs_trailing": snapshot.route_avg_latency_delta_vs_trailing,
            "p95_latency_delta_vs_trailing": snapshot.route_p95_latency_delta_vs_trailing,
        }
    }))
}

fn parse_operation(raw: &str) -> Result<Operation> {
    let normalized = raw.trim().to_ascii_lowercase();
    let op = match normalized.as_str() {
        "setworkspacemode" | "set_workspace_mode" => Operation::SetWorkspaceMode,
        "getworkspacemode" | "get_workspace_mode" => Operation::GetWorkspaceMode,
        "getrepomap" | "get_repo_map" => Operation::GetRepoMap,
        "getdirectorybrief" | "get_directory_brief" => Operation::GetDirectoryBrief,
        "getfileoutline" | "get_file_outline" => Operation::GetFileOutline,
        "getfilebrief" | "get_file_brief" => Operation::GetFileBrief,
        "searchsymbol" | "search_symbol" => Operation::SearchSymbol,
        "getsymbolbrief" | "get_symbol_brief" => Operation::GetSymbolBrief,
        "getfunction" | "get_function" => Operation::GetFunction,
        "getclass" | "get_class" => Operation::GetClass,
        "getdependencies" | "get_dependencies" => Operation::GetDependencies,
        "getcodespan" | "get_code_span" => Operation::GetCodeSpan,
        "getlogicnodes" | "get_logic_nodes" => Operation::GetLogicNodes,
        "getlogicneighborhood" | "get_logic_neighborhood" => Operation::GetLogicNeighborhood,
        "getlogicspan" | "get_logic_span" => Operation::GetLogicSpan,
        "getdependencyneighborhood" | "get_dependency_neighborhood" => {
            Operation::GetDependencyNeighborhood
        }
        "getsymbolneighborhood" | "get_symbol_neighborhood" => Operation::GetSymbolNeighborhood,
        "getreasoningcontext" | "get_reasoning_context" => Operation::GetReasoningContext,
        "getplannedcontext" | "get_planned_context" => Operation::GetPlannedContext,
        "getrepomaphierarchy" | "get_repo_map_hierarchy" => Operation::GetRepoMapHierarchy,
        "getmoduledependencies" | "get_module_dependencies" => Operation::GetModuleDependencies,
        "searchsemanticsymbol" | "search_semantic_symbol" => Operation::SearchSemanticSymbol,
        "getworkspacereasoningcontext" | "get_workspace_reasoning_context" => {
            Operation::GetWorkspaceReasoningContext
        }
        "plansafeedit" | "plan_safe_edit" => Operation::PlanSafeEdit,
        "getcontrolflowhints" | "get_control_flow_hints" => Operation::GetControlFlowHints,
        "getdataflowhints" | "get_data_flow_hints" => Operation::GetDataFlowHints,
        "getcontrolflowslice" | "get_control_flow_slice" => Operation::GetControlFlowSlice,
        "getdataflowslice" | "get_data_flow_slice" => Operation::GetDataFlowSlice,
        "getlogicclusters" | "get_logic_clusters" => Operation::GetLogicClusters,
        "gethybridrankedcontext" | "get_hybrid_ranked_context" => Operation::GetHybridRankedContext,
        "getdebuggraph" | "get_debug_graph" => Operation::GetDebugGraph,
        "getpipelinegraph" | "get_pipeline_graph" => Operation::GetPipelineGraph,
        "getrootcausecandidates" | "get_root_cause_candidates" => Operation::GetRootCauseCandidates,
        "gettestgaps" | "get_test_gaps" => Operation::GetTestGaps,
        "getdeploymenthistory" | "get_deployment_history" => Operation::GetDeploymentHistory,
        "getperformancestats" | "get_performance_stats" => Operation::GetPerformanceStats,
        "getprojectsummary" | "get_project_summary" => Operation::GetProjectSummary,
        "getsectionbrief" | "get_section_brief" => Operation::GetSectionBrief,
        "geterrorcontext" | "get_error_context" => Operation::GetErrorContext,
        "recorderror" | "record_error" => Operation::RecordError,
        "recordsolution" | "record_solution" => Operation::RecordSolution,
        "getknowledgegraph" | "get_knowledge_graph" => Operation::GetKnowledgeGraph,
        "appendknowledge" | "append_knowledge" => Operation::AppendKnowledge,
        "getchangepropagation" | "get_change_propagation" => Operation::GetChangePropagation,
        _ => return Err(anyhow!("unknown operation '{raw}'")),
    };
    Ok(op)
}

fn print_output(value: &serde_json::Value, output: &OutputArgs) {
    match output.output {
        OutputMode::Json => {
            if output.compact {
                println!(
                    "{}",
                    serde_json::to_string(&value).unwrap_or_else(|_| value.to_string())
                );
            } else {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string())
                );
            }
        }
        OutputMode::Text => {
            println!("{}", text_output(&value, output.verbose));
        }
    }
}

fn ensure_route_verification_at_least(
    value: &serde_json::Value,
    threshold: VerificationThreshold,
) -> Result<()> {
    let Some(intent) = value.get("intent").and_then(|v| v.as_str()) else {
        return Ok(());
    };
    let verification = value
        .get("verification")
        .ok_or_else(|| anyhow!("route for intent '{intent}' did not include verification metadata"))?;
    let status = verification
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    if verification_status_satisfies(status, threshold) {
        return Ok(());
    }
    let action = verification
        .get("recommended_action")
        .and_then(|v| v.as_str())
        .unwrap_or("review returned context before continuing");
    let issues = verification
        .get("issues")
        .and_then(|v| v.as_array())
        .map(|items| {
            items.iter()
                .filter_map(|item| item.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "none".to_string());
    Err(anyhow!(
        "route verification is '{status}' for intent '{intent}' but requires at least '{}'; action: {action}; issues: {issues}",
        verification_threshold_label(threshold)
    ))
}

fn ensure_route_mutation_ready(value: &serde_json::Value) -> Result<()> {
    let Some(intent) = value.get("intent").and_then(|v| v.as_str()) else {
        return Ok(());
    };
    let verification = value
        .get("verification")
        .ok_or_else(|| anyhow!("route for intent '{intent}' did not include verification metadata"))?;
    let mutation_state = verification
        .get("mutation_state")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    if matches!(mutation_state, "ready" | "not_applicable") {
        return Ok(());
    }
    let reason = verification
        .get("mutation_block_reason")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let follow_up = verification
        .get("recommended_cli_follow_up")
        .and_then(|v| v.as_str())
        .unwrap_or("none");
    Err(anyhow!(
        "route mutation safety is '{mutation_state}' for intent '{intent}' and requires 'ready'; block_reason: {reason}; follow_up: {follow_up}"
    ))
}

fn verification_status_satisfies(status: &str, threshold: VerificationThreshold) -> bool {
    match threshold {
        VerificationThreshold::HighConfidence => status == "high_confidence",
        VerificationThreshold::NeedsReview => {
            matches!(status, "high_confidence" | "needs_review")
        }
    }
}

fn verification_threshold_label(threshold: VerificationThreshold) -> &'static str {
    match threshold {
        VerificationThreshold::NeedsReview => "needs_review",
        VerificationThreshold::HighConfidence => "high_confidence",
    }
}

fn text_output(value: &serde_json::Value, verbose: bool) -> String {
    if value.get("kind").and_then(|v| v.as_str()) == Some("quality_status") {
        let status = value.get("status").and_then(|v| v.as_str()).unwrap_or("unknown");
        let health = value.get("health").and_then(|v| v.as_str()).unwrap_or("unknown");
        let latency_health = value
            .get("latency_health")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let graph_drift_health = value
            .get("graph_drift_health")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let diagnosis = value
            .get("diagnosis")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let action_recommendation = value
            .get("action_recommendation")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let action_priority = value
            .get("action_priority")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let triage_path = value
            .get("triage_path")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let action_target = value
            .get("action_target")
            .and_then(|v| v.as_str());
        let action_checklist: Vec<String> = value
            .get("action_checklist")
            .and_then(|v| v.as_array())
            .map(|items| {
                items.iter()
                    .filter_map(|item| item.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        let action_commands: Vec<String> = value
            .get("action_commands")
            .and_then(|v| v.as_array())
            .map(|items| {
                items.iter()
                    .filter_map(|item| item.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        let action_primary_command = value
            .get("action_primary_command")
            .and_then(|v| v.as_str());
        let action_command_categories: Vec<String> = value
            .get("action_command_categories")
            .and_then(|v| v.as_array())
            .map(|items| {
                items.iter()
                    .filter_map(|item| item.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        let action_primary_command_category = value
            .get("action_primary_command_category")
            .and_then(|v| v.as_str());
        let action_source_artifacts: Vec<String> = value
            .get("action_source_artifacts")
            .and_then(|v| v.as_array())
            .map(|items| {
                items.iter()
                    .filter_map(|item| item.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        let latency_hotspot = value
            .get("latency_hotspot")
            .and_then(|v| v.as_str());
        let graph_drift_hotspot = value
            .get("graph_drift_hotspot")
            .and_then(|v| v.as_str());
        let latency_hotspot_bucket_id = value
            .get("latency_hotspot_bucket_id")
            .and_then(|v| v.as_str());
        let graph_drift_hotspot_bucket_id = value
            .get("graph_drift_hotspot_bucket_id")
            .and_then(|v| v.as_str());
        let summary_lookup_hint = value
            .get("summary_lookup_hint")
            .and_then(|v| v.as_str());
        let summary_lookup_scope = value
            .get("summary_lookup_scope")
            .and_then(|v| v.as_str());
        let latency_severity = value
            .get("latency_severity")
            .and_then(|v| v.as_str());
        let latency_severity_reason = value
            .get("latency_severity_reason")
            .and_then(|v| v.as_str());
        let latency_score = value
            .get("latency_score")
            .and_then(|v| v.as_f64())
            .unwrap_or_default();
        let latency_score_delta_vs_trailing = value
            .get("latency_score_delta_vs_trailing")
            .and_then(|v| v.as_f64())
            .unwrap_or_default();
        let latency_score_direction = value
            .get("latency_score_direction")
            .and_then(|v| v.as_str());
        let regressions = value
            .get("regression_count")
            .and_then(|v| v.as_u64())
            .unwrap_or_default();
        let thresholds = value
            .get("threshold_failure_count")
            .and_then(|v| v.as_u64())
            .unwrap_or_default();
        let retrieval_avg = value
            .get("retrieval")
            .and_then(|v| v.get("avg_latency_ms"))
            .and_then(|v| v.as_f64())
            .unwrap_or_default();
        let retrieval_p95 = value
            .get("retrieval")
            .and_then(|v| v.get("p95_latency_ms"))
            .and_then(|v| v.as_f64())
            .unwrap_or_default();
        let route_avg = value
            .get("route")
            .and_then(|v| v.get("avg_latency_ms"))
            .and_then(|v| v.as_f64())
            .unwrap_or_default();
        let route_p95 = value
            .get("route")
            .and_then(|v| v.get("p95_latency_ms"))
            .and_then(|v| v.as_f64())
            .unwrap_or_default();
        let retrieval_delta = value
            .get("retrieval")
            .and_then(|v| v.get("p95_latency_delta_vs_trailing"))
            .and_then(|v| v.as_f64())
            .unwrap_or_default();
        let route_delta = value
            .get("route")
            .and_then(|v| v.get("p95_latency_delta_vs_trailing"))
            .and_then(|v| v.as_f64())
            .unwrap_or_default();
        let leading_graph_drift = value
            .get("leading_graph_drift")
            .and_then(|v| v.as_str());
        let graph_drift_trend = value
            .get("graph_drift_trend")
            .and_then(|v| v.as_str());
        let leading_graph_drift_fixture = value
            .get("leading_graph_drift_fixture")
            .and_then(|v| v.as_str());
        let graph_drift_severity = value
            .get("graph_drift_severity")
            .and_then(|v| v.as_str());
        let graph_drift_severity_reason = value
            .get("graph_drift_severity_reason")
            .and_then(|v| v.as_str());
        let graph_drift_score = value
            .get("graph_drift_score")
            .and_then(|v| v.as_f64())
            .unwrap_or_default();
        let graph_drift_score_delta_vs_trailing = value
            .get("graph_drift_score_delta_vs_trailing")
            .and_then(|v| v.as_f64())
            .unwrap_or_default();
        let graph_drift_score_direction = value
            .get("graph_drift_score_direction")
            .and_then(|v| v.as_str());
        let graph_drift_fixture_trend = value
            .get("graph_drift_fixture_trend")
            .and_then(|v| v.as_str());
        let top_worsening_graph_drift_fixture = value
            .get("top_worsening_graph_drift_fixture")
            .and_then(|v| v.as_str());
        let mutation_scope_incomplete_rate = value
            .get("mutation_scope_incomplete_rate")
            .and_then(|v| v.as_f64())
            .unwrap_or_default();

        let mut out = format!(
            "quality_status: {health}\ndiagnosis: {diagnosis}\naction_recommendation: {action_recommendation}\naction_priority: {action_priority}\ntriage_path: {triage_path}\nlatency_health: {latency_health}\ngraph_drift_health: {graph_drift_health}\nstatus: {status}\nregressions: {regressions}\nthreshold_failures: {thresholds}\nretrieval: avg={retrieval_avg:.1}ms p95={retrieval_p95:.1}ms trailing_delta={retrieval_delta:+.1}ms\nroute: avg={route_avg:.1}ms p95={route_p95:.1}ms trailing_delta={route_delta:+.1}ms"
        );
        if let Some(action_target) = action_target {
            out.push_str(&format!("\naction_target: {action_target}"));
        }
        if !action_checklist.is_empty() {
            out.push_str(&format!("\naction_checklist: {}", action_checklist.join(" | ")));
        }
        if !action_commands.is_empty() {
            out.push_str(&format!("\naction_commands: {}", action_commands.join(" | ")));
        }
        if let Some(action_primary_command) = action_primary_command {
            out.push_str(&format!(
                "\naction_primary_command: {action_primary_command}"
            ));
        }
        if let Some(action_primary_command_category) = action_primary_command_category {
            out.push_str(&format!(
                "\naction_primary_command_category: {action_primary_command_category}"
            ));
        }
        if !action_command_categories.is_empty() {
            out.push_str(&format!(
                "\naction_command_categories: {}",
                action_command_categories.join(" | ")
            ));
        }
        if !action_source_artifacts.is_empty() {
            out.push_str(&format!(
                "\naction_source_artifacts: {}",
                action_source_artifacts.join(" | ")
            ));
        }
        if let Some(latency_hotspot) = latency_hotspot {
            out.push_str(&format!("\nlatency_hotspot: {latency_hotspot}"));
        }
        if let Some(graph_drift_hotspot) = graph_drift_hotspot {
            out.push_str(&format!("\ngraph_drift_hotspot: {graph_drift_hotspot}"));
        }
        if let Some(latency_hotspot_bucket_id) = latency_hotspot_bucket_id {
            out.push_str(&format!(
                "\nlatency_hotspot_bucket_id: {latency_hotspot_bucket_id}"
            ));
        }
        if let Some(graph_drift_hotspot_bucket_id) = graph_drift_hotspot_bucket_id {
            out.push_str(&format!(
                "\ngraph_drift_hotspot_bucket_id: {graph_drift_hotspot_bucket_id}"
            ));
        }
        if let Some(summary_lookup_hint) = summary_lookup_hint {
            out.push_str(&format!("\nsummary_lookup_hint: {summary_lookup_hint}"));
        }
        if let Some(summary_lookup_scope) = summary_lookup_scope {
            out.push_str(&format!("\nsummary_lookup_scope: {summary_lookup_scope}"));
        }
        if let Some(latency_severity) = latency_severity {
            out.push_str(&format!("\nlatency_severity: {latency_severity}"));
        }
        if let Some(latency_severity_reason) = latency_severity_reason {
            out.push_str(&format!(
                "\nlatency_severity_reason: {latency_severity_reason}"
            ));
        }
        if latency_score > 0.0 || latency_severity.is_some() {
            out.push_str(&format!(
                "\nlatency_score: {:.1} ({:+.1} vs trailing)",
                latency_score, latency_score_delta_vs_trailing
            ));
        }
        if let Some(latency_score_direction) = latency_score_direction {
            out.push_str(&format!(
                "\nlatency_score_direction: {latency_score_direction}"
            ));
        }
        if let Some(leading_graph_drift) = leading_graph_drift {
            out.push_str(&format!("\nleading_graph_drift: {leading_graph_drift}"));
        }
        if let Some(graph_drift_trend) = graph_drift_trend {
            out.push_str(&format!("\ngraph_drift_trend: {graph_drift_trend}"));
        }
        if let Some(leading_graph_drift_fixture) = leading_graph_drift_fixture {
            out.push_str(&format!(
                "\nleading_graph_drift_fixture: {leading_graph_drift_fixture}"
            ));
        }
        if let Some(graph_drift_severity) = graph_drift_severity {
            out.push_str(&format!("\ngraph_drift_severity: {graph_drift_severity}"));
        }
        if let Some(graph_drift_severity_reason) = graph_drift_severity_reason {
            out.push_str(&format!(
                "\ngraph_drift_severity_reason: {graph_drift_severity_reason}"
            ));
        }
        if graph_drift_score > 0.0 || graph_drift_severity.is_some() {
            out.push_str(&format!(
                "\ngraph_drift_score: {:.1} ({:+.1} vs trailing)",
                graph_drift_score, graph_drift_score_delta_vs_trailing
            ));
        }
        if let Some(graph_drift_score_direction) = graph_drift_score_direction {
            out.push_str(&format!(
                "\ngraph_drift_score_direction: {graph_drift_score_direction}"
            ));
        }
        if let Some(graph_drift_fixture_trend) = graph_drift_fixture_trend {
            out.push_str(&format!(
                "\ngraph_drift_fixture_trend: {graph_drift_fixture_trend}"
            ));
        }
        if let Some(top_worsening_graph_drift_fixture) = top_worsening_graph_drift_fixture {
            out.push_str(&format!(
                "\ntop_worsening_graph_drift_fixture: {top_worsening_graph_drift_fixture}"
            ));
        }
        if mutation_scope_incomplete_rate > 0.0 {
            out.push_str(&format!(
                "\nmutation_scope_incomplete_rate: {:.0}%",
                mutation_scope_incomplete_rate * 100.0
            ));
        }
        if verbose {
            if let Some(path) = value.get("snapshot_path").and_then(|v| v.as_str()) {
                out.push_str(&format!("\nsnapshot_path: {path}"));
            }
        }
        return out;
    }
    if value.get("ok").and_then(|v| v.as_bool()) == Some(true)
        && value.get("repo_root").is_some()
        && value.get("index_revision").is_some()
    {
        let repo_root = value
            .get("repo_root")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let index_revision = value
            .get("index_revision")
            .and_then(|v| v.as_u64())
            .unwrap_or_default();
        let index_available = value
            .get("index_available")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let indexed_file_count = value
            .get("indexed_file_count")
            .and_then(|v| v.as_u64())
            .unwrap_or_default();
        let indexed_path_hints: Vec<String> = value
            .get("indexed_path_hints")
            .and_then(|v| v.as_array())
            .map(|items| {
                items.iter()
                    .filter_map(|item| item.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        let watcher_running = value
            .get("watcher_running")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let bootstrap_index_action = value
            .get("bootstrap_index_action")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let indexing_mode = value
            .get("indexing_mode")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let indexing_completeness = value
            .get("indexing_completeness")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let workspace_mode_enabled = value
            .get("workspace_mode_enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let llm_router_configured = value
            .get("llm_router_configured")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let symbol_avg_ms = value
            .get("performance")
            .and_then(|v| v.get("symbol_lookup"))
            .and_then(|v| v.get("avg_ms"))
            .and_then(|v| v.as_f64())
            .unwrap_or_default();
        let planned_p95_ms = value
            .get("performance")
            .and_then(|v| v.get("planned_context"))
            .and_then(|v| v.get("p95_ms"))
            .and_then(|v| v.as_f64())
            .unwrap_or_default();

        let mut out = format!(
            "status: ok\nrepo_root: {repo_root}\nindex_revision: {index_revision}\nindex_available: {index_available}\nindexed_file_count: {indexed_file_count}\nwatcher_running: {watcher_running}\nbootstrap_index_action: {bootstrap_index_action}\nindexing_mode: {indexing_mode}\nindexing_completeness: {indexing_completeness}\nworkspace_mode_enabled: {workspace_mode_enabled}\nllm_router_configured: {llm_router_configured}\nperformance: symbol_lookup_avg={symbol_avg_ms:.1}ms planned_context_p95={planned_p95_ms:.1}ms"
        );
        if !indexed_path_hints.is_empty() {
            out.push_str(&format!(
                "\nindexed_path_hints: {}",
                indexed_path_hints.join(" | ")
            ));
        }
        if verbose {
            if let Some(workspace_roots) = value.get("workspace_roots").and_then(|v| v.as_array()) {
                let roots: Vec<&str> = workspace_roots
                    .iter()
                    .filter_map(|item| item.as_str())
                    .collect();
                if !roots.is_empty() {
                    out.push_str(&format!("\nworkspace_roots: {}", roots.join(" | ")));
                }
            }
        }
        return out;
    }
    if let Some(intent) = value.get("intent").and_then(|v| v.as_str()) {
        let mut out = format!("intent: {intent}");
        if value
            .get("auto_index_applied")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            let target = value
                .get("auto_index_target")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            out.push_str(&format!("\nauto_index: applied @ {target}"));
            if let Some(count) = value.get("indexed_file_count").and_then(|v| v.as_u64()) {
                out.push_str(&format!("\nindexed_file_count: {count}"));
            }
            let indexed_path_hints: Vec<String> = value
                .get("indexed_path_hints")
                .and_then(|v| v.as_array())
                .map(|items| {
                    items.iter()
                        .filter_map(|item| item.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            if !indexed_path_hints.is_empty() {
                out.push_str(&format!(
                    "\nindexed_path_hints: {}",
                    indexed_path_hints.join(" | ")
                ));
            }
        }
        if let Some(tool) = value.get("selected_tool").and_then(|v| v.as_str()) {
            out.push_str(&format!("\nselected_tool: {tool}"));
        }
        if let Some(required) = value
            .get("verification_threshold")
            .and_then(|v| v.as_str())
        {
            let actual = value
                .get("verification")
                .and_then(|v| v.get("status"))
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            out.push_str(&format!(
                "\nverification_gate: min={required} actual={actual}"
            ));
        }
        if let Some(required) = value.get("mutation_gate").and_then(|v| v.as_str()) {
            let actual = value
                .get("verification")
                .and_then(|v| v.get("mutation_state"))
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            out.push_str(&format!("\nmutation_gate: min={required} actual={actual}"));
        }
        if let Some(verification) = value.get("verification") {
            if let Some(status) = verification.get("status").and_then(|v| v.as_str()) {
                out.push_str(&format!("\nverification: {status}"));
                if let Some(action) = verification
                    .get("recommended_action")
                    .and_then(|v| v.as_str())
                {
                    out.push_str(&format!("\nverification_action: {action}"));
                }
                let target_symbol = verification
                    .get("target_symbol")
                    .and_then(|v| v.as_str())
                    .filter(|value| !value.is_empty());
                let top_context_file = verification
                    .get("top_context_file")
                    .and_then(|v| v.as_str())
                    .filter(|value| !value.is_empty());
                match (target_symbol, top_context_file) {
                    (Some(symbol), Some(file)) => {
                        out.push_str(&format!("\nverification_scope: {symbol} @ {file}"));
                    }
                    (Some(symbol), None) => {
                        out.push_str(&format!("\nverification_scope: {symbol}"));
                    }
                    (None, Some(file)) => {
                        out.push_str(&format!("\nverification_scope: {file}"));
                    }
                    (None, None) => {}
                }
                if let Some(index_coverage) = verification
                    .get("index_coverage")
                    .and_then(|v| v.as_str())
                {
                    if let Some(target) = verification
                        .get("index_coverage_target")
                        .and_then(|v| v.as_str())
                    {
                        out.push_str(&format!(
                            "\nindex_coverage: {index_coverage} @ {target}"
                        ));
                    } else {
                        out.push_str(&format!("\nindex_coverage: {index_coverage}"));
                    }
                }
                if let Some(command) = verification
                    .get("suggested_index_command")
                    .and_then(|v| v.as_str())
                {
                    out.push_str(&format!("\nindex_follow_up: {command}"));
                }
                let issues = verification
                    .get("issues")
                    .and_then(|v| v.as_array())
                    .map(|items| {
                        items.iter()
                            .filter_map(|item| item.as_str())
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                if let Some(mutation_state) = verification
                    .get("mutation_state")
                    .and_then(|v| v.as_str())
                    .filter(|value| *value != "not_applicable")
                {
                    out.push_str(&format!("\nmutation_safety: {mutation_state}"));
                    if let Some(bundle_status) = verification
                        .get("mutation_bundle")
                        .and_then(|v| v.get("status"))
                        .and_then(|v| v.as_str())
                    {
                        out.push_str(&format!("\nmutation_bundle: {bundle_status}"));
                    }
                    if let Some(reason) = verification
                        .get("mutation_block_reason")
                        .and_then(|v| v.as_str())
                    {
                        out.push_str(&format!("\nmutation_block_reason: {reason}"));
                    }
                    if !verbose && status != "high_confidence" {
                        let mutation_scope_issue = if issues
                            .iter()
                            .any(|issue| *issue == "impact_scope_graph_misaligned")
                        {
                            Some("graph_misaligned")
                        } else if issues
                            .iter()
                            .any(|issue| *issue == "impact_scope_not_anchored_to_target")
                        {
                            Some("unanchored")
                        } else if issues
                            .iter()
                            .any(|issue| *issue == "impact_scope_graph_incomplete")
                        {
                            Some("incomplete")
                        } else {
                            None
                        };
                        if let Some(issue) = mutation_scope_issue {
                            out.push_str(&format!("\nmutation_scope_issue: {issue}"));
                        }
                    }
                }
                if !verbose && status != "high_confidence" && !issues.is_empty() {
                    let issue_summary = if issues.len() == 1 {
                        issues[0].to_string()
                    } else {
                        format!("{} (+{} more)", issues[0], issues.len() - 1)
                    };
                    out.push_str(&format!("\nverification_issue: {issue_summary}"));
                }
                if let Some(graph_details) = verification.get("impact_scope_graph_details") {
                    let missing_files = graph_details
                        .get("missing_files")
                        .and_then(|v| v.as_array())
                        .map(|items| {
                            items.iter()
                                .filter_map(|item| item.as_str())
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    let extra_files = graph_details
                        .get("extra_files")
                        .and_then(|v| v.as_array())
                        .map(|items| {
                            items.iter()
                                .filter_map(|item| item.as_str())
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    let missing_symbols = graph_details
                        .get("missing_symbols")
                        .and_then(|v| v.as_array())
                        .map(|items| {
                            items.iter()
                                .filter_map(|item| item.as_str())
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    let extra_symbols = graph_details
                        .get("extra_symbols")
                        .and_then(|v| v.as_array())
                        .map(|items| {
                            items.iter()
                                .filter_map(|item| item.as_str())
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    let graph_issue_parts = [
                        (!missing_files.is_empty()).then(|| {
                            format!("missing_files={}", missing_files.join(", "))
                        }),
                        (!extra_files.is_empty())
                            .then(|| format!("extra_files={}", extra_files.join(", "))),
                        (!missing_symbols.is_empty()).then(|| {
                            format!("missing_symbols={}", missing_symbols.join(", "))
                        }),
                        (!extra_symbols.is_empty()).then(|| {
                            format!("extra_symbols={}", extra_symbols.join(", "))
                        }),
                    ]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>();
                    if !verbose && status != "high_confidence" && !graph_issue_parts.is_empty() {
                        out.push_str(&format!(
                            "\nverification_graph_issue: {}",
                            graph_issue_parts.join(" | ")
                        ));
                    }
                    if verbose {
                        if let Some(aligned) =
                            graph_details.get("aligned").and_then(|v| v.as_bool())
                        {
                            out.push_str(&format!("\nverification_graph_aligned: {aligned}"));
                        }
                        if !graph_issue_parts.is_empty() {
                            out.push_str(&format!(
                                "\nverification_graph_diff: {}",
                                graph_issue_parts.join(" | ")
                            ));
                        }
                    }
                }
                if status != "high_confidence" {
                    if let Some(follow_up) = verification
                        .get("recommended_cli_follow_up")
                        .and_then(|v| v.as_str())
                    {
                        out.push_str(&format!("\nverification_follow_up: {follow_up}"));
                    }
                }
                if verbose {
                    if let Some(confidence) = verification
                        .get("confidence_band")
                        .and_then(|v| v.as_str())
                    {
                        out.push_str(&format!("\nverification_confidence: {confidence}"));
                    }
                    let target_in_file = verification
                        .get("exact_target_in_top_context")
                        .and_then(|v| v.as_bool());
                    let target_span = verification
                        .get("exact_target_span_in_top_context")
                        .and_then(|v| v.as_bool());
                    let deps = verification
                        .get("exact_dependencies_in_reported_files")
                        .and_then(|v| v.as_bool());
                    let scope = verification
                        .get("exact_impact_scope_alignment")
                        .and_then(|v| v.as_bool());
                    let scope_graph = verification
                        .get("exact_impact_scope_graph_alignment")
                        .and_then(|v| v.as_bool());
                    let scope_anchor = verification
                        .get("exact_impact_scope_target_anchor")
                        .and_then(|v| v.as_bool());
                    let scope_complete = verification
                        .get("exact_impact_scope_graph_complete")
                        .and_then(|v| v.as_bool());
                    let workspace = verification
                        .get("workspace_boundary_alignment")
                        .and_then(|v| v.as_bool());
                    let evidence = [
                        target_in_file.map(|value| format!("target_in_file={value}")),
                        target_span.map(|value| format!("target_span={value}")),
                        deps.map(|value| format!("deps={value}")),
                        scope.map(|value| format!("scope={value}")),
                        scope_graph.map(|value| format!("scope_graph={value}")),
                        scope_anchor.map(|value| format!("scope_anchor={value}")),
                        scope_complete.map(|value| format!("scope_complete={value}")),
                        workspace.map(|value| format!("workspace={value}")),
                    ]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>();
                    if !evidence.is_empty() {
                        out.push_str(&format!("\nverification_checks: {}", evidence.join(", ")));
                    }
                    if let Some(bundle) = verification.get("mutation_bundle") {
                        let bundle_status = bundle
                            .get("status")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown");
                        let failed = bundle
                            .get("failed_checks")
                            .and_then(|v| v.as_array())
                            .map(|items| {
                                items.iter()
                                    .filter_map(|item| item.as_str())
                                    .collect::<Vec<_>>()
                            })
                            .unwrap_or_default();
                        let missing = bundle
                            .get("missing_checks")
                            .and_then(|v| v.as_array())
                            .map(|items| {
                                items.iter()
                                    .filter_map(|item| item.as_str())
                                    .collect::<Vec<_>>()
                            })
                            .unwrap_or_default();
                        let ready_without_retry = bundle
                            .get("ready_without_retry")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        out.push_str(&format!(
                            "\nmutation_bundle_detail: status={bundle_status}, ready_without_retry={ready_without_retry}"
                        ));
                        if !failed.is_empty() {
                            out.push_str(&format!(
                                "\nmutation_bundle_failed: {}",
                                failed.join(", ")
                            ));
                        }
                        if !missing.is_empty() {
                            out.push_str(&format!(
                                "\nmutation_bundle_missing: {}",
                                missing.join(", ")
                            ));
                        }
                    }
                    if !issues.is_empty() {
                        out.push_str(&format!("\nverification_issues: {}", issues.join(", ")));
                    }
                }
            }
        }
        if let Some(session_id) = value.get("session_id").and_then(|v| v.as_str()) {
            out.push_str(&format!("\nsession_id: {session_id}"));
        }
        if let Some(result) = value.get("result") {
            out.push_str(&format!("\nresult: {}", summarize_value(result, verbose)));
        }
        return out;
    }
    if value.get("ok").and_then(|v| v.as_bool()) == Some(true) {
        if let Some(operation) = value.get("operation").and_then(|v| v.as_str()) {
            let mut out = format!("operation: {operation}");
            if value
                .get("auto_index_applied")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            {
                let target = value
                    .get("auto_index_target")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                out.push_str(&format!("\nauto_index: applied @ {target}"));
                if let Some(count) = value.get("indexed_file_count").and_then(|v| v.as_u64()) {
                    out.push_str(&format!("\nindexed_file_count: {count}"));
                }
                let indexed_path_hints: Vec<String> = value
                    .get("indexed_path_hints")
                    .and_then(|v| v.as_array())
                    .map(|items| {
                        items.iter()
                            .filter_map(|item| item.as_str().map(|s| s.to_string()))
                            .collect()
                    })
                    .unwrap_or_default();
                if !indexed_path_hints.is_empty() {
                    out.push_str(&format!(
                        "\nindexed_path_hints: {}",
                        indexed_path_hints.join(" | ")
                    ));
                }
            }
            if let Some(result) = value.get("result") {
                out.push_str(&format!("\nresult: {}", summarize_value(result, verbose)));
                if let Some(command) = result
                    .get("suggested_index_command")
                    .and_then(|v| v.as_str())
                {
                    out.push_str(&format!("\nindex_follow_up: {command}"));
                }
            }
            return out;
        }
    }
    summarize_value(value, verbose)
}

#[cfg(test)]
mod tests {
    use super::{
        ensure_route_mutation_ready, ensure_route_verification_at_least, load_quality_status,
        text_output,
        QualityTrendSnapshot, VerificationThreshold,
    };
    use std::fs;

    #[test]
    fn quality_status_text_output_is_compact_and_readable() {
        let value = serde_json::json!({
            "kind": "quality_status",
            "status": "clean",
            "health": "stable",
            "latency_health": "stable",
            "graph_drift_health": "stable",
            "diagnosis": "clean",
            "action_recommendation": "no_action",
            "action_priority": "none",
            "triage_path": "none",
            "action_target": null,
            "action_checklist": [],
            "action_commands": [],
            "action_primary_command": null,
            "action_command_categories": [],
            "action_primary_command_category": null,
            "action_source_artifacts": [],
            "latency_hotspot": null,
            "graph_drift_hotspot": null,
            "latency_hotspot_bucket_id": null,
            "graph_drift_hotspot_bucket_id": null,
            "summary_lookup_hint": null,
            "summary_lookup_scope": null,
            "latency_severity": "watch",
            "latency_severity_reason": "watch:route_p95=+1.3ms_vs_trailing",
            "latency_score": 4.2,
            "latency_score_delta_vs_trailing": 1.3,
            "latency_score_direction": "worsening",
            "regression_count": 0,
            "threshold_failure_count": 0,
            "leading_graph_drift": "missing_files (25% in workspace_shared_file_noise)",
            "graph_drift_trend": "missing_files worsening (+10pp vs trailing)",
            "leading_graph_drift_fixture": "workspace_shared_file_noise (missing_files 25%)",
            "graph_drift_severity": "regressing",
            "graph_drift_severity_reason": "top_fixture=workspace_shared_file_noise (workspace_shared_file_noise worsening (missing_files, +10pp vs trailing))",
            "graph_drift_score": 35.0,
            "graph_drift_score_delta_vs_trailing": 10.0,
            "graph_drift_score_direction": "worsening",
            "graph_drift_fixture_trend": "workspace_shared_file_noise worsening (missing_files, +10pp vs trailing)",
            "top_worsening_graph_drift_fixture": "workspace_shared_file_noise (workspace_shared_file_noise worsening (missing_files, +10pp vs trailing))",
            "mutation_scope_incomplete_rate": 0.15,
            "retrieval": {
                "avg_latency_ms": 2.1,
                "p95_latency_ms": 3.7,
                "p95_latency_delta_vs_trailing": -0.8
            },
            "route": {
                "avg_latency_ms": 2.0,
                "p95_latency_ms": 2.8,
                "p95_latency_delta_vs_trailing": -1.3
            }
        });
        let rendered = text_output(&value, false);
        assert!(rendered.contains("quality_status: stable"));
        assert!(rendered.contains("diagnosis: clean"));
        assert!(rendered.contains("action_recommendation: no_action"));
        assert!(rendered.contains("action_priority: none"));
        assert!(rendered.contains("triage_path: none"));
        assert!(rendered.contains("latency_health: stable"));
        assert!(rendered.contains("graph_drift_health: stable"));
        assert!(!rendered.contains("graph_drift_hotspot:"));
        assert!(rendered.contains("latency_severity: watch"));
        assert!(rendered.contains("latency_severity_reason: watch:route_p95=+1.3ms_vs_trailing"));
        assert!(rendered.contains("latency_score: 4.2 (+1.3 vs trailing)"));
        assert!(rendered.contains("latency_score_direction: worsening"));
        assert!(rendered.contains("retrieval: avg=2.1ms p95=3.7ms trailing_delta=-0.8ms"));
        assert!(rendered.contains("route: avg=2.0ms p95=2.8ms trailing_delta=-1.3ms"));
        assert!(rendered.contains(
            "leading_graph_drift: missing_files (25% in workspace_shared_file_noise)"
        ));
        assert!(rendered.contains(
            "graph_drift_trend: missing_files worsening (+10pp vs trailing)"
        ));
        assert!(rendered.contains(
            "leading_graph_drift_fixture: workspace_shared_file_noise (missing_files 25%)"
        ));
        assert!(rendered.contains("graph_drift_severity: regressing"));
        assert!(rendered.contains(
            "graph_drift_severity_reason: top_fixture=workspace_shared_file_noise (workspace_shared_file_noise worsening (missing_files, +10pp vs trailing))"
        ));
        assert!(rendered.contains("graph_drift_score: 35.0 (+10.0 vs trailing)"));
        assert!(rendered.contains("graph_drift_score_direction: worsening"));
        assert!(rendered.contains(
            "graph_drift_fixture_trend: workspace_shared_file_noise worsening (missing_files, +10pp vs trailing)"
        ));
        assert!(rendered.contains(
            "top_worsening_graph_drift_fixture: workspace_shared_file_noise (workspace_shared_file_noise worsening (missing_files, +10pp vs trailing))"
        ));
        assert!(rendered.contains("mutation_scope_incomplete_rate: 15%"));
    }

    #[test]
    fn quality_status_text_output_surfaces_incomplete_mutation_scope_actioning() {
        let value = serde_json::json!({
            "kind": "quality_status",
            "status": "clean",
            "health": "watch",
            "latency_health": "stable",
            "graph_drift_health": "watch",
            "diagnosis": "graph_only_drift",
            "action_recommendation": "inspect_incomplete_mutation_scope",
            "action_priority": "medium",
            "triage_path": "inspect",
            "action_target": "workspace_mixed_module_noise (incomplete 20%)",
            "action_checklist": [
                "inspect docs/doc_ignore/quality_report_summary.md for incomplete mutation neighborhood coverage"
            ],
            "action_commands": [
                "powershell -ExecutionPolicy Bypass -File scripts/export-quality-report.ps1"
            ],
            "action_primary_command": "powershell -ExecutionPolicy Bypass -File scripts/export-quality-report.ps1",
            "action_command_categories": ["export"],
            "action_primary_command_category": "export",
            "action_source_artifacts": [
                "docs/doc_ignore/quality_report_summary.md",
                "docs/doc_ignore/quality_report_trend_snapshot.json",
                "docs/doc_ignore/quality_report_history.json",
                "docs/doc_ignore/quality_report.json"
            ],
            "latency_hotspot": null,
            "graph_drift_hotspot": "workspace_mixed_module_noise (incomplete 20%)",
            "latency_hotspot_bucket_id": null,
            "graph_drift_hotspot_bucket_id": "workspace_mixed_module_noise__incomplete_20_",
            "summary_lookup_hint": "search quality_report_summary.md for `mutation-scope-bucket: workspace_mixed_module_noise__mutation_scope`",
            "summary_lookup_scope": "mutation_scope_bucket",
            "latency_severity": null,
            "latency_severity_reason": null,
            "latency_score": 0.0,
            "latency_score_delta_vs_trailing": 0.0,
            "latency_score_direction": "flat",
            "regression_count": 0,
            "threshold_failure_count": 0,
            "leading_graph_drift": "incomplete (20% in workspace_mixed_module_noise)",
            "graph_drift_trend": "incomplete new (+20pp vs trailing)",
            "leading_graph_drift_fixture": "workspace_mixed_module_noise (incomplete 20%)",
            "graph_drift_severity": "watch",
            "graph_drift_severity_reason": "leading_mode=incomplete | fixture_trend=workspace_mixed_module_noise new (incomplete, +20pp vs trailing)",
            "graph_drift_score": 40.0,
            "graph_drift_score_delta_vs_trailing": 20.0,
            "graph_drift_score_direction": "worsening",
            "graph_drift_fixture_trend": "workspace_mixed_module_noise new (incomplete, +20pp vs trailing)",
            "top_worsening_graph_drift_fixture": "workspace_mixed_module_noise (workspace_mixed_module_noise new (incomplete, +20pp vs trailing))",
            "leading_graph_drift_delta_vs_trailing_pp": 20.0,
            "mutation_scope_incomplete_rate": 0.2,
            "retrieval": {
                "avg_latency_ms": 1.5,
                "p95_latency_ms": 2.1,
                "p95_latency_delta_vs_trailing": 0.0
            },
            "route": {
                "avg_latency_ms": 2.4,
                "p95_latency_ms": 3.2,
                "p95_latency_delta_vs_trailing": 0.1
            }
        });
        let rendered = text_output(&value, false);
        assert!(rendered.contains("action_recommendation: inspect_incomplete_mutation_scope"));
        assert!(rendered.contains("action_priority: medium"));
        assert!(rendered.contains("triage_path: inspect"));
        assert!(rendered.contains("action_source_artifacts: docs/doc_ignore/quality_report_summary.md | docs/doc_ignore/quality_report_trend_snapshot.json | docs/doc_ignore/quality_report_history.json | docs/doc_ignore/quality_report.json"));
        assert!(rendered.contains("graph_drift_hotspot: workspace_mixed_module_noise (incomplete 20%)"));
        assert!(rendered.contains("summary_lookup_scope: mutation_scope_bucket"));
        assert!(rendered.contains("leading_graph_drift: incomplete (20% in workspace_mixed_module_noise)"));
        assert!(rendered.contains("graph_drift_severity_reason: leading_mode=incomplete | fixture_trend=workspace_mixed_module_noise new (incomplete, +20pp vs trailing)"));
        assert!(rendered.contains("mutation_scope_incomplete_rate: 20%"));
    }

    #[test]
    fn quality_status_text_output_prioritizes_incomplete_mutation_scope_in_mixed_drift() {
        let value = serde_json::json!({
            "kind": "quality_status",
            "status": "clean",
            "health": "drifting",
            "latency_health": "drifting",
            "graph_drift_health": "watch",
            "diagnosis": "mixed_drift",
            "action_recommendation": "investigate_mixed_incomplete_mutation_scope",
            "action_priority": "high",
            "triage_path": "investigate",
            "action_target": "incomplete_graph=workspace_duplicate_symbols (incomplete 25%) | latency=workspace_shared_file_noise/route/refactor:refactor_worker_handler (18.00ms)",
            "action_checklist": [
                "rerun scripts/export-quality-report.ps1 to confirm mixed latency and incomplete mutation drift"
            ],
            "action_commands": [
                "powershell -ExecutionPolicy Bypass -File scripts/export-quality-report.ps1"
            ],
            "action_primary_command": "powershell -ExecutionPolicy Bypass -File scripts/export-quality-report.ps1",
            "action_command_categories": ["export"],
            "action_primary_command_category": "export",
            "action_source_artifacts": [
                "docs/doc_ignore/quality_report_summary.md",
                "docs/doc_ignore/quality_report_trend_snapshot.json",
                "docs/doc_ignore/quality_report_history.json",
                "docs/doc_ignore/quality_report.json",
                "docs/doc_ignore/quality_report_baseline.json"
            ],
            "latency_hotspot": "workspace_shared_file_noise/route/refactor:refactor_worker_handler (18.00ms)",
            "graph_drift_hotspot": "workspace_duplicate_symbols (incomplete 25%)",
            "latency_hotspot_bucket_id": "workspace_shared_file_noise/route/refactor",
            "graph_drift_hotspot_bucket_id": "workspace_duplicate_symbols__incomplete_25_",
            "summary_lookup_hint": "search quality_report_summary.md for `mutation-scope-bucket: workspace_duplicate_symbols__mutation_scope`",
            "summary_lookup_scope": "mutation_scope_bucket",
            "latency_severity": "regressing",
            "latency_severity_reason": "regressing:route_p95=+5.0ms_vs_trailing",
            "latency_score": 9.0,
            "latency_score_delta_vs_trailing": 5.0,
            "latency_score_direction": "worsening",
            "regression_count": 1,
            "threshold_failure_count": 0,
            "leading_graph_drift": "incomplete (25% in workspace_duplicate_symbols)",
            "graph_drift_trend": "incomplete worsening (+25pp vs trailing)",
            "leading_graph_drift_fixture": "workspace_duplicate_symbols (incomplete 25%)",
            "graph_drift_severity": "watch",
            "graph_drift_severity_reason": "leading_mode=incomplete | fixture_trend=workspace_duplicate_symbols new (incomplete, +25pp vs trailing)",
            "graph_drift_score": 50.0,
            "graph_drift_score_delta_vs_trailing": 25.0,
            "graph_drift_score_direction": "worsening",
            "graph_drift_fixture_trend": "workspace_duplicate_symbols new (incomplete, +25pp vs trailing)",
            "top_worsening_graph_drift_fixture": "workspace_duplicate_symbols (workspace_duplicate_symbols new (incomplete, +25pp vs trailing))",
            "leading_graph_drift_delta_vs_trailing_pp": 25.0,
            "mutation_scope_incomplete_rate": 0.25,
            "retrieval": {
                "avg_latency_ms": 2.0,
                "p95_latency_ms": 3.5,
                "p95_latency_delta_vs_trailing": 0.8
            },
            "route": {
                "avg_latency_ms": 5.0,
                "p95_latency_ms": 8.0,
                "p95_latency_delta_vs_trailing": 5.0
            }
        });
        let rendered = text_output(&value, false);
        assert!(rendered.contains("diagnosis: mixed_drift"));
        assert!(rendered.contains("action_recommendation: investigate_mixed_incomplete_mutation_scope"));
        assert!(rendered.contains("action_priority: high"));
        assert!(rendered.contains("triage_path: investigate"));
        assert!(rendered.contains("action_target: incomplete_graph=workspace_duplicate_symbols (incomplete 25%) | latency=workspace_shared_file_noise/route/refactor:refactor_worker_handler (18.00ms)"));
        assert!(rendered.contains("summary_lookup_scope: mutation_scope_bucket"));
        assert!(rendered.contains("latency_health: drifting"));
        assert!(rendered.contains("graph_drift_health: watch"));
        assert!(rendered.contains("mutation_scope_incomplete_rate: 25%"));
    }

    #[test]
    fn runtime_status_text_output_surfaces_indexing_mode_and_completeness() {
        let value = serde_json::json!({
            "ok": true,
            "repo_root": "C:/repo",
            "index_revision": 42,
            "index_available": true,
            "indexed_file_count": 128,
            "indexed_path_hints": ["src/auth", "src/shared"],
            "watcher_running": false,
            "bootstrap_index_action": "reuse_existing",
            "indexing_mode": "full_with_default_excludes",
            "indexing_completeness": "source_focused",
            "workspace_mode_enabled": false,
            "workspace_roots": [],
            "llm_router_configured": true,
            "performance": {
                "symbol_lookup": { "avg_ms": 2.4 },
                "planned_context": { "p95_ms": 8.1 }
            }
        });
        let rendered = text_output(&value, false);
        assert!(rendered.contains("status: ok"));
        assert!(rendered.contains("repo_root: C:/repo"));
        assert!(rendered.contains("index_revision: 42"));
        assert!(rendered.contains("index_available: true"));
        assert!(rendered.contains("indexed_file_count: 128"));
        assert!(rendered.contains("indexed_path_hints: src/auth | src/shared"));
        assert!(rendered.contains("bootstrap_index_action: reuse_existing"));
        assert!(rendered.contains("indexing_mode: full_with_default_excludes"));
        assert!(rendered.contains("indexing_completeness: source_focused"));
        assert!(rendered.contains("performance: symbol_lookup_avg=2.4ms planned_context_p95=8.1ms"));
    }

    #[test]
    fn runtime_status_text_output_in_verbose_mode_lists_workspace_roots() {
        let value = serde_json::json!({
            "ok": true,
            "repo_root": "C:/repo",
            "index_revision": 7,
            "index_available": true,
            "indexed_file_count": 2,
            "indexed_path_hints": ["packages/api/src", "packages/worker/src"],
            "watcher_running": true,
            "bootstrap_index_action": "bootstrap_full",
            "indexing_mode": "full_with_default_excludes",
            "indexing_completeness": "source_focused",
            "workspace_mode_enabled": true,
            "workspace_roots": [
                "C:/repo/packages/api",
                "C:/repo/packages/worker"
            ],
            "llm_router_configured": false,
            "performance": {
                "symbol_lookup": { "avg_ms": 1.0 },
                "planned_context": { "p95_ms": 4.0 }
            }
        });
        let rendered = text_output(&value, true);
        assert!(rendered.contains("workspace_roots: C:/repo/packages/api | C:/repo/packages/worker"));
    }

    #[test]
    fn load_quality_status_reads_snapshot_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let snapshot_dir = temp.path().join("docs").join("doc_ignore");
        fs::create_dir_all(&snapshot_dir).expect("mkdirs");
        let snapshot = QualityTrendSnapshot {
            timestamp_unix_ms: 1,
            status: "clean".to_string(),
            health: "stable".to_string(),
            latency_health: "watch".to_string(),
            graph_drift_health: "improving".to_string(),
            diagnosis: "latency_only_drift".to_string(),
            action_recommendation: "watch_latency".to_string(),
            action_priority: "low".to_string(),
            triage_path: "refresh".to_string(),
            action_target: Some("watch:route_p95=+0.5ms_vs_trailing".to_string()),
            action_checklist: vec![
                "rerun scripts/export-quality-report.ps1 to confirm the latency signal".to_string(),
                "inspect docs/doc_ignore/quality_report_summary.md recent trend for latency deltas".to_string(),
                "inspect latency target: watch:route_p95=+0.5ms_vs_trailing".to_string(),
            ],
            action_commands: vec![
                "powershell -ExecutionPolicy Bypass -File scripts/export-quality-report.ps1".to_string(),
                r#"semantic --repo . status --quality --output json"#.to_string(),
                r#"echo inspect_target="watch:route_p95=+0.5ms_vs_trailing""#.to_string(),
            ],
            action_primary_command: Some(
                "powershell -ExecutionPolicy Bypass -File scripts/export-quality-report.ps1"
                    .to_string(),
            ),
            action_command_categories: vec![
                "export".to_string(),
                "status_refresh".to_string(),
                "target_hint".to_string(),
            ],
            action_primary_command_category: Some("export".to_string()),
            action_source_artifacts: vec![
                "docs/doc_ignore/quality_report_summary.md".to_string(),
                "docs/doc_ignore/quality_report_trend_snapshot.json".to_string(),
            ],
            latency_hotspot: Some(
                "workspace_shared_file_noise/route/debug:diagnose_worker_process_job_root_cause (24.58ms)"
                    .to_string(),
            ),
            graph_drift_hotspot: Some(
                "cross_stack_app (extra_symbols 10%)".to_string(),
            ),
            latency_hotspot_bucket_id: Some(
                "workspace_shared_file_noise/route/debug".to_string(),
            ),
            graph_drift_hotspot_bucket_id: Some(
                "cross_stack_app__extra_symbols_10_".to_string(),
            ),
            summary_lookup_hint: Some(
                "search quality_report_summary.md for `workspace_shared_file_noise/route/debug`"
                    .to_string(),
            ),
            summary_lookup_scope: Some("latency_bucket".to_string()),
            latency_severity: Some("watch".to_string()),
            latency_severity_reason: Some("watch:route_p95=+0.5ms_vs_trailing".to_string()),
            latency_score: 3.5,
            latency_score_delta_vs_trailing: 0.5,
            latency_score_direction: Some("worsening".to_string()),
            regression_count: 0,
            threshold_failure_count: 0,
            fixture_count: 5,
            leading_graph_drift: Some("extra_symbols (10% in cross_stack_app)".to_string()),
            leading_graph_drift_fixture: Some("cross_stack_app (extra_symbols 10%)".to_string()),
            graph_drift_severity: Some("improving".to_string()),
            graph_drift_severity_reason: Some(
                "fixture_trend=cross_stack_app improving (extra_symbols, -5pp vs trailing)"
                    .to_string()
            ),
            graph_drift_score: 10.0,
            graph_drift_score_delta_vs_trailing: -5.0,
            graph_drift_score_direction: Some("improving".to_string()),
            graph_drift_trend: Some("extra_symbols improving (-5pp vs trailing)".to_string()),
            graph_drift_fixture_trend: Some(
                "cross_stack_app improving (extra_symbols, -5pp vs trailing)".to_string()
            ),
            top_worsening_graph_drift_fixture: None,
            leading_graph_drift_delta_vs_trailing_pp: -5.0,
            mutation_scope_incomplete_rate: 0.1,
            retrieval_avg_latency_ms: 2.0,
            retrieval_p95_latency_ms: 3.0,
            route_avg_latency_ms: 4.0,
            route_p95_latency_ms: 5.0,
            retrieval_avg_latency_delta_vs_trailing: -0.5,
            retrieval_p95_latency_delta_vs_trailing: -1.0,
            route_avg_latency_delta_vs_trailing: 0.25,
            route_p95_latency_delta_vs_trailing: 0.5,
        };
        fs::write(
            snapshot_dir.join("quality_report_trend_snapshot.json"),
            serde_json::to_string_pretty(&snapshot).expect("serialize"),
        )
        .expect("write snapshot");

        let value = load_quality_status(temp.path()).expect("load snapshot");
        assert_eq!(value.get("health").and_then(|v| v.as_str()), Some("stable"));
        assert_eq!(value.get("latency_health").and_then(|v| v.as_str()), Some("watch"));
        assert_eq!(
            value.get("graph_drift_health").and_then(|v| v.as_str()),
            Some("improving")
        );
        assert_eq!(
            value.get("diagnosis").and_then(|v| v.as_str()),
            Some("latency_only_drift")
        );
        assert_eq!(
            value.get("action_recommendation").and_then(|v| v.as_str()),
            Some("watch_latency")
        );
        assert_eq!(
            value.get("action_priority").and_then(|v| v.as_str()),
            Some("low")
        );
        assert_eq!(
            value.get("triage_path").and_then(|v| v.as_str()),
            Some("refresh")
        );
        assert_eq!(
            value.get("action_target").and_then(|v| v.as_str()),
            Some("watch:route_p95=+0.5ms_vs_trailing")
        );
        assert_eq!(
            value
                .get("action_checklist")
                .and_then(|v| v.as_array())
                .map(|items| items.len()),
            Some(3)
        );
        assert_eq!(
            value
                .get("action_commands")
                .and_then(|v| v.as_array())
                .map(|items| items.len()),
            Some(3)
        );
        assert_eq!(
            value
                .get("action_primary_command")
                .and_then(|v| v.as_str()),
            Some("powershell -ExecutionPolicy Bypass -File scripts/export-quality-report.ps1")
        );
        assert_eq!(
            value
                .get("action_primary_command_category")
                .and_then(|v| v.as_str()),
            Some("export")
        );
        assert_eq!(
            value
                .get("action_command_categories")
                .and_then(|v| v.as_array())
                .map(|items| items.len()),
            Some(3)
        );
        assert_eq!(
            value
                .get("action_source_artifacts")
                .and_then(|v| v.as_array())
                .map(|items| items.len()),
            Some(2)
        );
        assert_eq!(
            value.get("latency_hotspot").and_then(|v| v.as_str()),
            Some("workspace_shared_file_noise/route/debug:diagnose_worker_process_job_root_cause (24.58ms)")
        );
        assert_eq!(
            value.get("graph_drift_hotspot").and_then(|v| v.as_str()),
            Some("cross_stack_app (extra_symbols 10%)")
        );
        assert_eq!(
            value
                .get("latency_hotspot_bucket_id")
                .and_then(|v| v.as_str()),
            Some("workspace_shared_file_noise/route/debug")
        );
        assert_eq!(
            value
                .get("graph_drift_hotspot_bucket_id")
                .and_then(|v| v.as_str()),
            Some("cross_stack_app__extra_symbols_10_")
        );
        assert_eq!(
            value
                .get("summary_lookup_hint")
                .and_then(|v| v.as_str()),
            Some("search quality_report_summary.md for `workspace_shared_file_noise/route/debug`")
        );
        assert_eq!(
            value
                .get("summary_lookup_scope")
                .and_then(|v| v.as_str()),
            Some("latency_bucket")
        );
        assert_eq!(
            value.get("latency_severity").and_then(|v| v.as_str()),
            Some("watch")
        );
        assert_eq!(
            value
                .get("latency_severity_reason")
                .and_then(|v| v.as_str()),
            Some("watch:route_p95=+0.5ms_vs_trailing")
        );
        assert_eq!(
            value.get("latency_score").and_then(|v| v.as_f64()),
            Some(3.5)
        );
        assert_eq!(
            value
                .get("latency_score_delta_vs_trailing")
                .and_then(|v| v.as_f64()),
            Some(0.5)
        );
        assert_eq!(
            value
                .get("latency_score_direction")
                .and_then(|v| v.as_str()),
            Some("worsening")
        );
        assert_eq!(
            value.get("retrieval")
                .and_then(|v| v.get("p95_latency_ms"))
                .and_then(|v| v.as_f64()),
            Some(3.0)
        );
        assert_eq!(
            value.get("leading_graph_drift").and_then(|v| v.as_str()),
            Some("extra_symbols (10% in cross_stack_app)")
        );
        assert_eq!(
            value.get("graph_drift_trend").and_then(|v| v.as_str()),
            Some("extra_symbols improving (-5pp vs trailing)")
        );
        assert_eq!(
            value.get("leading_graph_drift_fixture").and_then(|v| v.as_str()),
            Some("cross_stack_app (extra_symbols 10%)")
        );
        assert_eq!(
            value.get("graph_drift_severity").and_then(|v| v.as_str()),
            Some("improving")
        );
        assert_eq!(
            value
                .get("graph_drift_severity_reason")
                .and_then(|v| v.as_str()),
            Some("fixture_trend=cross_stack_app improving (extra_symbols, -5pp vs trailing)")
        );
        assert_eq!(
            value.get("graph_drift_score").and_then(|v| v.as_f64()),
            Some(10.0)
        );
        assert_eq!(
            value
                .get("graph_drift_score_delta_vs_trailing")
                .and_then(|v| v.as_f64()),
            Some(-5.0)
        );
        assert_eq!(
            value
                .get("graph_drift_score_direction")
                .and_then(|v| v.as_str()),
            Some("improving")
        );
        assert_eq!(
            value.get("graph_drift_fixture_trend").and_then(|v| v.as_str()),
            Some("cross_stack_app improving (extra_symbols, -5pp vs trailing)")
        );
        assert!(value
            .get("top_worsening_graph_drift_fixture")
            .map(|v| v.is_null())
            .unwrap_or(false));
        assert_eq!(
            value
                .get("leading_graph_drift_delta_vs_trailing_pp")
                .and_then(|v| v.as_f64()),
            Some(-5.0)
        );
        assert_eq!(
            value
                .get("mutation_scope_incomplete_rate")
                .and_then(|v| v.as_f64()),
            Some(0.1)
        );
    }

    #[test]
    fn route_text_output_surfaces_verification_status_and_action() {
        let value = serde_json::json!({
            "intent": "debug",
            "selected_tool": "get_planned_context",
            "session_id": "abc123",
            "verification": {
                "status": "needs_review",
                "recommended_action": "review returned spans",
                "recommended_cli_follow_up": "semantic-cli retrieve --op get_file_outline --file packages/api/src/auth/init.ts --output text",
                "mutation_state": "blocked",
                "mutation_bundle": {
                    "status": "blocked",
                    "failed_checks": ["exact_target_span_in_top_context"],
                    "missing_checks": ["exact_dependencies_in_reported_files"],
                    "ready_without_retry": false
                },
                "mutation_block_reason": "top_context_span_does_not_overlap_target_symbol",
                "target_symbol": "initAuth",
                "top_context_file": "packages/api/src/auth/init.ts",
                "confidence_band": "medium",
                "issues": [
                    "top_context_span_does_not_overlap_target_symbol",
                    "context_crosses_workspace_boundary"
                ]
            },
            "result": {
                "symbol": "initAuth"
            }
        });
        let rendered = text_output(&value, false);
        assert!(rendered.contains("verification: needs_review"));
        assert!(rendered.contains("verification_action: review returned spans"));
        assert!(rendered.contains(
            "verification_scope: initAuth @ packages/api/src/auth/init.ts"
        ));
        assert!(rendered.contains("mutation_safety: blocked"));
        assert!(rendered.contains("mutation_bundle: blocked"));
        assert!(rendered.contains(
            "mutation_block_reason: top_context_span_does_not_overlap_target_symbol"
        ));
        assert!(rendered.contains(
            "verification_follow_up: semantic-cli retrieve --op get_file_outline --file packages/api/src/auth/init.ts --output text"
        ));
        assert!(rendered.contains(
            "verification_issue: top_context_span_does_not_overlap_target_symbol (+1 more)"
        ));
        assert!(!rendered.contains("verification_issues:"));
    }

    #[test]
    fn route_text_output_surfaces_graph_scope_issue_summary() {
        let value = serde_json::json!({
            "intent": "refactor",
            "selected_tool": "get_hybrid_ranked_context",
            "verification": {
                "status": "needs_review",
                "recommended_action": "review returned spans",
                "recommended_cli_follow_up": "semantic-cli retrieve --op get_file_outline --file packages/api/src/auth/flow.ts --output text",
                "mutation_state": "blocked",
                "mutation_bundle": {
                    "status": "blocked",
                    "failed_checks": ["exact_impact_scope_graph_alignment"],
                    "missing_checks": [],
                    "ready_without_retry": false
                },
                "target_symbol": "initAuth",
                "top_context_file": "packages/api/src/auth/flow.ts",
                "issues": ["impact_scope_graph_misaligned"],
                "impact_scope_graph_details": {
                    "aligned": false,
                    "missing_files": ["packages/api/tests/auth.spec.ts"],
                    "extra_files": ["packages/worker/src/auth/flow.ts"],
                    "missing_symbols": ["testInitAuth"],
                    "extra_symbols": ["initAuth"]
                }
            },
            "result": {
                "symbol": "initAuth"
            }
        });
        let rendered = text_output(&value, false);
        assert!(rendered.contains("verification_issue: impact_scope_graph_misaligned"));
        assert!(rendered.contains(
            "verification_graph_issue: missing_files=packages/api/tests/auth.spec.ts | extra_files=packages/worker/src/auth/flow.ts | missing_symbols=testInitAuth | extra_symbols=initAuth"
        ));
    }

    #[test]
    fn route_text_output_surfaces_compact_mutation_scope_issue_summary() {
        let value = serde_json::json!({
            "intent": "refactor",
            "selected_tool": "get_hybrid_ranked_context",
            "verification": {
                "status": "needs_review",
                "recommended_action": "review returned spans",
                "recommended_cli_follow_up": "semantic-cli retrieve --op get_file_outline --file packages/api/src/auth/flow.ts --output text",
                "mutation_state": "blocked",
                "mutation_bundle": {
                    "status": "blocked",
                    "failed_checks": ["exact_impact_scope_graph_complete"],
                    "missing_checks": [],
                    "ready_without_retry": false
                },
                "mutation_block_reason": "impact_scope_graph_incomplete",
                "target_symbol": "initAuth",
                "top_context_file": "packages/api/src/auth/flow.ts",
                "issues": ["impact_scope_graph_incomplete"]
            },
            "result": {
                "symbol": "initAuth"
            }
        });
        let rendered = text_output(&value, false);
        assert!(rendered.contains("mutation_scope_issue: incomplete"));
    }

    #[test]
    fn route_text_output_surfaces_verification_gate_summary() {
        let value = serde_json::json!({
            "intent": "debug",
            "selected_tool": "get_planned_context",
            "verification_threshold": "needs_review",
            "mutation_gate": "ready",
            "verification": {
                "status": "needs_review",
                "recommended_action": "review returned spans",
                "recommended_cli_follow_up": "semantic-cli retrieve --op get_file_outline --file packages/worker/src/auth/init.ts --output text",
                "mutation_state": "blocked",
                "mutation_bundle": {
                    "status": "blocked",
                    "failed_checks": ["workspace_boundary_alignment"],
                    "missing_checks": [],
                    "ready_without_retry": false
                },
                "mutation_block_reason": "context_crosses_workspace_boundary",
                "target_symbol": "initAuth",
                "top_context_file": "packages/worker/src/auth/init.ts",
                "issues": [
                    "context_crosses_workspace_boundary"
                ]
            },
            "result": {
                "symbol": "initAuth"
            }
        });
        let rendered = text_output(&value, false);
        assert!(rendered.contains("verification_gate: min=needs_review actual=needs_review"));
        assert!(rendered.contains("mutation_gate: min=ready actual=blocked"));
        assert!(rendered.contains(
            "verification_scope: initAuth @ packages/worker/src/auth/init.ts"
        ));
        assert!(rendered.contains("mutation_safety: blocked"));
        assert!(rendered.contains("mutation_bundle: blocked"));
        assert!(rendered.contains(
            "verification_follow_up: semantic-cli retrieve --op get_file_outline --file packages/worker/src/auth/init.ts --output text"
        ));
        assert!(rendered.contains("verification_issue: context_crosses_workspace_boundary"));
    }

    #[test]
    fn route_text_output_keeps_high_confidence_results_compact() {
        let value = serde_json::json!({
            "intent": "understand",
            "selected_tool": "get_planned_context",
            "mutation_gate": "ready",
            "verification": {
                "status": "high_confidence",
                "recommended_action": "safe to proceed with semantic context",
                "mutation_state": "ready",
                "index_coverage": "indexed_target",
                "index_coverage_target": "src/config/loadConfig.ts",
                "mutation_bundle": {
                    "status": "exact_ready",
                    "failed_checks": [],
                    "missing_checks": [],
                    "ready_without_retry": true
                },
                "target_symbol": "loadConfig",
                "top_context_file": "src/config/loadConfig.ts",
                "issues": []
            },
            "result": {
                "symbol": "loadConfig"
            }
        });
        let rendered = text_output(&value, false);
        assert!(rendered.contains("verification: high_confidence"));
        assert!(rendered.contains("mutation_gate: min=ready actual=ready"));
        assert!(rendered.contains(
            "verification_scope: loadConfig @ src/config/loadConfig.ts"
        ));
        assert!(rendered.contains("mutation_safety: ready"));
        assert!(rendered.contains("mutation_bundle: exact_ready"));
        assert!(rendered.contains("index_coverage: indexed_target @ src/config/loadConfig.ts"));
        assert!(!rendered.contains("verification_issue:"));
        assert!(!rendered.contains("verification_follow_up:"));
    }

    #[test]
    fn route_text_output_surfaces_unindexed_target_coverage() {
        let value = serde_json::json!({
            "intent": "understand",
            "selected_tool": "get_directory_brief",
            "verification": {
                "status": "needs_review",
                "recommended_action": "review returned spans",
                "index_coverage": "unindexed_target",
                "index_coverage_target": "src/worker",
                "suggested_index_command": "semantic index --path src/worker",
                "issues": ["target_path_not_indexed"]
            },
            "result": {}
        });
        let rendered = text_output(&value, false);
        assert!(rendered.contains("index_coverage: unindexed_target @ src/worker"));
        assert!(rendered.contains("index_follow_up: semantic index --path src/worker"));
        assert!(rendered.contains("verification_issue: target_path_not_indexed"));
    }

    #[test]
    fn retrieve_text_output_surfaces_suggested_index_command() {
        let value = serde_json::json!({
            "ok": true,
            "operation": "get_directory_brief",
            "result": {
                "summary_text": "src/worker: 4 files",
                "index_coverage": "unindexed_target",
                "index_coverage_target": "src/worker",
                "suggested_index_command": "semantic index --path src/worker"
            }
        });
        let rendered = text_output(&value, false);
        assert!(rendered.contains("index_follow_up: semantic index --path src/worker"));
    }

    #[test]
    fn route_text_output_surfaces_auto_index_growth_summary() {
        let value = serde_json::json!({
            "intent": "understand",
            "auto_index_applied": true,
            "auto_index_target": "src/worker/job.ts",
            "indexed_file_count": 2,
            "indexed_path_hints": ["src/auth", "src/worker"],
            "verification": {
                "status": "low_confidence"
            },
            "result": {
                "symbol": "runJob"
            }
        });
        let rendered = text_output(&value, false);
        assert!(rendered.contains("auto_index: applied @ src/worker/job.ts"));
        assert!(rendered.contains("indexed_file_count: 2"));
        assert!(rendered.contains("indexed_path_hints: src/auth | src/worker"));
    }

    #[test]
    fn retrieve_text_output_surfaces_auto_index_growth_summary() {
        let value = serde_json::json!({
            "ok": true,
            "operation": "search_symbol",
            "auto_index_applied": true,
            "auto_index_target": "src/worker/job.ts",
            "indexed_file_count": 2,
            "indexed_path_hints": ["src/auth", "src/worker"],
            "result": {
                "index_coverage": "indexed_target",
                "index_coverage_target": "src/worker/job.ts"
            }
        });
        let rendered = text_output(&value, false);
        assert!(rendered.contains("auto_index: applied @ src/worker/job.ts"));
        assert!(rendered.contains("indexed_file_count: 2"));
        assert!(rendered.contains("indexed_path_hints: src/auth | src/worker"));
    }

    #[test]
    fn verbose_route_text_output_surfaces_verification_issues() {
        let value = serde_json::json!({
            "intent": "debug",
            "selected_tool": "get_planned_context",
            "verification": {
                "status": "needs_review",
                "recommended_action": "review returned spans",
                "recommended_cli_follow_up": "semantic-cli retrieve --op get_file_outline --file packages/api/src/auth/init.ts --output text",
                "mutation_state": "blocked",
                "mutation_bundle": {
                    "status": "blocked",
                    "failed_checks": ["exact_target_span_in_top_context"],
                    "missing_checks": [],
                    "ready_without_retry": false
                },
                "mutation_block_reason": "top_context_span_does_not_overlap_target_symbol",
                "target_symbol": "initAuth",
                "top_context_file": "packages/api/src/auth/init.ts",
                "confidence_band": "medium",
                "exact_target_in_top_context": true,
                "exact_target_span_in_top_context": false,
                "exact_dependencies_in_reported_files": true,
                "workspace_boundary_alignment": false,
                "issues": [
                    "top_context_span_does_not_overlap_target_symbol",
                    "context_crosses_workspace_boundary"
                ]
            },
            "result": {
                "symbol": "initAuth"
            }
        });
        let rendered = text_output(&value, true);
        assert!(rendered.contains("verification: needs_review"));
        assert!(rendered.contains("verification_confidence: medium"));
        assert!(rendered.contains("mutation_safety: blocked"));
        assert!(rendered.contains("mutation_bundle: blocked"));
        assert!(rendered.contains(
            "verification_checks: target_in_file=true, target_span=false, deps=true, workspace=false"
        ));
        assert!(rendered.contains(
            "mutation_bundle_detail: status=blocked, ready_without_retry=false"
        ));
        assert!(rendered.contains(
            "mutation_bundle_failed: exact_target_span_in_top_context"
        ));
        assert!(rendered.contains(
            "verification_follow_up: semantic-cli retrieve --op get_file_outline --file packages/api/src/auth/init.ts --output text"
        ));
        assert!(rendered.contains(
            "verification_issues: top_context_span_does_not_overlap_target_symbol, context_crosses_workspace_boundary"
        ));
    }

    #[test]
    fn verbose_route_text_output_surfaces_graph_scope_diff() {
        let value = serde_json::json!({
            "intent": "refactor",
            "selected_tool": "get_hybrid_ranked_context",
            "verification": {
                "status": "needs_review",
                "recommended_action": "review returned spans",
                "recommended_cli_follow_up": "semantic-cli retrieve --op get_file_outline --file packages/api/src/auth/flow.ts --output text",
                "mutation_state": "blocked",
                "mutation_bundle": {
                    "status": "blocked",
                    "failed_checks": ["exact_impact_scope_graph_alignment"],
                    "missing_checks": [],
                    "ready_without_retry": false
                },
                "mutation_block_reason": "impact_scope_graph_misaligned",
                "target_symbol": "initAuth",
                "top_context_file": "packages/api/src/auth/flow.ts",
                "confidence_band": "medium",
                "exact_target_in_top_context": true,
                "exact_target_span_in_top_context": true,
                "exact_dependencies_in_reported_files": true,
                "exact_impact_scope_alignment": true,
                "exact_impact_scope_graph_alignment": false,
                "workspace_boundary_alignment": true,
                "issues": ["impact_scope_graph_misaligned"],
                "impact_scope_graph_details": {
                    "aligned": false,
                    "missing_files": ["packages/api/tests/auth.spec.ts"],
                    "extra_files": ["packages/worker/src/auth/flow.ts"],
                    "missing_symbols": ["testInitAuth"],
                    "extra_symbols": ["initAuth"]
                }
            },
            "result": {
                "symbol": "initAuth"
            }
        });
        let rendered = text_output(&value, true);
        assert!(rendered.contains(
            "verification_checks: target_in_file=true, target_span=true, deps=true, scope=true, scope_graph=false, workspace=true"
        ));
        assert!(rendered.contains("verification_graph_aligned: false"));
        assert!(rendered.contains(
            "verification_graph_diff: missing_files=packages/api/tests/auth.spec.ts | extra_files=packages/worker/src/auth/flow.ts | missing_symbols=testInitAuth | extra_symbols=initAuth"
        ));
    }

    #[test]
    fn verbose_route_text_output_surfaces_scope_anchor_and_completeness_checks() {
        let value = serde_json::json!({
            "intent": "refactor",
            "selected_tool": "get_hybrid_ranked_context",
            "verification": {
                "status": "needs_review",
                "recommended_action": "review returned spans",
                "recommended_cli_follow_up": "semantic-cli retrieve --op get_file_outline --file packages/api/src/auth/flow.ts --output text",
                "mutation_state": "blocked",
                "mutation_bundle": {
                    "status": "blocked",
                    "failed_checks": ["exact_impact_scope_target_anchor", "exact_impact_scope_graph_complete"],
                    "missing_checks": [],
                    "ready_without_retry": false
                },
                "mutation_block_reason": "impact_scope_graph_incomplete",
                "target_symbol": "initAuth",
                "top_context_file": "packages/api/src/auth/flow.ts",
                "confidence_band": "medium",
                "exact_target_in_top_context": true,
                "exact_target_span_in_top_context": true,
                "exact_dependencies_in_reported_files": true,
                "exact_impact_scope_alignment": true,
                "exact_impact_scope_graph_alignment": true,
                "exact_impact_scope_target_anchor": false,
                "exact_impact_scope_graph_complete": false,
                "workspace_boundary_alignment": true,
                "issues": ["impact_scope_not_anchored_to_target", "impact_scope_graph_incomplete"]
            },
            "result": {
                "symbol": "initAuth"
            }
        });
        let rendered = text_output(&value, true);
        assert!(rendered.contains(
            "verification_checks: target_in_file=true, target_span=true, deps=true, scope=true, scope_graph=true, scope_anchor=false, scope_complete=false, workspace=true"
        ));
    }

    #[test]
    fn verbose_route_text_output_shows_gate_before_failure_context() {
        let value = serde_json::json!({
            "intent": "debug",
            "selected_tool": "get_planned_context",
            "verification_threshold": "high_confidence",
            "mutation_gate": "ready",
            "verification": {
                "status": "needs_review",
                "recommended_action": "review returned spans",
                "recommended_cli_follow_up": "semantic-cli retrieve --op get_file_outline --file packages/api/src/auth/init.ts --output text",
                "mutation_state": "blocked",
                "mutation_block_reason": "context_crosses_workspace_boundary",
                "target_symbol": "initAuth",
                "top_context_file": "packages/api/src/auth/init.ts",
                "confidence_band": "medium",
                "exact_target_in_top_context": true,
                "exact_target_span_in_top_context": false,
                "issues": [
                    "context_crosses_workspace_boundary"
                ]
            },
            "result": {
                "symbol": "initAuth"
            }
        });
        let rendered = text_output(&value, true);
        assert!(rendered.contains("verification_gate: min=high_confidence actual=needs_review"));
        assert!(rendered.contains("mutation_gate: min=ready actual=blocked"));
        assert!(rendered.contains(
            "verification_scope: initAuth @ packages/api/src/auth/init.ts"
        ));
        assert!(rendered.contains("mutation_safety: blocked"));
        assert!(rendered.contains(
            "verification_follow_up: semantic-cli retrieve --op get_file_outline --file packages/api/src/auth/init.ts --output text"
        ));
        assert!(rendered.contains("verification_confidence: medium"));
    }

    #[test]
    fn require_high_confidence_route_accepts_high_confidence_results() {
        let value = serde_json::json!({
            "intent": "understand",
            "verification": {
                "status": "high_confidence",
                "recommended_action": "safe to proceed with semantic context",
                "issues": []
            }
        });
        assert!(ensure_route_verification_at_least(
            &value,
            VerificationThreshold::HighConfidence
        )
        .is_ok());
    }

    #[test]
    fn require_high_confidence_route_rejects_review_status() {
        let value = serde_json::json!({
            "intent": "debug",
            "verification": {
                "status": "needs_review",
                "recommended_action": "review returned spans",
                "issues": [
                    "context_crosses_workspace_boundary"
                ]
            }
        });
        let error = ensure_route_verification_at_least(
            &value,
            VerificationThreshold::HighConfidence,
        )
        .expect_err("should fail");
        let message = error.to_string();
        assert!(message.contains("route verification is 'needs_review'"));
        assert!(message.contains("context_crosses_workspace_boundary"));
    }

    #[test]
    fn min_verification_needs_review_accepts_review_status() {
        let value = serde_json::json!({
            "intent": "debug",
            "verification": {
                "status": "needs_review",
                "recommended_action": "review returned spans",
                "issues": [
                    "context_crosses_workspace_boundary"
                ]
            }
        });
        assert!(ensure_route_verification_at_least(
            &value,
            VerificationThreshold::NeedsReview
        )
        .is_ok());
    }

    #[test]
    fn min_verification_needs_review_rejects_low_confidence_status() {
        let value = serde_json::json!({
            "intent": "debug",
            "verification": {
                "status": "low_confidence",
                "recommended_action": "inspect low_confidence_raw_context",
                "issues": [
                    "top_context_span_does_not_overlap_target_symbol"
                ]
            }
        });
        let error = ensure_route_verification_at_least(
            &value,
            VerificationThreshold::NeedsReview,
        )
        .expect_err("should fail");
        assert!(error
            .to_string()
            .contains("requires at least 'needs_review'"));
    }

    #[test]
    fn require_mutation_ready_accepts_ready_state() {
        let value = serde_json::json!({
            "intent": "implement",
            "verification": {
                "mutation_state": "ready"
            }
        });
        assert!(ensure_route_mutation_ready(&value).is_ok());
    }

    #[test]
    fn require_mutation_ready_accepts_not_applicable_state() {
        let value = serde_json::json!({
            "intent": "understand",
            "verification": {
                "mutation_state": "not_applicable"
            }
        });
        assert!(ensure_route_mutation_ready(&value).is_ok());
    }

    #[test]
    fn require_mutation_ready_rejects_blocked_state() {
        let value = serde_json::json!({
            "intent": "refactor",
            "verification": {
                "mutation_state": "blocked",
                "mutation_block_reason": "context_crosses_workspace_boundary",
                "recommended_cli_follow_up": "semantic-cli retrieve --op get_file_outline --file packages/api/src/auth/init.ts --output text"
            }
        });
        let error = ensure_route_mutation_ready(&value).expect_err("should fail");
        let message = error.to_string();
        assert!(message.contains("route mutation safety is 'blocked'"));
        assert!(message.contains("context_crosses_workspace_boundary"));
    }
}

fn summarize_value(value: &serde_json::Value, verbose: bool) -> String {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(summary) = map.get("summary_text").and_then(|v| v.as_str()) {
                return summary.to_string();
            }
            if let Some(message) = map.get("message").and_then(|v| v.as_str()) {
                return message.to_string();
            }
            if verbose {
                serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
            } else {
                let keys = map.keys().cloned().collect::<Vec<_>>().join(", ");
                format!("object {{{keys}}}")
            }
        }
        serde_json::Value::Array(items) => format!("{} items", items.len()),
        serde_json::Value::String(s) => s.clone(),
        _ => value.to_string(),
    }
}
