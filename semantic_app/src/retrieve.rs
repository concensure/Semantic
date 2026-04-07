use crate::models::RetrieveRequestBody;
use crate::runtime::summarize_indexed_path_hints;
use crate::runtime::AppRuntime;
use crate::session::{
    apply_session_context_reuse, apply_session_raw_expansion_controls, touch_or_create_session,
    RawExpansionMode,
};
use anyhow::Result;
use change_propagation::ChangePropagationEngine;
use dependency_intelligence::DependencyIntelligence;
use engine::{Operation, RetrievalResponse};
use knowledge_graph::KnowledgeEntry;
use org_graph::OrganizationGraphBuilder;
use tracing::error;

impl AppRuntime {
    pub fn handle_retrieve(&self, body: RetrieveRequestBody) -> serde_json::Value {
        match self.try_handle_retrieve(body) {
            Ok(value) => value,
            Err(err) => {
                error!("retrieve failed: {err}");
                serde_json::json!({"ok": false, "error": err.to_string()})
            }
        }
    }

    fn try_handle_retrieve(&self, body: RetrieveRequestBody) -> Result<serde_json::Value> {
        let retry_body_template = body.clone();
        match &body.request.operation {
            Operation::GetControlFlowHints => {
                let symbol = body
                    .request
                    .name
                    .clone()
                    .or_else(|| body.request.query.clone())
                    .unwrap_or_default();
                return Ok(
                    match self.retrieval().lock().get_control_flow_hints(&symbol) {
                        Ok(result) => {
                            serde_json::json!({"ok": true, "operation": "get_control_flow_hints", "result": result})
                        }
                        Err(err) => serde_json::json!({"ok": false, "error": err.to_string()}),
                    },
                );
            }
            Operation::GetDataFlowHints => {
                let symbol = body
                    .request
                    .name
                    .clone()
                    .or_else(|| body.request.query.clone())
                    .unwrap_or_default();
                return Ok(match self.retrieval().lock().get_data_flow_hints(&symbol) {
                    Ok(result) => {
                        serde_json::json!({"ok": true, "operation": "get_data_flow_hints", "result": result})
                    }
                    Err(err) => serde_json::json!({"ok": false, "error": err.to_string()}),
                });
            }
            Operation::GetHybridRankedContext => {
                let query = body.request.query.clone().unwrap_or_default();
                let max_tokens = body.request.max_tokens.unwrap_or(1400);
                let single_file_fast_path = body.single_file_fast_path.unwrap_or(true);
                return Ok(
                    match self.retrieval().lock().get_hybrid_ranked_context(
                        &query,
                        max_tokens,
                        single_file_fast_path,
                    ) {
                        Ok(result) => {
                            serde_json::json!({"ok": true, "operation": "get_hybrid_ranked_context", "result": result})
                        }
                        Err(err) => serde_json::json!({"ok": false, "error": err.to_string()}),
                    },
                );
            }
            Operation::GetDebugGraph => {
                return Ok(self
                    .simple_result("get_debug_graph", self.retrieval().lock().get_debug_graph()));
            }
            Operation::GetPipelineGraph => {
                return Ok(self.simple_result(
                    "get_pipeline_graph",
                    self.retrieval().lock().get_pipeline_graph(),
                ));
            }
            Operation::GetRootCauseCandidates => {
                return Ok(self.simple_result(
                    "get_root_cause_candidates",
                    self.retrieval().lock().get_root_cause_candidates(),
                ));
            }
            Operation::GetTestGaps => {
                return Ok(
                    self.simple_result("get_test_gaps", self.retrieval().lock().get_test_gaps())
                );
            }
            Operation::GetDeploymentHistory => {
                return Ok(self.simple_result(
                    "get_deployment_history",
                    self.retrieval().lock().get_deployment_history(),
                ));
            }
            Operation::GetPerformanceStats => {
                return Ok(serde_json::json!({
                    "ok": true,
                    "operation": "get_performance_stats",
                    "result": self.retrieval().lock().get_performance_stats()
                }));
            }
            Operation::GetProjectSummary => {
                let max_tokens = body.request.max_tokens.unwrap_or(800);
                let result = self.retrieval().lock().with_storage(|storage| {
                    project_summariser::ProjectSummariser::new(storage).build(max_tokens)
                });
                return Ok(match result {
                    Ok(doc) => serde_json::json!({
                        "ok": true,
                        "operation": "get_project_summary",
                        "token_estimate": doc.token_estimate,
                        "cache_hit": doc.cache_hit,
                        "summary": doc.to_json(),
                        "summary_text": doc.summary_text,
                    }),
                    Err(err) => serde_json::json!({"ok": false, "error": err.to_string()}),
                });
            }
            Operation::GetDirectoryBrief => {
                let path = body
                    .request
                    .path
                    .clone()
                    .or(body.request.file.clone())
                    .unwrap_or_default();
                return Ok(
                    match self.retrieval().lock().get_directory_brief(Some(&path)) {
                        Ok(result) => {
                            serde_json::json!({"ok": true, "operation": "get_directory_brief", "result": result})
                        }
                        Err(err) => serde_json::json!({"ok": false, "error": err.to_string()}),
                    },
                );
            }
            Operation::GetFileBrief => {
                let file = body
                    .request
                    .file
                    .clone()
                    .or(body.request.path.clone())
                    .unwrap_or_default();
                return Ok(match self.retrieval().lock().get_file_brief(&file) {
                    Ok(result) => {
                        serde_json::json!({"ok": true, "operation": "get_file_brief", "result": result})
                    }
                    Err(err) => serde_json::json!({"ok": false, "error": err.to_string()}),
                });
            }
            Operation::GetSymbolBrief => {
                let symbol = body
                    .request
                    .name
                    .clone()
                    .or(body.request.query.clone())
                    .unwrap_or_default();
                let file = body.request.file.clone();
                return Ok(
                    match self
                        .retrieval()
                        .lock()
                        .get_symbol_brief(&symbol, file.as_deref())
                    {
                        Ok(result) => {
                            serde_json::json!({"ok": true, "operation": "get_symbol_brief", "result": result})
                        }
                        Err(err) => serde_json::json!({"ok": false, "error": err.to_string()}),
                    },
                );
            }
            Operation::GetSectionBrief => {
                let file = body
                    .request
                    .file
                    .clone()
                    .or(body.request.path.clone())
                    .unwrap_or_default();
                return Ok(
                    match self
                        .retrieval()
                        .lock()
                        .get_section_brief(&file, body.request.heading.as_deref())
                    {
                        Ok(result) => {
                            serde_json::json!({"ok": true, "operation": "get_section_brief", "result": result})
                        }
                        Err(err) => serde_json::json!({"ok": false, "error": err.to_string()}),
                    },
                );
            }
            Operation::GetKnowledgeGraph => {
                return Ok(match self.knowledge_graph().lock().list() {
                    Ok(entries) => {
                        serde_json::json!({"ok": true, "operation": "GetKnowledgeGraph", "result": entries})
                    }
                    Err(err) => serde_json::json!({"ok": false, "error": err.to_string()}),
                });
            }
            Operation::AppendKnowledge => {
                let entry = KnowledgeEntry {
                    timestamp: crate::session::now_epoch_s(),
                    category: body.request.query.clone().unwrap_or_default(),
                    title: body.request.name.clone().unwrap_or_default(),
                    details: body.request.edit_description.clone().unwrap_or_default(),
                    repository: body.request.file.clone().unwrap_or_default(),
                };
                return Ok(match self.knowledge_graph().lock().append(&entry) {
                    Ok(()) => serde_json::json!({"ok": true, "operation": "AppendKnowledge"}),
                    Err(err) => serde_json::json!({"ok": false, "error": err.to_string()}),
                });
            }
            Operation::GetChangePropagation => {
                let origin_repo = body
                    .request
                    .name
                    .clone()
                    .or_else(|| body.request.query.clone())
                    .unwrap_or_default();
                let result = {
                    let retrieval = self.retrieval();
                    let guard = retrieval.lock();
                    let org_graph =
                        OrganizationGraphBuilder::build(guard.storage_ref()).unwrap_or_default();
                    DependencyIntelligence::analyze(guard.storage_ref(), &org_graph)
                        .map(|insight| ChangePropagationEngine::predict(&origin_repo, &insight))
                };
                return Ok(match result {
                    Ok(result) => {
                        serde_json::json!({"ok": true, "operation": "GetChangePropagation", "result": result})
                    }
                    Err(err) => serde_json::json!({"ok": false, "error": err.to_string()}),
                });
            }
            _ => {}
        }

        if body.semantic_enabled == Some(false) {
            return Ok(serde_json::json!({
                "ok": true,
                "semantic_enabled": false,
                "skipped": true,
                "message": "Semantic layer disabled for this request."
            }));
        }

        let mut request = body.request;
        let request_file_hint = request.file.clone();
        let request_path_hint = request.path.clone();
        if body.input_compressed == Some(true)
            && should_block_compressed_semantic(&request.operation)
        {
            if let Some(original_query) = body.original_query.clone() {
                if request.query.is_none() {
                    request.query = Some(original_query.clone());
                }
                if request.name.is_none() {
                    request.name = Some(original_query);
                }
            } else {
                return Ok(serde_json::json!({
                    "ok": false,
                    "error": "input_compressed=true can reduce semantic retrieval precision. Send original_query or disable compression for semantic operations."
                }));
            }
        }

        let query_for_session = request.query.clone();
        let result = self.retrieval().lock().handle_with_options_ext(
            request,
            body.single_file_fast_path,
            Some(!body.reference_only.unwrap_or(true)),
            body.mapping_mode.as_deref(),
            body.max_footprint_items,
        );

        Ok(match result {
            Ok(mut response) => {
                let mut reused_context_count = 0usize;
                if let Some(session_id) = body.session_id.as_ref() {
                    let index_revision = self.retrieval().lock().index_revision();
                    let middleware = self.middleware();
                    let mut middleware = middleware.lock();
                    let session =
                        touch_or_create_session(&mut middleware, session_id, index_revision);
                    if body.reuse_session_context.unwrap_or(true) {
                        reused_context_count =
                            apply_session_context_reuse(&mut response.result, session);
                    }
                    let raw_mode =
                        RawExpansionMode::parse(body.raw_expansion_mode.as_deref());
                    let raw_outcome = apply_session_raw_expansion_controls(
                        &mut response.result,
                        session,
                        raw_mode,
                    );
                    if let Some(symbol) = response.result.get("symbol").and_then(|v| v.as_str()) {
                        session.last_target_symbols.push_back(symbol.to_string());
                        while session.last_target_symbols.len() > 32 {
                            session.last_target_symbols.pop_front();
                        }
                    }
                    if let Some(query) = query_for_session.as_ref() {
                        if let Some(symbol) = response.result.get("symbol").and_then(|v| v.as_str())
                        {
                            session
                                .intent_symbol_cache
                                .insert(query.to_lowercase(), symbol.to_string());
                        }
                    }
                    if let Some(obj) = response.result.as_object_mut() {
                        obj.insert(
                            "raw_expansion_mode".to_string(),
                            serde_json::json!(raw_mode.label()),
                        );
                        obj.insert(
                            "already_opened_refs".to_string(),
                            serde_json::json!(raw_outcome.already_opened_hits),
                        );
                        obj.insert(
                            "raw_budget_exhausted".to_string(),
                            serde_json::json!(raw_outcome.budget_exhausted),
                        );
                    }
                }
                if let Some(obj) = response.result.as_object_mut() {
                    obj.insert(
                        "reused_context_count".to_string(),
                        serde_json::json!(reused_context_count),
                    );
                    let indexed_files = self
                        .retrieval()
                        .lock()
                        .with_storage(|storage| storage.list_files())
                        .unwrap_or_default();
                    let (coverage, target) = compute_index_coverage(
                        &indexed_files,
                        request_file_hint.as_deref(),
                        request_path_hint.as_deref(),
                    );
                    let readiness = index_readiness(indexed_files.len(), coverage);
                    if body.auto_index_target.unwrap_or(false) && coverage == "unindexed_target" {
                        if let Some(target) = target.as_deref() {
                            self.indexer()
                                .lock()
                                .index_paths(self.repo_root(), &[target.to_string()])?;
                            let mut retry_body = retry_body_template;
                            retry_body.auto_index_target = Some(false);
                            let mut retried = self.try_handle_retrieve(retry_body)?;
                            if retrieve_coverage_resolved(&retried, target) {
                                let indexed_files = self
                                    .retrieval()
                                    .lock()
                                    .with_storage(|storage| storage.list_files())
                                    .unwrap_or_default();
                                if let Some(obj) = retried.as_object_mut() {
                                    obj.insert(
                                        "auto_index_applied".to_string(),
                                        serde_json::json!(true),
                                    );
                                    obj.insert(
                                        "auto_index_target".to_string(),
                                        serde_json::json!(target),
                                    );
                                    obj.insert(
                                        "indexed_file_count".to_string(),
                                        serde_json::json!(indexed_files.len()),
                                    );
                                    obj.insert(
                                        "indexed_path_hints".to_string(),
                                        serde_json::json!(summarize_indexed_path_hints(
                                            &indexed_files
                                        )),
                                    );
                                    obj.insert(
                                        "index_readiness".to_string(),
                                        serde_json::json!(index_readiness(indexed_files.len(), "indexed_target")),
                                    );
                                }
                            }
                            return Ok(retried);
                        }
                    }
                    obj.insert("index_readiness".to_string(), serde_json::json!(readiness));
                    obj.insert("index_coverage".to_string(), serde_json::json!(coverage));
                    if let Some(target) = target {
                        obj.insert("index_coverage_target".to_string(), serde_json::json!(target));
                        if let Some(command) =
                            suggested_index_command(coverage, Some(target.as_str()))
                        {
                            obj.insert(
                                "suggested_index_command".to_string(),
                                serde_json::json!(command),
                            );
                        }
                    }
                }
                success(response)
            }
            Err(err) => serde_json::json!({"ok": false, "error": err.to_string()}),
        })
    }

