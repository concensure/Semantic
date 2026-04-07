use anyhow::{anyhow, Result};
use engine::{Operation, RetrievalRequest};
use indexer::Indexer;
use retrieval::RetrievalService;
use semantic_app::{AppRuntime, RuntimeOptions};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use storage::Storage;
use test_support::{
    estimate_tokens_json, list_quality_fixtures, materialize_quality_fixture,
    summarize_retrieval_reports, summarize_route_reports, RetrievalCase, RetrievalCaseReport,
    RetrievalQualitySummary, RetrievalThresholds, RouteCase, RouteCaseReport,
    RouteQualitySummary, RouteThresholds,
};

const LATENCY_REGRESSION_TOLERANCE_PCT: f64 = 0.25;
const LATENCY_REGRESSION_ABS_TOLERANCE_MS: f64 = 16.0;
const LATENCY_CEILING_JITTER_MS: f64 = 5.0;
const TINY_BASELINE_LATENCY_MS: f64 = 5.0;
const TINY_BASELINE_LATENCY_ABS_TOLERANCE_MS: f64 = 30.0;
const P95_IMPROVEMENT_MIN_DELTA_MS: f64 = 5.0;
const P95_FIXTURE_REGRESSION_MIN_DELTA_MS: f64 = 2.0;
const P95_FIXTURE_IMPROVEMENT_MIN_DELTA_MS: f64 = 6.0;
const P95_SCOPE_REGRESSION_MIN_DELTA_MS: f64 = 2.0;
const P95_SCOPE_IMPROVEMENT_MIN_DELTA_MS: f64 = 6.0;
const LATENCY_HEALTH_DRIFT_P95_DELTA_MS: f64 = 5.0;
const LATENCY_HEALTH_DRIFT_AVG_DELTA_MS: f64 = 3.0;
const LATENCY_HEALTH_WATCH_P95_DELTA_MS: f64 = 1.0;
const LATENCY_HEALTH_WATCH_AVG_DELTA_MS: f64 = 0.75;
const LATENCY_HEALTH_MIN_ABS_P95_MS: f64 = 8.0;
const LATENCY_HEALTH_MIN_ABS_AVG_MS: f64 = 5.0;
const TOKEN_REGRESSION_TOLERANCE_PCT: f64 = 0.20;
const EPSILON: f64 = 0.000_001;
const GRAPH_DRIFT_REPORT_MIN_RATE: f64 = 0.005;
const QUALITY_MEASUREMENT_WARMUP_RUNS: usize = 1;
const QUALITY_MEASUREMENT_REPEATS: usize = 2;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FixtureQualityReport {
    fixture: String,
    #[serde(default)]
    retrieval_max_avg_latency_ms: Option<f64>,
    #[serde(default)]
    route_max_avg_latency_ms: Option<f64>,
    retrieval: Option<RetrievalQualitySummary>,
    route: Option<RouteQualitySummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WorstCaseRow {
    fixture: String,
    section: String,
    bucket: String,
    case_name: String,
    latency_ms: f64,
    approx_tokens: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct QualityReport {
    generated_by: String,
    fixture_count: usize,
    #[serde(default)]
    worst_retrieval_latency: Vec<WorstCaseRow>,
    #[serde(default)]
    worst_retrieval_tokens: Vec<WorstCaseRow>,
    #[serde(default)]
    worst_route_latency: Vec<WorstCaseRow>,
    #[serde(default)]
    worst_route_tokens: Vec<WorstCaseRow>,
    fixtures: Vec<FixtureQualityReport>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct QualityTrendEntry {
    timestamp_unix_ms: u64,
    fixture_count: usize,
    status: String,
    regression_count: usize,
    threshold_failure_count: usize,
    retrieval_avg_latency_ms: f64,
    retrieval_p95_latency_ms: f64,
    retrieval_avg_tokens: f64,
    route_avg_latency_ms: f64,
    route_p95_latency_ms: f64,
    route_avg_tokens: f64,
    #[serde(default)]
    mutation_scope_incomplete_rate: f64,
    #[serde(default)]
    mutation_scope_missing_files_rate: f64,
    #[serde(default)]
    mutation_scope_extra_files_rate: f64,
    #[serde(default)]
    mutation_scope_missing_symbols_rate: f64,
    #[serde(default)]
    mutation_scope_extra_symbols_rate: f64,
    #[serde(default)]
    fixture_graph_drift: Vec<FixtureGraphDriftEntry>,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FixtureGraphDriftEntry {
    fixture: String,
    #[serde(default)]
    leading_mode: Option<String>,
    leading_rate: f64,
}

fn default_history_path(output_path: &Path) -> PathBuf {
    let parent = output_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let stem = output_path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("quality_report");
    parent.join(format!("{stem}_history.json"))
}

fn default_snapshot_path(output_path: &Path) -> PathBuf {
    let parent = output_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let stem = output_path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("quality_report");
    parent.join(format!("{stem}_trend_snapshot.json"))
}

fn default_summary_path(output_path: &Path) -> PathBuf {
    let parent = output_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let stem = output_path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("quality_report");
    parent.join(format!("{stem}_summary.md"))
}

fn build_trend_entry(
    report: &QualityReport,
    status: &str,
    regression_count: usize,
    threshold_failure_count: usize,
) -> QualityTrendEntry {
    let retrieval_summaries: Vec<&RetrievalQualitySummary> = report
        .fixtures
        .iter()
        .filter_map(|fixture| fixture.retrieval.as_ref())
        .collect();
    let route_summaries: Vec<&RouteQualitySummary> = report
        .fixtures
        .iter()
        .filter_map(|fixture| fixture.route.as_ref())
        .collect();

    QualityTrendEntry {
        timestamp_unix_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64,
        fixture_count: report.fixture_count,
        status: status.to_string(),
        regression_count,
        threshold_failure_count,
        retrieval_avg_latency_ms: average_f64_slice(
            retrieval_summaries
                .iter()
                .map(|summary| summary.avg_latency_ms)
                .collect(),
        ),
        retrieval_p95_latency_ms: average_f64_slice(
            retrieval_summaries
                .iter()
                .map(|summary| summary.p95_latency_ms)
                .collect(),
        ),
        retrieval_avg_tokens: average_f64_slice(
            retrieval_summaries
                .iter()
                .map(|summary| summary.avg_tokens)
                .collect(),
        ),
        route_avg_latency_ms: average_f64_slice(
            route_summaries
                .iter()
                .map(|summary| summary.avg_latency_ms)
                .collect(),
        ),
        route_p95_latency_ms: average_f64_slice(
            route_summaries
                .iter()
                .map(|summary| summary.p95_latency_ms)
                .collect(),
        ),
        route_avg_tokens: average_f64_slice(
            route_summaries
                .iter()
                .map(|summary| summary.avg_tokens)
                .collect(),
        ),
        mutation_scope_incomplete_rate: weighted_route_metric_average(
            &route_summaries,
            |summary| summary.mutation_scope_incomplete_rate,
            |summary| summary.mutation_case_count,
        ),
        mutation_scope_missing_files_rate: weighted_route_metric_average(
            &route_summaries,
            |summary| summary.mutation_scope_missing_files_rate,
            |summary| summary.mutation_case_count,
        ),
        mutation_scope_extra_files_rate: weighted_route_metric_average(
            &route_summaries,
            |summary| summary.mutation_scope_extra_files_rate,
            |summary| summary.mutation_case_count,
        ),
        mutation_scope_missing_symbols_rate: weighted_route_metric_average(
            &route_summaries,
            |summary| summary.mutation_scope_missing_symbols_rate,
            |summary| summary.mutation_case_count,
        ),
        mutation_scope_extra_symbols_rate: weighted_route_metric_average(
            &route_summaries,
            |summary| summary.mutation_scope_extra_symbols_rate,
            |summary| summary.mutation_case_count,
        ),
        fixture_graph_drift: fixture_graph_drift_entries(report),
    }
}

fn update_quality_history(
    history_path: &Path,
    entry: QualityTrendEntry,
) -> Result<Vec<QualityTrendEntry>> {
    let mut history: Vec<QualityTrendEntry> = if history_path.exists() {
        serde_json::from_str(&fs::read_to_string(history_path)?)?
    } else {
        Vec::new()
    };
    history.push(entry);
    if history.len() > 25 {
        let drain_len = history.len() - 25;
        history.drain(0..drain_len);
    }
    if let Some(parent) = history_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(history_path, serde_json::to_string_pretty(&history)?)?;
    Ok(history)
}

fn average_f64_slice(values: Vec<f64>) -> f64 {
    if values.is_empty() {
        0.0
    } else {
        values.iter().sum::<f64>() / values.len() as f64
    }
}

fn weighted_route_metric_average<F, W>(
    summaries: &[&RouteQualitySummary],
    metric: F,
    weight: W,
) -> f64
where
    F: Fn(&RouteQualitySummary) -> f64,
    W: Fn(&RouteQualitySummary) -> usize,
{
    let mut total_weight = 0usize;
    let mut total_value = 0.0;
    for summary in summaries {
        let sample_weight = weight(summary);
        if sample_weight == 0 {
            continue;
        }
        total_weight += sample_weight;
        total_value += metric(summary) * sample_weight as f64;
    }
    if total_weight == 0 {
        0.0
    } else {
        total_value / total_weight as f64
    }
}

fn main() -> Result<()> {
    let mut output_path = PathBuf::from("docs")
        .join("doc_ignore")
        .join("quality_report.json");
    let mut baseline_path = PathBuf::from("docs")
        .join("doc_ignore")
        .join("quality_report_baseline.json");
    let mut summary_path: Option<PathBuf> = None;
    let mut history_path: Option<PathBuf> = None;
    let mut snapshot_path: Option<PathBuf> = None;
    let mut write_baseline = false;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--baseline" => {
                let value = args
                    .next()
                    .ok_or_else(|| anyhow!("--baseline requires a path"))?;
                baseline_path = PathBuf::from(value);
            }
            "--write-baseline" => {
                write_baseline = true;
            }
            "--summary" => {
                let value = args
                    .next()
                    .ok_or_else(|| anyhow!("--summary requires a path"))?;
                summary_path = Some(PathBuf::from(value));
            }
            "--history" => {
                let value = args
                    .next()
                    .ok_or_else(|| anyhow!("--history requires a path"))?;
                history_path = Some(PathBuf::from(value));
            }
            "--snapshot" => {
                let value = args
                    .next()
                    .ok_or_else(|| anyhow!("--snapshot requires a path"))?;
                snapshot_path = Some(PathBuf::from(value));
            }
            value => {
                output_path = PathBuf::from(value);
            }
        }
    }
    let summary_path = summary_path.unwrap_or_else(|| default_summary_path(&output_path));
    let history_path = history_path.unwrap_or_else(|| default_history_path(&output_path));
    let snapshot_path = snapshot_path.unwrap_or_else(|| default_snapshot_path(&output_path));

    let mut reports = Vec::new();
    let mut threshold_failures = Vec::new();
    for fixture_name in list_quality_fixtures()? {
        let fixture = materialize_quality_fixture(&fixture_name)?;
        let retrieval_summary =
            build_retrieval_summary(&fixture_name, fixture.repo_root(), &mut threshold_failures)?;
        let route_summary =
            build_route_summary(&fixture_name, fixture.repo_root(), &mut threshold_failures)?;
        reports.push(FixtureQualityReport {
            fixture: fixture_name,
            retrieval_max_avg_latency_ms: fixture
                .manifest()
                .retrieval_thresholds
                .as_ref()
                .and_then(|thresholds| thresholds.max_avg_latency_ms),
            route_max_avg_latency_ms: fixture
                .manifest()
                .route_thresholds
                .as_ref()
                .and_then(|thresholds| thresholds.max_avg_latency_ms),
            retrieval: retrieval_summary,
            route: route_summary,
        });
    }

    let report = QualityReport {
        generated_by: "cargo run -p test_support --example export_quality_report".to_string(),
        fixture_count: reports.len(),
        worst_retrieval_latency: worst_retrieval_rows(&reports, true, true),
        worst_retrieval_tokens: worst_retrieval_rows(&reports, false, true),
        worst_route_latency: worst_route_rows(&reports, true, true),
        worst_route_tokens: worst_route_rows(&reports, false, true),
        fixtures: reports,
    };

    let baseline = if !write_baseline && baseline_path.exists() {
        Some(serde_json::from_str::<QualityReport>(&fs::read_to_string(&baseline_path)?)?)
    } else {
        None
    };

    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&output_path, serde_json::to_string_pretty(&report)?)?;
    if let Some(parent) = summary_path.parent() {
        fs::create_dir_all(parent)?;
    }

    let mut regression_failures = Vec::new();
    if let Some(baseline) = baseline.as_ref() {
        regression_failures = compare_reports(baseline, &report);
    }

    let status = if !regression_failures.is_empty() {
        "regression".to_string()
    } else if !threshold_failures.is_empty() {
        "threshold_failure".to_string()
    } else {
        "clean".to_string()
    };
    let history = update_quality_history(
        &history_path,
        build_trend_entry(
            &report,
            &status,
            regression_failures.len(),
            threshold_failures.len(),
        ),
    )?;
    let trend_snapshot = build_trend_snapshot(&report, &history);
    if let Some(parent) = snapshot_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&snapshot_path, serde_json::to_string_pretty(&trend_snapshot)?)?;

    if write_baseline {
        if let Some(parent) = baseline_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&baseline_path, serde_json::to_string_pretty(&report)?)?;
    }

    fs::write(
        &summary_path,
        render_summary_markdown(&report, baseline.as_ref(), &history),
    )?;

    println!("{}", output_path.display());
    println!("summary: {}", summary_path.display());
    println!("history: {}", history_path.display());
    println!("snapshot: {}", snapshot_path.display());
    if write_baseline {
        println!("baseline: {}", baseline_path.display());
    }
    if !regression_failures.is_empty() {
        for failure in &regression_failures {
            eprintln!("regression: {failure}");
        }
        return Err(anyhow!("quality regressions detected"));
    }
    if !threshold_failures.is_empty() {
        for failure in &threshold_failures {
            eprintln!("threshold: {failure}");
        }
        return Err(anyhow!("quality threshold failures detected"));
    }
    Ok(())
}

