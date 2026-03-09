use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

const DEPLOYMENT_HISTORY_FILE: &str = ".semantic/deployment_history.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineStage {
    pub stage_name: String,
    pub dependencies: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PipelineGraph {
    pub stages: Vec<PipelineStage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineAnalysisRequest {
    pub failure_stage: String,
    pub failure_message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PipelineAnalysisResult {
    pub linked_debug_context: Option<debug_graph::DebugAnalysisState>,
    pub slow_stages: Vec<String>,
    pub redundant_builds: Vec<String>,
    pub unused_tests: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeploymentRecord {
    pub version: String,
    pub environment: String,
    pub deployed_at: u64,
    pub rollback_from_version: Option<String>,
    pub config_hash: String,
}

pub struct PipelineIntelligence;

impl PipelineIntelligence {
    pub fn default_graph() -> PipelineGraph {
        PipelineGraph {
            stages: vec![
                PipelineStage {
                    stage_name: "checkout".to_string(),
                    dependencies: vec![],
                },
                PipelineStage {
                    stage_name: "build".to_string(),
                    dependencies: vec!["checkout".to_string()],
                },
                PipelineStage {
                    stage_name: "unit_tests".to_string(),
                    dependencies: vec!["build".to_string()],
                },
                PipelineStage {
                    stage_name: "integration_tests".to_string(),
                    dependencies: vec!["unit_tests".to_string()],
                },
                PipelineStage {
                    stage_name: "deploy".to_string(),
                    dependencies: vec!["integration_tests".to_string()],
                },
            ],
        }
    }

    pub fn analyze(
        repo_root: &Path,
        request: &PipelineAnalysisRequest,
    ) -> Result<PipelineAnalysisResult> {
        let debug_context = debug_graph::DebugGraphEngine::read_state(repo_root).ok();
        let graph = Self::default_graph();
        let mut slow_stages = Vec::new();
        let mut redundant_builds = Vec::new();
        let mut unused_tests = Vec::new();

        for stage in &graph.stages {
            if stage.stage_name.contains("integration") {
                slow_stages.push(stage.stage_name.clone());
            }
            if stage.stage_name.contains("build") && request.failure_stage.contains("test") {
                redundant_builds.push(stage.stage_name.clone());
            }
            if stage.stage_name.contains("test")
                && request.failure_message.to_lowercase().contains("timeout")
            {
                unused_tests.push(stage.stage_name.clone());
            }
        }

        Ok(PipelineAnalysisResult {
            linked_debug_context: debug_context,
            slow_stages,
            redundant_builds,
            unused_tests,
        })
    }

    pub fn list_deployments(repo_root: &Path) -> Result<Vec<DeploymentRecord>> {
        let path = repo_root.join(DEPLOYMENT_HISTORY_FILE);
        if !path.exists() {
            let seed = vec![DeploymentRecord {
                version: "v0.1.0".to_string(),
                environment: "staging".to_string(),
                deployed_at: now_ts(),
                rollback_from_version: None,
                config_hash: "default".to_string(),
            }];
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&path, serde_json::to_string_pretty(&seed)?)?;
            return Ok(seed);
        }
        let raw = fs::read_to_string(path)?;
        Ok(serde_json::from_str(&raw).unwrap_or_default())
    }
}

fn now_ts() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}