    fn simple_result(
        &self,
        operation: &str,
        result: Result<serde_json::Value>,
    ) -> serde_json::Value {
        match result {
            Ok(result) => serde_json::json!({"ok": true, "operation": operation, "result": result}),
            Err(err) => serde_json::json!({"ok": false, "error": err.to_string()}),
        }
    }
}

fn compute_index_coverage(
    indexed_files: &[String],
    file_hint: Option<&str>,
    path_hint: Option<&str>,
) -> (&'static str, Option<String>) {
    if indexed_files.is_empty() {
        if let Some(target) = file_hint.or(path_hint) {
            return ("unindexed_repo", Some(normalize_coverage_path(target)));
        }
        return ("unindexed_repo", None);
    }
    if let Some(file) = file_hint {
        let normalized = normalize_coverage_path(file);
        let indexed = indexed_files.iter().any(|item| item == &normalized);
        return (
            if indexed { "indexed_target" } else { "unindexed_target" },
            Some(normalized),
        );
    }
    if let Some(path) = path_hint {
        let normalized = normalize_coverage_path(path);
        let indexed = if looks_like_file_path(&normalized) {
            indexed_files.iter().any(|item| item == &normalized)
        } else {
            indexed_files
                .iter()
                .any(|item| item == &normalized || item.starts_with(&format!("{normalized}/")))
        };
        return (
            if indexed { "indexed_target" } else { "unindexed_target" },
            Some(normalized),
        );
    }
    ("indexed_repo", None)
}