fn build_trend_snapshot(report: &QualityReport, history: &[QualityTrendEntry]) -> QualityTrendSnapshot {
    let latest = history
        .last()
        .cloned()
        .unwrap_or(QualityTrendEntry {
            timestamp_unix_ms: 0,
            fixture_count: 0,
            status: "unknown".to_string(),
            regression_count: 0,
            threshold_failure_count: 0,
            retrieval_avg_latency_ms: 0.0,
            retrieval_p95_latency_ms: 0.0,
            retrieval_avg_tokens: 0.0,
            route_avg_latency_ms: 0.0,
            route_p95_latency_ms: 0.0,
            route_avg_tokens: 0.0,
            mutation_scope_incomplete_rate: 0.0,
            mutation_scope_missing_files_rate: 0.0,
            mutation_scope_extra_files_rate: 0.0,
            mutation_scope_missing_symbols_rate: 0.0,
            mutation_scope_extra_symbols_rate: 0.0,
            fixture_graph_drift: Vec::new(),
        });
    let trailing_opt = trailing_average_entry(history, 3);
    let latency_health = latency_health_with_history(history, &latest, trailing_opt.as_ref()).to_string();
    let graph_drift_health = graph_drift_health_marker(
        &latest,
        trailing_opt.as_ref().unwrap_or(&latest),
    )
    .to_string();
    let leading_graph_drift_mode = leading_graph_drift_from_entry(&latest).map(|(label, _)| label);
    let diagnosis = quality_diagnosis(&latency_health, &graph_drift_health).to_string();
    let action_recommendation = quality_action_recommendation(
        &diagnosis,
        &latency_health,
        &graph_drift_health,
        leading_graph_drift_mode,
    )
    .to_string();
    let action_priority = quality_action_priority(
        &diagnosis,
        &latency_health,
        &graph_drift_health,
        leading_graph_drift_mode,
    )
    .to_string();
    let triage_path = quality_triage_path(&diagnosis, &action_recommendation).to_string();
    let latency_severity =
        latency_severity_with_history(history, &latest, trailing_opt.as_ref()).map(str::to_string);
    let latency_severity_reason =
        latency_severity_reason(&latest, trailing_opt.as_ref(), latency_severity.as_deref());
    let (latency_hotspot, latency_hotspot_bucket_id) = if latency_health == "stable" {
        (None, None)
    } else {
        top_latency_hotspot(report)
    };
    let latency_score_value = latency_score(&latest, trailing_opt.as_ref());
    let latency_score_delta_vs_trailing =
        latency_score_value - latency_score(trailing_opt.as_ref().unwrap_or(&latest), None);
    let latency_score_direction = Some(
        summarize_trend_direction(history, |entry| latency_score(entry, None)).to_string(),
    );
    let health =
        overall_health_marker(&latest, trailing_opt.as_ref(), &graph_drift_health).to_string();
    let trailing = trailing_opt.unwrap_or(QualityTrendEntry {
        timestamp_unix_ms: 0,
        fixture_count: latest.fixture_count,
        status: "trailing_avg".to_string(),
        regression_count: 0,
        threshold_failure_count: 0,
        retrieval_avg_latency_ms: latest.retrieval_avg_latency_ms,
        retrieval_p95_latency_ms: latest.retrieval_p95_latency_ms,
        retrieval_avg_tokens: latest.retrieval_avg_tokens,
        route_avg_latency_ms: latest.route_avg_latency_ms,
        route_p95_latency_ms: latest.route_p95_latency_ms,
        route_avg_tokens: latest.route_avg_tokens,
        mutation_scope_incomplete_rate: latest.mutation_scope_incomplete_rate,
        mutation_scope_missing_files_rate: latest.mutation_scope_missing_files_rate,
        mutation_scope_extra_files_rate: latest.mutation_scope_extra_files_rate,
        mutation_scope_missing_symbols_rate: latest.mutation_scope_missing_symbols_rate,
        mutation_scope_extra_symbols_rate: latest.mutation_scope_extra_symbols_rate,
        fixture_graph_drift: Vec::new(),
    });
    let (leading_graph_drift, graph_drift_trend, graph_drift_delta_pp) =
        graph_drift_snapshot_fields(report, &latest, &trailing);
    let (leading_graph_drift_fixture, graph_drift_fixture_trend) =
        graph_drift_fixture_snapshot_fields(report, history);
    let top_worsening_graph_drift_fixture =
        top_worsening_graph_drift_fixture_text(report, history);
    let graph_drift_severity = graph_drift_severity(
        graph_drift_trend.as_deref(),
        graph_drift_fixture_trend.as_deref(),
        top_worsening_graph_drift_fixture.as_deref(),
    );
    let graph_drift_severity_reason = graph_drift_severity_reason(
        graph_drift_severity.as_deref(),
        graph_drift_trend.as_deref(),
        graph_drift_fixture_trend.as_deref(),
        top_worsening_graph_drift_fixture.as_deref(),
        leading_graph_drift_mode,
    );
    let (graph_drift_hotspot, graph_drift_hotspot_bucket_id) = if graph_drift_health == "stable" {
        (None, None)
    } else {
        graph_drift_hotspot_fields(
            top_worsening_graph_drift_fixture.as_deref(),
            leading_graph_drift_fixture.as_deref(),
            leading_graph_drift.as_deref(),
        )
    };
    let graph_drift_score_value = graph_drift_score(&latest, &trailing);
    let graph_drift_score_delta_vs_trailing =
        graph_drift_score_value - graph_drift_score(&trailing, &trailing);
    let graph_drift_score_direction = Some(
        summarize_trend_direction(history, |entry| {
            graph_drift_score(entry, entry)
        })
        .to_string(),
    );
    let action_target = quality_action_target(
        &diagnosis,
        latency_hotspot.as_deref(),
        latency_severity_reason.as_deref(),
        top_worsening_graph_drift_fixture.as_deref(),
        graph_drift_severity_reason.as_deref(),
        leading_graph_drift_mode,
    );
    let action_checklist = quality_action_checklist(
        &diagnosis,
        &action_recommendation,
        action_target.as_deref(),
    );
    let action_commands = quality_action_commands(
        &diagnosis,
        &action_recommendation,
        action_target.as_deref(),
    );
    let action_primary_command = action_commands.first().cloned();
    let action_command_categories = action_commands
        .iter()
        .map(|command| quality_action_command_category(command).to_string())
        .collect::<Vec<_>>();
    let action_primary_command_category = action_commands
        .first()
        .map(|command| quality_action_command_category(command).to_string());
    let action_source_artifacts =
        quality_action_source_artifacts(&diagnosis, &action_recommendation);
    let summary_lookup_hint = quality_summary_lookup_hint(
        &action_recommendation,
        latency_hotspot_bucket_id.as_deref(),
        graph_drift_hotspot_bucket_id.as_deref(),
    );
    let summary_lookup_scope = quality_summary_lookup_scope(
        &action_recommendation,
        latency_hotspot_bucket_id.as_deref(),
        graph_drift_hotspot_bucket_id.as_deref(),
    );

    QualityTrendSnapshot {
        timestamp_unix_ms: latest.timestamp_unix_ms,
        status: latest.status,
        health,
        latency_health,
        graph_drift_health,
        diagnosis,
        action_recommendation,
        action_priority,
        triage_path,
        action_target,
        action_checklist,
        action_commands,
        action_primary_command,
        action_command_categories,
        action_primary_command_category,
        action_source_artifacts,
        latency_hotspot,
        graph_drift_hotspot,
        latency_hotspot_bucket_id,
        graph_drift_hotspot_bucket_id,
        summary_lookup_hint,
        summary_lookup_scope,
        latency_severity,
        latency_severity_reason,
        latency_score: latency_score_value,
        latency_score_delta_vs_trailing,
        latency_score_direction,
        regression_count: latest.regression_count,
        threshold_failure_count: latest.threshold_failure_count,
        fixture_count: latest.fixture_count,
        leading_graph_drift,
        leading_graph_drift_fixture,
        graph_drift_severity,
        graph_drift_severity_reason,
        graph_drift_score: graph_drift_score_value,
        graph_drift_score_delta_vs_trailing,
        graph_drift_score_direction,
        graph_drift_trend,
        graph_drift_fixture_trend,
        top_worsening_graph_drift_fixture,
        leading_graph_drift_delta_vs_trailing_pp: graph_drift_delta_pp,
        mutation_scope_incomplete_rate: latest.mutation_scope_incomplete_rate,
        retrieval_avg_latency_ms: latest.retrieval_avg_latency_ms,
        retrieval_p95_latency_ms: latest.retrieval_p95_latency_ms,
        route_avg_latency_ms: latest.route_avg_latency_ms,
        route_p95_latency_ms: latest.route_p95_latency_ms,
        retrieval_avg_latency_delta_vs_trailing:
            latest.retrieval_avg_latency_ms - trailing.retrieval_avg_latency_ms,
        retrieval_p95_latency_delta_vs_trailing:
            latest.retrieval_p95_latency_ms - trailing.retrieval_p95_latency_ms,
        route_avg_latency_delta_vs_trailing:
            latest.route_avg_latency_ms - trailing.route_avg_latency_ms,
        route_p95_latency_delta_vs_trailing:
            latest.route_p95_latency_ms - trailing.route_p95_latency_ms,
    }
}

fn graph_drift_score(latest: &QualityTrendEntry, trailing: &QualityTrendEntry) -> f64 {
    let latest_peak = [
        latest.mutation_scope_missing_files_rate,
        latest.mutation_scope_extra_files_rate,
        latest.mutation_scope_missing_symbols_rate,
        latest.mutation_scope_extra_symbols_rate,
    ]
    .into_iter()
    .fold(0.0, f64::max);
    let trailing_peak = [
        trailing.mutation_scope_missing_files_rate,
        trailing.mutation_scope_extra_files_rate,
        trailing.mutation_scope_missing_symbols_rate,
        trailing.mutation_scope_extra_symbols_rate,
    ]
    .into_iter()
    .fold(0.0, f64::max);
    ((latest_peak * 100.0) + ((latest_peak - trailing_peak).max(0.0) * 100.0)).max(0.0)
}

fn graph_drift_severity(
    graph_drift_trend: Option<&str>,
    graph_drift_fixture_trend: Option<&str>,
    top_worsening_graph_drift_fixture: Option<&str>,
) -> Option<String> {
    let combined = [
        graph_drift_trend,
        graph_drift_fixture_trend,
        top_worsening_graph_drift_fixture,
    ];
    if combined
        .iter()
        .flatten()
        .any(|value| value.contains("worsening"))
    {
        return Some("regressing".to_string());
    }
    if combined.iter().flatten().any(|value| value.contains("new")) {
        return Some("watch".to_string());
    }
    if combined
        .iter()
        .flatten()
        .any(|value| value.contains("improving") || value.contains("cleared"))
    {
        return Some("improving".to_string());
    }
    if combined.iter().flatten().any(|value| value.contains("flat")) {
        return Some("stable".to_string());
    }
    None
}

fn graph_drift_severity_reason(
    severity: Option<&str>,
    graph_drift_trend: Option<&str>,
    graph_drift_fixture_trend: Option<&str>,
    top_worsening_graph_drift_fixture: Option<&str>,
    leading_graph_drift_mode: Option<&str>,
) -> Option<String> {
    let base = match severity {
        Some("regressing") => top_worsening_graph_drift_fixture
            .map(|value| format!("top_fixture={value}"))
            .or_else(|| graph_drift_fixture_trend.map(|value| format!("fixture_trend={value}")))
            .or_else(|| graph_drift_trend.map(|value| format!("mode_trend={value}"))),
        Some("watch") => graph_drift_fixture_trend
            .map(|value| format!("fixture_trend={value}"))
            .or_else(|| graph_drift_trend.map(|value| format!("mode_trend={value}"))),
        Some("improving") => graph_drift_fixture_trend
            .map(|value| format!("fixture_trend={value}"))
            .or_else(|| graph_drift_trend.map(|value| format!("mode_trend={value}"))),
        Some("stable") => graph_drift_trend
            .map(|value| format!("mode_trend={value}"))
            .or_else(|| graph_drift_fixture_trend.map(|value| format!("fixture_trend={value}"))),
        _ => None,
    };
    if leading_graph_drift_mode == Some("incomplete") {
        return Some(match base {
            Some(reason) => format!("leading_mode=incomplete | {reason}"),
            None => "leading_mode=incomplete".to_string(),
        });
    }
    base
}

fn graph_drift_snapshot_fields(
    report: &QualityReport,
    latest: &QualityTrendEntry,
    trailing: &QualityTrendEntry,
) -> (Option<String>, Option<String>, f64) {
    let current = leading_graph_drift_from_report(report);
    let current_leader = leading_graph_drift_from_entry(latest);
    let trailing_leader = leading_graph_drift_from_entry(trailing);

    let Some((current_label, current_rate)) = current_leader else {
        let trend = trailing_leader.map(|(label, rate)| {
            format!("cleared {label} ({:+.0}pp vs trailing)", -rate * 100.0)
        });
        return (current, trend, 0.0);
    };

    let trailing_rate = graph_drift_rate_for_label(trailing, current_label);
    let delta = current_rate - trailing_rate;
    let trend = if trailing_rate <= EPSILON && current_rate > EPSILON {
        Some(format!(
            "new {current_label} ({:+.0}pp vs trailing)",
            delta * 100.0
        ))
    } else if delta > 0.01 {
        Some(format!(
            "{current_label} worsening ({:+.0}pp vs trailing)",
            delta * 100.0
        ))
    } else if delta < -0.01 {
        Some(format!(
            "{current_label} improving ({:+.0}pp vs trailing)",
            delta * 100.0
        ))
    } else {
        Some(format!(
            "{current_label} flat ({:+.0}pp vs trailing)",
            delta * 100.0
        ))
    };

    (current, trend, delta * 100.0)
}

fn graph_drift_fixture_snapshot_fields(
    report: &QualityReport,
    history: &[QualityTrendEntry],
) -> (Option<String>, Option<String>) {
    let Some((fixture, mode, rate)) = leading_graph_drift_fixture_from_report(report) else {
        let trailing = leading_fixture_graph_drift_from_history(history, 3);
        let trend = trailing.map(|entry| {
            format!(
                "cleared {} ({}, {:+.0}pp vs trailing)",
                entry.fixture,
                entry.leading_mode.unwrap_or_else(|| "unknown".to_string()),
                -entry.leading_rate * 100.0
            )
        });
        return (None, trend);
    };

    let trailing_rate = trailing_fixture_graph_drift_rate(history, &fixture, 3);
    let delta = (rate - trailing_rate) * 100.0;
    let fixture_line = Some(format!("{fixture} ({mode} {:.0}%)", rate * 100.0));
    let trend = if trailing_rate <= EPSILON && rate > EPSILON {
        Some(format!(
            "{fixture} new ({mode}, {:+.0}pp vs trailing)",
            delta
        ))
    } else if delta > 1.0 {
        Some(format!(
            "{fixture} worsening ({mode}, {:+.0}pp vs trailing)",
            delta
        ))
    } else if delta < -1.0 {
        Some(format!(
            "{fixture} improving ({mode}, {:+.0}pp vs trailing)",
            delta
        ))
    } else {
        Some(format!(
            "{fixture} flat ({mode}, {:+.0}pp vs trailing)",
            delta
        ))
    };
    (fixture_line, trend)
}

fn leading_graph_drift_from_report(report: &QualityReport) -> Option<String> {
    let mut leader: Option<(&str, f64, &str)> = None;
    for fixture in &report.fixtures {
        let Some(route) = fixture.route.as_ref() else {
            continue;
        };
        for (label, rate) in [
            ("missing_files", route.mutation_scope_missing_files_rate),
            ("extra_files", route.mutation_scope_extra_files_rate),
            ("missing_symbols", route.mutation_scope_missing_symbols_rate),
            ("extra_symbols", route.mutation_scope_extra_symbols_rate),
        ] {
            if rate <= EPSILON {
                continue;
            }
            match leader {
                Some((_, best_rate, _)) if rate <= best_rate => {}
                _ => leader = Some((label, rate, fixture.fixture.as_str())),
            }
        }
    }
    leader.map(|(label, rate, fixture)| format!("{label} ({:.0}% in {fixture})", rate * 100.0))
}

fn leading_graph_drift_fixture_from_report(
    report: &QualityReport,
) -> Option<(String, String, f64)> {
    let mut leader: Option<(String, String, f64)> = None;
    for fixture in &report.fixtures {
        let Some(route) = fixture.route.as_ref() else {
            continue;
        };
        if let Some((mode, rate)) = leading_mutation_scope_failure(route) {
            match leader {
                Some((_, _, best_rate)) if rate <= best_rate => {}
                _ => leader = Some((fixture.fixture.clone(), mode.to_string(), rate)),
            }
        }
    }
    leader
}

fn leading_graph_drift_from_entry(entry: &QualityTrendEntry) -> Option<(&'static str, f64)> {
    [
        ("missing_files", entry.mutation_scope_missing_files_rate),
        ("extra_files", entry.mutation_scope_extra_files_rate),
        ("missing_symbols", entry.mutation_scope_missing_symbols_rate),
        ("extra_symbols", entry.mutation_scope_extra_symbols_rate),
    ]
    .into_iter()
    .filter(|(_, rate)| *rate > GRAPH_DRIFT_REPORT_MIN_RATE)
    .max_by(|(_, left), (_, right)| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal))
}

fn graph_drift_rate_for_label(entry: &QualityTrendEntry, label: &str) -> f64 {
    match label {
        "missing_files" => entry.mutation_scope_missing_files_rate,
        "extra_files" => entry.mutation_scope_extra_files_rate,
        "missing_symbols" => entry.mutation_scope_missing_symbols_rate,
        "extra_symbols" => entry.mutation_scope_extra_symbols_rate,
        _ => 0.0,
    }
}

fn fixture_graph_drift_entries(report: &QualityReport) -> Vec<FixtureGraphDriftEntry> {
    report
        .fixtures
        .iter()
        .filter_map(|fixture| {
            let route = fixture.route.as_ref()?;
            if route.mutation_case_count == 0 {
                return None;
            }
            let (leading_mode, leading_rate) = leading_mutation_scope_failure(route)
                .map(|(mode, rate)| (Some(mode.to_string()), rate))
                .unwrap_or((None, 0.0));
            Some(FixtureGraphDriftEntry {
                fixture: fixture.fixture.clone(),
                leading_mode,
                leading_rate,
            })
        })
        .collect()
}

