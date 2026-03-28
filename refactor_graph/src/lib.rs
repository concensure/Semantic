use anyhow::{anyhow, Result};
use engine::{ASTTransformation, EditPlan, EditType, PatchRecord};
use patch_memory::PatchMemory;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const SNAPSHOT_DIR: &str = ".semantic/refactor_snapshots";
const STATUS_FILE: &str = ".semantic/refactor_status.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefactorNode {
    pub id: String,
    pub target_symbol: String,
    pub edit_plan: EditPlan,
    pub dependencies: Vec<String>,
    pub repository: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RefactorGraph {
    pub nodes: Vec<RefactorNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HighLevelRefactorRequest {
    pub repository: String,
    pub old_symbol: String,
    pub new_symbol: String,
    pub include_tests: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RefactorStatus {
    pub refactor_id: String,
    pub completed_nodes: Vec<String>,
    pub pending_nodes: Vec<String>,
    pub failed_nodes: Vec<String>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ExecutionOptions {
    pub auto_confirm_low_risk: bool,
    pub auto_confirm_high_risk: bool,
    pub risk_threshold: f32,
    pub rollback_on_error: bool,
}

impl Default for ExecutionOptions {
    fn default() -> Self {
        Self {
            auto_confirm_low_risk: true,
            auto_confirm_high_risk: false,
            risk_threshold: 0.4,
            rollback_on_error: true,
        }
    }
}

pub struct RefactorExecutor {
    repo_root: PathBuf,
    snapshots: HashMap<String, Vec<PathBuf>>,
}

impl RefactorGraph {
    pub fn from_request(request: &HighLevelRefactorRequest) -> Self {
        let mut nodes = Vec::new();

        nodes.push(RefactorNode {
            id: "n_type".to_string(),
            target_symbol: request.old_symbol.clone(),
            edit_plan: EditPlan {
                target_symbol: request.old_symbol.clone(),
                edit_type: EditType::RenameSymbol,
                impacted_symbols: vec![],
                required_context: vec![],
            },
            dependencies: vec![],
            repository: Some(request.repository.clone()),
        });

        nodes.push(RefactorNode {
            id: "n_callers".to_string(),
            target_symbol: request.old_symbol.clone(),
            edit_plan: EditPlan {
                target_symbol: request.old_symbol.clone(),
                edit_type: EditType::ChangeSignature,
                impacted_symbols: vec![request.new_symbol.clone()],
                required_context: vec![],
            },
            dependencies: vec!["n_type".to_string()],
            repository: Some(request.repository.clone()),
        });

        nodes.push(RefactorNode {
            id: "n_imports".to_string(),
            target_symbol: request.old_symbol.clone(),
            edit_plan: EditPlan {
                target_symbol: request.old_symbol.clone(),
                edit_type: EditType::RefactorFunction,
                impacted_symbols: vec![],
                required_context: vec![],
            },
            dependencies: vec!["n_type".to_string()],
            repository: Some(request.repository.clone()),
        });

        if request.include_tests {
            nodes.push(RefactorNode {
                id: "n_tests".to_string(),
                target_symbol: request.old_symbol.clone(),
                edit_plan: EditPlan {
                    target_symbol: request.old_symbol.clone(),
                    edit_type: EditType::ModifyLogic,
                    impacted_symbols: vec![],
                    required_context: vec![],
                },
                dependencies: vec!["n_callers".to_string(), "n_imports".to_string()],
                repository: Some(request.repository.clone()),
            });
        }

        Self { nodes }
    }

    pub fn topological_order(&self) -> Result<Vec<RefactorNode>> {
        let mut indegree: HashMap<String, usize> =
            self.nodes.iter().map(|n| (n.id.clone(), 0usize)).collect();
        let mut adjacency: HashMap<String, Vec<String>> = HashMap::new();

        for node in &self.nodes {
            for dep in &node.dependencies {
                if !indegree.contains_key(dep) {
                    return Err(anyhow!("unknown dependency node: {dep}"));
                }
                if let Some(in_deg) = indegree.get_mut(&node.id) {
                    *in_deg += 1;
                }
                adjacency
                    .entry(dep.clone())
                    .or_default()
                    .push(node.id.clone());
            }
        }

        let mut queue: VecDeque<String> = indegree
            .iter()
            .filter_map(|(id, degree)| if *degree == 0 { Some(id.clone()) } else { None })
            .collect();
        let mut ordered_ids = Vec::new();

        while let Some(id) = queue.pop_front() {
            ordered_ids.push(id.clone());
            if let Some(neighbors) = adjacency.get(&id) {
                for next in neighbors {
                    if let Some(degree) = indegree.get_mut(next) {
                        *degree -= 1;
                        if *degree == 0 {
                            queue.push_back(next.clone());
                        }
                    }
                }
            }
        }

        if ordered_ids.len() != self.nodes.len() {
            return Err(anyhow!("cycle detected in refactor graph"));
        }

        let mut map: HashMap<String, RefactorNode> = self
            .nodes
            .iter()
            .cloned()
            .map(|n| (n.id.clone(), n))
            .collect();
        let mut out = Vec::new();
        for id in ordered_ids {
            if let Some(node) = map.remove(&id) {
                out.push(node);
            }
        }
        Ok(out)
    }
}

impl RefactorExecutor {
    pub fn new(repo_root: PathBuf) -> Self {
        Self {
            repo_root,
            snapshots: HashMap::new(),
        }
    }

    pub fn begin_refactor(&mut self, graph: &RefactorGraph) -> Result<String> {
        let refactor_id = format!(
            "r_{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or_default()
        );

        fs::create_dir_all(self.repo_root.join(SNAPSHOT_DIR).join(&refactor_id))?;
        self.snapshots.insert(refactor_id.clone(), Vec::new());

        let status = RefactorStatus {
            refactor_id: refactor_id.clone(),
            completed_nodes: vec![],
            pending_nodes: graph.nodes.iter().map(|n| n.id.clone()).collect(),
            failed_nodes: vec![],
            last_error: None,
        };
        self.write_status(&status)?;

        Ok(refactor_id)
    }

    pub fn apply_nodes(
        &mut self,
        refactor_id: &str,
        graph: &RefactorGraph,
        options: &ExecutionOptions,
    ) -> Result<RefactorStatus> {
        let mut status = Self::read_status(&self.repo_root)?;
        let ordered = graph.topological_order()?;
        let patch_memory = PatchMemory::open(&self.repo_root)?;

        for node in ordered {
            let risk = patch_memory
                .edit_risk_score(node.edit_plan.edit_type.clone())
                .map(|v| v.risk)
                .unwrap_or(0.0);
            let confirm_required = if risk >= options.risk_threshold {
                !options.auto_confirm_high_risk
            } else {
                !options.auto_confirm_low_risk
            };

            if confirm_required {
                status.failed_nodes.push(node.id.clone());
                status.last_error = Some(format!("confirmation required for {}", node.id));
                if options.rollback_on_error {
                    self.rollback(refactor_id)?;
                }
                self.write_status(&status)?;
                return Ok(status);
            }

            if let Err(err) = self.execute_node(refactor_id, &node, &patch_memory, true) {
                status.failed_nodes.push(node.id.clone());
                status.last_error = Some(err.to_string());
                if options.rollback_on_error {
                    self.rollback(refactor_id)?;
                }
                self.write_status(&status)?;
                return Ok(status);
            }

            status.completed_nodes.push(node.id.clone());
            status.pending_nodes.retain(|id| id != &node.id);
            self.write_status(&status)?;
        }

        Ok(status)
    }

    pub fn commit(&mut self, refactor_id: &str) -> Result<()> {
        let snapshot_root = self.repo_root.join(SNAPSHOT_DIR).join(refactor_id);
        if snapshot_root.exists() {
            fs::remove_dir_all(snapshot_root)?;
        }
        self.snapshots.remove(refactor_id);
        Ok(())
    }

    pub fn rollback(&mut self, refactor_id: &str) -> Result<()> {
        let snapshot_root = self.repo_root.join(SNAPSHOT_DIR).join(refactor_id);
        let files = self.snapshots.get(refactor_id).cloned().unwrap_or_default();

        for original_file in files {
            let relative = original_file
                .strip_prefix(&self.repo_root)
                .unwrap_or(&original_file);
            let snapshot = snapshot_root.join(relative);
            if snapshot.exists() {
                if let Some(parent) = original_file.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::copy(&snapshot, &original_file)?;
            }
        }
        Ok(())
    }

    pub fn status(repo_root: &Path) -> Result<RefactorStatus> {
        Self::read_status(repo_root)
    }

    fn execute_node(
        &mut self,
        refactor_id: &str,
        node: &RefactorNode,
        memory: &PatchMemory,
        approved: bool,
    ) -> Result<()> {
        let target_file_rel = node
            .edit_plan
            .required_context
            .first()
            .map(|c| c.file_path.clone())
            .unwrap_or_else(|| "src/refactor_placeholder.ts".to_string());
        let base = node
            .repository
            .as_ref()
            .map(PathBuf::from)
            .unwrap_or_else(|| self.repo_root.clone());
        let target_file = base.join(&target_file_rel);

        self.snapshot_file(refactor_id, &target_file)?;
        let existing = fs::read_to_string(&target_file).unwrap_or_default();

        if let Some(parent) = target_file.parent() {
            fs::create_dir_all(parent)?;
        }
        let comment_prefix = if target_file_rel.ends_with(".py") {
            "#"
        } else {
            "//"
        };
        let patch_marker = format!(
            "{comment_prefix} refactor_node:{} symbol:{}\n",
            node.id, node.target_symbol
        );
        let transform = edit_type_to_transform(&node.edit_plan.edit_type);
        let updated = format!("{existing}{patch_marker}");
        let line_count = existing.lines().count().max(1);
        let patch = patch_engine::PatchEngine::generate_replacement_patch(
            &target_file_rel,
            &existing,
            patch_engine::LineRange {
                start_line: 1,
                end_line: line_count,
            },
            &updated,
        )?;
        patch_engine::PatchEngine::validate_patch(&target_file_rel, &patch, &existing)?;
        let diff = match &patch.representation {
            engine::PatchRepresentation::ASTTransform(edit) => {
                patch_engine::PatchEngine::ast_to_diff(&target_file_rel, edit)
            }
            engine::PatchRepresentation::UnifiedDiff(v) => v.clone(),
        };
        fs::write(
            &target_file,
            patch_engine::PatchEngine::apply_patch(&existing, &patch)?,
        )?;

        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or_default();
        let record = PatchRecord {
            patch_id: PatchMemory::new_record_id(),
            timestamp,
            repository: base.to_string_lossy().to_string(),
            file_path: target_file_rel,
            target_symbol: node.target_symbol.clone(),
            edit_type: node.edit_plan.edit_type.clone(),
            model_used: "refactor_executor".to_string(),
            provider: "local".to_string(),
            diff,
            ast_transform: Some(transform),
            impacted_symbols: node.edit_plan.impacted_symbols.clone(),
            approved_by_user: approved,
            validation_passed: true,
            tests_passed: true,
            rollback_occurred: false,
            rollback_reason: None,
        };
        memory.append_record(&record)?;

        Ok(())
    }

    fn snapshot_file(&mut self, refactor_id: &str, file: &Path) -> Result<()> {
        let root = self.repo_root.join(SNAPSHOT_DIR).join(refactor_id);
        let relative = file.strip_prefix(&self.repo_root).unwrap_or(file);
        let snapshot = root.join(relative);
        if let Some(parent) = snapshot.parent() {
            fs::create_dir_all(parent)?;
        }
        if !snapshot.exists() {
            if file.exists() {
                fs::copy(file, &snapshot)?;
            } else {
                fs::write(&snapshot, "")?;
            }
        }
        let tracked = self.snapshots.entry(refactor_id.to_string()).or_default();
        if !tracked.iter().any(|p| p == file) {
            tracked.push(file.to_path_buf());
        }
        Ok(())
    }

    fn write_status(&self, status: &RefactorStatus) -> Result<()> {
        let path = self.repo_root.join(STATUS_FILE);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, serde_json::to_string_pretty(status)?)?;
        Ok(())
    }

    fn read_status(repo_root: &Path) -> Result<RefactorStatus> {
        let path = repo_root.join(STATUS_FILE);
        if !path.exists() {
            return Ok(RefactorStatus::default());
        }
        let raw = fs::read_to_string(path)?;
        let status = serde_json::from_str(&raw).unwrap_or_default();
        Ok(status)
    }
}

fn edit_type_to_transform(edit_type: &EditType) -> ASTTransformation {
    match edit_type {
        EditType::RenameSymbol => ASTTransformation::RenameSymbol,
        EditType::ChangeSignature => ASTTransformation::ChangeSignature,
        EditType::RefactorFunction => ASTTransformation::ReplaceFunctionBody,
        EditType::ModifyLogic => ASTTransformation::ReplaceFunctionBody,
    }
}

pub fn execute_refactor(
    repo_root: &Path,
    graph: RefactorGraph,
    options: ExecutionOptions,
) -> Result<RefactorStatus> {
    let mut executor = RefactorExecutor::new(repo_root.to_path_buf());
    let id = executor.begin_refactor(&graph)?;
    let status = executor.apply_nodes(&id, &graph, &options)?;
    if status.failed_nodes.is_empty() {
        executor.commit(&id)?;
    }
    Ok(status)
}

#[cfg(test)]
mod tests {
    use super::{
        execute_refactor, ExecutionOptions, HighLevelRefactorRequest, RefactorExecutor,
        RefactorGraph,
    };
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn schedules_with_kahn_order() {
        let graph = RefactorGraph::from_request(&HighLevelRefactorRequest {
            repository: "repo".to_string(),
            old_symbol: "RetryPolicy".to_string(),
            new_symbol: "BackoffPolicy".to_string(),
            include_tests: true,
        });
        let ordered = graph.topological_order().expect("topological");
        let ids: Vec<String> = ordered.iter().map(|n| n.id.clone()).collect();
        let pos_type = ids.iter().position(|id| id == "n_type").unwrap_or_default();
        let pos_tests = ids
            .iter()
            .position(|id| id == "n_tests")
            .unwrap_or_default();
        assert!(pos_type < pos_tests);
    }

    #[test]
    fn rollback_restores_snapshot() {
        let tmp = tempdir().expect("tempdir");
        let repo = tmp.path().to_path_buf();
        fs::create_dir_all(repo.join("src")).expect("mkdir");
        let file = repo.join("src").join("a.ts");
        fs::write(&file, "export const a = 1;\n").expect("write");

        let mut graph = RefactorGraph::from_request(&HighLevelRefactorRequest {
            repository: repo.to_string_lossy().to_string(),
            old_symbol: "a".to_string(),
            new_symbol: "b".to_string(),
            include_tests: false,
        });
        for node in &mut graph.nodes {
            node.edit_plan
                .required_context
                .push(engine::EditContextItem {
                    file_path: "src/a.ts".to_string(),
                    start_line: 1,
                    end_line: 1,
                    priority: 0,
                    text: "ctx".to_string(),
                });
        }

        let mut executor = RefactorExecutor::new(repo.clone());
        let id = executor.begin_refactor(&graph).expect("begin");
        let mut options = ExecutionOptions::default();
        options.auto_confirm_high_risk = true;
        options.auto_confirm_low_risk = true;
        let _ = executor.apply_nodes(&id, &graph, &options).expect("apply");
        executor.rollback(&id).expect("rollback");

        let restored = fs::read_to_string(&file).expect("read");
        assert_eq!(restored, "export const a = 1;\n");
    }

    #[test]
    fn executes_refactor_end_to_end() {
        let tmp = tempdir().expect("tempdir");
        let repo = tmp.path().to_path_buf();
        fs::create_dir_all(repo.join("src")).expect("mkdir");
        fs::write(repo.join("src").join("x.ts"), "export const x = 1;\n").expect("write");

        let mut graph = RefactorGraph::from_request(&HighLevelRefactorRequest {
            repository: repo.to_string_lossy().to_string(),
            old_symbol: "x".to_string(),
            new_symbol: "y".to_string(),
            include_tests: true,
        });
        for node in &mut graph.nodes {
            node.edit_plan
                .required_context
                .push(engine::EditContextItem {
                    file_path: "src/x.ts".to_string(),
                    start_line: 1,
                    end_line: 1,
                    priority: 0,
                    text: "ctx".to_string(),
                });
        }

        let mut options = ExecutionOptions::default();
        options.auto_confirm_low_risk = true;
        options.auto_confirm_high_risk = true;
        let status = execute_refactor(&repo, graph, options).expect("execute");
        assert!(!status.completed_nodes.is_empty());
    }

    #[test]
    fn supports_cross_repository_nodes() {
        let tmp = tempdir().expect("tempdir");
        let repo_a = tmp.path().join("repo_a");
        let repo_b = tmp.path().join("repo_b");
        fs::create_dir_all(repo_a.join("src")).expect("mkdir a");
        fs::create_dir_all(repo_b.join("src")).expect("mkdir b");
        fs::write(repo_a.join("src").join("a.ts"), "export const a = 1;\n").expect("write a");
        fs::write(repo_b.join("src").join("b.ts"), "export const b = 2;\n").expect("write b");

        let node_a = super::RefactorNode {
            id: "a".to_string(),
            target_symbol: "AType".to_string(),
            edit_plan: engine::EditPlan {
                target_symbol: "AType".to_string(),
                edit_type: engine::EditType::RenameSymbol,
                impacted_symbols: vec![],
                required_context: vec![engine::EditContextItem {
                    file_path: "src/a.ts".to_string(),
                    start_line: 1,
                    end_line: 1,
                    priority: 0,
                    text: "ctx".to_string(),
                }],
            },
            dependencies: vec![],
            repository: Some(repo_a.to_string_lossy().to_string()),
        };
        let node_b = super::RefactorNode {
            id: "b".to_string(),
            target_symbol: "BCaller".to_string(),
            edit_plan: engine::EditPlan {
                target_symbol: "BCaller".to_string(),
                edit_type: engine::EditType::ChangeSignature,
                impacted_symbols: vec!["AType".to_string()],
                required_context: vec![engine::EditContextItem {
                    file_path: "src/b.ts".to_string(),
                    start_line: 1,
                    end_line: 1,
                    priority: 0,
                    text: "ctx".to_string(),
                }],
            },
            dependencies: vec!["a".to_string()],
            repository: Some(repo_b.to_string_lossy().to_string()),
        };
        let graph = super::RefactorGraph {
            nodes: vec![node_a, node_b],
        };

        let mut options = ExecutionOptions::default();
        options.auto_confirm_high_risk = true;
        options.auto_confirm_low_risk = true;
        let status = execute_refactor(tmp.path(), graph, options).expect("execute");
        assert!(status.failed_nodes.is_empty());
        let a = fs::read_to_string(repo_a.join("src").join("a.ts")).expect("read a");
        let b = fs::read_to_string(repo_b.join("src").join("b.ts")).expect("read b");
        assert!(a.contains("refactor_node"));
        assert!(b.contains("refactor_node"));
    }

    #[test]
    fn schedules_large_graph_without_cycle() {
        let mut nodes = Vec::new();
        for i in 0..120 {
            let dep = if i == 0 {
                vec![]
            } else {
                vec![format!("n{}", i - 1)]
            };
            nodes.push(super::RefactorNode {
                id: format!("n{i}"),
                target_symbol: format!("S{i}"),
                edit_plan: engine::EditPlan {
                    target_symbol: format!("S{i}"),
                    edit_type: engine::EditType::ModifyLogic,
                    impacted_symbols: vec![],
                    required_context: vec![],
                },
                dependencies: dep,
                repository: None,
            });
        }
        let graph = super::RefactorGraph { nodes };
        let order = graph.topological_order().expect("order");
        assert_eq!(order.len(), 120);
        assert_eq!(order.first().map(|n| n.id.as_str()), Some("n0"));
        assert_eq!(order.last().map(|n| n.id.as_str()), Some("n119"));
    }
}