fn normalize_coverage_path(path: &str) -> String {
    path.trim().replace('\\', "/").trim_matches('/').to_string()
}

fn looks_like_file_path(path: &str) -> bool {
    path.rsplit('/')
        .next()
        .map(|segment| segment.contains('.'))
        .unwrap_or(false)
}

fn suggested_index_command(coverage: &str, target: Option<&str>) -> Option<String> {
    if coverage != "unindexed_target" {
        return None;
    }
    let target = target?.trim();
    if target.is_empty() {
        return None;
    }
    Some(format!("semantic index --path {target}"))
}

fn retrieve_coverage_resolved(value: &serde_json::Value, target: &str) -> bool {
    let result = match value.get("result") {
        Some(result) => result,
        None => return false,
    };
    let coverage = result.get("index_coverage").and_then(|v| v.as_str());
    let current_target = result.get("index_coverage_target").and_then(|v| v.as_str());
    coverage != Some("unindexed_target") || current_target != Some(target)
}

fn index_readiness(indexed_file_count: usize, coverage: &str) -> &'static str {
    if indexed_file_count == 0 || coverage == "unindexed_repo" {
        "unindexed_repo"
    } else if coverage == "indexed_target" {
        "target_ready"
    } else if coverage == "unindexed_target" {
        "partial_index_missing_target"
    } else {
        "indexed_repo"
    }
}