fn leading_fixture_graph_drift_from_history(
    history: &[QualityTrendEntry],
    window_size: usize,
) -> Option<FixtureGraphDriftEntry> {
    history
        .iter()
        .rev()
        .skip(1)
        .take(window_size)
        .flat_map(|entry| entry.fixture_graph_drift.iter())
        .filter(|entry| entry.leading_rate > GRAPH_DRIFT_REPORT_MIN_RATE)
        .cloned()
        .max_by(|left, right| {
            left.leading_rate
                .partial_cmp(&right.leading_rate)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
}

fn leading_nonzero_fixture_graph_drift_entry(
    entries: &[FixtureGraphDriftEntry],
) -> Option<FixtureGraphDriftEntry> {
    entries
        .iter()
        .filter(|entry| entry.leading_rate > GRAPH_DRIFT_REPORT_MIN_RATE)
        .cloned()
        .max_by(|left, right| {
            left.leading_rate
                .partial_cmp(&right.leading_rate)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
}

fn trailing_fixture_graph_drift_rate(
    history: &[QualityTrendEntry],
    fixture: &str,
    window_size: usize,
) -> f64 {
    let values: Vec<f64> = history
        .iter()
        .rev()
        .skip(1)
        .take(window_size)
        .filter_map(|entry| {
            entry
                .fixture_graph_drift
                .iter()
                .find(|item| item.fixture == fixture)
                .map(|item| item.leading_rate)
        })
        .collect();
    average_f64_slice(values)
}

fn render_summary_markdown(
    report: &QualityReport,
    baseline: Option<&QualityReport>,
    history: &[QualityTrendEntry],
) -> String {
    let mut lines = vec![
        "# Quality Report Summary".to_string(),
        String::new(),
        format!("- Generated by: `{}`", report.generated_by),
        format!("- Fixture count: {}", report.fixture_count),
        String::new(),
    ];

    append_recent_trend(&mut lines, report, history);

    if let Some(baseline) = baseline {
        append_baseline_overview(&mut lines, baseline, report);
    } else {
        lines.extend([
            "## Baseline Delta".to_string(),
            "- Baseline comparison: unavailable".to_string(),
            String::new(),
        ]);
    }

    lines.push("## Fixture Snapshot".to_string());

    for fixture in &report.fixtures {
        lines.push(format!("### {}", fixture.fixture));
        if let Some(retrieval) = &fixture.retrieval {
            lines.push(format!(
                "- Retrieval: cases={}, target={:.0}%, top-file={:.0}%, top-span={:.0}%, omission={:.2}, overfetch={:.2}, avg-latency={:.1}ms, p95={:.1}ms, avg-tokens={:.1}",
                retrieval.case_count,
                retrieval.target_match_rate * 100.0,
                retrieval.top_file_match_rate * 100.0,
                retrieval.top_span_match_rate * 100.0,
                retrieval.omission_rate,
                retrieval.overfetch_rate,
                retrieval.avg_latency_ms,
                retrieval.p95_latency_ms,
                retrieval.avg_tokens,
            ));
            if let Some(base_fixture) = baseline.and_then(|base| {
                base.fixtures
                    .iter()
                    .find(|candidate| candidate.fixture == fixture.fixture)
            }) {
                if let Some(base_retrieval) = &base_fixture.retrieval {
                    lines.push(format!(
                        "  delta: target {:+.0}pp, omission {:+.2}, avg-latency {:+.1}ms, p95 {:+.1}ms, avg-tokens {:+.1}",
                        pct_delta(retrieval.target_match_rate, base_retrieval.target_match_rate),
                        retrieval.omission_rate - base_retrieval.omission_rate,
                        retrieval.avg_latency_ms - base_retrieval.avg_latency_ms,
                        retrieval.p95_latency_ms - base_retrieval.p95_latency_ms,
                        retrieval.avg_tokens - base_retrieval.avg_tokens,
                    ));
                }
            }
            if !retrieval.operation_breakdown.is_empty() {
                lines.push("- Retrieval buckets:".to_string());
                for bucket in &retrieval.operation_breakdown {
                    lines.push(format!(
                        "  - `{}`: cases={}, target={:.0}%, avg-latency={:.1}ms, p95={:.1}ms, avg-tokens={:.1}",
                        bucket.operation,
                        bucket.case_count,
                        bucket.target_match_rate * 100.0,
                        bucket.avg_latency_ms,
                        bucket.p95_latency_ms,
                        bucket.avg_tokens,
                    ));
                }
            }
        }
        if let Some(route) = &fixture.route {
            lines.push(format!(
                "- Route: cases={}, reviewable={:.0}%, high-confidence={:.0}%, intent={:.0}%, tool={:.0}%, budget={:.0}%, result={:.0}%, avg-latency={:.1}ms, p95={:.1}ms, avg-tokens={:.1}",
                route.case_count,
                route.reviewable_or_better_rate * 100.0,
                route.high_confidence_rate * 100.0,
                route.intent_match_rate * 100.0,
                route.selected_tool_match_rate * 100.0,
                route.budget_match_rate * 100.0,
                route.result_symbol_match_rate * 100.0,
                route.avg_latency_ms,
                route.p95_latency_ms,
                route.avg_tokens,
            ));
            if route.mutation_case_count > 0 {
                lines.push(format!(
                    "  mutation-scope-bucket: {}",
                    mutation_scope_bucket_id(&fixture.fixture)
                ));
                lines.push(format!(
                    "  mutation-route-trust: cases={}, ready={:.0}%, exact-ready={:.0}%, ready-without-retry={:.0}%, scope-aligned={:.0}%, scope-incomplete={:.0}%, scope-missing-files={:.0}%, scope-extra-files={:.0}%, scope-missing-symbols={:.0}%, scope-extra-symbols={:.0}%, reviewable={:.0}%, high-confidence={:.0}%, retry-recovered={:.0}%, still-blocked={:.0}%",
                    route.mutation_case_count,
                    route.mutation_ready_rate * 100.0,
                    route.mutation_exact_ready_rate * 100.0,
                    route.mutation_ready_without_retry_rate * 100.0,
                    route.mutation_scope_aligned_rate * 100.0,
                    route.mutation_scope_incomplete_rate * 100.0,
                    route.mutation_scope_missing_files_rate * 100.0,
                    route.mutation_scope_extra_files_rate * 100.0,
                    route.mutation_scope_missing_symbols_rate * 100.0,
                    route.mutation_scope_extra_symbols_rate * 100.0,
                    route.mutation_reviewable_or_better_rate * 100.0,
                    route.mutation_high_confidence_rate * 100.0,
                    route.mutation_retry_recovered_rate * 100.0,
                    route.mutation_blocked_rate * 100.0,
                ));
                if let Some((label, rate)) = leading_mutation_scope_failure(route) {
                    lines.push(format!(
                        "  leading-graph-drift: {} ({:.0}% of mutation cases)",
                        label,
                        rate * 100.0
                    ));
                }
            }
            if let Some(base_fixture) = baseline.and_then(|base| {
                base.fixtures
                    .iter()
                    .find(|candidate| candidate.fixture == fixture.fixture)
            }) {
                if let Some(base_route) = &base_fixture.route {
                    lines.push(format!(
                        "  delta: reviewable {:+.0}pp, high-confidence {:+.0}pp, intent {:+.0}pp, tool {:+.0}pp, avg-latency {:+.1}ms, p95 {:+.1}ms, avg-tokens {:+.1}",
                        pct_delta(
                            route.reviewable_or_better_rate,
                            base_route.reviewable_or_better_rate
                        ),
                        pct_delta(route.high_confidence_rate, base_route.high_confidence_rate),
                        pct_delta(route.intent_match_rate, base_route.intent_match_rate),
                        pct_delta(
                            route.selected_tool_match_rate,
                            base_route.selected_tool_match_rate
                        ),
                        route.avg_latency_ms - base_route.avg_latency_ms,
                        route.p95_latency_ms - base_route.p95_latency_ms,
                        route.avg_tokens - base_route.avg_tokens,
                    ));
                    if route.mutation_case_count > 0 || base_route.mutation_case_count > 0 {
                        lines.push(format!(
                            "  mutation delta: ready {:+.0}pp, exact-ready {:+.0}pp, ready-without-retry {:+.0}pp, scope-aligned {:+.0}pp, incomplete {:+.0}pp, missing-files {:+.0}pp, extra-files {:+.0}pp, missing-symbols {:+.0}pp, extra-symbols {:+.0}pp, reviewable {:+.0}pp, high-confidence {:+.0}pp, retry-recovered {:+.0}pp, still-blocked {:+.0}pp",
                            pct_delta(route.mutation_ready_rate, base_route.mutation_ready_rate),
                            pct_delta(
                                route.mutation_exact_ready_rate,
                                base_route.mutation_exact_ready_rate
                            ),
                            pct_delta(
                                route.mutation_ready_without_retry_rate,
                                base_route.mutation_ready_without_retry_rate
                            ),
                            pct_delta(
                                route.mutation_scope_aligned_rate,
                                base_route.mutation_scope_aligned_rate
                            ),
                            pct_delta(
                                route.mutation_scope_incomplete_rate,
                                base_route.mutation_scope_incomplete_rate
                            ),
                            pct_delta(
                                route.mutation_scope_missing_files_rate,
                                base_route.mutation_scope_missing_files_rate
                            ),
                            pct_delta(
                                route.mutation_scope_extra_files_rate,
                                base_route.mutation_scope_extra_files_rate
                            ),
                            pct_delta(
                                route.mutation_scope_missing_symbols_rate,
                                base_route.mutation_scope_missing_symbols_rate
                            ),
                            pct_delta(
                                route.mutation_scope_extra_symbols_rate,
                                base_route.mutation_scope_extra_symbols_rate
                            ),
                            pct_delta(
                                route.mutation_reviewable_or_better_rate,
                                base_route.mutation_reviewable_or_better_rate
                            ),
                            pct_delta(
                                route.mutation_high_confidence_rate,
                                base_route.mutation_high_confidence_rate
                            ),
                            pct_delta(
                                route.mutation_retry_recovered_rate,
                                base_route.mutation_retry_recovered_rate
                            ),
                            pct_delta(route.mutation_blocked_rate, base_route.mutation_blocked_rate),
                        ));
                        if let Some(delta) = leading_mutation_scope_failure_delta(route, base_route) {
                            lines.push(format!("  graph-drift-delta: {delta}"));
                        }
                    }
                }
            }
            if !route.intent_breakdown.is_empty() {
                lines.push("- Route buckets:".to_string());
                for bucket in &route.intent_breakdown {
                    lines.push(format!(
                        "  - `{}`: cases={}, reviewable={:.0}%, high-confidence={:.0}%, intent={:.0}%, tool={:.0}%, avg-latency={:.1}ms, p95={:.1}ms, avg-tokens={:.1}",
                        bucket.intent,
                        bucket.case_count,
                        bucket.reviewable_or_better_rate * 100.0,
                        bucket.high_confidence_rate * 100.0,
                        bucket.intent_match_rate * 100.0,
                        bucket.selected_tool_match_rate * 100.0,
                        bucket.avg_latency_ms,
                        bucket.p95_latency_ms,
                        bucket.avg_tokens,
                    ));
                }
            }
        }
        lines.push(String::new());
    }

    append_worst_case_section(
        &mut lines,
        "Worst Retrieval Latency",
        &report.worst_retrieval_latency,
    );
    append_worst_case_section(
        &mut lines,
        "Worst Retrieval Tokens",
        &report.worst_retrieval_tokens,
    );
    append_worst_case_section(&mut lines, "Worst Route Latency", &report.worst_route_latency);
    append_worst_case_section(&mut lines, "Worst Route Tokens", &report.worst_route_tokens);

    lines.join("\n")
}

fn append_recent_trend(
    lines: &mut Vec<String>,
    report: &QualityReport,
    history: &[QualityTrendEntry],
) {
    lines.push("## Recent Trend".to_string());
    if history.is_empty() {
        lines.push("- History: unavailable".to_string());
        lines.push(String::new());
        return;
    }

    if let Some(latest) = history.last() {
        let trailing = trailing_average_entry(history, 3);
        let trailing_ref = trailing.as_ref().unwrap_or(latest);
        lines.push(format!(
            "- Latest status: `{}` with {} regression(s) and {} threshold failure(s)",
            latest.status, latest.regression_count, latest.threshold_failure_count
        ));
        lines.push(format!(
            "- Latest health: `{}`",
            overall_health_marker(
                latest,
                trailing.as_ref(),
                graph_drift_health_marker(latest, trailing_ref)
            )
        ));
        lines.push(format!(
            "- Health split: latency=`{}`, graph_drift=`{}`",
            latency_health_marker(latest, trailing.as_ref()),
            graph_drift_health_marker(latest, trailing_ref),
        ));
        lines.push(format!(
            "- Latest aggregates: retrieval avg={:.1}ms p95={:.1}ms tokens={:.1}; route avg={:.1}ms p95={:.1}ms tokens={:.1}",
            latest.retrieval_avg_latency_ms,
            latest.retrieval_p95_latency_ms,
            latest.retrieval_avg_tokens,
            latest.route_avg_latency_ms,
            latest.route_p95_latency_ms,
            latest.route_avg_tokens,
        ));
        if let Some(previous) = trailing.as_ref() {
            lines.push(format!(
                "- Latest vs trailing avg(3): retrieval avg {:+.1}ms, retrieval p95 {:+.1}ms, route avg {:+.1}ms, route p95 {:+.1}ms",
                latest.retrieval_avg_latency_ms - previous.retrieval_avg_latency_ms,
                latest.retrieval_p95_latency_ms - previous.retrieval_p95_latency_ms,
                latest.route_avg_latency_ms - previous.route_avg_latency_ms,
                latest.route_p95_latency_ms - previous.route_p95_latency_ms,
            ));
            let trailing_graph_drift_score = graph_drift_score(previous, previous);
            let latest_graph_drift_score = graph_drift_score(latest, latest);
            if latest_graph_drift_score > 0.0 || trailing_graph_drift_score > 0.0 {
                lines.push(format!(
                    "- Latest graph drift score vs trailing avg(3): {:.1} ({:+.1})",
                    latest_graph_drift_score,
                    latest_graph_drift_score - trailing_graph_drift_score,
                ));
            }
            if let Some((label, rate)) = leading_graph_drift_from_entry(latest) {
                let delta = (rate - graph_drift_rate_for_label(previous, label)) * 100.0;
                lines.push(format!(
                    "- Latest graph drift vs trailing avg(3): {} at {:.0}% ({:+.0}pp)",
                    label,
                    rate * 100.0,
                    delta,
                ));
            } else if let Some((label, rate)) = leading_graph_drift_from_entry(previous) {
                lines.push(format!(
                    "- Latest graph drift vs trailing avg(3): cleared {} ({:+.0}pp)",
                    label,
                    -rate * 100.0,
                ));
            }
            if let Some(entry) = leading_nonzero_fixture_graph_drift_entry(&latest.fixture_graph_drift)
            {
                let delta =
                    (entry.leading_rate - trailing_fixture_graph_drift_rate(history, &entry.fixture, 3))
                        * 100.0;
                lines.push(format!(
                    "- Latest graph drift fixture vs trailing avg(3): {} ({}, {:.0}%, {:+.0}pp)",
                    entry.fixture,
                    entry.leading_mode.unwrap_or_else(|| "unknown".to_string()),
                    entry.leading_rate * 100.0,
                    delta,
                ));
            } else if let Some(entry) = leading_fixture_graph_drift_from_history(history, 3) {
                lines.push(format!(
                    "- Latest graph drift fixture vs trailing avg(3): cleared {} ({}, {:+.0}pp)",
                    entry.fixture,
                    entry.leading_mode.unwrap_or_else(|| "unknown".to_string()),
                    -entry.leading_rate * 100.0,
                ));
            }
        }
        lines.push(format!(
            "- 5-run direction: retrieval avg {}, retrieval p95 {}, route avg {}, route p95 {}",
            summarize_trend_direction(history, |entry| entry.retrieval_avg_latency_ms),
            summarize_trend_direction(history, |entry| entry.retrieval_p95_latency_ms),
            summarize_trend_direction(history, |entry| entry.route_avg_latency_ms),
            summarize_trend_direction(history, |entry| entry.route_p95_latency_ms),
        ));
        lines.push(format!(
            "- 5-run graph drift: incomplete {}, missing_files {}, extra_files {}, missing_symbols {}, extra_symbols {}",
            summarize_trend_direction(history, |entry| entry.mutation_scope_incomplete_rate * 100.0),
            summarize_trend_direction(history, |entry| entry.mutation_scope_missing_files_rate * 100.0),
            summarize_trend_direction(history, |entry| entry.mutation_scope_extra_files_rate * 100.0),
            summarize_trend_direction(history, |entry| entry.mutation_scope_missing_symbols_rate * 100.0),
            summarize_trend_direction(history, |entry| entry.mutation_scope_extra_symbols_rate * 100.0),
        ));
        lines.push(format!(
            "- 5-run graph drift score: {}",
            summarize_trend_direction(history, |entry| graph_drift_score(entry, entry))
        ));
        if let (Some(fixture), Some(trend)) = top_worsening_graph_drift_fixture(report, history) {
            lines.push(format!(
                "- Top worsening graph-drift fixture: {} ({})",
                fixture, trend
            ));
        }
    }

    for entry in history.iter().rev().take(5) {
        lines.push(format!(
            "- Run {}: status=`{}`, retrieval avg={:.1}ms/p95={:.1}ms, route avg={:.1}ms/p95={:.1}ms",
            entry.timestamp_unix_ms,
            entry.status,
            entry.retrieval_avg_latency_ms,
            entry.retrieval_p95_latency_ms,
            entry.route_avg_latency_ms,
            entry.route_p95_latency_ms,
        ));
    }
    lines.push(String::new());
}

fn top_worsening_graph_drift_fixture(
    report: &QualityReport,
    history: &[QualityTrendEntry],
) -> (Option<String>, Option<String>) {
    let (fixture, trend) = graph_drift_fixture_snapshot_fields(report, history);
    let Some(trend_text) = trend else {
        return (None, None);
    };
    if !(trend_text.contains("worsening") || trend_text.contains("new")) {
        return (None, None);
    }
    (fixture, Some(trend_text))
}

fn top_worsening_graph_drift_fixture_text(
    report: &QualityReport,
    history: &[QualityTrendEntry],
) -> Option<String> {
    let (fixture, trend) = top_worsening_graph_drift_fixture(report, history);
    match (fixture, trend) {
        (Some(fixture), Some(trend)) => Some(format!("{fixture} ({trend})")),
        _ => None,
    }
}

fn overall_health_marker(
    latest: &QualityTrendEntry,
    trailing: Option<&QualityTrendEntry>,
    graph_drift_health: &str,
) -> &'static str {
    if latest.status != "clean" {
        return "drifting";
    }
    let latency = latency_health_marker(latest, trailing);
    if latency == "drifting" || graph_drift_health == "drifting" {
        "drifting"
    } else if latency == "watch" || graph_drift_health == "watch" {
        "watch"
    } else {
        "stable"
    }
}

fn quality_diagnosis(latency_health: &str, graph_drift_health: &str) -> &'static str {
    let latency_bad = matches!(latency_health, "watch" | "drifting");
    let graph_bad = matches!(graph_drift_health, "watch" | "drifting");
    match (latency_bad, graph_bad) {
        (false, false) => "clean",
        (true, false) => "latency_only_drift",
        (false, true) => "graph_only_drift",
        (true, true) => "mixed_drift",
    }
}

fn quality_action_recommendation(
    diagnosis: &str,
    latency_health: &str,
    graph_drift_health: &str,
    leading_graph_drift_mode: Option<&str>,
) -> &'static str {
    match diagnosis {
        "clean" => "no_action",
        "latency_only_drift" => {
            if latency_health == "drifting" {
                "investigate_latency_regression"
            } else {
                "watch_latency"
            }
        }
        "graph_only_drift" => {
            if leading_graph_drift_mode == Some("incomplete") {
                if graph_drift_health == "drifting" {
                    "inspect_incomplete_mutation_scope"
                } else {
                    "watch_incomplete_mutation_scope"
                }
            } else if graph_drift_health == "drifting" {
                "inspect_graph_drift"
            } else {
                "watch_graph_drift"
            }
        }
        "mixed_drift" => {
            if leading_graph_drift_mode == Some("incomplete") {
                "investigate_mixed_incomplete_mutation_scope"
            } else {
                "investigate_mixed_regression"
            }
        }
        _ => "review_quality_status",
    }
}

fn quality_action_priority(
    diagnosis: &str,
    latency_health: &str,
    graph_drift_health: &str,
    leading_graph_drift_mode: Option<&str>,
) -> &'static str {
    match diagnosis {
        "clean" => "none",
        "mixed_drift" => "high",
        "graph_only_drift" => {
            if leading_graph_drift_mode == Some("incomplete") {
                if graph_drift_health == "drifting" {
                    "high"
                } else {
                    "medium"
                }
            } else if graph_drift_health == "drifting" {
                "high"
            } else {
                "medium"
            }
        }
        "latency_only_drift" => {
            if latency_health == "drifting" {
                "medium"
            } else {
                "low"
            }
        }
        _ => "medium",
    }
}

