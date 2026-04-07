use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

#[derive(Debug, Clone, Deserialize)]
pub struct FixtureManifest {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub workspace_roots: Vec<String>,
    #[serde(default)]
    pub files: Vec<String>,
    #[serde(default)]
    pub symbols: Vec<ExpectedSymbol>,
    #[serde(default)]
    pub dependencies: Vec<ExpectedDependency>,
    #[serde(default)]
    pub retrieval_cases: Vec<RetrievalCase>,
    #[serde(default)]
    pub route_cases: Vec<RouteCase>,
    #[serde(default)]
    pub retrieval_thresholds: Option<RetrievalThresholds>,
    #[serde(default)]
    pub route_thresholds: Option<RouteThresholds>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExpectedSymbol {
    pub name: String,
    #[serde(rename = "type")]
    pub symbol_type: String,
    pub file: String,
    pub start_line: u32,
    pub end_line: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExpectedDependency {
    pub caller_symbol: String,
    pub callee_symbol: String,
    pub file: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RetrievalCase {
    pub name: String,
    pub operation: String,
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default)]
    pub symbol: Option<String>,
    #[serde(default)]
    pub max_tokens: Option<usize>,
    #[serde(default)]
    pub logic_radius: Option<usize>,
    #[serde(default)]
    pub dependency_radius: Option<usize>,
    #[serde(default)]
    pub expected_target_symbol: Option<String>,
    #[serde(default)]
    pub expected_top_file: Option<String>,
    #[serde(default)]
    pub expected_top_span: Option<ExpectedSpan>,
    #[serde(default)]
    pub must_include_files: Vec<String>,
    #[serde(default)]
    pub must_not_include_files: Vec<String>,
    #[serde(default)]
    pub max_latency_ms: Option<u128>,
    #[serde(default)]
    pub max_approx_tokens: Option<usize>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExpectedSpan {
    pub file: String,
    pub start_line: u32,
    pub end_line: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RouteCase {
    pub name: String,
    pub task: String,
    #[serde(default)]
    pub include_summary: Option<bool>,
    #[serde(default)]
    pub expected_intent: Option<String>,
    #[serde(default)]
    pub expected_selected_tool: Option<String>,
    #[serde(default)]
    pub expected_max_tokens: Option<usize>,
    #[serde(default)]
    pub expected_reference_only: Option<bool>,
    #[serde(default)]
    pub expected_single_file_fast_path: Option<bool>,
    #[serde(default)]
    pub expected_result_symbol: Option<String>,
    #[serde(default)]
    pub expected_project_summary: Option<bool>,
    #[serde(default)]
    pub max_latency_ms: Option<u128>,
    #[serde(default)]
    pub max_approx_tokens: Option<usize>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RetrievalThresholds {
    #[serde(default)]
    pub min_target_match_rate: Option<f64>,
    #[serde(default)]
    pub min_top_file_match_rate: Option<f64>,
    #[serde(default)]
    pub min_top_span_match_rate: Option<f64>,
    #[serde(default)]
    pub max_omission_rate: Option<f64>,
    #[serde(default)]
    pub max_overfetch_rate: Option<f64>,
    #[serde(default)]
    pub max_avg_latency_ms: Option<f64>,
    #[serde(default)]
    pub max_avg_tokens: Option<f64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RouteThresholds {
    #[serde(default)]
    pub min_reviewable_or_better_rate: Option<f64>,
    #[serde(default)]
    pub min_high_confidence_rate: Option<f64>,
    #[serde(default)]
    pub min_mutation_ready_rate: Option<f64>,
    #[serde(default)]
    pub min_mutation_retry_recovered_rate: Option<f64>,
    #[serde(default)]
    pub min_mutation_ready_without_retry_rate: Option<f64>,
    #[serde(default)]
    pub min_mutation_scope_aligned_rate: Option<f64>,
    #[serde(default)]
    pub max_mutation_scope_incomplete_rate: Option<f64>,
    #[serde(default)]
    pub max_mutation_scope_missing_files_rate: Option<f64>,
    #[serde(default)]
    pub max_mutation_scope_extra_files_rate: Option<f64>,
    #[serde(default)]
    pub max_mutation_scope_missing_symbols_rate: Option<f64>,
    #[serde(default)]
    pub max_mutation_scope_extra_symbols_rate: Option<f64>,
    #[serde(default)]
    pub max_mutation_blocked_rate: Option<f64>,
    #[serde(default)]
    pub min_intent_match_rate: Option<f64>,
    #[serde(default)]
    pub min_selected_tool_match_rate: Option<f64>,
    #[serde(default)]
    pub min_budget_match_rate: Option<f64>,
    #[serde(default)]
    pub min_reference_only_match_rate: Option<f64>,
    #[serde(default)]
    pub min_single_file_fast_path_match_rate: Option<f64>,
    #[serde(default)]
    pub min_result_symbol_match_rate: Option<f64>,
    #[serde(default)]
    pub min_project_summary_match_rate: Option<f64>,
    #[serde(default)]
    pub max_avg_latency_ms: Option<f64>,
    #[serde(default)]
    pub max_avg_tokens: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RetrievalCaseReport {
    pub case_name: String,
    #[serde(default)]
    pub operation_bucket: String,
    pub target_match: bool,
    pub top_file_match: bool,
    pub top_span_match: bool,
    pub omission_count: usize,
    pub overfetch_count: usize,
    pub latency_ms: f64,
    pub approx_tokens: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RetrievalOperationSummary {
    pub operation: String,
    pub case_count: usize,
    pub target_match_rate: f64,
    pub top_file_match_rate: f64,
    pub top_span_match_rate: f64,
    pub omission_rate: f64,
    pub overfetch_rate: f64,
    pub avg_latency_ms: f64,
    pub p95_latency_ms: f64,
    pub avg_tokens: f64,
    pub max_tokens: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RetrievalQualitySummary {
    pub case_count: usize,
    pub target_match_rate: f64,
    pub top_file_match_rate: f64,
    pub top_span_match_rate: f64,
    pub omission_rate: f64,
    pub overfetch_rate: f64,
    pub avg_latency_ms: f64,
    pub p95_latency_ms: f64,
    pub avg_tokens: f64,
    pub max_tokens: usize,
    #[serde(default)]
    pub operation_breakdown: Vec<RetrievalOperationSummary>,
    pub reports: Vec<RetrievalCaseReport>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RouteCaseReport {
    pub case_name: String,
    #[serde(default)]
    pub intent_bucket: String,
    #[serde(default)]
    pub verification_status: String,
    #[serde(default)]
    pub mutation_bundle_status: String,
    #[serde(default)]
    pub mutation_state: String,
    #[serde(default)]
    pub mutation_retry_recovered: bool,
    #[serde(default)]
    pub mutation_ready: bool,
    #[serde(default)]
    pub mutation_ready_without_retry: bool,
    #[serde(default)]
    pub mutation_scope_aligned: bool,
    #[serde(default)]
    pub mutation_scope_incomplete: bool,
    #[serde(default)]
    pub mutation_scope_missing_files: bool,
    #[serde(default)]
    pub mutation_scope_extra_files: bool,
    #[serde(default)]
    pub mutation_scope_missing_symbols: bool,
    #[serde(default)]
    pub mutation_scope_extra_symbols: bool,
    pub intent_match: bool,
    pub selected_tool_match: bool,
    pub budget_match: bool,
    pub reference_only_match: bool,
    pub single_file_fast_path_match: bool,
    pub result_symbol_match: bool,
    pub project_summary_match: bool,
    #[serde(default)]
    pub reviewable_or_better: bool,
    #[serde(default)]
    pub high_confidence: bool,
    pub latency_ms: f64,
    pub approx_tokens: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RouteIntentSummary {
    pub intent: String,
    pub case_count: usize,
    #[serde(default)]
    pub reviewable_or_better_rate: f64,
    #[serde(default)]
    pub high_confidence_rate: f64,
    #[serde(default)]
    pub mutation_retry_recovered_rate: f64,
    #[serde(default)]
    pub mutation_ready_rate: f64,
    #[serde(default)]
    pub mutation_ready_without_retry_rate: f64,
    #[serde(default)]
    pub mutation_exact_ready_rate: f64,
    #[serde(default)]
    pub mutation_scope_aligned_rate: f64,
    #[serde(default)]
    pub mutation_scope_incomplete_rate: f64,
    #[serde(default)]
    pub mutation_scope_missing_files_rate: f64,
    #[serde(default)]
    pub mutation_scope_extra_files_rate: f64,
    #[serde(default)]
    pub mutation_scope_missing_symbols_rate: f64,
    #[serde(default)]
    pub mutation_scope_extra_symbols_rate: f64,
    pub intent_match_rate: f64,
    pub selected_tool_match_rate: f64,
    pub budget_match_rate: f64,
    pub reference_only_match_rate: f64,
    pub single_file_fast_path_match_rate: f64,
    pub result_symbol_match_rate: f64,
    pub project_summary_match_rate: f64,
    pub avg_latency_ms: f64,
    pub p95_latency_ms: f64,
    pub avg_tokens: f64,
    pub max_tokens: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RouteQualitySummary {
    pub case_count: usize,
    #[serde(default)]
    pub reviewable_or_better_rate: f64,
    #[serde(default)]
    pub high_confidence_rate: f64,
    #[serde(default)]
    pub mutation_case_count: usize,
    #[serde(default)]
    pub mutation_reviewable_or_better_rate: f64,
    #[serde(default)]
    pub mutation_high_confidence_rate: f64,
    #[serde(default)]
    pub mutation_retry_recovered_rate: f64,
    #[serde(default)]
    pub mutation_blocked_rate: f64,
    #[serde(default)]
    pub mutation_ready_rate: f64,
    #[serde(default)]
    pub mutation_ready_without_retry_rate: f64,
    #[serde(default)]
    pub mutation_exact_ready_rate: f64,
    #[serde(default)]
    pub mutation_scope_aligned_rate: f64,
    #[serde(default)]
    pub mutation_scope_incomplete_rate: f64,
    #[serde(default)]
    pub mutation_scope_missing_files_rate: f64,
    #[serde(default)]
    pub mutation_scope_extra_files_rate: f64,
    #[serde(default)]
    pub mutation_scope_missing_symbols_rate: f64,
    #[serde(default)]
    pub mutation_scope_extra_symbols_rate: f64,
    pub intent_match_rate: f64,
    pub selected_tool_match_rate: f64,
    pub budget_match_rate: f64,
    pub reference_only_match_rate: f64,
    pub single_file_fast_path_match_rate: f64,
    pub result_symbol_match_rate: f64,
    pub project_summary_match_rate: f64,
    pub avg_latency_ms: f64,
    pub p95_latency_ms: f64,
    pub avg_tokens: f64,
    pub max_tokens: usize,
    #[serde(default)]
    pub intent_breakdown: Vec<RouteIntentSummary>,
    pub reports: Vec<RouteCaseReport>,
}

#[derive(Debug)]
pub struct MaterializedFixture {
    _temp_dir: TempDir,
    repo_root: PathBuf,
    manifest: FixtureManifest,
}

impl MaterializedFixture {
    pub fn repo_root(&self) -> &Path {
        &self.repo_root
    }

    pub fn manifest(&self) -> &FixtureManifest {
        &self.manifest
    }
}

pub fn materialize_quality_fixture(name: &str) -> Result<MaterializedFixture> {
    let fixture_root = quality_fixture_root().join(name);
    if !fixture_root.exists() {
        return Err(anyhow!("unknown quality fixture: {name}"));
    }

    let temp_dir = tempfile::tempdir()?;
    let repo_root = temp_dir.path().join("repo");
    copy_dir_all(&fixture_root.join("repo"), &repo_root)?;
    let manifest: FixtureManifest =
        serde_json::from_str(&fs::read_to_string(fixture_root.join("manifest.json"))?)?;

    Ok(MaterializedFixture {
        _temp_dir: temp_dir,
        repo_root,
        manifest,
    })
}

pub fn list_quality_fixtures() -> Result<Vec<String>> {
    let mut fixtures = Vec::new();
    for entry in fs::read_dir(quality_fixture_root())? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            fixtures.push(entry.file_name().to_string_lossy().to_string());
        }
    }
    fixtures.sort();
    Ok(fixtures)
}

pub fn summarize_retrieval_reports(reports: Vec<RetrievalCaseReport>) -> RetrievalQualitySummary {
    let case_count = reports.len();
    let operation_breakdown = summarize_retrieval_operations(&reports);
    RetrievalQualitySummary {
        case_count,
        target_match_rate: match_rate(case_count, reports.iter().filter(|r| r.target_match).count()),
        top_file_match_rate: match_rate(case_count, reports.iter().filter(|r| r.top_file_match).count()),
        top_span_match_rate: match_rate(case_count, reports.iter().filter(|r| r.top_span_match).count()),
        omission_rate: if case_count == 0 {
            0.0
        } else {
            reports.iter().map(|r| r.omission_count).sum::<usize>() as f64 / case_count as f64
        },
        overfetch_rate: if case_count == 0 {
            0.0
        } else {
            reports.iter().map(|r| r.overfetch_count).sum::<usize>() as f64 / case_count as f64
        },
        avg_latency_ms: average_f64(case_count, reports.iter().map(|r| r.latency_ms).sum()),
        p95_latency_ms: percentile_f64(reports.iter().map(|r| r.latency_ms).collect(), 0.95),
        avg_tokens: average_usize(case_count, reports.iter().map(|r| r.approx_tokens).sum()),
        max_tokens: reports.iter().map(|r| r.approx_tokens).max().unwrap_or_default(),
        operation_breakdown,
        reports,
    }
}

fn summarize_retrieval_operations(reports: &[RetrievalCaseReport]) -> Vec<RetrievalOperationSummary> {
    let mut operations: Vec<String> = reports
        .iter()
        .filter(|report| !report.operation_bucket.is_empty())
        .map(|report| report.operation_bucket.clone())
        .collect();
    operations.sort();
    operations.dedup();

    operations
        .into_iter()
        .map(|operation| {
            let bucket: Vec<&RetrievalCaseReport> = reports
                .iter()
                .filter(|report| report.operation_bucket == operation)
                .collect();
            let case_count = bucket.len();
            RetrievalOperationSummary {
                operation,
                case_count,
                target_match_rate: match_rate(
                    case_count,
                    bucket.iter().filter(|report| report.target_match).count(),
                ),
                top_file_match_rate: match_rate(
                    case_count,
                    bucket.iter().filter(|report| report.top_file_match).count(),
                ),
                top_span_match_rate: match_rate(
                    case_count,
                    bucket.iter().filter(|report| report.top_span_match).count(),
                ),
                omission_rate: if case_count == 0 {
                    0.0
                } else {
                    bucket
                        .iter()
                        .map(|report| report.omission_count)
                        .sum::<usize>() as f64
                        / case_count as f64
                },
                overfetch_rate: if case_count == 0 {
                    0.0
                } else {
                    bucket
                        .iter()
                        .map(|report| report.overfetch_count)
                        .sum::<usize>() as f64
                        / case_count as f64
                },
                avg_latency_ms: average_f64(
                    case_count,
                    bucket.iter().map(|report| report.latency_ms).sum(),
                ),
                p95_latency_ms: percentile_f64(
                    bucket.iter().map(|report| report.latency_ms).collect(),
                    0.95,
                ),
                avg_tokens: average_usize(
                    case_count,
                    bucket.iter().map(|report| report.approx_tokens).sum(),
                ),
                max_tokens: bucket
                    .iter()
                    .map(|report| report.approx_tokens)
                    .max()
                    .unwrap_or_default(),
            }
        })
        .collect()
}

pub fn summarize_route_reports(reports: Vec<RouteCaseReport>) -> RouteQualitySummary {
    let case_count = reports.len();
    let intent_breakdown = summarize_route_intents(&reports);
    let mutation_reports: Vec<&RouteCaseReport> = reports
        .iter()
        .filter(|report| matches!(report.intent_bucket.as_str(), "implement" | "refactor"))
        .collect();
    let mutation_case_count = mutation_reports.len();
    RouteQualitySummary {
        case_count,
        reviewable_or_better_rate: match_rate(
            case_count,
            reports.iter().filter(|r| r.reviewable_or_better).count(),
        ),
        high_confidence_rate: match_rate(
            case_count,
            reports.iter().filter(|r| r.high_confidence).count(),
        ),
        mutation_case_count,
        mutation_reviewable_or_better_rate: match_rate(
            mutation_case_count,
            mutation_reports
                .iter()
                .filter(|report| report.reviewable_or_better)
                .count(),
        ),
        mutation_high_confidence_rate: match_rate(
            mutation_case_count,
            mutation_reports
                .iter()
                .filter(|report| report.high_confidence)
                .count(),
        ),
        mutation_retry_recovered_rate: match_rate(
            mutation_case_count,
            mutation_reports
                .iter()
                .filter(|report| report.mutation_retry_recovered)
                .count(),
        ),
        mutation_ready_without_retry_rate: match_rate(
            mutation_case_count,
            mutation_reports
                .iter()
                .filter(|report| report.mutation_ready_without_retry)
                .count(),
        ),
        mutation_exact_ready_rate: match_rate(
            mutation_case_count,
            mutation_reports
                .iter()
                .filter(|report| report.mutation_bundle_status == "exact_ready")
                .count(),
        ),
        mutation_scope_aligned_rate: match_rate(
            mutation_case_count,
            mutation_reports
                .iter()
                .filter(|report| report.mutation_scope_aligned)
                .count(),
        ),
        mutation_scope_incomplete_rate: match_rate(
            mutation_case_count,
            mutation_reports
                .iter()
                .filter(|report| report.mutation_scope_incomplete)
                .count(),
        ),
        mutation_scope_missing_files_rate: match_rate(
            mutation_case_count,
            mutation_reports
                .iter()
                .filter(|report| report.mutation_scope_missing_files)
                .count(),
        ),
        mutation_scope_extra_files_rate: match_rate(
            mutation_case_count,
            mutation_reports
                .iter()
                .filter(|report| report.mutation_scope_extra_files)
                .count(),
        ),
        mutation_scope_missing_symbols_rate: match_rate(
            mutation_case_count,
            mutation_reports
                .iter()
                .filter(|report| report.mutation_scope_missing_symbols)
                .count(),
        ),
        mutation_scope_extra_symbols_rate: match_rate(
            mutation_case_count,
            mutation_reports
                .iter()
                .filter(|report| report.mutation_scope_extra_symbols)
                .count(),
        ),
        mutation_ready_rate: match_rate(
            mutation_case_count,
            mutation_reports
                .iter()
                .filter(|report| report.mutation_ready)
                .count(),
        ),
        mutation_blocked_rate: match_rate(
            mutation_case_count,
            mutation_reports
                .iter()
                .filter(|report| report.mutation_state == "blocked")
                .count(),
        ),
        intent_match_rate: match_rate(case_count, reports.iter().filter(|r| r.intent_match).count()),
        selected_tool_match_rate: match_rate(
            case_count,
            reports.iter().filter(|r| r.selected_tool_match).count(),
        ),
        budget_match_rate: match_rate(case_count, reports.iter().filter(|r| r.budget_match).count()),
        reference_only_match_rate: match_rate(
            case_count,
            reports.iter().filter(|r| r.reference_only_match).count(),
        ),
        single_file_fast_path_match_rate: match_rate(
            case_count,
            reports
                .iter()
                .filter(|r| r.single_file_fast_path_match)
                .count(),
        ),
        result_symbol_match_rate: match_rate(
            case_count,
            reports.iter().filter(|r| r.result_symbol_match).count(),
        ),
        project_summary_match_rate: match_rate(
            case_count,
            reports.iter().filter(|r| r.project_summary_match).count(),
        ),
        avg_latency_ms: average_f64(case_count, reports.iter().map(|r| r.latency_ms).sum()),
        p95_latency_ms: percentile_f64(reports.iter().map(|r| r.latency_ms).collect(), 0.95),
        avg_tokens: average_usize(case_count, reports.iter().map(|r| r.approx_tokens).sum()),
        max_tokens: reports.iter().map(|r| r.approx_tokens).max().unwrap_or_default(),
        intent_breakdown,
        reports,
    }
}

fn summarize_route_intents(reports: &[RouteCaseReport]) -> Vec<RouteIntentSummary> {
    let mut intents: Vec<String> = reports
        .iter()
        .filter(|report| !report.intent_bucket.is_empty())
        .map(|report| report.intent_bucket.clone())
        .collect();
    intents.sort();
    intents.dedup();

    intents
        .into_iter()
        .map(|intent| {
            let bucket: Vec<&RouteCaseReport> = reports
                .iter()
                .filter(|report| report.intent_bucket == intent)
                .collect();
            let case_count = bucket.len();
            RouteIntentSummary {
                intent,
                case_count,
                reviewable_or_better_rate: match_rate(
                    case_count,
                    bucket
                        .iter()
                        .filter(|report| report.reviewable_or_better)
                        .count(),
                ),
                high_confidence_rate: match_rate(
                    case_count,
                    bucket
                        .iter()
                        .filter(|report| report.high_confidence)
                        .count(),
                ),
                mutation_retry_recovered_rate: match_rate(
                    case_count,
                    bucket
                        .iter()
                        .filter(|report| report.mutation_retry_recovered)
                        .count(),
                ),
                mutation_ready_without_retry_rate: match_rate(
                    case_count,
                    bucket
                        .iter()
                        .filter(|report| report.mutation_ready_without_retry)
                        .count(),
                ),
                mutation_exact_ready_rate: match_rate(
                    case_count,
                    bucket
                        .iter()
                        .filter(|report| report.mutation_bundle_status == "exact_ready")
                        .count(),
                ),
                mutation_scope_aligned_rate: match_rate(
                    case_count,
                    bucket
                        .iter()
                        .filter(|report| report.mutation_scope_aligned)
                        .count(),
                ),
                mutation_scope_incomplete_rate: match_rate(
                    case_count,
                    bucket
                        .iter()
                        .filter(|report| report.mutation_scope_incomplete)
                        .count(),
                ),
                mutation_scope_missing_files_rate: match_rate(
                    case_count,
                    bucket
                        .iter()
                        .filter(|report| report.mutation_scope_missing_files)
                        .count(),
                ),
                mutation_scope_extra_files_rate: match_rate(
                    case_count,
                    bucket
                        .iter()
                        .filter(|report| report.mutation_scope_extra_files)
                        .count(),
                ),
                mutation_scope_missing_symbols_rate: match_rate(
                    case_count,
                    bucket
                        .iter()
                        .filter(|report| report.mutation_scope_missing_symbols)
                        .count(),
                ),
                mutation_scope_extra_symbols_rate: match_rate(
                    case_count,
                    bucket
                        .iter()
                        .filter(|report| report.mutation_scope_extra_symbols)
                        .count(),
                ),
                mutation_ready_rate: match_rate(
                    case_count,
                    bucket
                        .iter()
                        .filter(|report| report.mutation_ready)
                        .count(),
                ),
                intent_match_rate: match_rate(
                    case_count,
                    bucket.iter().filter(|report| report.intent_match).count(),
                ),
                selected_tool_match_rate: match_rate(
                    case_count,
                    bucket
                        .iter()
                        .filter(|report| report.selected_tool_match)
                        .count(),
                ),
                budget_match_rate: match_rate(
                    case_count,
                    bucket.iter().filter(|report| report.budget_match).count(),
                ),
                reference_only_match_rate: match_rate(
                    case_count,
                    bucket
                        .iter()
                        .filter(|report| report.reference_only_match)
                        .count(),
                ),
                single_file_fast_path_match_rate: match_rate(
                    case_count,
                    bucket
                        .iter()
                        .filter(|report| report.single_file_fast_path_match)
                        .count(),
                ),
                result_symbol_match_rate: match_rate(
                    case_count,
                    bucket
                        .iter()
                        .filter(|report| report.result_symbol_match)
                        .count(),
                ),
                project_summary_match_rate: match_rate(
                    case_count,
                    bucket
                        .iter()
                        .filter(|report| report.project_summary_match)
                        .count(),
                ),
                avg_latency_ms: average_f64(
                    case_count,
                    bucket.iter().map(|report| report.latency_ms).sum(),
                ),
                p95_latency_ms: percentile_f64(
                    bucket.iter().map(|report| report.latency_ms).collect(),
                    0.95,
                ),
                avg_tokens: average_usize(
                    case_count,
                    bucket.iter().map(|report| report.approx_tokens).sum(),
                ),
                max_tokens: bucket
                    .iter()
                    .map(|report| report.approx_tokens)
                    .max()
                    .unwrap_or_default(),
            }
        })
        .collect()
}

fn match_rate(total: usize, matched: usize) -> f64 {
    if total == 0 {
        0.0
    } else {
        matched as f64 / total as f64
    }
}

pub fn estimate_tokens_json(value: &serde_json::Value) -> usize {
    let serialized = serde_json::to_string(value).unwrap_or_default();
    ((serialized.len() as f64) / 3.0).ceil() as usize
}

fn average_f64(total: usize, sum: f64) -> f64 {
    if total == 0 {
        0.0
    } else {
        sum / total as f64
    }
}

fn average_usize(total: usize, sum: usize) -> f64 {
    if total == 0 {
        0.0
    } else {
        sum as f64 / total as f64
    }
}

fn percentile_f64(mut values: Vec<f64>, percentile: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let idx = ((values.len() as f64) * percentile).floor() as usize;
    values[idx.min(values.len() - 1)]
}

fn quality_fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("test_fixtures")
        .join("quality")
}

fn copy_dir_all(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_all(&src_path, &dst_path)?;
        } else {
            fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{list_quality_fixtures, materialize_quality_fixture};

    #[test]
    fn loads_cross_stack_fixture_manifest_and_repo() {
        let fixture = materialize_quality_fixture("cross_stack_app").expect("fixture");
        assert_eq!(fixture.manifest().name, "cross_stack_app");
        assert!(fixture.repo_root().join("src").join("api").join("client.ts").exists());
        assert!(fixture
            .manifest()
            .symbols
            .iter()
            .any(|symbol| symbol.name == "fetchData"));
        assert!(!fixture.manifest().retrieval_cases.is_empty());
    }

    #[test]
    fn materializes_all_quality_fixtures() {
        let fixtures = list_quality_fixtures().expect("list fixtures");
        assert!(
            fixtures.contains(&"workspace_path_collisions".to_string()),
            "new workspace path collision fixture should be discoverable"
        );
        assert!(
            fixtures.contains(&"workspace_shared_file_noise".to_string()),
            "shared-file workspace fixture should be discoverable"
        );
        assert!(
            fixtures.contains(&"cross_file_import_duplicates".to_string()),
            "cross-file import duplicate fixture should be discoverable"
        );
        assert!(
            fixtures.contains(&"import_alias_reexports".to_string()),
            "import alias and re-export fixture should be discoverable"
        );
        assert!(
            fixtures.contains(&"multi_hop_export_star".to_string()),
            "multi-hop export-star fixture should be discoverable"
        );
        assert!(
            fixtures.contains(&"default_export_aliases".to_string()),
            "default export alias fixture should be discoverable"
        );
        assert!(
            fixtures.contains(&"unsupported_default_boundary".to_string()),
            "unsupported default boundary fixture should be discoverable"
        );
        assert!(
            fixtures.contains(&"unsupported_default_barrel_boundary".to_string()),
            "unsupported default barrel boundary fixture should be discoverable"
        );
        assert!(
            fixtures.contains(&"unsupported_commonjs_boundary".to_string()),
            "unsupported commonjs boundary fixture should be discoverable"
        );
        assert!(
            fixtures.contains(&"unsupported_namespace_export_boundary".to_string()),
            "unsupported namespace export boundary fixture should be discoverable"
        );
        assert!(
            fixtures.contains(&"unsupported_commonjs_destructure_boundary".to_string()),
            "unsupported CommonJS destructure boundary fixture should be discoverable"
        );
        assert!(
            fixtures.contains(&"unsupported_commonjs_object_boundary".to_string()),
            "unsupported CommonJS object boundary fixture should be discoverable"
        );
        assert!(
            fixtures.contains(&"mixed_module_pattern_noise".to_string()),
            "mixed module pattern noise fixture should be discoverable"
        );
        assert!(
            fixtures.contains(&"workspace_mixed_module_noise".to_string()),
            "workspace mixed module noise fixture should be discoverable"
        );
        assert!(
            fixtures.contains(&"workspace_mixed_module_with_tests".to_string()),
            "workspace mixed module with tests fixture should be discoverable"
        );
        assert!(
            fixtures.contains(&"python_workspace_noise".to_string()),
            "python workspace noise fixture should be discoverable"
        );
        for name in fixtures {
            let fixture = materialize_quality_fixture(&name).expect("fixture materializes");
            assert_eq!(fixture.manifest().name, name);
            assert!(fixture.repo_root().exists(), "fixture repo root should exist");
        }
    }
}