#[cfg(test)]
mod tests {
    use super::{
        compute_index_coverage, index_readiness, retrieve_coverage_resolved,
        suggested_index_command,
    };
    use crate::{runtime::AppRuntime, RuntimeOptions};
    use engine::{Operation, RetrievalRequest};
    use std::fs;

    #[test]
    fn compute_index_coverage_marks_unindexed_target_when_path_is_outside_partial_index() {
        let indexed = vec!["src/auth/session.ts".to_string()];
        let (coverage, target) = compute_index_coverage(&indexed, None, Some("src/worker"));
        assert_eq!(coverage, "unindexed_target");
        assert_eq!(target.as_deref(), Some("src/worker"));
    }

    #[test]
    fn compute_index_coverage_marks_indexed_target_for_exact_file() {
        let indexed = vec!["src/auth/session.ts".to_string()];
        let (coverage, target) =
            compute_index_coverage(&indexed, Some("src/auth/session.ts"), None);
        assert_eq!(coverage, "indexed_target");
        assert_eq!(target.as_deref(), Some("src/auth/session.ts"));
    }

    #[test]
    fn compute_index_coverage_preserves_exact_file_path_hint() {
        let indexed = vec!["src/auth/session.ts".to_string()];
        let (coverage, target) =
            compute_index_coverage(&indexed, None, Some("src/worker/job.ts"));
        assert_eq!(coverage, "unindexed_target");
        assert_eq!(target.as_deref(), Some("src/worker/job.ts"));
    }

