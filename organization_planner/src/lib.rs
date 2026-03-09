use anyhow::Result;
use change_propagation::ChangePropagation;
use engine::{EditContextItem, EditPlan, EditType};
use refactor_graph::{ExecutionOptions, RefactorGraph, RefactorNode};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

const ORG_STATUS_FILE: &str = ".semantic/org_refactor_status.json";
const ORG_TELEMETRY_FILE: &str = ".semantic/org_refactor_telemetry.jsonl";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrganizationRefactorPlan {
    pub origin_repo: String,
    pub ordered_repositories: Vec<String>,
    pub execution_batches: Vec<ExecutionBatch>,
    pub steps: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionBatch {
    pub stage: String,
    pub repositories: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OrganizationRefactorStatus {
    pub completed_repositories: Vec<String>,
    pub pending_repositories: Vec<String>,
    pub failed_repositories: Vec<String>,
    pub repository_update_status: HashMap<String, String>,
    pub execution_success: bool,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrganizationExecutionResult {
    pub status: OrganizationRefactorStatus,
    pub executed_batches: Vec<ExecutionBatch>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrganizationTelemetryEvent {
    pub timestamp: u64,
    pub event_type: String,
    pub origin_repo: String,
    pub repository: Option<String>,
    pub status: String,
    pub detail: String,
}

pub struct OrganizationPlanner;

impl OrganizationPlanner {
    pub fn plan(refactor_origin: &str, propagation: &ChangePropagation) -> OrganizationRefactorPlan {
        let mut repos = Vec::new();
        repos.push(refactor_origin.to_string());
        repos.extend(propagation.impacted_repositories.clone());

        let mut libraries = repos
            .iter()
            .filter(|r| r.contains("lib") || r.contains("sdk"))
            .cloned()
            .collect::<Vec<_>>();
        let mut services = repos
            .iter()
            .filter(|r| r.contains("service") || r.contains("api"))
            .cloned()
            .collect::<Vec<_>>();
        let mut clients = repos
            .iter()
            .filter(|r| r.contains("client") || r.contains("app"))
            .cloned()
            .collect::<Vec<_>>();

        libraries.sort();
        services.sort();
        clients.sort();

        let mut ordered_repositories = Vec::new();
        ordered_repositories.extend(libraries);
        ordered_repositories.extend(services);
        ordered_repositories.extend(clients);
        for r in repos {
            if !ordered_repositories.contains(&r) {
                ordered_repositories.push(r);
            }
        }

        let execution_batches = build_batches(&ordered_repositories);
        let mut steps = Vec::new();
        for batch in &execution_batches {
            if batch.repositories.is_empty() {
                continue;
            }
            steps.push(format!(
                "update {}: {}",
                batch.stage,
                batch.repositories.join(", ")
            ));
        }
        steps.push("update docs".to_string());

        OrganizationRefactorPlan {
            origin_repo: refactor_origin.to_string(),
            ordered_repositories,
            execution_batches,
            steps,
        }
    }

    pub fn build_multi_repo_refactor_graph(plan: &OrganizationRefactorPlan) -> RefactorGraph {
        let mut nodes = Vec::new();
        let mut previous_repo_test_node: Option<String> = None;

        for repo in &plan.ordered_repositories {
            let repo_node_id = format!("{repo}:repository");
            let service_node_id = format!("{repo}:service");
            let refactor_node_id = format!("{repo}:refactor");
            let test_node_id = format!("{repo}:tests");

            let mut repo_dependencies = Vec::new();
            if let Some(previous) = &previous_repo_test_node {
                repo_dependencies.push(previous.clone());
            }

            nodes.push(make_node(
                &repo_node_id,
                repo,
                "repository_scope",
                EditType::RenameSymbol,
                repo_dependencies,
            ));
            nodes.push(make_node(
                &service_node_id,
                repo,
                "service_scope",
                EditType::ChangeSignature,
                vec![repo_node_id.clone()],
            ));
            nodes.push(make_node(
                &refactor_node_id,
                repo,
                "refactor_scope",
                EditType::RefactorFunction,
                vec![service_node_id],
            ));
            nodes.push(make_node(
                &test_node_id,
                repo,
                "tests_scope",
                EditType::ModifyLogic,
                vec![refactor_node_id],
            ));
            previous_repo_test_node = Some(test_node_id);
        }

        RefactorGraph { nodes }
    }

    pub fn execute_distributed(
        repo_root: &Path,
        plan: &OrganizationRefactorPlan,
        propagation: &ChangePropagation,
    ) -> Result<OrganizationExecutionResult> {
        let mut status = OrganizationRefactorStatus {
            completed_repositories: vec![],
            pending_repositories: plan.ordered_repositories.clone(),
            failed_repositories: vec![],
            repository_update_status: plan
                .ordered_repositories
                .iter()
                .map(|repo| (repo.clone(), "pending".to_string()))
                .collect(),
            execution_success: false,
            last_error: None,
        };

        Self::append_telemetry(
            repo_root,
            &OrganizationTelemetryEvent {
                timestamp: now_ts(),
                event_type: "refactor_propagation".to_string(),
                origin_repo: plan.origin_repo.clone(),
                repository: None,
                status: "started".to_string(),
                detail: format!(
                    "origin={} impacted={}",
                    propagation.origin_repo,
                    propagation.impacted_repositories.join(",")
                ),
            },
        )?;

        for batch in &plan.execution_batches {
            for repo in &batch.repositories {
                if !status.pending_repositories.contains(repo) {
                    continue;
                }

                status
                    .repository_update_status
                    .insert(repo.clone(), "running".to_string());
                Self::append_telemetry(
                    repo_root,
                    &OrganizationTelemetryEvent {
                        timestamp: now_ts(),
                        event_type: "repository_update_status".to_string(),
                        origin_repo: plan.origin_repo.clone(),
                        repository: Some(repo.clone()),
                        status: "running".to_string(),
                        detail: format!("stage={}", batch.stage),
                    },
                )?;

                let single_repo_plan = OrganizationRefactorPlan {
                    origin_repo: repo.clone(),
                    ordered_repositories: vec![repo.clone()],
                    execution_batches: vec![ExecutionBatch {
                        stage: batch.stage.clone(),
                        repositories: vec![repo.clone()],
                    }],
                    steps: vec![format!("update repository '{repo}'")],
                };
                let graph = Self::build_multi_repo_refactor_graph(&single_repo_plan);
                let mut options = ExecutionOptions::default();
                options.auto_confirm_low_risk = true;
                options.auto_confirm_high_risk = true;

                match refactor_graph::execute_refactor(repo_root, graph, options) {
                    Ok(exec_status) if exec_status.failed_nodes.is_empty() => {
                        status.completed_repositories.push(repo.clone());
                        status.pending_repositories.retain(|r| r != repo);
                        status
                            .repository_update_status
                            .insert(repo.clone(), "completed".to_string());
                        Self::append_telemetry(
                            repo_root,
                            &OrganizationTelemetryEvent {
                                timestamp: now_ts(),
                                event_type: "execution_success".to_string(),
                                origin_repo: plan.origin_repo.clone(),
                                repository: Some(repo.clone()),
                                status: "success".to_string(),
                                detail: format!("completed nodes={}", exec_status.completed_nodes.len()),
                            },
                        )?;
                    }
                    Ok(exec_status) => {
                        status.failed_repositories.push(repo.clone());
                        status.pending_repositories.retain(|r| r != repo);
                        status
                            .repository_update_status
                            .insert(repo.clone(), "failed".to_string());
                        status.last_error = exec_status.last_error;
                        Self::append_telemetry(
                            repo_root,
                            &OrganizationTelemetryEvent {
                                timestamp: now_ts(),
                                event_type: "execution_success".to_string(),
                                origin_repo: plan.origin_repo.clone(),
                                repository: Some(repo.clone()),
                                status: "failed".to_string(),
                                detail: "phase7 executor reported failed nodes".to_string(),
                            },
                        )?;
                    }
                    Err(err) => {
                        status.failed_repositories.push(repo.clone());
                        status.pending_repositories.retain(|r| r != repo);
                        status
                            .repository_update_status
                            .insert(repo.clone(), "failed".to_string());
                        status.last_error = Some(err.to_string());
                        Self::append_telemetry(
                            repo_root,
                            &OrganizationTelemetryEvent {
                                timestamp: now_ts(),
                                event_type: "execution_success".to_string(),
                                origin_repo: plan.origin_repo.clone(),
                                repository: Some(repo.clone()),
                                status: "error".to_string(),
                                detail: err.to_string(),
                            },
                        )?;
                    }
                }
                Self::write_status(repo_root, &status)?;
            }
        }

        status.execution_success = status.failed_repositories.is_empty();
        Self::write_status(repo_root, &status)?;

        Ok(OrganizationExecutionResult {
            status,
            executed_batches: plan.execution_batches.clone(),
        })
    }

    pub fn write_status(repo_root: &Path, status: &OrganizationRefactorStatus) -> Result<()> {
        let path = repo_root.join(ORG_STATUS_FILE);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, serde_json::to_string_pretty(status)?)?;
        Ok(())
    }

    pub fn read_status(repo_root: &Path) -> Result<OrganizationRefactorStatus> {
        let path = repo_root.join(ORG_STATUS_FILE);
        if !path.exists() {
            return Ok(OrganizationRefactorStatus::default());
        }
        let raw = fs::read_to_string(path)?;
        Ok(serde_json::from_str(&raw).unwrap_or_default())
    }

    pub fn append_telemetry(repo_root: &Path, event: &OrganizationTelemetryEvent) -> Result<()> {
        let path = repo_root.join(ORG_TELEMETRY_FILE);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut existing = String::new();
        if path.exists() {
            existing = fs::read_to_string(&path)?;
        }
        existing.push_str(&serde_json::to_string(event)?);
        existing.push('\n');
        fs::write(path, existing)?;
        Ok(())
    }

    pub fn read_telemetry(repo_root: &Path) -> Result<Vec<OrganizationTelemetryEvent>> {
        let path = repo_root.join(ORG_TELEMETRY_FILE);
        if !path.exists() {
            return Ok(Vec::new());
        }
        let raw = fs::read_to_string(path)?;
        let mut out = Vec::new();
        for line in raw.lines() {
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(event) = serde_json::from_str::<OrganizationTelemetryEvent>(line) {
                out.push(event);
            }
        }
        Ok(out)
    }
}

fn make_node(
    node_id: &str,
    repo_name: &str,
    stage: &str,
    edit_type: EditType,
    dependencies: Vec<String>,
) -> RefactorNode {
    RefactorNode {
        id: node_id.to_string(),
        target_symbol: format!("{repo_name}_{stage}"),
        edit_plan: EditPlan {
            target_symbol: format!("{repo_name}_{stage}"),
            edit_type,
            impacted_symbols: vec![],
            required_context: vec![EditContextItem {
                file_path: format!("organization/{repo_name}/{stage}.ts"),
                start_line: 1,
                end_line: 1,
                priority: 1,
                text: "organization-level refactor orchestration".to_string(),
            }],
        },
        dependencies,
        repository: None,
    }
}

fn build_batches(ordered_repositories: &[String]) -> Vec<ExecutionBatch> {
    let mut libraries = Vec::new();
    let mut services = Vec::new();
    let mut clients = Vec::new();
    let mut others = Vec::new();

    for repo in ordered_repositories {
        if repo.contains("lib") || repo.contains("sdk") {
            libraries.push(repo.clone());
        } else if repo.contains("service") || repo.contains("api") {
            services.push(repo.clone());
        } else if repo.contains("client") || repo.contains("app") {
            clients.push(repo.clone());
        } else {
            others.push(repo.clone());
        }
    }

    vec![
        ExecutionBatch {
            stage: "libraries".to_string(),
            repositories: libraries,
        },
        ExecutionBatch {
            stage: "services".to_string(),
            repositories: services,
        },
        ExecutionBatch {
            stage: "clients".to_string(),
            repositories: clients,
        },
        ExecutionBatch {
            stage: "others".to_string(),
            repositories: others,
        },
    ]
}

fn now_ts() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}