fn quality_triage_path(diagnosis: &str, action_recommendation: &str) -> &'static str {
    match diagnosis {
        "clean" => "none",
        "latency_only_drift" => {
            if action_recommendation == "investigate_latency_regression" {
                "investigate"
            } else {
                "refresh"
            }
        }
        "graph_only_drift" => {
            if matches!(
                action_recommendation,
                "inspect_graph_drift" | "inspect_incomplete_mutation_scope"
            ) {
                "inspect"
            } else {
                "refresh"
            }
        }
        "mixed_drift" => "investigate",
        _ => "inspect",
    }
}

fn quality_action_target(
    diagnosis: &str,
    latency_hotspot: Option<&str>,
    latency_severity_reason: Option<&str>,
    top_worsening_graph_drift_fixture: Option<&str>,
    graph_drift_severity_reason: Option<&str>,
    leading_graph_drift_mode: Option<&str>,
) -> Option<String> {
    match diagnosis {
        "latency_only_drift" => latency_hotspot
            .map(|hotspot| match latency_severity_reason {
                Some(reason) => format!("{hotspot} | {reason}"),
                None => hotspot.to_string(),
            })
            .or_else(|| latency_severity_reason.map(|reason| reason.to_string())),
        "graph_only_drift" => top_worsening_graph_drift_fixture
            .map(|value| value.to_string())
            .or_else(|| graph_drift_severity_reason.map(|reason| reason.to_string())),
        "mixed_drift" => {
            if leading_graph_drift_mode == Some("incomplete") {
                top_worsening_graph_drift_fixture
                    .map(|value| match latency_hotspot {
                        Some(hotspot) => format!("incomplete_graph={value} | latency={hotspot}"),
                        None => format!("incomplete_graph={value}"),
                    })
                    .or_else(|| {
                        graph_drift_severity_reason.map(|reason| match latency_severity_reason {
                            Some(latency_reason) => {
                                format!("incomplete_graph={reason} | latency={latency_reason}")
                            }
                            None => format!("incomplete_graph={reason}"),
                        })
                    })
                    .or_else(|| {
                        latency_hotspot.map(|hotspot| match latency_severity_reason {
                            Some(reason) => format!("{hotspot} | {reason}"),
                            None => hotspot.to_string(),
                        })
                    })
            } else {
                top_worsening_graph_drift_fixture
                    .map(|value| format!("graph={value}"))
                    .or_else(|| {
                        latency_hotspot.map(|hotspot| match latency_severity_reason {
                            Some(reason) => format!("latency={hotspot} | {reason}"),
                            None => format!("latency={hotspot}"),
                        })
                    })
            }
            .or_else(|| graph_drift_severity_reason.map(|reason| format!("graph={reason}")))
            .or_else(|| latency_severity_reason.map(|reason| format!("latency={reason}")))
        }
        _ => None,
    }
}

fn top_latency_hotspot(report: &QualityReport) -> (Option<String>, Option<String>) {
    let route = report.worst_route_latency.first();
    let retrieval = report.worst_retrieval_latency.first();
    let winner = match (route, retrieval) {
        (Some(route), Some(retrieval)) => {
            if route.latency_ms >= retrieval.latency_ms {
                route
            } else {
                retrieval
            }
        }
        (Some(route), None) => route,
        (None, Some(retrieval)) => retrieval,
        (None, None) => return (None, None),
    };
    (
        Some(format!(
            "{}/{}/{}:{} ({:.2}ms)",
            winner.fixture, winner.section, winner.bucket, winner.case_name, winner.latency_ms
        )),
        Some(format!(
            "{}/{}/{}",
            winner.fixture, winner.section, winner.bucket
        )),
    )
}

fn graph_drift_hotspot_fields(
    top_worsening_graph_drift_fixture: Option<&str>,
    leading_graph_drift_fixture: Option<&str>,
    leading_graph_drift: Option<&str>,
) -> (Option<String>, Option<String>) {
    if let Some(text) = top_worsening_graph_drift_fixture {
        return (
            Some(text.to_string()),
            Some(sanitize_graph_drift_bucket_id(text)),
        );
    }
    if let Some(text) = leading_graph_drift_fixture {
        return (
            Some(text.to_string()),
            Some(sanitize_graph_drift_bucket_id(text)),
        );
    }
    if let Some(text) = leading_graph_drift {
        return (
            Some(text.to_string()),
            Some(sanitize_graph_drift_bucket_id(text)),
        );
    }
    (None, None)
}