    #[test]
    fn suggested_index_command_is_emitted_for_unindexed_target() {
        assert_eq!(
            suggested_index_command("unindexed_target", Some("src/worker")),
            Some("semantic index --path src/worker".to_string())
        );
        assert_eq!(suggested_index_command("indexed_target", Some("src/worker")), None);
    }

    #[test]
    fn retrieve_coverage_resolved_requires_actual_coverage_change() {
        let unresolved = serde_json::json!({
            "result": {
                "index_coverage": "unindexed_target",
                "index_coverage_target": "src/worker"
            }
        });
        let resolved = serde_json::json!({
            "result": {
                "index_readiness": "target_ready",
                "index_coverage": "indexed_target",
                "index_coverage_target": "src/worker"
            }
        });
        assert!(!retrieve_coverage_resolved(&unresolved, "src/worker"));
        assert!(retrieve_coverage_resolved(&resolved, "src/worker"));
    }

    #[test]
    fn index_readiness_marks_partial_index_missing_target() {
        assert_eq!(index_readiness(0, "unindexed_repo"), "unindexed_repo");
        assert_eq!(index_readiness(1, "indexed_target"), "target_ready");
        assert_eq!(
            index_readiness(1, "unindexed_target"),
            "partial_index_missing_target"
        );
        assert_eq!(index_readiness(1, "indexed_repo"), "indexed_repo");
    }

    #[test]
    fn retrieve_can_auto_index_exact_file_target() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join("src").join("auth")).expect("mkdir auth");
        fs::create_dir_all(repo.join("src").join("worker")).expect("mkdir worker");
        fs::write(
            repo.join("src").join("auth").join("session.ts"),
            "export function buildSession(){ return 1; }\n",
        )
        .expect("write auth");
        fs::write(
            repo.join("src").join("worker").join("job.ts"),
            "export function runJob(){ return 1; }\n",
        )
        .expect("write worker");

        let runtime = AppRuntime::bootstrap(
            repo.clone(),
            RuntimeOptions {
                start_watcher: false,
                ensure_config: true,
                bootstrap_index_policy: crate::BootstrapIndexPolicy::Skip,
            },
        )
        .expect("bootstrap runtime");
        runtime
            .indexer()
            .lock()
            .index_paths(runtime.repo_root(), &[String::from("src/auth")])
            .expect("targeted index");

        let value = runtime.handle_retrieve(crate::models::RetrieveRequestBody {
            request: RetrievalRequest {
                operation: Operation::SearchSymbol,
                name: Some("runJob".to_string()),
                path: Some("src/worker/job.ts".to_string()),
                ..Default::default()
            },
            semantic_enabled: Some(true),
            input_compressed: None,
            original_query: None,
            single_file_fast_path: Some(true),
            reference_only: Some(true),
            mapping_mode: None,
            max_footprint_items: None,
            reuse_session_context: Some(true),
            session_id: None,
            raw_expansion_mode: None,
            auto_index_target: Some(true),
        });

        assert_eq!(
            value.get("auto_index_applied").and_then(|v| v.as_bool()),
            Some(true)
        );
        assert_eq!(
            value.get("auto_index_target").and_then(|v| v.as_str()),
            Some("src/worker/job.ts")
        );
        assert_eq!(
            value.get("indexed_file_count").and_then(|v| v.as_u64()),
            Some(2)
        );
        assert!(
            value.get("indexed_path_hints")
                .and_then(|v| v.as_array())
                .map(|items| items.iter().any(|item| item.as_str() == Some("src/worker")))
                .unwrap_or(false)
        );
        let result = value.get("result").expect("result");
        assert_eq!(
            result.get("index_readiness").and_then(|v| v.as_str()),
            Some("target_ready")
        );
        assert_eq!(
            result.get("index_coverage").and_then(|v| v.as_str()),
            Some("indexed_target")
        );
        assert_eq!(
            result.get("index_readiness").and_then(|v| v.as_str()),
            Some("target_ready")
        );
        assert_eq!(
            result.get("index_coverage_target").and_then(|v| v.as_str()),
            Some("src/worker/job.ts")
        );
    }
}

pub fn should_block_compressed_semantic(operation: &Operation) -> bool {
    matches!(
        operation,
        Operation::SearchSymbol
            | Operation::GetFunction
            | Operation::GetClass
            | Operation::GetDependencies
            | Operation::GetLogicNodes
            | Operation::GetDependencyNeighborhood
            | Operation::GetSymbolNeighborhood
            | Operation::GetReasoningContext
            | Operation::GetPlannedContext
            | Operation::SearchSemanticSymbol
            | Operation::GetWorkspaceReasoningContext
            | Operation::PlanSafeEdit
    )
}

pub fn success(response: RetrievalResponse) -> serde_json::Value {
    serde_json::json!({
        "ok": true,
        "operation": response.operation,
        "result": response.result,
    })
}