fn sanitize_graph_drift_bucket_id(text: &str) -> String {
    text.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '/' | '_' | '-') {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn mutation_scope_bucket_id(fixture: &str) -> String {
    sanitize_graph_drift_bucket_id(&format!("{fixture}__mutation_scope"))
}

fn quality_summary_lookup_hint(
    action_recommendation: &str,
    latency_hotspot_bucket_id: Option<&str>,
    graph_drift_hotspot_bucket_id: Option<&str>,
) -> Option<String> {
    if matches!(
        action_recommendation,
        "inspect_incomplete_mutation_scope"
            | "watch_incomplete_mutation_scope"
            | "investigate_mixed_incomplete_mutation_scope"
    ) {
        if let Some(bucket_id) = graph_drift_hotspot_bucket_id {
            let fixture = bucket_id.split("__").next().unwrap_or(bucket_id);
            return Some(format!(
                "search quality_report_summary.md for `mutation-scope-bucket: {}`",
                mutation_scope_bucket_id(fixture)
            ));
        }
    }
    if let Some(bucket_id) = latency_hotspot_bucket_id {
        return Some(format!("search quality_report_summary.md for `{bucket_id}`"));
    }
    if let Some(bucket_id) = graph_drift_hotspot_bucket_id {
        return Some(format!("search quality_report_summary.md for `{bucket_id}`"));
    }
    None
}

fn quality_summary_lookup_scope(
    action_recommendation: &str,
    latency_hotspot_bucket_id: Option<&str>,
    graph_drift_hotspot_bucket_id: Option<&str>,
) -> Option<String> {
    if matches!(
        action_recommendation,
        "inspect_incomplete_mutation_scope"
            | "watch_incomplete_mutation_scope"
            | "investigate_mixed_incomplete_mutation_scope"
    ) && graph_drift_hotspot_bucket_id.is_some()
    {
        return Some("mutation_scope_bucket".to_string());
    }
    if latency_hotspot_bucket_id.is_some() {
        return Some("latency_bucket".to_string());
    }
    if graph_drift_hotspot_bucket_id.is_some() {
        return Some("graph_drift_bucket".to_string());
    }
    None
}

fn quality_action_checklist(
    diagnosis: &str,
    action_recommendation: &str,
    action_target: Option<&str>,
) -> Vec<String> {
    match diagnosis {
        "clean" => Vec::new(),
        "latency_only_drift" => {
            let mut items = vec![
                "rerun scripts/export-quality-report.ps1 to confirm the latency signal".to_string(),
                "inspect docs/doc_ignore/quality_report_summary.md recent trend for latency deltas"
                    .to_string(),
            ];
            if let Some(target) = action_target {
                items.push(format!("inspect latency target: {target}"));
            }
            if action_recommendation == "investigate_latency_regression" {
                items.push(
                    "compare current worst latency buckets against the baseline snapshot".to_string(),
                );
            }
            items
        }
        "graph_only_drift" => {
            let mut items = if matches!(
                action_recommendation,
                "inspect_incomplete_mutation_scope" | "watch_incomplete_mutation_scope"
            ) {
                vec![
                    "inspect docs/doc_ignore/quality_report_summary.md for incomplete mutation neighborhood coverage".to_string(),
                    "check mutation_scope_incomplete_rate and leading_graph_drift in quality_report_trend_snapshot.json"
                        .to_string(),
                ]
            } else {
                vec![
                    "inspect docs/doc_ignore/quality_report_summary.md for graph drift details".to_string(),
                    "check the latest leading_graph_drift and fixture trend in quality_report_trend_snapshot.json"
                        .to_string(),
                ]
            };
            if let Some(target) = action_target {
                if matches!(
                    action_recommendation,
                    "inspect_incomplete_mutation_scope" | "watch_incomplete_mutation_scope"
                ) {
                    items.push(format!("inspect incomplete mutation target: {target}"));
                } else {
                    items.push(format!("inspect graph drift target: {target}"));
                }
            }
            items
        }
        "mixed_drift" => {
            let mut items = if action_recommendation == "investigate_mixed_incomplete_mutation_scope" {
                vec![
                    "rerun scripts/export-quality-report.ps1 to confirm mixed latency and incomplete mutation drift".to_string(),
                    "inspect docs/doc_ignore/quality_report_summary.md for incomplete mutation coverage first, then review latency deltas".to_string(),
                    "check mutation_scope_incomplete_rate, leading_graph_drift, and route latency deltas in quality_report_trend_snapshot.json".to_string(),
                ]
            } else {
                vec![
                    "rerun scripts/export-quality-report.ps1 to confirm mixed drift".to_string(),
                    "inspect docs/doc_ignore/quality_report_summary.md for both latency and graph drift sections"
                        .to_string(),
                ]
            };
            if let Some(target) = action_target {
                items.push(format!("inspect primary target: {target}"));
            }
            items
        }
        _ => vec!["review docs/doc_ignore/quality_report_summary.md".to_string()],
    }
}

fn quality_action_commands(
    diagnosis: &str,
    action_recommendation: &str,
    action_target: Option<&str>,
) -> Vec<String> {
    match diagnosis {
        "clean" => Vec::new(),
        "latency_only_drift" => {
            let mut commands = vec![
                "powershell -ExecutionPolicy Bypass -File scripts/export-quality-report.ps1"
                    .to_string(),
                "semantic --repo . status --quality --output json".to_string(),
            ];
            if action_recommendation == "investigate_latency_regression" {
                commands.push(
                    r#"Get-Content docs/doc_ignore/quality_report_summary.md | Select-Object -First 80"#
                        .to_string(),
                );
            }
            if let Some(target) = action_target {
                commands.push(format!(r#"echo inspect_target="{target}""#));
            }
            commands
        }
        "graph_only_drift" => {
            let mut commands = vec![
                "powershell -ExecutionPolicy Bypass -File scripts/export-quality-report.ps1"
                    .to_string(),
                "semantic --repo . status --quality --output json".to_string(),
            ];
            if matches!(
                action_recommendation,
                "inspect_incomplete_mutation_scope" | "watch_incomplete_mutation_scope"
            ) {
                commands.push(
                    r#"Select-String -Path docs/doc_ignore/quality_report_summary.md -Pattern "scope-incomplete|leading-graph-drift""#
                        .to_string(),
                );
            } else {
                commands.push(
                    r#"Get-Content docs/doc_ignore/quality_report_summary.md | Select-Object -First 120"#
                        .to_string(),
                );
            }
            if let Some(target) = action_target {
                commands.push(format!(r#"echo inspect_target="{target}""#));
            }
            commands
        }
        "mixed_drift" => {
            let mut commands = vec![
                "powershell -ExecutionPolicy Bypass -File scripts/export-quality-report.ps1"
                    .to_string(),
                "semantic --repo . status --quality --output json".to_string(),
            ];
            if action_recommendation == "investigate_mixed_incomplete_mutation_scope" {
                commands.push(
                    r#"Select-String -Path docs/doc_ignore/quality_report_summary.md -Pattern "scope-incomplete|leading-graph-drift|Latest vs trailing avg\(3\)"#
                        .to_string(),
                );
            } else {
                commands.push(
                    r#"Get-Content docs/doc_ignore/quality_report_summary.md | Select-Object -First 140"#
                        .to_string(),
                );
            }
            if let Some(target) = action_target {
                commands.push(format!(r#"echo inspect_target="{target}""#));
            }
            commands
        }
        _ => vec![
            "semantic --repo . status --quality --output json".to_string(),
        ],
    }
}

fn quality_action_command_category(command: &str) -> &'static str {
    if command.contains("export-quality-report.ps1") {
        "export"
    } else if command.contains("status --quality") {
        "status_refresh"
    } else if command.contains("quality_report_summary.md") {
        "summary_inspect"
    } else if command.contains("inspect_target=") {
        "target_hint"
    } else {
        "inspect"
    }
}

fn quality_action_source_artifacts(
    diagnosis: &str,
    action_recommendation: &str,
) -> Vec<String> {
    match diagnosis {
        "clean" => Vec::new(),
        "latency_only_drift" => {
            let mut artifacts = vec![
                "docs/doc_ignore/quality_report_summary.md".to_string(),
                "docs/doc_ignore/quality_report_trend_snapshot.json".to_string(),
            ];
            if action_recommendation == "investigate_latency_regression" {
                artifacts.push("docs/doc_ignore/quality_report_baseline.json".to_string());
            }
            artifacts
        }
        "graph_only_drift" => {
            if matches!(
                action_recommendation,
                "inspect_incomplete_mutation_scope" | "watch_incomplete_mutation_scope"
            ) {
                vec![
                    "docs/doc_ignore/quality_report_summary.md".to_string(),
                    "docs/doc_ignore/quality_report_trend_snapshot.json".to_string(),
                    "docs/doc_ignore/quality_report_history.json".to_string(),
                    "docs/doc_ignore/quality_report.json".to_string(),
                ]
            } else {
                vec![
                    "docs/doc_ignore/quality_report_summary.md".to_string(),
                    "docs/doc_ignore/quality_report_trend_snapshot.json".to_string(),
                    "docs/doc_ignore/quality_report_history.json".to_string(),
                ]
            }
        }
        "mixed_drift" => {
            if action_recommendation == "investigate_mixed_incomplete_mutation_scope" {
                vec![
                    "docs/doc_ignore/quality_report_summary.md".to_string(),
                    "docs/doc_ignore/quality_report_trend_snapshot.json".to_string(),
                    "docs/doc_ignore/quality_report_history.json".to_string(),
                    "docs/doc_ignore/quality_report.json".to_string(),
                    "docs/doc_ignore/quality_report_baseline.json".to_string(),
                ]
            } else {
                vec![
                    "docs/doc_ignore/quality_report_summary.md".to_string(),
                    "docs/doc_ignore/quality_report_trend_snapshot.json".to_string(),
                    "docs/doc_ignore/quality_report_history.json".to_string(),
                    "docs/doc_ignore/quality_report_baseline.json".to_string(),
                ]
            }
        }
        _ => vec!["docs/doc_ignore/quality_report_summary.md".to_string()],
    }
}

fn latency_health_marker(
    latest: &QualityTrendEntry,
    trailing: Option<&QualityTrendEntry>,
) -> &'static str {
    if latest.status != "clean" {
        return "drifting";
    }
    let Some(trailing) = trailing else {
        return "stable";
    };
    let retrieval_p95_delta = latest.retrieval_p95_latency_ms - trailing.retrieval_p95_latency_ms;
    let route_p95_delta = latest.route_p95_latency_ms - trailing.route_p95_latency_ms;
    let retrieval_avg_delta = latest.retrieval_avg_latency_ms - trailing.retrieval_avg_latency_ms;
    let route_avg_delta = latest.route_avg_latency_ms - trailing.route_avg_latency_ms;

    let worst_tail_delta = retrieval_p95_delta.max(route_p95_delta);
    let worst_avg_delta = retrieval_avg_delta.max(route_avg_delta);
    let worst_tail_abs = latest.retrieval_p95_latency_ms.max(latest.route_p95_latency_ms);
    let worst_avg_abs = latest.retrieval_avg_latency_ms.max(latest.route_avg_latency_ms);
    let meaningful_absolute_pressure = worst_tail_abs > LATENCY_HEALTH_MIN_ABS_P95_MS
        || worst_avg_abs > LATENCY_HEALTH_MIN_ABS_AVG_MS;

    if !meaningful_absolute_pressure {
        return "stable";
    }

    if worst_tail_delta > LATENCY_HEALTH_DRIFT_P95_DELTA_MS
        || worst_avg_delta > LATENCY_HEALTH_DRIFT_AVG_DELTA_MS
    {
        "drifting"
    } else if worst_tail_delta > LATENCY_HEALTH_WATCH_P95_DELTA_MS
        || worst_avg_delta > LATENCY_HEALTH_WATCH_AVG_DELTA_MS
    {
        "watch"
    } else {
        "stable"
    }
}

fn latency_health_with_history(
    history: &[QualityTrendEntry],
    latest: &QualityTrendEntry,
    trailing: Option<&QualityTrendEntry>,
) -> &'static str {
    let health = latency_health_marker(latest, trailing);
    if health != "drifting" || latest.status != "clean" || latency_pressure_is_persistent(history) {
        return health;
    }
    "watch"
}

fn latency_severity_marker(
    latest: &QualityTrendEntry,
    trailing: Option<&QualityTrendEntry>,
) -> Option<&'static str> {
    let Some(trailing) = trailing else {
        return None;
    };
    if latest.status != "clean" {
        return Some("regressing");
    }
    let retrieval_p95_delta = latest.retrieval_p95_latency_ms - trailing.retrieval_p95_latency_ms;
    let route_p95_delta = latest.route_p95_latency_ms - trailing.route_p95_latency_ms;
    let retrieval_avg_delta = latest.retrieval_avg_latency_ms - trailing.retrieval_avg_latency_ms;
    let route_avg_delta = latest.route_avg_latency_ms - trailing.route_avg_latency_ms;
    let worst_tail_delta = retrieval_p95_delta.max(route_p95_delta);
    let worst_avg_delta = retrieval_avg_delta.max(route_avg_delta);
    let worst_tail_abs = latest.retrieval_p95_latency_ms.max(latest.route_p95_latency_ms);
    let worst_avg_abs = latest.retrieval_avg_latency_ms.max(latest.route_avg_latency_ms);
    let meaningful_absolute_pressure = worst_tail_abs > LATENCY_HEALTH_MIN_ABS_P95_MS
        || worst_avg_abs > LATENCY_HEALTH_MIN_ABS_AVG_MS;

    if !meaningful_absolute_pressure {
        return None;
    }

    if worst_tail_delta > LATENCY_HEALTH_DRIFT_P95_DELTA_MS
        || worst_avg_delta > LATENCY_HEALTH_DRIFT_AVG_DELTA_MS
    {
        Some("regressing")
    } else if worst_tail_delta > LATENCY_HEALTH_WATCH_P95_DELTA_MS
        || worst_avg_delta > LATENCY_HEALTH_WATCH_AVG_DELTA_MS
    {
        Some("watch")
    } else if worst_tail_delta < -LATENCY_HEALTH_WATCH_P95_DELTA_MS
        || worst_avg_delta < -LATENCY_HEALTH_WATCH_AVG_DELTA_MS
    {
        Some("improving")
    } else {
        None
    }
}

fn latency_severity_with_history(
    history: &[QualityTrendEntry],
    latest: &QualityTrendEntry,
    trailing: Option<&QualityTrendEntry>,
) -> Option<&'static str> {
    let severity = latency_severity_marker(latest, trailing);
    if severity != Some("regressing")
        || latest.status != "clean"
        || latency_pressure_is_persistent(history)
    {
        return severity;
    }
    Some("watch")
}

fn latency_severity_reason(
    latest: &QualityTrendEntry,
    trailing: Option<&QualityTrendEntry>,
    severity: Option<&str>,
) -> Option<String> {
    let Some(trailing) = trailing else {
        return None;
    };
    let retrieval_p95_delta = latest.retrieval_p95_latency_ms - trailing.retrieval_p95_latency_ms;
    let route_p95_delta = latest.route_p95_latency_ms - trailing.route_p95_latency_ms;
    let retrieval_avg_delta = latest.retrieval_avg_latency_ms - trailing.retrieval_avg_latency_ms;
    let route_avg_delta = latest.route_avg_latency_ms - trailing.route_avg_latency_ms;

    let (metric, delta) = [
        ("retrieval_p95", retrieval_p95_delta),
        ("route_p95", route_p95_delta),
        ("retrieval_avg", retrieval_avg_delta),
        ("route_avg", route_avg_delta),
    ]
    .into_iter()
    .max_by(|(_, left), (_, right)| {
        left.abs()
            .partial_cmp(&right.abs())
            .unwrap_or(std::cmp::Ordering::Equal)
    })
    .unwrap_or(("none", 0.0));

    severity.map(|label| format!("{label}:{metric}={:+.1}ms_vs_trailing", delta))
}

fn latency_score(latest: &QualityTrendEntry, trailing: Option<&QualityTrendEntry>) -> f64 {
    let baseline = trailing.unwrap_or(latest);
    let retrieval_p95_delta =
        (latest.retrieval_p95_latency_ms - baseline.retrieval_p95_latency_ms).max(0.0);
    let route_p95_delta = (latest.route_p95_latency_ms - baseline.route_p95_latency_ms).max(0.0);
    let retrieval_avg_delta =
        (latest.retrieval_avg_latency_ms - baseline.retrieval_avg_latency_ms).max(0.0);
    let route_avg_delta = (latest.route_avg_latency_ms - baseline.route_avg_latency_ms).max(0.0);

    retrieval_p95_delta + route_p95_delta + (retrieval_avg_delta * 0.5) + (route_avg_delta * 0.5)
}

fn latency_pressure_is_persistent(history: &[QualityTrendEntry]) -> bool {
    if history.len() < 3 {
        return false;
    }
    let previous = &history[history.len() - 2];
    let prior_history = &history[..history.len() - 1];
    let previous_trailing = trailing_average_entry(prior_history, 3);
    matches!(
        latency_health_marker(previous, previous_trailing.as_ref()),
        "watch" | "drifting"
    )
}

fn graph_drift_health_marker(
    latest: &QualityTrendEntry,
    trailing: &QualityTrendEntry,
) -> &'static str {
    let score = graph_drift_score(latest, trailing);
    let delta = score - graph_drift_score(trailing, trailing);
    if score > 15.0 || delta > 10.0 {
        "drifting"
    } else if score > 0.0 || delta > 0.0 {
        "watch"
    } else {
        "stable"
    }
}

fn trailing_average_entry(
    history: &[QualityTrendEntry],
    window_size: usize,
) -> Option<QualityTrendEntry> {
    if history.len() < 2 {
        return None;
    }
    let trailing: Vec<&QualityTrendEntry> = history
        .iter()
        .rev()
        .skip(1)
        .take(window_size)
        .collect();
    if trailing.is_empty() {
        return None;
    }
    Some(QualityTrendEntry {
        timestamp_unix_ms: 0,
        fixture_count: trailing.last().map(|entry| entry.fixture_count).unwrap_or_default(),
        status: "trailing_avg".to_string(),
        regression_count: 0,
        threshold_failure_count: 0,
        retrieval_avg_latency_ms: average_f64_slice(
            trailing
                .iter()
                .map(|entry| entry.retrieval_avg_latency_ms)
                .collect(),
        ),
        retrieval_p95_latency_ms: average_f64_slice(
            trailing
                .iter()
                .map(|entry| entry.retrieval_p95_latency_ms)
                .collect(),
        ),
        retrieval_avg_tokens: average_f64_slice(
            trailing.iter().map(|entry| entry.retrieval_avg_tokens).collect(),
        ),
        route_avg_latency_ms: average_f64_slice(
            trailing.iter().map(|entry| entry.route_avg_latency_ms).collect(),
        ),
        route_p95_latency_ms: average_f64_slice(
            trailing.iter().map(|entry| entry.route_p95_latency_ms).collect(),
        ),
        route_avg_tokens: average_f64_slice(
            trailing.iter().map(|entry| entry.route_avg_tokens).collect(),
        ),
        mutation_scope_incomplete_rate: average_f64_slice(
            trailing
                .iter()
                .map(|entry| entry.mutation_scope_incomplete_rate)
                .collect(),
        ),
        mutation_scope_missing_files_rate: average_f64_slice(
            trailing
                .iter()
                .map(|entry| entry.mutation_scope_missing_files_rate)
                .collect(),
        ),
        mutation_scope_extra_files_rate: average_f64_slice(
            trailing
                .iter()
                .map(|entry| entry.mutation_scope_extra_files_rate)
                .collect(),
        ),
        mutation_scope_missing_symbols_rate: average_f64_slice(
            trailing
                .iter()
                .map(|entry| entry.mutation_scope_missing_symbols_rate)
                .collect(),
        ),
        mutation_scope_extra_symbols_rate: average_f64_slice(
            trailing
                .iter()
                .map(|entry| entry.mutation_scope_extra_symbols_rate)
                .collect(),
        ),
        fixture_graph_drift: Vec::new(),
    })
}

fn summarize_trend_direction<F>(history: &[QualityTrendEntry], metric: F) -> &'static str
where
    F: Fn(&QualityTrendEntry) -> f64,
{
    if history.len() < 2 {
        return "insufficient history";
    }
    let window: Vec<&QualityTrendEntry> = history.iter().rev().take(5).collect();
    let newest = metric(window[0]);
    let oldest = metric(window[window.len() - 1]);
    let delta = newest - oldest;
    if delta > 1.0 {
        "worsening"
    } else if delta < -1.0 {
        "improving"
    } else {
        "flat"
    }
}

fn append_baseline_overview(
    lines: &mut Vec<String>,
    baseline: &QualityReport,
    current: &QualityReport,
) {
    let regressions = compare_reports(baseline, current);
    let improvements = collect_improvements(baseline, current);
    let hotspot_changes = collect_hotspot_changes(baseline, current);
    let (p95_regressions, p95_improvements) = collect_p95_movers(baseline, current);
    let (p95_regression_fixtures, p95_improvement_fixtures) =
        summarize_p95_movers_by_fixture(&p95_regressions, &p95_improvements);
    lines.push("## Baseline Delta".to_string());
    lines.push(format!(
        "- Baseline comparison: {}",
        if regressions.is_empty() {
            "clean"
        } else {
            "regressions detected"
        }
    ));
    lines.push(format!("- Improvement signals: {}", improvements.len()));
    if improvements.is_empty() {
        lines.push("- No positive deltas exceeded the reporting threshold.".to_string());
    } else {
        for improvement in improvements.iter().take(8) {
            lines.push(format!("- improvement: {improvement}"));
        }
        if improvements.len() > 8 {
            lines.push(format!(
                "- improvement: {} more improvements omitted for brevity",
                improvements.len() - 8
            ));
        }
    }
    if regressions.is_empty() {
        lines.push("- Regressions: none".to_string());
    } else {
        for regression in regressions.iter().take(8) {
            lines.push(format!("- regression: {regression}"));
        }
        if regressions.len() > 8 {
            lines.push(format!(
                "- regression: {} more regressions omitted for brevity",
                regressions.len() - 8
            ));
        }
    }
    if hotspot_changes.is_empty() {
        lines.push("- Hotspot churn: none".to_string());
    } else {
        for change in hotspot_changes.iter().take(8) {
            lines.push(format!("- hotspot: {change}"));
        }
        if hotspot_changes.len() > 8 {
            lines.push(format!(
                "- hotspot: {} more hotspot changes omitted for brevity",
                hotspot_changes.len() - 8
            ));
        }
    }
    if p95_regressions.is_empty() {
        lines.push("- p95 regressions: none".to_string());
    } else {
        for fixture in p95_regression_fixtures.iter().take(3) {
            lines.push(format!("- p95 regression fixture: {fixture}"));
        }
        for mover in p95_regressions
            .iter()
            .filter(|mover| mover.magnitude_ms >= P95_SCOPE_REGRESSION_MIN_DELTA_MS)
            .take(6)
        {
            lines.push(format!("- p95 regression scope: {}", mover.label));
        }
        let remaining_regression_count = p95_regressions
            .iter()
            .filter(|mover| mover.magnitude_ms >= P95_SCOPE_REGRESSION_MIN_DELTA_MS)
            .count();
        if remaining_regression_count > 6 {
            lines.push(format!(
                "- p95 regression: {} more regressions omitted for brevity",
                remaining_regression_count - 6
            ));
        }
    }
    if p95_improvements.is_empty() {
        lines.push("- p95 improvements: none".to_string());
    } else {
        for fixture in p95_improvement_fixtures.iter().take(3) {
            lines.push(format!("- p95 improvement fixture: {fixture}"));
        }
        for mover in p95_improvements
            .iter()
            .filter(|mover| mover.magnitude_ms >= P95_SCOPE_IMPROVEMENT_MIN_DELTA_MS)
            .take(6)
        {
            lines.push(format!("- p95 improvement scope: {}", mover.label));
        }
        let remaining_improvement_count = p95_improvements
            .iter()
            .filter(|mover| mover.magnitude_ms >= P95_SCOPE_IMPROVEMENT_MIN_DELTA_MS)
            .count();
        if remaining_improvement_count > 6 {
            lines.push(format!(
                "- p95 improvement: {} more improvements omitted for brevity",
                remaining_improvement_count - 6
            ));
        }
    }
    lines.push(String::new());
}

fn collect_improvements(baseline: &QualityReport, current: &QualityReport) -> Vec<String> {
    let mut improvements = Vec::new();
    for base_fixture in &baseline.fixtures {
        let Some(current_fixture) = current
            .fixtures
            .iter()
            .find(|fixture| fixture.fixture == base_fixture.fixture)
        else {
            continue;
        };

        if let (Some(base), Some(now)) = (&base_fixture.retrieval, &current_fixture.retrieval) {
            push_metric_improvement(
                &mut improvements,
                &base_fixture.fixture,
                "retrieval",
                "target_match_rate",
                base.target_match_rate,
                now.target_match_rate,
                true,
            );
            push_metric_improvement(
                &mut improvements,
                &base_fixture.fixture,
                "retrieval",
                "top_file_match_rate",
                base.top_file_match_rate,
                now.top_file_match_rate,
                true,
            );
            push_metric_improvement(
                &mut improvements,
                &base_fixture.fixture,
                "retrieval",
                "top_span_match_rate",
                base.top_span_match_rate,
                now.top_span_match_rate,
                true,
            );
            push_metric_improvement(
                &mut improvements,
                &base_fixture.fixture,
                "retrieval",
                "omission_rate",
                base.omission_rate,
                now.omission_rate,
                false,
            );
            push_metric_improvement(
                &mut improvements,
                &base_fixture.fixture,
                "retrieval",
                "overfetch_rate",
                base.overfetch_rate,
                now.overfetch_rate,
                false,
            );
        }
        if let (Some(base), Some(now)) = (&base_fixture.route, &current_fixture.route) {
            push_metric_improvement(
                &mut improvements,
                &base_fixture.fixture,
                "route",
                "intent_match_rate",
                base.intent_match_rate,
                now.intent_match_rate,
                true,
            );
            push_metric_improvement(
                &mut improvements,
                &base_fixture.fixture,
                "route",
                "selected_tool_match_rate",
                base.selected_tool_match_rate,
                now.selected_tool_match_rate,
                true,
            );
            push_metric_improvement(
                &mut improvements,
                &base_fixture.fixture,
                "route",
                "result_symbol_match_rate",
                base.result_symbol_match_rate,
                now.result_symbol_match_rate,
                true,
            );
        }
    }
    improvements
}

fn push_metric_improvement(
    improvements: &mut Vec<String>,
    fixture: &str,
    section: &str,
    metric: &str,
    baseline: f64,
    current: f64,
    higher_is_better: bool,
) {
    let improved = if higher_is_better {
        current > baseline + EPSILON
    } else {
        current + EPSILON < baseline
    };
    if improved {
        improvements.push(format!(
            "{fixture}.{section}.{metric} improved: baseline={baseline:.4}, current={current:.4}"
        ));
    }
}

fn collect_hotspot_changes(baseline: &QualityReport, current: &QualityReport) -> Vec<String> {
    let mut changes = Vec::new();
    append_hotspot_deltas(
        &mut changes,
        "worst_retrieval_latency",
        &baseline.worst_retrieval_latency,
        &current.worst_retrieval_latency,
        true,
    );
    append_hotspot_deltas(
        &mut changes,
        "worst_retrieval_tokens",
        &baseline.worst_retrieval_tokens,
        &current.worst_retrieval_tokens,
        false,
    );
    append_hotspot_deltas(
        &mut changes,
        "worst_route_latency",
        &baseline.worst_route_latency,
        &current.worst_route_latency,
        true,
    );
    append_hotspot_deltas(
        &mut changes,
        "worst_route_tokens",
        &baseline.worst_route_tokens,
        &current.worst_route_tokens,
        false,
    );
    changes
}

#[derive(Debug, Clone)]
struct P95Mover {
    fixture: String,
    magnitude_ms: f64,
    label: String,
}

fn collect_p95_movers(
    baseline: &QualityReport,
    current: &QualityReport,
) -> (Vec<P95Mover>, Vec<P95Mover>) {
    let mut regressions = Vec::new();
    let mut improvements = Vec::new();

    for base_fixture in &baseline.fixtures {
        let Some(current_fixture) = current
            .fixtures
            .iter()
            .find(|fixture| fixture.fixture == base_fixture.fixture)
        else {
            continue;
        };

        if let (Some(base), Some(now)) = (&base_fixture.retrieval, &current_fixture.retrieval) {
            push_p95_mover(
                &mut regressions,
                &mut improvements,
                &base_fixture.fixture,
                format!("{}.retrieval.p95_latency_ms", base_fixture.fixture),
                base.p95_latency_ms,
                now.p95_latency_ms,
            );
            for base_bucket in &base.operation_breakdown {
                if let Some(now_bucket) = now
                    .operation_breakdown
                    .iter()
                    .find(|bucket| bucket.operation == base_bucket.operation)
                {
                    push_p95_mover(
                        &mut regressions,
                        &mut improvements,
                        &base_fixture.fixture,
                        format!(
                            "{}.retrieval.{}.p95_latency_ms",
                            base_fixture.fixture, base_bucket.operation
                        ),
                        base_bucket.p95_latency_ms,
                        now_bucket.p95_latency_ms,
                    );
                }
            }
        }

        if let (Some(base), Some(now)) = (&base_fixture.route, &current_fixture.route) {
            push_p95_mover(
                &mut regressions,
                &mut improvements,
                &base_fixture.fixture,
                format!("{}.route.p95_latency_ms", base_fixture.fixture),
                base.p95_latency_ms,
                now.p95_latency_ms,
            );
            for base_bucket in &base.intent_breakdown {
                if let Some(now_bucket) = now
                    .intent_breakdown
                    .iter()
                    .find(|bucket| bucket.intent == base_bucket.intent)
                {
                    push_p95_mover(
                        &mut regressions,
                        &mut improvements,
                        &base_fixture.fixture,
                        format!(
                            "{}.route.{}.p95_latency_ms",
                            base_fixture.fixture, base_bucket.intent
                        ),
                        base_bucket.p95_latency_ms,
                        now_bucket.p95_latency_ms,
                    );
                }
            }
        }
    }

    regressions.sort_by(|a, b| {
        b.magnitude_ms
            .partial_cmp(&a.magnitude_ms)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    improvements.sort_by(|a, b| {
        b.magnitude_ms
            .partial_cmp(&a.magnitude_ms)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    (regressions, improvements)
}

fn push_p95_mover(
    regressions: &mut Vec<P95Mover>,
    improvements: &mut Vec<P95Mover>,
    fixture: &str,
    scope: String,
    baseline: f64,
    current: f64,
) {
    let delta = current - baseline;
    if delta > 0.0 {
        if delta <= 1.0 {
            return;
        }
        let entry = P95Mover {
            fixture: fixture.to_string(),
            magnitude_ms: delta.abs(),
            label: format!("{scope} moved by {:+.2}ms ({:.2}ms -> {:.2}ms)", delta, baseline, current),
        };
        regressions.push(entry);
    } else {
        if delta.abs() <= P95_IMPROVEMENT_MIN_DELTA_MS {
            return;
        }
        let entry = P95Mover {
            fixture: fixture.to_string(),
            magnitude_ms: delta.abs(),
            label: format!("{scope} moved by {:+.2}ms ({:.2}ms -> {:.2}ms)", delta, baseline, current),
        };
        improvements.push(entry);
    }
}

fn summarize_p95_movers_by_fixture(
    regressions: &[P95Mover],
    improvements: &[P95Mover],
) -> (Vec<String>, Vec<String>) {
    (
        summarize_single_p95_direction_by_fixture(regressions, "regression"),
        summarize_single_p95_direction_by_fixture(improvements, "improvement"),
    )
}

fn summarize_single_p95_direction_by_fixture(movers: &[P95Mover], label: &str) -> Vec<String> {
    let mut per_fixture: std::collections::BTreeMap<String, (usize, f64)> =
        std::collections::BTreeMap::new();
    for mover in movers {
        let entry = per_fixture
            .entry(mover.fixture.clone())
            .or_insert((0, 0.0));
        entry.0 += 1;
        entry.1 = entry.1.max(mover.magnitude_ms);
    }

    let mut rows: Vec<(String, usize, f64)> = per_fixture
        .into_iter()
        .map(|(fixture, (count, max_delta))| (fixture, count, max_delta))
        .collect();
    let min_delta = if label == "regression" {
        P95_FIXTURE_REGRESSION_MIN_DELTA_MS
    } else {
        P95_FIXTURE_IMPROVEMENT_MIN_DELTA_MS
    };
    rows.retain(|(_, _, max_delta)| *max_delta >= min_delta);
    rows.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
    rows.into_iter()
        .map(|(fixture, count, max_delta)| {
            format!(
                "{fixture} has {count} {label} mover(s), max |delta|={max_delta:.2}ms"
            )
        })
        .collect()
}

fn append_hotspot_deltas(
    changes: &mut Vec<String>,
    label: &str,
    baseline_rows: &[WorstCaseRow],
    current_rows: &[WorstCaseRow],
    by_latency: bool,
) {
    for (index, row) in current_rows.iter().enumerate() {
        let identifier = hotspot_identifier(row);
        match baseline_rows
            .iter()
            .find(|candidate| hotspot_identifier(candidate) == identifier)
        {
            Some(previous) => {
                if by_latency {
                    let delta = row.latency_ms - previous.latency_ms;
                    if delta > LATENCY_REGRESSION_ABS_TOLERANCE_MS {
                        changes.push(format!(
                            "{label}[{}] `{}` latency increased by {:.2}ms ({:.2}ms -> {:.2}ms)",
                            index + 1,
                            identifier,
                            delta,
                            previous.latency_ms,
                            row.latency_ms
                        ));
                    }
                } else if row.approx_tokens > previous.approx_tokens {
                    let delta = row.approx_tokens - previous.approx_tokens;
                    let allowed = ((previous.approx_tokens as f64) * TOKEN_REGRESSION_TOLERANCE_PCT)
                        .ceil() as usize;
                    if delta > allowed.max(25) {
                        changes.push(format!(
                            "{label}[{}] `{}` tokens increased by {} ({} -> {})",
                            index + 1,
                            identifier,
                            delta,
                            previous.approx_tokens,
                            row.approx_tokens
                        ));
                    }
                }
            }
            None => {
                if hotspot_entry_is_material(baseline_rows, row, index, by_latency) {
                    changes.push(format!(
                        "{label}[{}] new hotspot `{}` entered the top set at {}{}",
                        index + 1,
                        identifier,
                        if by_latency {
                            format!("{:.2}", row.latency_ms)
                        } else {
                            row.approx_tokens.to_string()
                        },
                        if by_latency { "ms" } else { " tokens" }
                    ));
                }
            }
        }
    }
}

fn hotspot_identifier(row: &WorstCaseRow) -> String {
    format!(
        "{}/{}/{}/{}",
        row.fixture, row.section, row.bucket, row.case_name
    )
}

fn hotspot_entry_is_material(
    baseline_rows: &[WorstCaseRow],
    candidate: &WorstCaseRow,
    rank_index: usize,
    by_latency: bool,
) -> bool {
    let Some(rank_cutoff) = baseline_rows.get(rank_index).or_else(|| baseline_rows.last()) else {
        return true;
    };
    if by_latency {
        let allowed = (rank_cutoff.latency_ms * (1.0 + LATENCY_REGRESSION_TOLERANCE_PCT))
            .max(rank_cutoff.latency_ms + LATENCY_REGRESSION_ABS_TOLERANCE_MS);
        candidate.latency_ms > allowed
    } else {
        let allowed =
            ((rank_cutoff.approx_tokens as f64) * (1.0 + TOKEN_REGRESSION_TOLERANCE_PCT)).ceil()
                as usize;
        candidate.approx_tokens > allowed.max(rank_cutoff.approx_tokens + 25)
    }
}

fn pct_delta(current: f64, baseline: f64) -> f64 {
    (current - baseline) * 100.0
}

fn append_worst_case_section(lines: &mut Vec<String>, title: &str, rows: &[WorstCaseRow]) {
    lines.push(format!("## {title}"));
    if rows.is_empty() {
        lines.push("- none".to_string());
        lines.push(String::new());
        return;
    }
    for row in rows {
        lines.push(format!(
            "- `{}` / `{}` / `{}`: case=`{}`, latency={:.2}ms, tokens={}",
            row.fixture, row.section, row.bucket, row.case_name, row.latency_ms, row.approx_tokens
        ));
    }
    lines.push(String::new());
}

fn compare_reports(baseline: &QualityReport, current: &QualityReport) -> Vec<String> {
    let mut failures = Vec::new();
    for base_fixture in &baseline.fixtures {
        let Some(current_fixture) = current
            .fixtures
            .iter()
            .find(|fixture| fixture.fixture == base_fixture.fixture)
        else {
            failures.push(format!("missing fixture in current report: {}", base_fixture.fixture));
            continue;
        };

        if let (Some(base), Some(now)) = (&base_fixture.retrieval, &current_fixture.retrieval) {
            compare_retrieval_summary(
                &mut failures,
                &base_fixture.fixture,
                "retrieval",
                base,
                now,
                current_fixture.retrieval_max_avg_latency_ms,
            );
        }
        if let (Some(base), Some(now)) = (&base_fixture.route, &current_fixture.route) {
            compare_route_summary(
                &mut failures,
                &base_fixture.fixture,
                "route",
                base,
                now,
                current_fixture.route_max_avg_latency_ms,
            );
        }
    }
    failures
}

fn worst_retrieval_rows(
    fixtures: &[FixtureQualityReport],
    by_latency: bool,
    descending: bool,
) -> Vec<WorstCaseRow> {
    let mut rows: Vec<WorstCaseRow> = fixtures
        .iter()
        .flat_map(|fixture| {
            fixture
                .retrieval
                .as_ref()
                .into_iter()
                .flat_map(move |summary| {
                    summary.reports.iter().map(move |report| WorstCaseRow {
                        fixture: fixture.fixture.clone(),
                        section: "retrieval".to_string(),
                        bucket: report.operation_bucket.clone(),
                        case_name: report.case_name.clone(),
                        latency_ms: report.latency_ms,
                        approx_tokens: report.approx_tokens,
                    })
                })
        })
        .collect();
    if by_latency {
        rows.sort_by(|a, b| {
            a.latency_ms
                .partial_cmp(&b.latency_ms)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    } else {
        rows.sort_by_key(|row| row.approx_tokens);
    }
    if descending {
        rows.reverse();
    }
    rows.truncate(5);
    rows
}

fn worst_route_rows(
    fixtures: &[FixtureQualityReport],
    by_latency: bool,
    descending: bool,
) -> Vec<WorstCaseRow> {
    let mut rows: Vec<WorstCaseRow> = fixtures
        .iter()
        .flat_map(|fixture| {
            fixture.route.as_ref().into_iter().flat_map(move |summary| {
                summary.reports.iter().map(move |report| WorstCaseRow {
                    fixture: fixture.fixture.clone(),
                    section: "route".to_string(),
                    bucket: report.intent_bucket.clone(),
                    case_name: report.case_name.clone(),
                    latency_ms: report.latency_ms,
                    approx_tokens: report.approx_tokens,
                })
            })
        })
        .collect();
    if by_latency {
        rows.sort_by(|a, b| {
            a.latency_ms
                .partial_cmp(&b.latency_ms)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    } else {
        rows.sort_by_key(|row| row.approx_tokens);
    }
    if descending {
        rows.reverse();
    }
    rows.truncate(5);
    rows
}

fn compare_retrieval_summary(
    failures: &mut Vec<String>,
    fixture: &str,
    section: &str,
    baseline: &RetrievalQualitySummary,
    current: &RetrievalQualitySummary,
    latency_ceiling_ms: Option<f64>,
) {
    compare_non_decreasing_metric(
        failures,
        fixture,
        section,
        "target_match_rate",
        baseline.target_match_rate,
        current.target_match_rate,
    );
    compare_non_decreasing_metric(
        failures,
        fixture,
        section,
        "top_file_match_rate",
        baseline.top_file_match_rate,
        current.top_file_match_rate,
    );
    compare_non_decreasing_metric(
        failures,
        fixture,
        section,
        "top_span_match_rate",
        baseline.top_span_match_rate,
        current.top_span_match_rate,
    );
    compare_non_increasing_metric(
        failures,
        fixture,
        section,
        "omission_rate",
        baseline.omission_rate,
        current.omission_rate,
    );
    compare_non_increasing_metric(
        failures,
        fixture,
        section,
        "overfetch_rate",
        baseline.overfetch_rate,
        current.overfetch_rate,
    );
    compare_tolerated_increase(
        failures,
        fixture,
        section,
        "avg_latency_ms",
        baseline.avg_latency_ms,
        current.avg_latency_ms,
        LATENCY_REGRESSION_TOLERANCE_PCT,
        latency_ceiling_ms,
    );
    compare_tolerated_increase(
        failures,
        fixture,
        section,
        "avg_tokens",
        baseline.avg_tokens,
        current.avg_tokens,
        TOKEN_REGRESSION_TOLERANCE_PCT,
        None,
    );
    compare_retrieval_operation_breakdown(
        failures,
        fixture,
        section,
        baseline,
        current,
        latency_ceiling_ms,
    );
}

fn compare_retrieval_operation_breakdown(
    failures: &mut Vec<String>,
    fixture: &str,
    section: &str,
    baseline: &RetrievalQualitySummary,
    current: &RetrievalQualitySummary,
    latency_ceiling_ms: Option<f64>,
) {
    for baseline_operation in &baseline.operation_breakdown {
        let Some(current_operation) = current
            .operation_breakdown
            .iter()
            .find(|operation| operation.operation == baseline_operation.operation)
        else {
            failures.push(format!(
                "{fixture}.{section}.operation_breakdown missing operation bucket: {}",
                baseline_operation.operation
            ));
            continue;
        };

        let scope = format!("{section}.operation_{}", baseline_operation.operation);
        compare_non_decreasing_metric(
            failures,
            fixture,
            &scope,
            "target_match_rate",
            baseline_operation.target_match_rate,
            current_operation.target_match_rate,
        );
        compare_non_decreasing_metric(
            failures,
            fixture,
            &scope,
            "top_file_match_rate",
            baseline_operation.top_file_match_rate,
            current_operation.top_file_match_rate,
        );
        compare_non_decreasing_metric(
            failures,
            fixture,
            &scope,
            "top_span_match_rate",
            baseline_operation.top_span_match_rate,
            current_operation.top_span_match_rate,
        );
        compare_non_increasing_metric(
            failures,
            fixture,
            &scope,
            "omission_rate",
            baseline_operation.omission_rate,
            current_operation.omission_rate,
        );
        compare_non_increasing_metric(
            failures,
            fixture,
            &scope,
            "overfetch_rate",
            baseline_operation.overfetch_rate,
            current_operation.overfetch_rate,
        );
        compare_tolerated_increase(
            failures,
            fixture,
            &scope,
            "avg_latency_ms",
            baseline_operation.avg_latency_ms,
            current_operation.avg_latency_ms,
            LATENCY_REGRESSION_TOLERANCE_PCT,
            latency_ceiling_ms,
        );
        compare_tolerated_increase(
            failures,
            fixture,
            &scope,
            "avg_tokens",
            baseline_operation.avg_tokens,
            current_operation.avg_tokens,
            TOKEN_REGRESSION_TOLERANCE_PCT,
            None,
        );
    }
}

fn compare_route_summary(
    failures: &mut Vec<String>,
    fixture: &str,
    section: &str,
    baseline: &RouteQualitySummary,
    current: &RouteQualitySummary,
    latency_ceiling_ms: Option<f64>,
) {
    compare_non_decreasing_metric(
        failures,
        fixture,
        section,
        "reviewable_or_better_rate",
        baseline.reviewable_or_better_rate,
        current.reviewable_or_better_rate,
    );
    compare_non_decreasing_metric(
        failures,
        fixture,
        section,
        "high_confidence_rate",
        baseline.high_confidence_rate,
        current.high_confidence_rate,
    );
    compare_non_decreasing_metric(
        failures,
        fixture,
        section,
        "mutation_scope_aligned_rate",
        baseline.mutation_scope_aligned_rate,
        current.mutation_scope_aligned_rate,
    );
    compare_non_decreasing_metric(
        failures,
        fixture,
        section,
        "intent_match_rate",
        baseline.intent_match_rate,
        current.intent_match_rate,
    );
    compare_non_decreasing_metric(
        failures,
        fixture,
        section,
        "selected_tool_match_rate",
        baseline.selected_tool_match_rate,
        current.selected_tool_match_rate,
    );
    compare_non_decreasing_metric(
        failures,
        fixture,
        section,
        "budget_match_rate",
        baseline.budget_match_rate,
        current.budget_match_rate,
    );
    compare_non_decreasing_metric(
        failures,
        fixture,
        section,
        "reference_only_match_rate",
        baseline.reference_only_match_rate,
        current.reference_only_match_rate,
    );
    compare_non_decreasing_metric(
        failures,
        fixture,
        section,
        "single_file_fast_path_match_rate",
        baseline.single_file_fast_path_match_rate,
        current.single_file_fast_path_match_rate,
    );
    compare_non_decreasing_metric(
        failures,
        fixture,
        section,
        "result_symbol_match_rate",
        baseline.result_symbol_match_rate,
        current.result_symbol_match_rate,
    );
    compare_non_decreasing_metric(
        failures,
        fixture,
        section,
        "project_summary_match_rate",
        baseline.project_summary_match_rate,
        current.project_summary_match_rate,
    );
    compare_tolerated_increase(
        failures,
        fixture,
        section,
        "avg_latency_ms",
        baseline.avg_latency_ms,
        current.avg_latency_ms,
        LATENCY_REGRESSION_TOLERANCE_PCT,
        latency_ceiling_ms,
    );
    compare_tolerated_increase(
        failures,
        fixture,
        section,
        "avg_tokens",
        baseline.avg_tokens,
        current.avg_tokens,
        TOKEN_REGRESSION_TOLERANCE_PCT,
        None,
    );
    compare_route_intent_breakdown(
        failures,
        fixture,
        section,
        baseline,
        current,
        latency_ceiling_ms,
    );
}

fn compare_route_intent_breakdown(
    failures: &mut Vec<String>,
    fixture: &str,
    section: &str,
    baseline: &RouteQualitySummary,
    current: &RouteQualitySummary,
    latency_ceiling_ms: Option<f64>,
) {
    for baseline_intent in &baseline.intent_breakdown {
        let Some(current_intent) = current
            .intent_breakdown
            .iter()
            .find(|intent| intent.intent == baseline_intent.intent)
        else {
            failures.push(format!(
                "{fixture}.{section}.intent_breakdown missing intent bucket: {}",
                baseline_intent.intent
            ));
            continue;
        };

        let scope = format!("{section}.intent_{}", baseline_intent.intent);
        compare_non_decreasing_metric(
            failures,
            fixture,
            &scope,
            "reviewable_or_better_rate",
            baseline_intent.reviewable_or_better_rate,
            current_intent.reviewable_or_better_rate,
        );
        compare_non_decreasing_metric(
            failures,
            fixture,
            &scope,
            "high_confidence_rate",
            baseline_intent.high_confidence_rate,
            current_intent.high_confidence_rate,
        );
        compare_non_decreasing_metric(
            failures,
            fixture,
            &scope,
            "mutation_scope_aligned_rate",
            baseline_intent.mutation_scope_aligned_rate,
            current_intent.mutation_scope_aligned_rate,
        );
        compare_non_decreasing_metric(
            failures,
            fixture,
            &scope,
            "intent_match_rate",
            baseline_intent.intent_match_rate,
            current_intent.intent_match_rate,
        );
        compare_non_decreasing_metric(
            failures,
            fixture,
            &scope,
            "selected_tool_match_rate",
            baseline_intent.selected_tool_match_rate,
            current_intent.selected_tool_match_rate,
        );
        compare_non_decreasing_metric(
            failures,
            fixture,
            &scope,
            "budget_match_rate",
            baseline_intent.budget_match_rate,
            current_intent.budget_match_rate,
        );
        compare_non_decreasing_metric(
            failures,
            fixture,
            &scope,
            "reference_only_match_rate",
            baseline_intent.reference_only_match_rate,
            current_intent.reference_only_match_rate,
        );
        compare_non_decreasing_metric(
            failures,
            fixture,
            &scope,
            "single_file_fast_path_match_rate",
            baseline_intent.single_file_fast_path_match_rate,
            current_intent.single_file_fast_path_match_rate,
        );
        compare_non_decreasing_metric(
            failures,
            fixture,
            &scope,
            "result_symbol_match_rate",
            baseline_intent.result_symbol_match_rate,
            current_intent.result_symbol_match_rate,
        );
        compare_non_decreasing_metric(
            failures,
            fixture,
            &scope,
            "project_summary_match_rate",
            baseline_intent.project_summary_match_rate,
            current_intent.project_summary_match_rate,
        );
        compare_tolerated_increase(
            failures,
            fixture,
            &scope,
            "avg_latency_ms",
            baseline_intent.avg_latency_ms,
            current_intent.avg_latency_ms,
            LATENCY_REGRESSION_TOLERANCE_PCT,
            latency_ceiling_ms,
        );
        compare_tolerated_increase(
            failures,
            fixture,
            &scope,
            "avg_tokens",
            baseline_intent.avg_tokens,
            current_intent.avg_tokens,
            TOKEN_REGRESSION_TOLERANCE_PCT,
            None,
        );
    }
}

fn compare_non_decreasing_metric(
    failures: &mut Vec<String>,
    fixture: &str,
    section: &str,
    metric: &str,
    baseline: f64,
    current: f64,
) {
    if current + EPSILON < baseline {
        failures.push(format!(
            "{fixture}.{section}.{metric} regressed: baseline={baseline:.4}, current={current:.4}"
        ));
    }
}

fn leading_mutation_scope_failure(summary: &RouteQualitySummary) -> Option<(&'static str, f64)> {
    let candidates = [
        ("incomplete", summary.mutation_scope_incomplete_rate),
        ("missing_files", summary.mutation_scope_missing_files_rate),
        ("extra_files", summary.mutation_scope_extra_files_rate),
        ("missing_symbols", summary.mutation_scope_missing_symbols_rate),
        ("extra_symbols", summary.mutation_scope_extra_symbols_rate),
    ];
    candidates
        .into_iter()
        .filter(|(_, rate)| *rate > EPSILON)
        .max_by(|a, b| a.1.total_cmp(&b.1))
}

fn leading_mutation_scope_failure_delta(
    current: &RouteQualitySummary,
    baseline: &RouteQualitySummary,
) -> Option<String> {
    let current_leader = leading_mutation_scope_failure(current);
    let baseline_leader = leading_mutation_scope_failure(baseline);
    match (current_leader, baseline_leader) {
        (Some((current_label, current_rate)), Some((baseline_label, baseline_rate))) => {
            Some(format!(
                "{} {:+.0}pp vs baseline leader {}",
                current_label,
                pct_delta(current_rate, baseline_rate),
                baseline_label
            ))
        }
        (Some((current_label, current_rate)), None) => Some(format!(
            "{} emerged at {:.0}%",
            current_label,
            current_rate * 100.0
        )),
        (None, Some((baseline_label, _))) => {
            Some(format!("no current graph drift (baseline leader was {})", baseline_label))
        }
        (None, None) => None,
    }
}

fn compare_non_increasing_metric(
    failures: &mut Vec<String>,
    fixture: &str,
    section: &str,
    metric: &str,
    baseline: f64,
    current: f64,
) {
    if current > baseline + EPSILON {
        failures.push(format!(
            "{fixture}.{section}.{metric} regressed: baseline={baseline:.4}, current={current:.4}"
        ));
    }
}

fn compare_tolerated_increase(
    failures: &mut Vec<String>,
    fixture: &str,
    section: &str,
    metric: &str,
    baseline: f64,
    current: f64,
    tolerance_pct: f64,
    latency_ceiling_ms: Option<f64>,
) {
    if baseline <= EPSILON {
        return;
    }
    let allowed = if metric.contains("latency") {
        let abs_tolerance = if baseline <= TINY_BASELINE_LATENCY_MS {
            TINY_BASELINE_LATENCY_ABS_TOLERANCE_MS
        } else {
            LATENCY_REGRESSION_ABS_TOLERANCE_MS
        };
        let baseline_allowed = (baseline * (1.0 + tolerance_pct)).max(baseline + abs_tolerance);
        latency_ceiling_ms
            .map(|ceiling| baseline_allowed.max(ceiling + LATENCY_CEILING_JITTER_MS))
            .unwrap_or(baseline_allowed)
    } else {
        baseline * (1.0 + tolerance_pct)
    };
    if current > allowed {
        failures.push(format!(
            "{fixture}.{section}.{metric} regressed beyond tolerance: baseline={baseline:.4}, current={current:.4}, allowed={allowed:.4}"
        ));
    }
}

fn compare_retrieval_case_thresholds(
    failures: &mut Vec<String>,
    fixture: &str,
    case: &RetrievalCase,
    report: &RetrievalCaseReport,
) {
    if let Some(max_latency_ms) = case.max_latency_ms {
        if report.latency_ms > max_latency_ms as f64 {
            failures.push(format!(
                "{fixture}.retrieval_case.{}.latency_ms exceeded threshold: current={:.2}, allowed={}",
                case.name, report.latency_ms, max_latency_ms
            ));
        }
    }
    if let Some(max_tokens) = case.max_approx_tokens {
        if report.approx_tokens > max_tokens {
            failures.push(format!(
                "{fixture}.retrieval_case.{}.approx_tokens exceeded threshold: current={}, allowed={}",
                case.name, report.approx_tokens, max_tokens
            ));
        }
    }
}

fn compare_route_case_thresholds(
    failures: &mut Vec<String>,
    fixture: &str,
    case: &RouteCase,
    report: &RouteCaseReport,
) {
    if let Some(max_latency_ms) = case.max_latency_ms {
        if report.latency_ms > max_latency_ms as f64 {
            failures.push(format!(
                "{fixture}.route_case.{}.latency_ms exceeded threshold: current={:.2}, allowed={}",
                case.name, report.latency_ms, max_latency_ms
            ));
        }
    }
    if let Some(max_tokens) = case.max_approx_tokens {
        if report.approx_tokens > max_tokens {
            failures.push(format!(
                "{fixture}.route_case.{}.approx_tokens exceeded threshold: current={}, allowed={}",
                case.name, report.approx_tokens, max_tokens
            ));
        }
    }
}

fn compare_retrieval_thresholds(
    failures: &mut Vec<String>,
    fixture: &str,
    thresholds: &RetrievalThresholds,
    summary: &RetrievalQualitySummary,
) {
    if let Some(min) = thresholds.min_target_match_rate {
        if summary.target_match_rate + EPSILON < min {
            failures.push(format!(
                "{fixture}.retrieval.target_match_rate below threshold: current={:.4}, required={:.4}",
                summary.target_match_rate, min
            ));
        }
    }
    if let Some(min) = thresholds.min_top_file_match_rate {
        if summary.top_file_match_rate + EPSILON < min {
            failures.push(format!(
                "{fixture}.retrieval.top_file_match_rate below threshold: current={:.4}, required={:.4}",
                summary.top_file_match_rate, min
            ));
        }
    }
    if let Some(min) = thresholds.min_top_span_match_rate {
        if summary.top_span_match_rate + EPSILON < min {
            failures.push(format!(
                "{fixture}.retrieval.top_span_match_rate below threshold: current={:.4}, required={:.4}",
                summary.top_span_match_rate, min
            ));
        }
    }
    if let Some(max) = thresholds.max_omission_rate {
        if summary.omission_rate > max + EPSILON {
            failures.push(format!(
                "{fixture}.retrieval.omission_rate above threshold: current={:.4}, allowed={:.4}",
                summary.omission_rate, max
            ));
        }
    }
    if let Some(max) = thresholds.max_overfetch_rate {
        if summary.overfetch_rate > max + EPSILON {
            failures.push(format!(
                "{fixture}.retrieval.overfetch_rate above threshold: current={:.4}, allowed={:.4}",
                summary.overfetch_rate, max
            ));
        }
    }
    if let Some(max) = thresholds.max_avg_latency_ms {
        if summary.avg_latency_ms > max + EPSILON {
            failures.push(format!(
                "{fixture}.retrieval.avg_latency_ms above threshold: current={:.4}, allowed={:.4}",
                summary.avg_latency_ms, max
            ));
        }
    }
    if let Some(max) = thresholds.max_avg_tokens {
        if summary.avg_tokens > max + EPSILON {
            failures.push(format!(
                "{fixture}.retrieval.avg_tokens above threshold: current={:.4}, allowed={:.4}",
                summary.avg_tokens, max
            ));
        }
    }
}

fn compare_route_thresholds(
    failures: &mut Vec<String>,
    fixture: &str,
    thresholds: &RouteThresholds,
    summary: &RouteQualitySummary,
) {
    if let Some(min) = thresholds.min_reviewable_or_better_rate {
        if summary.reviewable_or_better_rate + EPSILON < min {
            failures.push(format!(
                "{fixture}.route.reviewable_or_better_rate below threshold: current={:.4}, required={:.4}",
                summary.reviewable_or_better_rate, min
            ));
        }
    }
    if let Some(min) = thresholds.min_high_confidence_rate {
        if summary.high_confidence_rate + EPSILON < min {
            failures.push(format!(
                "{fixture}.route.high_confidence_rate below threshold: current={:.4}, required={:.4}",
                summary.high_confidence_rate, min
            ));
        }
    }
    if let Some(min) = thresholds.min_mutation_ready_rate {
        if summary.mutation_ready_rate + EPSILON < min {
            failures.push(format!(
                "{fixture}.route.mutation_ready_rate below threshold: current={:.4}, required={:.4}",
                summary.mutation_ready_rate, min
            ));
        }
    }
    if let Some(min) = thresholds.min_mutation_ready_without_retry_rate {
        if summary.mutation_ready_without_retry_rate + EPSILON < min {
            failures.push(format!(
                "{fixture}.route.mutation_ready_without_retry_rate below threshold: current={:.4}, required={:.4}",
                summary.mutation_ready_without_retry_rate, min
            ));
        }
    }
    if let Some(min) = thresholds.min_mutation_scope_aligned_rate {
        if summary.mutation_scope_aligned_rate + EPSILON < min {
            failures.push(format!(
                "{fixture}.route.mutation_scope_aligned_rate below threshold: current={:.4}, required={:.4}",
                summary.mutation_scope_aligned_rate, min
            ));
        }
    }
    if let Some(max) = thresholds.max_mutation_scope_incomplete_rate {
        if summary.mutation_scope_incomplete_rate > max + EPSILON {
            failures.push(format!(
                "{fixture}.route.mutation_scope_incomplete_rate above threshold: current={:.4}, allowed={:.4}",
                summary.mutation_scope_incomplete_rate, max
            ));
        }
    }
    if let Some(max) = thresholds.max_mutation_scope_missing_files_rate {
        if summary.mutation_scope_missing_files_rate > max + EPSILON {
            failures.push(format!(
                "{fixture}.route.mutation_scope_missing_files_rate above threshold: current={:.4}, allowed={:.4}",
                summary.mutation_scope_missing_files_rate, max
            ));
        }
    }
    if let Some(max) = thresholds.max_mutation_scope_extra_files_rate {
        if summary.mutation_scope_extra_files_rate > max + EPSILON {
            failures.push(format!(
                "{fixture}.route.mutation_scope_extra_files_rate above threshold: current={:.4}, allowed={:.4}",
                summary.mutation_scope_extra_files_rate, max
            ));
        }
    }
    if let Some(max) = thresholds.max_mutation_scope_missing_symbols_rate {
        if summary.mutation_scope_missing_symbols_rate > max + EPSILON {
            failures.push(format!(
                "{fixture}.route.mutation_scope_missing_symbols_rate above threshold: current={:.4}, allowed={:.4}",
                summary.mutation_scope_missing_symbols_rate, max
            ));
        }
    }
    if let Some(max) = thresholds.max_mutation_scope_extra_symbols_rate {
        if summary.mutation_scope_extra_symbols_rate > max + EPSILON {
            failures.push(format!(
                "{fixture}.route.mutation_scope_extra_symbols_rate above threshold: current={:.4}, allowed={:.4}",
                summary.mutation_scope_extra_symbols_rate, max
            ));
        }
    }
    if let Some(min) = thresholds.min_mutation_retry_recovered_rate {
        if summary.mutation_retry_recovered_rate + EPSILON < min {
            failures.push(format!(
                "{fixture}.route.mutation_retry_recovered_rate below threshold: current={:.4}, required={:.4}",
                summary.mutation_retry_recovered_rate, min
            ));
        }
    }
    if let Some(max) = thresholds.max_mutation_blocked_rate {
        if summary.mutation_blocked_rate > max + EPSILON {
            failures.push(format!(
                "{fixture}.route.mutation_blocked_rate above threshold: current={:.4}, allowed={:.4}",
                summary.mutation_blocked_rate, max
            ));
        }
    }
    if let Some(min) = thresholds.min_intent_match_rate {
        if summary.intent_match_rate + EPSILON < min {
            failures.push(format!(
                "{fixture}.route.intent_match_rate below threshold: current={:.4}, required={:.4}",
                summary.intent_match_rate, min
            ));
        }
    }
    if let Some(min) = thresholds.min_selected_tool_match_rate {
        if summary.selected_tool_match_rate + EPSILON < min {
            failures.push(format!(
                "{fixture}.route.selected_tool_match_rate below threshold: current={:.4}, required={:.4}",
                summary.selected_tool_match_rate, min
            ));
        }
    }
    if let Some(min) = thresholds.min_budget_match_rate {
        if summary.budget_match_rate + EPSILON < min {
            failures.push(format!(
                "{fixture}.route.budget_match_rate below threshold: current={:.4}, required={:.4}",
                summary.budget_match_rate, min
            ));
        }
    }
    if let Some(min) = thresholds.min_reference_only_match_rate {
        if summary.reference_only_match_rate + EPSILON < min {
            failures.push(format!(
                "{fixture}.route.reference_only_match_rate below threshold: current={:.4}, required={:.4}",
                summary.reference_only_match_rate, min
            ));
        }
    }
    if let Some(min) = thresholds.min_single_file_fast_path_match_rate {
        if summary.single_file_fast_path_match_rate + EPSILON < min {
            failures.push(format!(
                "{fixture}.route.single_file_fast_path_match_rate below threshold: current={:.4}, required={:.4}",
                summary.single_file_fast_path_match_rate, min
            ));
        }
    }
    if let Some(min) = thresholds.min_result_symbol_match_rate {
        if summary.result_symbol_match_rate + EPSILON < min {
            failures.push(format!(
                "{fixture}.route.result_symbol_match_rate below threshold: current={:.4}, required={:.4}",
                summary.result_symbol_match_rate, min
            ));
        }
    }
    if let Some(min) = thresholds.min_project_summary_match_rate {
        if summary.project_summary_match_rate + EPSILON < min {
            failures.push(format!(
                "{fixture}.route.project_summary_match_rate below threshold: current={:.4}, required={:.4}",
                summary.project_summary_match_rate, min
            ));
        }
    }
    if let Some(max) = thresholds.max_avg_latency_ms {
        if summary.avg_latency_ms > max + EPSILON {
            failures.push(format!(
                "{fixture}.route.avg_latency_ms above threshold: current={:.4}, allowed={:.4}",
                summary.avg_latency_ms, max
            ));
        }
    }
    if let Some(max) = thresholds.max_avg_tokens {
        if summary.avg_tokens > max + EPSILON {
            failures.push(format!(
                "{fixture}.route.avg_tokens above threshold: current={:.4}, allowed={:.4}",
                summary.avg_tokens, max
            ));
        }
    }
}

fn build_retrieval_summary(
    fixture_name: &str,
    repo_root: &Path,
    threshold_failures: &mut Vec<String>,
) -> Result<Option<RetrievalQualitySummary>> {
    let fixture = materialize_quality_fixture(fixture_name)?;
    if fixture.manifest().retrieval_cases.is_empty() {
        return Ok(None);
    }

    let temp_dir = tempfile::tempdir()?;
    let db = temp_dir.path().join("semantic.db");
    let idx = temp_dir.path().join("tantivy");
    let storage = Storage::open(&db, &idx)?;
    let mut indexer = Indexer::new(storage);
    indexer.index_repo(repo_root)?;
    let service = RetrievalService::new(repo_root.to_path_buf(), indexer.storage);

    let mut reports = Vec::new();
    for case in &fixture.manifest().retrieval_cases {
        let (result, latency_ms) = measure_retrieval_case(&service, case)?;
        let report = retrieval_case_report(
            &result,
            case,
            latency_ms,
        );
        compare_retrieval_case_thresholds(threshold_failures, fixture_name, case, &report);
        reports.push(report);
    }
    let summary = summarize_retrieval_reports(reports);
    if let Some(thresholds) = &fixture.manifest().retrieval_thresholds {
        compare_retrieval_thresholds(threshold_failures, fixture_name, thresholds, &summary);
    }
    Ok(Some(summary))
}

fn build_route_summary(
    fixture_name: &str,
    repo_root: &Path,
    threshold_failures: &mut Vec<String>,
) -> Result<Option<RouteQualitySummary>> {
    let fixture = materialize_quality_fixture(fixture_name)?;
    if fixture.manifest().route_cases.is_empty() {
        return Ok(None);
    }

    let runtime = AppRuntime::bootstrap(
        repo_root.to_path_buf(),
        RuntimeOptions {
            start_watcher: false,
            ensure_config: true,
            bootstrap_index_policy: semantic_app::BootstrapIndexPolicy::ReuseExistingOrCreate,
        },
    )?;

    let mut reports = Vec::new();
    for case in &fixture.manifest().route_cases {
        let (value, latency_ms) = measure_route_case(&runtime, case);
        let report = route_case_report(
            &value,
            case,
            latency_ms,
        );
        compare_route_case_thresholds(threshold_failures, fixture_name, case, &report);
        reports.push(report);
    }

    let summary = summarize_route_reports(reports);
    if let Some(thresholds) = &fixture.manifest().route_thresholds {
        compare_route_thresholds(threshold_failures, fixture_name, thresholds, &summary);
    }
    Ok(Some(summary))
}

fn measure_retrieval_case(
    service: &RetrievalService,
    case: &RetrievalCase,
) -> Result<(serde_json::Value, f64)> {
    for _ in 0..QUALITY_MEASUREMENT_WARMUP_RUNS {
        let _ = run_retrieval_case(service, case)?;
    }
    let mut best_result = None;
    let mut best_latency = f64::MAX;
    for _ in 0..QUALITY_MEASUREMENT_REPEATS {
        let started = Instant::now();
        let result = run_retrieval_case(service, case)?;
        let latency_ms = started.elapsed().as_secs_f64() * 1000.0;
        if latency_ms < best_latency {
            best_latency = latency_ms;
            best_result = Some(result);
        }
    }
    Ok((
        best_result.expect("retrieval measurement should produce a result"),
        best_latency,
    ))
}

fn measure_route_case(runtime: &AppRuntime, case: &RouteCase) -> (serde_json::Value, f64) {
    for _ in 0..QUALITY_MEASUREMENT_WARMUP_RUNS {
        let _ = runtime.handle_autoroute(semantic_app::models::IdeAutoRouteRequest {
            task: Some(case.task.clone()),
            include_summary: case.include_summary,
            raw_expansion_mode: None,
            auto_index_target: None,
            action: None,
            action_input: None,
            session_id: None,
            max_tokens: None,
            single_file_fast_path: None,
            reference_only: None,
            mapping_mode: None,
            max_footprint_items: None,
            reuse_session_context: None,
            auto_minimal_raw: None,
        });
    }
    let mut best_result = None;
    let mut best_latency = f64::MAX;
    for _ in 0..QUALITY_MEASUREMENT_REPEATS {
        let started = Instant::now();
        let value = runtime.handle_autoroute(semantic_app::models::IdeAutoRouteRequest {
            task: Some(case.task.clone()),
            include_summary: case.include_summary,
            raw_expansion_mode: None,
            auto_index_target: None,
            action: None,
            action_input: None,
            session_id: None,
            max_tokens: None,
            single_file_fast_path: None,
            reference_only: None,
            mapping_mode: None,
            max_footprint_items: None,
            reuse_session_context: None,
            auto_minimal_raw: None,
        });
        let latency_ms = started.elapsed().as_secs_f64() * 1000.0;
        if latency_ms < best_latency {
            best_latency = latency_ms;
            best_result = Some(value);
        }
    }
    (
        best_result.expect("route measurement should produce a result"),
        best_latency,
    )
}

fn run_retrieval_case(service: &RetrievalService, case: &RetrievalCase) -> Result<serde_json::Value> {
    let response = match case.operation.as_str() {
        "GetPlannedContext" => service.handle(RetrievalRequest {
            operation: Operation::GetPlannedContext,
            query: case.query.clone(),
            max_tokens: case.max_tokens,
            ..Default::default()
        })?,
        "GetReasoningContext" => service.handle(RetrievalRequest {
            operation: Operation::GetReasoningContext,
            name: case.symbol.clone(),
            query: case.query.clone(),
            logic_radius: case.logic_radius,
            dependency_radius: case.dependency_radius,
            ..Default::default()
        })?,
        "GetHybridRankedContext" => service.handle(RetrievalRequest {
            operation: Operation::GetHybridRankedContext,
            query: case.query.clone(),
            max_tokens: case.max_tokens,
            ..Default::default()
        })?,
        "SearchSymbol" => service.handle(RetrievalRequest {
            operation: Operation::SearchSymbol,
            name: case.symbol.clone(),
            limit: Some(10),
            ..Default::default()
        })?,
        other => return Err(anyhow!("unsupported retrieval case operation: {other}")),
    };
    Ok(response.result)
}

fn result_context_files(result: &serde_json::Value) -> Vec<String> {
    if let Some(items) = result.get("context").and_then(|v| v.as_array()) {
        return items
            .iter()
            .filter_map(|item| item.get("file").and_then(|v| v.as_str()).map(|s| s.to_string()))
            .collect();
    }
    if let Some(items) = result.get("ranked_context").and_then(|v| v.as_array()) {
        return items
            .iter()
            .filter_map(|item| item.get("file").and_then(|v| v.as_str()).map(|s| s.to_string()))
            .collect();
    }
    if let Some(items) = result.get("dependency_spans").and_then(|v| v.as_array()) {
        return items
            .iter()
            .filter_map(|item| {
                item.get("file").and_then(|v| v.as_str()).map(|s| s.to_string())
            })
            .chain(
                result
                    .get("logic_spans")
                    .and_then(|v| v.as_array())
                    .into_iter()
                    .flatten()
                    .filter_map(|item| item.get("file").and_then(|v| v.as_str()).map(|s| s.to_string())),
            )
            .collect();
    }
    if let Some(items) = result.get("fallback").and_then(|v| v.as_array()) {
        return items
            .iter()
            .filter_map(|item| item.get("file").and_then(|v| v.as_str()).map(|s| s.to_string()))
            .collect();
    }
    Vec::new()
}

fn retrieval_case_report(
    result: &serde_json::Value,
    case: &RetrievalCase,
    latency_ms: f64,
) -> RetrievalCaseReport {
    let actual_symbol = result
        .get("symbol")
        .and_then(|v| v.as_str())
        .or_else(|| {
            result
                .get("symbol")
                .and_then(|v| v.get("name"))
                .and_then(|v| v.as_str())
        });
    let target_match = case
        .expected_target_symbol
        .as_deref()
        .map(|expected| actual_symbol == Some(expected))
        .unwrap_or(true);

    let top_item = result
        .get("context")
        .and_then(|v| v.as_array())
        .and_then(|items| items.first())
        .or_else(|| {
            result
                .get("ranked_context")
                .and_then(|v| v.as_array())
                .and_then(|items| items.first())
        });
    let top_file_match = case
        .expected_top_file
        .as_deref()
        .map(|expected| {
            top_item
                .and_then(|item| item.get("file"))
                .and_then(|v| v.as_str())
                == Some(expected)
        })
        .unwrap_or(true);
    let top_span_match = case
        .expected_top_span
        .as_ref()
        .map(|expected| {
            top_item
                .and_then(|item| item.get("file"))
                .and_then(|v| v.as_str())
                == Some(expected.file.as_str())
                && top_item
                    .and_then(|item| item.get("start"))
                    .and_then(|v| v.as_u64())
                    == Some(expected.start_line as u64)
                && top_item
                    .and_then(|item| item.get("end"))
                    .and_then(|v| v.as_u64())
                    == Some(expected.end_line as u64)
        })
        .unwrap_or(true);

    let files = result_context_files(result);
    let omission_count = case
        .must_include_files
        .iter()
        .filter(|expected| !files.iter().any(|actual| actual == *expected))
        .count();
    let overfetch_count = case
        .must_not_include_files
        .iter()
        .filter(|forbidden| files.iter().any(|actual| actual == *forbidden))
        .count();

    RetrievalCaseReport {
        case_name: case.name.clone(),
        operation_bucket: case.operation.clone(),
        target_match,
        top_file_match,
        top_span_match,
        omission_count,
        overfetch_count,
        latency_ms,
        approx_tokens: estimate_tokens_json(result),
    }
}

fn route_case_report(
    value: &serde_json::Value,
    case: &RouteCase,
    latency_ms: f64,
) -> RouteCaseReport {
    let verification_status = value
        .get("verification")
        .and_then(|v| v.get("status"))
        .and_then(|v| v.as_str())
        .unwrap_or("missing")
        .to_string();
    let mutation_bundle_status = value
        .get("verification")
        .and_then(|v| v.get("mutation_bundle"))
        .and_then(|v| v.get("status"))
        .and_then(|v| v.as_str())
        .unwrap_or("missing")
        .to_string();
    let mutation_state = value
        .get("verification")
        .and_then(|v| v.get("mutation_state"))
        .and_then(|v| v.as_str())
        .unwrap_or("not_applicable")
        .to_string();
    let mutation_retry_recovered = value
        .get("verification")
        .and_then(|v| v.get("mutation_recovered_by_retry"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let mutation_ready_without_retry = value
        .get("verification")
        .and_then(|v| v.get("mutation_ready_without_retry"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let mutation_scope_aligned = value
        .get("verification")
        .map(|verification| {
            let file_scope = verification
                .get("exact_impact_scope_alignment")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let graph_scope = verification
                .get("exact_impact_scope_graph_alignment")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let graph_complete = verification
                .get("exact_impact_scope_graph_complete")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            file_scope && graph_scope && graph_complete
        })
        .unwrap_or(false);
    let mutation_scope_incomplete = value
        .get("verification")
        .and_then(|v| v.get("exact_impact_scope_graph_complete"))
        .and_then(|v| v.as_bool())
        .map(|is_complete| !is_complete)
        .unwrap_or(false);
    let mutation_scope_missing_files = value
        .get("verification")
        .and_then(|v| v.get("impact_scope_graph_details"))
        .and_then(|v| v.get("missing_files"))
        .and_then(|v| v.as_array())
        .map(|items| !items.is_empty())
        .unwrap_or(false);
    let mutation_scope_extra_files = value
        .get("verification")
        .and_then(|v| v.get("impact_scope_graph_details"))
        .and_then(|v| v.get("extra_files"))
        .and_then(|v| v.as_array())
        .map(|items| !items.is_empty())
        .unwrap_or(false);
    let mutation_scope_missing_symbols = value
        .get("verification")
        .and_then(|v| v.get("impact_scope_graph_details"))
        .and_then(|v| v.get("missing_symbols"))
        .and_then(|v| v.as_array())
        .map(|items| !items.is_empty())
        .unwrap_or(false);
    let mutation_scope_extra_symbols = value
        .get("verification")
        .and_then(|v| v.get("impact_scope_graph_details"))
        .and_then(|v| v.get("extra_symbols"))
        .and_then(|v| v.as_array())
        .map(|items| !items.is_empty())
        .unwrap_or(false);
    let mutation_ready = mutation_state == "ready";
    RouteCaseReport {
        case_name: case.name.clone(),
        intent_bucket: case.expected_intent.clone().unwrap_or_else(|| "unknown".to_string()),
        verification_status: verification_status.clone(),
        mutation_bundle_status,
        mutation_state,
        mutation_retry_recovered,
        mutation_ready,
        mutation_ready_without_retry,
        mutation_scope_aligned,
        mutation_scope_incomplete,
        mutation_scope_missing_files,
        mutation_scope_extra_files,
        mutation_scope_missing_symbols,
        mutation_scope_extra_symbols,
        intent_match: case
            .expected_intent
            .as_deref()
            .map(|expected| value.get("intent").and_then(|v| v.as_str()) == Some(expected))
            .unwrap_or(true),
        selected_tool_match: case
            .expected_selected_tool
            .as_deref()
            .map(|expected| value.get("selected_tool").and_then(|v| v.as_str()) == Some(expected))
            .unwrap_or(true),
        budget_match: case
            .expected_max_tokens
            .map(|expected| value.get("max_tokens").and_then(|v| v.as_u64()) == Some(expected as u64))
            .unwrap_or(true),
        reference_only_match: case
            .expected_reference_only
            .map(|expected| value.get("reference_only").and_then(|v| v.as_bool()) == Some(expected))
            .unwrap_or(true),
        single_file_fast_path_match: case
            .expected_single_file_fast_path
            .map(|expected| {
                value.get("single_file_fast_path").and_then(|v| v.as_bool()) == Some(expected)
            })
            .unwrap_or(true),
        result_symbol_match: case
            .expected_result_symbol
            .as_deref()
            .map(|expected| {
                value.get("result")
                    .and_then(|v| v.get("symbol"))
                    .and_then(|v| v.as_str())
                    == Some(expected)
            })
            .unwrap_or(true),
        project_summary_match: case
            .expected_project_summary
            .map(|expected| value.get("project_summary").map(|v| !v.is_null()) == Some(expected))
            .unwrap_or(true),
        reviewable_or_better: matches!(
            verification_status.as_str(),
            "high_confidence" | "needs_review"
        ),
        high_confidence: verification_status == "high_confidence",
        latency_ms,
        approx_tokens: estimate_route_payload_tokens(value),
    }
}

fn estimate_route_payload_tokens(value: &serde_json::Value) -> usize {
    let mut trimmed = value.clone();
    if let Some(obj) = trimmed.as_object_mut() {
        obj.remove("verification");
    }
    estimate_tokens_json(&trimmed)
}


