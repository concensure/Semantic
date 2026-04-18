#[cfg(feature = "rust-support")]
use crate::config::load_rust_support_config;
use crate::models::RetrieveRequestBody;
use crate::runtime::{
    index_region_status, load_index_coverage_manifest, parser_support_for_target_path,
    resolve_indexed_target_alias, summarize_index_recovery_delta, summarize_indexed_path_hints,
    summarize_repo_source_boundary, should_skip_status_source_scan_path,
};
use crate::runtime::AppRuntime;
use crate::session::{
    apply_session_context_reuse, apply_session_raw_expansion_controls, touch_or_create_session,
    RawExpansionMode,
};
use anyhow::Result;
use change_propagation::ChangePropagationEngine;
use dependency_intelligence::DependencyIntelligence;
use engine::{
    Operation, RetrievalResponse, RustImportRecord, RustIndexedSymbolRecord, RustModuleDeclRecord,
    SymbolType,
};
use knowledge_graph::KnowledgeEntry;
use org_graph::OrganizationGraphBuilder;
use serde_json::json;
#[cfg(feature = "rust-support")]
use semantic_rust::RustAnalyzerSymbol;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::error;
use walkdir::WalkDir;

const MAX_UNSUPPORTED_SOURCE_MODULES: usize = 6;

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
            Operation::SearchRustSymbol => {
                let query = body
                    .request
                    .query
                    .clone()
                    .or_else(|| body.request.name.clone())
                    .unwrap_or_default();
                return Ok(self.handle_rust_symbol_search(&query, body.request.limit.unwrap_or(20)));
            }
            Operation::GetRustContext => {
                let query = body
                    .request
                    .query
                    .clone()
                    .or_else(|| body.request.name.clone())
                    .unwrap_or_default();
                return Ok(self.handle_rust_context_request(
                    &query,
                    body.request.max_tokens.unwrap_or(400),
                ));
            }
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
            Operation::GetArchitectureMap => {
                return Ok(match self.build_architecture_map() {
                    Ok(result) => json!({
                        "ok": true,
                        "operation": "get_architecture_map",
                        "result": result
                    }),
                    Err(err) => json!({"ok": false, "error": err.to_string()}),
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
        if let Some(target) = exact_unsupported_target(
            request_file_hint.as_deref(),
            request_path_hint.as_deref(),
        ) {
            let indexed_files = self
                .retrieval()
                .lock()
                .with_storage(|storage| storage.list_files())
                .unwrap_or_default();
            let index_manifest = load_index_coverage_manifest(self.repo_root());
            if let Some(result) = self.indexed_unsupported_path_fallback(&indexed_files, &target) {
                let mut result_obj = result.as_object().cloned().unwrap_or_default();
                result_obj.insert(
                    "index_region_status".to_string(),
                    serde_json::json!(index_region_status(
                        !indexed_files.is_empty(),
                        index_manifest.as_ref().map(|m| m.coverage_mode.as_str()),
                    )),
                );
                result_obj.insert(
                    "reused_context_count".to_string(),
                    serde_json::json!(0),
                );
                return Ok(serde_json::json!({
                    "ok": true,
                    "operation": request.operation,
                    "result": serde_json::Value::Object(result_obj)
                }));
            }
            return Ok(serde_json::json!({
                "ok": true,
                "operation": request.operation,
                "result": {
                    "message": "requested file is outside current parser coverage",
                    "index_readiness": "unsupported_target",
                    "index_region_status": index_region_status(
                        !indexed_files.is_empty(),
                        index_manifest.as_ref().map(|m| m.coverage_mode.as_str()),
                    ),
                    "index_recovery_mode": "unsupported_target",
                    "index_recovery_target_kind": "file",
                    "parser_target_support": "unsupported",
                    "index_coverage": "unsupported_target",
                    "index_coverage_target": target,
                }
            }));
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
                    let index_manifest = load_index_coverage_manifest(self.repo_root());
                    let (coverage, target) = compute_index_coverage(
                        &indexed_files,
                        request_file_hint.as_deref(),
                        request_path_hint.as_deref(),
                    );
                    let readiness = index_readiness(indexed_files.len(), coverage);
                    let auto_index_requested = body.auto_index_target.unwrap_or(false);
                    if body.auto_index_target.unwrap_or(false) && coverage == "unindexed_target" {
                        if let Some(target) = target.as_deref() {
                            let indexed_files_before = indexed_files.clone();
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
                                let (added_file_count, changed_files) =
                                    summarize_index_recovery_delta(
                                        &indexed_files_before,
                                        &indexed_files,
                                    );
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
                                        "parser_target_support".to_string(),
                                        serde_json::json!(parser_support_for_target_path(target)),
                                    );
                                    obj.insert(
                                        "index_recovery_target_kind".to_string(),
                                        serde_json::json!(index_recovery_target_kind(target)),
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
                                        "index_region_status".to_string(),
                                        serde_json::json!(index_region_status(
                                            !indexed_files.is_empty(),
                                            index_manifest.as_ref().map(|m| m.coverage_mode.as_str()),
                                        )),
                                    );
                                    obj.insert(
                                        "index_recovery_delta".to_string(),
                                        serde_json::json!({
                                            "added_file_count": added_file_count,
                                            "changed_files": changed_files,
                                        }),
                                    );
                                    obj.insert(
                                        "index_readiness".to_string(),
                                        serde_json::json!(index_readiness(indexed_files.len(), "indexed_target")),
                                    );
                                    obj.insert(
                                        "index_recovery_mode".to_string(),
                                        serde_json::json!("auto_index_applied"),
                                    );
                                    if let Some(result) =
                                        obj.get_mut("result").and_then(|v| v.as_object_mut())
                                    {
                                        result.insert(
                                            "index_recovery_mode".to_string(),
                                            serde_json::json!("auto_index_applied"),
                                        );
                                        result.insert(
                                            "index_recovery_target_kind".to_string(),
                                            serde_json::json!(index_recovery_target_kind(target)),
                                        );
                                        result.insert(
                                            "index_region_status".to_string(),
                                            serde_json::json!(index_region_status(
                                                !indexed_files.is_empty(),
                                                index_manifest
                                                    .as_ref()
                                                    .map(|m| m.coverage_mode.as_str()),
                                            )),
                                        );
                                        result.insert(
                                            "index_recovery_delta".to_string(),
                                            serde_json::json!({
                                                "added_file_count": added_file_count,
                                                "changed_files": changed_files,
                                            }),
                                        );
                                    }
                                }
                            } else if let Some(obj) = retried.as_object_mut() {
                                obj.insert(
                                    "index_recovery_mode".to_string(),
                                    serde_json::json!("auto_index_attempted_no_change"),
                                );
                                obj.insert(
                                    "parser_target_support".to_string(),
                                    serde_json::json!(parser_support_for_target_path(target)),
                                );
                                obj.insert(
                                    "index_recovery_target_kind".to_string(),
                                    serde_json::json!(index_recovery_target_kind(target)),
                                );
                                if let Some(result) =
                                    obj.get_mut("result").and_then(|v| v.as_object_mut())
                                {
                                    result.insert(
                                        "index_recovery_mode".to_string(),
                                        serde_json::json!("auto_index_attempted_no_change"),
                                    );
                                    result.insert(
                                        "index_recovery_target_kind".to_string(),
                                        serde_json::json!(index_recovery_target_kind(target)),
                                    );
                                }
                            }
                            return Ok(retried);
                        }
                    }
                    obj.insert("index_readiness".to_string(), serde_json::json!(readiness));
                    obj.insert(
                        "index_region_status".to_string(),
                        serde_json::json!(index_region_status(
                            !indexed_files.is_empty(),
                            index_manifest.as_ref().map(|m| m.coverage_mode.as_str()),
                        )),
                    );
                    obj.insert(
                        "index_recovery_mode".to_string(),
                        serde_json::json!(index_recovery_mode(auto_index_requested, coverage)),
                    );
                    obj.insert("index_coverage".to_string(), serde_json::json!(coverage));
                    if let Some(target) = target {
                        obj.insert(
                            "parser_target_support".to_string(),
                            serde_json::json!(parser_support_for_target_path(target.as_str())),
                        );
                        obj.insert(
                            "index_recovery_target_kind".to_string(),
                            serde_json::json!(index_recovery_target_kind(target.as_str())),
                        );
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

    fn build_architecture_map(&self) -> Result<serde_json::Value> {
        let retrieval = self.retrieval();
        let guard = retrieval.lock();
        let indexed_files = guard
            .with_storage(|storage| storage.list_files())?
            .into_iter()
            .filter(|file| !should_skip_architecture_path(file))
            .collect::<Vec<_>>();
        if indexed_files.is_empty() {
            return Ok(build_filesystem_architecture_map(self.repo_root()));
        }

        let module_records = guard.with_storage(|storage| storage.list_modules())?;
        let mut file_to_module = HashMap::<String, String>::new();
        let mut modules = Vec::new();

        if module_records.is_empty() {
            let derived = derive_modules_from_files(&indexed_files);
            for (module_name, files) in &derived {
                for file in files {
                    file_to_module.insert(file.clone(), module_name.clone());
                }
            }
            modules = derived
                .into_iter()
                .map(|(name, files)| (name.clone(), name, files))
                .collect();
        } else {
            for module in module_records {
                if should_skip_architecture_path(&module.path)
                    || should_skip_architecture_path(&module.name)
                {
                    continue;
                }
                let module_id = module.id.unwrap_or_default();
                let module_files = guard.with_storage(|storage| storage.list_module_files(module_id))?;
                let files = module_files
                    .into_iter()
                    .map(|item| item.file_path)
                    .filter(|file| !should_skip_architecture_path(file))
                    .collect::<Vec<_>>();
                if files.is_empty() {
                    continue;
                }
                for file in &files {
                    file_to_module.insert(file.clone(), module.name.clone());
                }
                modules.push((module.name, module.path, files));
            }
        }

        let all_symbols = guard.with_storage(|storage| storage.list_symbols())?;
        let all_dependencies = guard.with_storage(|storage| storage.list_all_dependencies())?;
        let named_module_deps = guard.with_storage(|storage| storage.list_named_module_dependencies())?;

        let mut called_symbols = HashSet::new();
        for dep in &all_dependencies {
            called_symbols.insert(dep.callee_symbol.clone());
        }

        let mut entry_points_by_module = BTreeMap::<String, BTreeSet<String>>::new();
        for symbol in &all_symbols {
            if matches!(symbol.symbol_type, engine::SymbolType::Import) {
                continue;
            }
            if called_symbols.contains(&symbol.name) {
                continue;
            }
            if let Some(module_name) = file_to_module.get(&symbol.file) {
                entry_points_by_module
                    .entry(module_name.clone())
                    .or_default()
                    .insert(symbol.name.clone());
            }
        }

        let mut fan_out_by_module = HashMap::<String, usize>::new();
        for (from, _) in named_module_deps {
            *fan_out_by_module.entry(from).or_default() += 1;
        }

        let recent_modules = rank_recent_modules(self.repo_root(), &modules);
        let mut architecture_modules = modules
            .into_iter()
            .map(|(name, path, files)| {
                summarize_architecture_module(
                    &name,
                    &path,
                    &files,
                    entry_points_by_module.get(&name),
                    *fan_out_by_module.get(&name).unwrap_or(&0),
                    recent_modules.contains(&name),
                )
            })
            .collect::<Vec<_>>();
        architecture_modules.extend(build_unsupported_source_architecture_modules(
            self.repo_root(),
            &architecture_modules,
        ));
        filter_fixture_dominant_architecture_modules(&mut architecture_modules);
        sort_architecture_modules(&mut architecture_modules);
        let discovered_module_count = architecture_modules.len();
        let indexed_module_count = architecture_modules
            .iter()
            .filter(|module| {
                module.get("support_level").and_then(|v| v.as_str()) == Some("indexed")
            })
            .count();
        let unsupported_module_count = architecture_modules
            .iter()
            .filter(|module| {
                matches!(
                    module.get("support_level").and_then(|v| v.as_str()),
                    Some("unsupported_source" | "unsupported_source_group")
                )
            })
            .count();
        architecture_modules = compress_architecture_modules(architecture_modules);
        let visible_module_count = architecture_modules.len();
        let grouped_hidden_module_count = grouped_hidden_module_count(&architecture_modules);

        let summary = architecture_high_priority_summary(&architecture_modules);
        let priority_modules = architecture_priority_modules(&architecture_modules);
        let priority_focus_mode = architecture_priority_focus_mode(&priority_modules);
        let priority_focus_reason =
            architecture_priority_focus_reason(&priority_modules, priority_focus_mode);
        let priority_focus_targets = architecture_priority_focus_targets(
            &priority_modules,
            priority_focus_mode,
        );
        let priority_focus_entries =
            architecture_priority_focus_entries(&priority_modules, &priority_focus_targets);
        let priority_focus_trust = architecture_priority_focus_trust(&priority_focus_entries);
        let priority_focus_commands =
            architecture_priority_focus_commands(&priority_focus_entries);
        let priority_focus_follow_up_operations =
            architecture_priority_focus_follow_up_operations(&priority_focus_entries);
        let priority_focus_primary_entry =
            architecture_priority_focus_primary_entry(&priority_focus_entries);
        let priority_focus_primary_trust =
            architecture_priority_entry_trust(&priority_focus_primary_entry);
        let priority_focus_secondary_entry =
            architecture_priority_focus_secondary_entry(&priority_focus_entries);
        let priority_focus_secondary_trust =
            if priority_focus_secondary_entry.is_null() {
                serde_json::Value::Null
            } else {
                serde_json::Value::String(
                    architecture_priority_entry_trust(&priority_focus_secondary_entry)
                        .to_string(),
                )
            };

        Ok(json!({
            "orientation_stage": "indexed_modules",
            "priority_scoring_model": "architecture_priority_v1",
            "priority_scoring_weights": architecture_priority_scoring_weights(),
            "priority_focus_mode": priority_focus_mode,
            "priority_focus_reason": priority_focus_reason,
            "priority_focus_trust": priority_focus_trust,
            "priority_focus_targets": priority_focus_targets,
            "priority_focus_entries": priority_focus_entries,
            "priority_focus_commands": priority_focus_commands,
            "priority_focus_follow_up_operations": priority_focus_follow_up_operations,
            "priority_focus_primary_target": priority_focus_primary_entry.get("name").cloned().unwrap_or(serde_json::Value::Null),
            "priority_focus_primary_path": priority_focus_primary_entry.get("path").cloned().unwrap_or(serde_json::Value::Null),
            "priority_focus_primary_importance": priority_focus_primary_entry.get("importance").cloned().unwrap_or(serde_json::Value::Null),
            "priority_focus_primary_support_level": priority_focus_primary_entry.get("support_level").cloned().unwrap_or(serde_json::Value::Null),
            "priority_focus_primary_actionability": priority_focus_primary_entry.get("actionability").cloned().unwrap_or(serde_json::Value::Null),
            "priority_focus_primary_trust": priority_focus_primary_trust,
            "priority_focus_primary_rank": priority_focus_primary_entry.get("priority_rank").cloned().unwrap_or(serde_json::Value::Null),
            "priority_focus_primary_score": priority_focus_primary_entry.get("priority_score").cloned().unwrap_or(serde_json::Value::Null),
            "priority_focus_primary_score_components": priority_focus_primary_entry.get("priority_score_components").cloned().unwrap_or(serde_json::Value::Null),
            "priority_focus_primary_score_gap_from_previous": priority_focus_primary_entry.get("priority_score_gap_from_previous").cloned().unwrap_or(serde_json::Value::Null),
            "priority_focus_primary_score_gap_to_next": priority_focus_primary_entry.get("priority_score_gap_to_next").cloned().unwrap_or(serde_json::Value::Null),
            "priority_focus_primary_score_separation": priority_focus_primary_entry.get("priority_score_separation").cloned().unwrap_or(serde_json::Value::Null),
            "priority_focus_primary_signals": priority_focus_primary_entry.get("signals").cloned().unwrap_or(serde_json::Value::Null),
            "priority_focus_primary_entry_points": priority_focus_primary_entry.get("entry_points").cloned().unwrap_or(serde_json::Value::Null),
            "priority_focus_primary_files": priority_focus_primary_entry.get("files").cloned().unwrap_or(serde_json::Value::Null),
            "priority_focus_primary_indexed_file_count": priority_focus_primary_entry.get("indexed_file_count").cloned().unwrap_or(serde_json::Value::Null),
            "priority_focus_primary_source_file_count": priority_focus_primary_entry.get("source_file_count").cloned().unwrap_or(serde_json::Value::Null),
            "priority_focus_primary_fan_out": priority_focus_primary_entry.get("fan_out").cloned().unwrap_or(serde_json::Value::Null),
            "priority_focus_primary_open_first_path": priority_focus_primary_entry.get("open_first_path").cloned().unwrap_or(serde_json::Value::Null),
            "priority_focus_primary_next_step_operation": priority_focus_primary_entry.get("next_step_operation").cloned().unwrap_or(serde_json::Value::Null),
            "priority_focus_primary_next_step_target_kind": priority_focus_primary_entry.get("next_step_target_kind").cloned().unwrap_or(serde_json::Value::Null),
            "priority_focus_primary_next_step_target_path": priority_focus_primary_entry.get("next_step_target_path").cloned().unwrap_or(serde_json::Value::Null),
            "priority_focus_primary_command": priority_focus_primary_entry.get("next_step_command").cloned().unwrap_or(serde_json::Value::Null),
            "priority_focus_secondary_target": priority_focus_secondary_entry.get("name").cloned().unwrap_or(serde_json::Value::Null),
            "priority_focus_secondary_path": priority_focus_secondary_entry.get("path").cloned().unwrap_or(serde_json::Value::Null),
            "priority_focus_secondary_importance": priority_focus_secondary_entry.get("importance").cloned().unwrap_or(serde_json::Value::Null),
            "priority_focus_secondary_support_level": priority_focus_secondary_entry.get("support_level").cloned().unwrap_or(serde_json::Value::Null),
            "priority_focus_secondary_actionability": priority_focus_secondary_entry.get("actionability").cloned().unwrap_or(serde_json::Value::Null),
            "priority_focus_secondary_trust": priority_focus_secondary_trust,
            "priority_focus_secondary_rank": priority_focus_secondary_entry.get("priority_rank").cloned().unwrap_or(serde_json::Value::Null),
            "priority_focus_secondary_score": priority_focus_secondary_entry.get("priority_score").cloned().unwrap_or(serde_json::Value::Null),
            "priority_focus_secondary_score_components": priority_focus_secondary_entry.get("priority_score_components").cloned().unwrap_or(serde_json::Value::Null),
            "priority_focus_secondary_score_gap_from_previous": priority_focus_secondary_entry.get("priority_score_gap_from_previous").cloned().unwrap_or(serde_json::Value::Null),
            "priority_focus_secondary_score_gap_to_next": priority_focus_secondary_entry.get("priority_score_gap_to_next").cloned().unwrap_or(serde_json::Value::Null),
            "priority_focus_secondary_score_separation": priority_focus_secondary_entry.get("priority_score_separation").cloned().unwrap_or(serde_json::Value::Null),
            "priority_focus_secondary_signals": priority_focus_secondary_entry.get("signals").cloned().unwrap_or(serde_json::Value::Null),
            "priority_focus_secondary_entry_points": priority_focus_secondary_entry.get("entry_points").cloned().unwrap_or(serde_json::Value::Null),
            "priority_focus_secondary_files": priority_focus_secondary_entry.get("files").cloned().unwrap_or(serde_json::Value::Null),
            "priority_focus_secondary_indexed_file_count": priority_focus_secondary_entry.get("indexed_file_count").cloned().unwrap_or(serde_json::Value::Null),
            "priority_focus_secondary_source_file_count": priority_focus_secondary_entry.get("source_file_count").cloned().unwrap_or(serde_json::Value::Null),
            "priority_focus_secondary_fan_out": priority_focus_secondary_entry.get("fan_out").cloned().unwrap_or(serde_json::Value::Null),
            "priority_focus_secondary_open_first_path": priority_focus_secondary_entry.get("open_first_path").cloned().unwrap_or(serde_json::Value::Null),
            "priority_focus_secondary_next_step_operation": priority_focus_secondary_entry.get("next_step_operation").cloned().unwrap_or(serde_json::Value::Null),
            "priority_focus_secondary_next_step_target_kind": priority_focus_secondary_entry.get("next_step_target_kind").cloned().unwrap_or(serde_json::Value::Null),
            "priority_focus_secondary_next_step_target_path": priority_focus_secondary_entry.get("next_step_target_path").cloned().unwrap_or(serde_json::Value::Null),
            "priority_focus_secondary_command": priority_focus_secondary_entry.get("next_step_command").cloned().unwrap_or(serde_json::Value::Null),
            "summary": architecture_map_summary(
                discovered_module_count,
                visible_module_count,
                indexed_module_count,
                unsupported_module_count,
            ),
            "discovered_module_count": discovered_module_count,
            "visible_module_count": visible_module_count,
            "grouped_hidden_module_count": grouped_hidden_module_count,
            "modules": architecture_modules,
            "high_priority_modules": summary,
            "priority_modules": priority_modules,
        }))
    }
}

impl AppRuntime {
    fn handle_rust_symbol_search(&self, query: &str, limit: usize) -> serde_json::Value {
        #[cfg(feature = "rust-support")]
        {
            let config = load_rust_support_config(self.repo_root());
            if !config.enabled {
                return serde_json::json!({
                    "ok": false,
                    "operation": "search_rust_symbol",
                    "error": "rust support is compiled but disabled; set .semantic/rust.toml enabled = true"
                });
            }
            return match self.indexed_rust_symbol_search(query, limit) {
                Ok(result) => serde_json::json!({
                    "ok": true,
                    "operation": "search_rust_symbol",
                    "result": result
                }),
                Err(err) => serde_json::json!({
                    "ok": false,
                    "operation": "search_rust_symbol",
                    "error": err.to_string(),
                }),
            };
        }
        #[cfg(not(feature = "rust-support"))]
        {
            let _ = (query, limit);
            serde_json::json!({
                "ok": false,
                "operation": "search_rust_symbol",
                "error": "rust support was not compiled into this build"
            })
        }
    }

    fn handle_rust_context_request(&self, query: &str, max_tokens: usize) -> serde_json::Value {
        #[cfg(feature = "rust-support")]
        {
            let config = load_rust_support_config(self.repo_root());
            if !config.enabled {
                return serde_json::json!({
                    "ok": false,
                    "operation": "get_rust_context",
                    "error": "rust support is compiled but disabled; set .semantic/rust.toml enabled = true"
                });
            }
            return match self.indexed_rust_context(query, max_tokens) {
                Ok(bundle) => serde_json::json!({
                    "ok": true,
                    "operation": "get_rust_context",
                    "result": bundle
                }),
                Err(err) => serde_json::json!({
                    "ok": false,
                    "operation": "get_rust_context",
                    "error": err.to_string(),
                }),
            };
        }
        #[cfg(not(feature = "rust-support"))]
        {
            let _ = (query, max_tokens);
            serde_json::json!({
                "ok": false,
                "operation": "get_rust_context",
                "error": "rust support was not compiled into this build"
            })
        }
    }

    #[cfg(feature = "rust-support")]
    fn indexed_rust_symbol_search(&self, query: &str, limit: usize) -> Result<serde_json::Value> {
        let normalized = query.trim();
        let cache_key = format!("rust_search::{normalized}::{limit}");
        if let Some(cached) = rust_cache_get(self, &cache_key, "rust_search") {
            return Ok(cached);
        }
        let matches = self
            .retrieval()
            .lock()
            .with_storage(|storage| {
                let mut matches =
                    storage.search_rust_indexed_symbols(normalized, limit.saturating_mul(12).max(48))?;
                if matches.is_empty() {
                    matches = storage.list_rust_indexed_symbols()?;
                }
                Ok::<_, anyhow::Error>(rank_rust_indexed_symbols(normalized, matches))
            })?;
        let hints = parse_rust_query_hints(normalized);
        let file_evidence =
            load_rust_file_evidence(self, &rust_candidate_files(&matches, limit.saturating_mul(8).max(24)))?;
        let matches = sort_rust_symbols_with_file_evidence(normalized, &hints, &file_evidence, matches);
        let candidate_files = rust_candidate_files(&matches, limit.saturating_mul(4).max(12));
        let ra_matches = rust_analyzer_workspace_symbol_search(self, normalized, limit.saturating_mul(4).max(12))
            .into_iter()
            .chain(
                rust_analyzer_document_symbol_search(self, &candidate_files, &hints)
                    .into_iter(),
            )
            .collect::<Vec<_>>();
        let ra_matches = dedup_rust_analyzer_symbols(ra_matches);
        if !ra_matches.is_empty() {
            let matches = dedup_rust_search_matches(
                ra_matches
                .iter()
                .map(|symbol| rust_search_match_json_from_rust_analyzer(symbol, &matches))
                .collect::<Vec<_>>(),
            )
                .into_iter()
                .take(limit.max(1))
                .collect::<Vec<_>>();
            let result = json!({
                "query": normalized,
                "strategy": "rust_analyzer_document_symbol",
                "matches": matches,
            });
            rust_cache_put(self, &cache_key, "rust_search", &result);
            return Ok(result);
        }
        let matches = matches
            .into_iter()
            .take(limit.max(1))
            .map(rust_indexed_search_match_json)
            .collect::<Vec<_>>();
        let result = json!({
            "query": normalized,
            "strategy": "indexed_rust_symbols",
            "matches": matches,
        });
        rust_cache_put(self, &cache_key, "rust_search", &result);
        Ok(result)
    }

    #[cfg(feature = "rust-support")]
    fn indexed_rust_context(&self, query: &str, max_tokens: usize) -> Result<serde_json::Value> {
        let normalized = query.trim();
        let cache_key = format!("rust_context::{normalized}::{max_tokens}");
        if let Some(cached) = rust_cache_get(self, &cache_key, "rust_context") {
            return Ok(cached);
        }
        let max_items = rust_context_item_budget(max_tokens);
        let all_symbols = self
            .retrieval()
            .lock()
            .with_storage(|storage| storage.list_rust_indexed_symbols())?;
        let hints = parse_rust_query_hints(normalized);
        let ranked = rank_rust_indexed_symbols(normalized, all_symbols.clone());
        let candidate_files = rust_candidate_files(&ranked, max_items.saturating_mul(6).max(24));
        let ra_matches = rust_analyzer_workspace_symbol_search(
            self,
            normalized,
            max_items.saturating_mul(4).max(12),
        )
        .into_iter()
        .chain(
            rust_analyzer_document_symbol_search(self, &candidate_files, &hints).into_iter(),
        )
        .collect::<Vec<_>>();
        let ra_matches = dedup_rust_analyzer_symbols(ra_matches);
        let file_evidence =
            load_rust_file_evidence(self, &candidate_files)?;
        let ranked = sort_rust_symbols_with_file_evidence(normalized, &hints, &file_evidence, ranked);
        let anchored_definitions =
            rust_definition_anchors_from_rust_analyzer(normalized, &hints, &ra_matches, &all_symbols);
        let preferred_crate = anchored_definitions
            .first()
            .or_else(|| {
                ranked
                    .iter()
                    .find(|symbol| is_rust_definition_candidate_indexed(symbol, normalized))
            })
            .and_then(|symbol| symbol.crate_name.clone());
        let definitions = if anchored_definitions.is_empty() {
            ranked
                .iter()
                .filter(|symbol| preferred_crate_match(symbol, preferred_crate.as_deref()))
                .filter(|symbol| is_rust_definition_candidate_indexed(symbol, normalized))
                .take(max_items)
                .cloned()
                .collect::<Vec<_>>()
        } else {
            anchored_definitions
                .into_iter()
                .filter(|symbol| preferred_crate_match(symbol, preferred_crate.as_deref()))
                .take(max_items)
                .collect::<Vec<_>>()
        };
        let definition_names = definitions.iter().map(|s| s.name.clone()).collect::<HashSet<_>>();
        let impls = rust_related_impls_from_metadata(
            normalized,
            &definitions,
            &all_symbols,
            preferred_crate.as_deref(),
        )
        .into_iter()
        .collect::<Vec<_>>();
        let impls = sort_rust_symbols_with_file_evidence(normalized, &hints, &file_evidence, impls);
        let impls = filter_rust_symbols_for_qualified_query(&hints, &file_evidence, impls)
            .into_iter()
            .take(max_items)
            .collect::<Vec<_>>();
        let methods = rust_related_methods_from_metadata(
            normalized,
            &definitions,
            &impls,
            &all_symbols,
            preferred_crate.as_deref(),
        )
        .into_iter()
        .collect::<Vec<_>>();
        let methods =
            filter_rust_symbols_for_qualified_query(
                &hints,
                &file_evidence,
                sort_rust_symbols_with_file_evidence(normalized, &hints, &file_evidence, methods),
            )
                .into_iter()
                .take(max_items)
                .collect::<Vec<_>>();
        let modules = rust_modules_for_symbols(definitions.iter().chain(impls.iter()).chain(methods.iter()));
        let token_strategy = if max_tokens <= 280 {
            "signature_first"
        } else if max_tokens <= 600 {
            "bounded_span"
        } else {
            "expanded_span"
        };
        let result = json!({
            "query": normalized,
            "strategy": if ra_matches.is_empty() {
                "indexed_rust_grouped_context"
            } else {
                "rust_analyzer_anchored_context"
            },
            "token_strategy": token_strategy,
            "definitions": definitions
                .iter()
                .map(|symbol| rust_indexed_context_item_json(self.repo_root(), symbol, max_tokens, false))
                .collect::<Vec<_>>(),
            "impl_blocks": impls
                .iter()
                .map(|symbol| rust_indexed_context_item_json(self.repo_root(), symbol, max_tokens, true))
                .collect::<Vec<_>>(),
            "associated_items": methods
                .iter()
                .map(|symbol| rust_indexed_context_item_json(self.repo_root(), symbol, max_tokens, false))
                .collect::<Vec<_>>(),
            "modules": modules,
            "preferred_crate": preferred_crate,
            "definition_names": definition_names.into_iter().collect::<BTreeSet<_>>(),
        });
        rust_cache_put(self, &cache_key, "rust_context", &result);
        Ok(result)
    }
}

#[cfg(feature = "rust-support")]
fn rust_context_item_budget(max_tokens: usize) -> usize {
    if max_tokens <= 250 {
        3
    } else if max_tokens <= 500 {
        6
    } else if max_tokens <= 900 {
        10
    } else {
        14
    }
}

#[cfg(feature = "rust-support")]
fn rust_cache_get(
    runtime: &AppRuntime,
    cache_key: &str,
    cache_kind: &str,
) -> Option<serde_json::Value> {
    let retrieval = runtime.retrieval();
    let guard = retrieval.lock();
    let revision = guard.index_revision();
    let entry = guard
        .with_storage(|storage| storage.get_retrieval_cache_entry(cache_key, cache_kind))
        .ok()
        .flatten()?;
    if entry.source_revision != revision {
        return None;
    }
    entry
        .value_json
        .as_deref()
        .and_then(|raw| serde_json::from_str(raw).ok())
}

#[cfg(feature = "rust-support")]
fn rust_cache_put(
    runtime: &AppRuntime,
    cache_key: &str,
    cache_kind: &str,
    value: &serde_json::Value,
) {
    let retrieval = runtime.retrieval();
    let guard = retrieval.lock();
    let revision = guard.index_revision();
    let entry = storage::RetrievalCacheEntry {
        cache_key: cache_key.to_string(),
        cache_kind: cache_kind.to_string(),
        value_json: Some(value.to_string()),
        prompt_text: None,
        cached_at_epoch_s: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|value| value.as_secs())
            .unwrap_or(0),
        source_revision: revision,
    };
    let _ = guard.with_storage(|storage| storage.upsert_retrieval_cache_entry(&entry));
}

#[cfg(feature = "rust-support")]
fn rust_analyzer_workspace_symbol_search(
    runtime: &AppRuntime,
    query: &str,
    limit: usize,
) -> Vec<RustAnalyzerSymbol> {
    semantic_rust::workspace_symbol_search(runtime.repo_root(), query, limit).unwrap_or_default()
}

#[cfg(feature = "rust-support")]
fn rust_analyzer_document_symbol_search(
    runtime: &AppRuntime,
    candidate_files: &[String],
    hints: &RustQueryHints,
) -> Vec<RustAnalyzerSymbol> {
    let mut matches = Vec::new();
    let mut seen = HashSet::new();
    for file in candidate_files.iter().take(8) {
        let Ok(symbols) = semantic_rust::document_symbol_search(runtime.repo_root(), file) else {
            continue;
        };
        for symbol in symbols {
            if !rust_analyzer_symbol_matches_query(&symbol, hints) {
                continue;
            }
            let key = format!("{}:{}:{}", symbol.file, symbol.start_line, symbol.name);
            if seen.insert(key) {
                matches.push(symbol);
            }
        }
    }
    matches
}

#[cfg(feature = "rust-support")]
fn dedup_rust_analyzer_symbols(symbols: Vec<RustAnalyzerSymbol>) -> Vec<RustAnalyzerSymbol> {
    let mut deduped = Vec::new();
    let mut seen = HashSet::new();
    for symbol in symbols {
        let key = format!("{}:{}:{}:{}", symbol.file, symbol.start_line, symbol.end_line, symbol.name);
        if seen.insert(key) {
            deduped.push(symbol);
        }
    }
    deduped
}

#[cfg(feature = "rust-support")]
fn dedup_rust_search_matches(matches: Vec<serde_json::Value>) -> Vec<serde_json::Value> {
    let mut deduped = Vec::new();
    let mut seen = HashSet::new();
    for item in matches {
        let key = format!(
            "{}:{}:{}:{}",
            item.get("file").and_then(|value| value.as_str()).unwrap_or_default(),
            item.get("start_line").and_then(|value| value.as_u64()).unwrap_or_default(),
            item.get("end_line").and_then(|value| value.as_u64()).unwrap_or_default(),
            item.get("name").and_then(|value| value.as_str()).unwrap_or_default(),
        );
        if seen.insert(key) {
            deduped.push(item);
        }
    }
    deduped
}

#[cfg(feature = "rust-support")]
fn rust_analyzer_symbol_matches_query(
    symbol: &RustAnalyzerSymbol,
    hints: &RustQueryHints,
) -> bool {
    let name = symbol.name.to_ascii_lowercase();
    let container = symbol
        .container_name
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    name == hints.leaf
        || name.contains(&hints.leaf)
        || container == hints.leaf
        || container.contains(&hints.leaf)
        || hints
            .module_hint
            .as_deref()
            .map(|module| container.contains(module) || name.contains(module))
            .unwrap_or(false)
}

#[cfg(feature = "rust-support")]
fn rank_rust_indexed_symbols(
    query: &str,
    symbols: Vec<RustIndexedSymbolRecord>,
) -> Vec<RustIndexedSymbolRecord> {
    let normalized = query.trim().to_ascii_lowercase();
    let mut ranked = symbols
        .into_iter()
        .filter(|symbol| rust_indexed_symbol_matches_query(symbol, &normalized))
        .collect::<Vec<_>>();
    ranked.sort_by(|a, b| {
        rust_indexed_symbol_rank(a, &normalized)
            .cmp(&rust_indexed_symbol_rank(b, &normalized))
            .then_with(|| a.file.cmp(&b.file))
            .then_with(|| a.start_line.cmp(&b.start_line))
    });
    ranked.dedup_by(|a, b| a.name == b.name && a.file == b.file && a.start_line == b.start_line);
    ranked
}

#[cfg(feature = "rust-support")]
#[derive(Debug, Clone)]
struct RustQueryHints {
    normalized_query: String,
    segments: Vec<String>,
    leaf: String,
    module_hint: Option<String>,
    crate_hint: Option<String>,
}

#[cfg(feature = "rust-support")]
#[derive(Debug, Clone, Default)]
struct RustFileEvidence {
    import_paths: Vec<String>,
    import_aliases: Vec<String>,
    import_leafs: Vec<String>,
    module_names: Vec<String>,
    resolved_paths: Vec<String>,
}

#[cfg(feature = "rust-support")]
fn parse_rust_query_hints(query: &str) -> RustQueryHints {
    let normalized_query = query.trim().to_ascii_lowercase();
    let segments = normalized_query
        .split("::")
        .filter(|segment| !segment.is_empty())
        .map(|segment| segment.to_string())
        .collect::<Vec<_>>();
    let leaf = segments
        .last()
        .cloned()
        .unwrap_or_else(|| normalized_query.clone());
    let module_hint = if segments.len() > 1 {
        Some(segments[..segments.len() - 1].join("::"))
    } else {
        None
    };
    let crate_hint = if segments.len() > 1 {
        segments.first().cloned().filter(|segment| !segment.is_empty())
    } else {
        None
    };
    RustQueryHints {
        normalized_query,
        segments,
        leaf,
        module_hint,
        crate_hint,
    }
}

#[cfg(feature = "rust-support")]
fn rust_candidate_files(symbols: &[RustIndexedSymbolRecord], limit: usize) -> Vec<String> {
    let mut files = Vec::new();
    let mut seen = HashSet::new();
    for symbol in symbols {
        if seen.insert(symbol.file.clone()) {
            files.push(symbol.file.clone());
        }
        if files.len() >= limit.max(1) {
            break;
        }
    }
    files
}

#[cfg(feature = "rust-support")]
fn load_rust_file_evidence(
    runtime: &AppRuntime,
    candidate_files: &[String],
) -> Result<HashMap<String, RustFileEvidence>> {
    if candidate_files.is_empty() {
        return Ok(HashMap::new());
    }
    runtime.retrieval().lock().with_storage(|storage| {
        let mut evidence = HashMap::new();
        for file in candidate_files {
            let imports = storage.list_rust_imports_for_file(file)?;
            let module_decls = storage.list_rust_module_decls_for_file(file)?;
            evidence.insert(
                file.clone(),
                rust_file_evidence_from_records(imports, module_decls),
            );
        }
        Ok::<_, anyhow::Error>(evidence)
    })
}

#[cfg(feature = "rust-support")]
fn rust_file_evidence_from_records(
    imports: Vec<RustImportRecord>,
    module_decls: Vec<RustModuleDeclRecord>,
) -> RustFileEvidence {
    let mut evidence = RustFileEvidence::default();
    for import in imports {
        let path = import.path.to_ascii_lowercase();
        if !path.is_empty() {
            evidence.import_paths.push(path.clone());
            if let Some(last) = path.split("::").filter(|segment| !segment.is_empty()).last() {
                evidence.import_leafs.push(last.to_string());
            }
        }
        if let Some(alias) = import.alias {
            let alias = alias.to_ascii_lowercase();
            if !alias.is_empty() {
                evidence.import_aliases.push(alias);
            }
        }
    }
    for module in module_decls {
        let module_name = module.module_name.to_ascii_lowercase();
        if !module_name.is_empty() {
            evidence.module_names.push(module_name);
        }
        if let Some(path) = module.resolved_path {
            let path = path.replace('\\', "/").to_ascii_lowercase();
            if !path.is_empty() {
                evidence.resolved_paths.push(path);
            }
        }
    }
    evidence.import_paths.sort();
    evidence.import_paths.dedup();
    evidence.import_aliases.sort();
    evidence.import_aliases.dedup();
    evidence.import_leafs.sort();
    evidence.import_leafs.dedup();
    evidence.module_names.sort();
    evidence.module_names.dedup();
    evidence.resolved_paths.sort();
    evidence.resolved_paths.dedup();
    evidence
}

#[cfg(feature = "rust-support")]
fn sort_rust_symbols_with_file_evidence(
    query: &str,
    hints: &RustQueryHints,
    file_evidence: &HashMap<String, RustFileEvidence>,
    mut symbols: Vec<RustIndexedSymbolRecord>,
) -> Vec<RustIndexedSymbolRecord> {
    let normalized = query.trim().to_ascii_lowercase();
    symbols.sort_by(|a, b| {
        rust_file_evidence_rank(a, hints, file_evidence)
            .cmp(&rust_file_evidence_rank(b, hints, file_evidence))
            .then_with(|| rust_indexed_symbol_rank(a, &normalized).cmp(&rust_indexed_symbol_rank(b, &normalized)))
            .then_with(|| a.file.cmp(&b.file))
            .then_with(|| a.start_line.cmp(&b.start_line))
    });
    symbols.dedup_by(|a, b| a.name == b.name && a.file == b.file && a.start_line == b.start_line);
    symbols
}

#[cfg(feature = "rust-support")]
fn rust_definition_anchors_from_rust_analyzer(
    query: &str,
    hints: &RustQueryHints,
    ra_matches: &[RustAnalyzerSymbol],
    all_symbols: &[RustIndexedSymbolRecord],
) -> Vec<RustIndexedSymbolRecord> {
    let mut anchors = Vec::new();
    let mut seen = HashSet::new();
    for ra_symbol in ra_matches {
        let mut candidates = all_symbols
            .iter()
            .filter(|symbol| symbol.file == ra_symbol.file)
            .filter(|symbol| is_rust_definition_candidate_indexed(symbol, query))
            .cloned()
            .collect::<Vec<_>>();
        candidates.sort_by(|a, b| {
            rust_analyzer_anchor_rank(a, ra_symbol, hints)
                .cmp(&rust_analyzer_anchor_rank(b, ra_symbol, hints))
                .then_with(|| a.file.cmp(&b.file))
                .then_with(|| a.start_line.cmp(&b.start_line))
        });
        if let Some(best) = candidates.into_iter().next() {
            let key = format!("{}:{}:{}", best.file, best.start_line, best.name);
            if seen.insert(key) {
                anchors.push(best);
            }
        }
    }
    anchors
}

#[cfg(feature = "rust-support")]
fn rust_analyzer_anchor_rank(
    symbol: &RustIndexedSymbolRecord,
    ra_symbol: &RustAnalyzerSymbol,
    hints: &RustQueryHints,
) -> (u8, u8, u32, String) {
    let leaf_match = if symbol.name.eq_ignore_ascii_case(&hints.leaf) {
        0
    } else {
        1
    };
    let kind_match = if rust_indexed_kind_matches_rust_analyzer(symbol, &ra_symbol.kind) {
        0
    } else {
        1
    };
    let line_distance = symbol.start_line.abs_diff(ra_symbol.start_line);
    (
        leaf_match,
        kind_match,
        line_distance,
        symbol.module_path.clone().unwrap_or_default(),
    )
}

#[cfg(feature = "rust-support")]
fn rust_indexed_kind_matches_rust_analyzer(
    symbol: &RustIndexedSymbolRecord,
    ra_kind: &str,
) -> bool {
    matches!(
        (symbol.kind.as_str(), ra_kind),
        ("struct", "struct")
            | ("enum", "enum")
            | ("trait", "interface")
            | ("trait", "class")
            | ("function", "function")
            | ("method", "method")
            | ("module", "module")
    )
}

#[cfg(feature = "rust-support")]
fn rust_search_match_json_from_rust_analyzer(
    symbol: &RustAnalyzerSymbol,
    indexed: &[RustIndexedSymbolRecord],
) -> serde_json::Value {
    if let Some(indexed_symbol) = indexed
        .iter()
        .filter(|candidate| candidate.file == symbol.file)
        .min_by(|a, b| {
            a.start_line
                .abs_diff(symbol.start_line)
                .cmp(&b.start_line.abs_diff(symbol.start_line))
                .then_with(|| a.file.cmp(&b.file))
        })
    {
        let mut value = rust_indexed_search_match_json(indexed_symbol.clone());
        if let Some(obj) = value.as_object_mut() {
            obj.insert("backend".to_string(), json!("rust_analyzer+index"));
            obj.insert("resolver_kind".to_string(), json!(symbol.kind));
            obj.insert("container_name".to_string(), json!(symbol.container_name));
        }
        return value;
    }
    json!({
        "name": symbol.name,
        "kind": symbol.kind,
        "file": symbol.file,
        "start_line": symbol.start_line,
        "end_line": symbol.end_line,
        "summary": serde_json::Value::Null,
        "signature": serde_json::Value::Null,
        "owner_name": serde_json::Value::Null,
        "trait_name": serde_json::Value::Null,
        "module_path": serde_json::Value::Null,
        "crate_name": serde_json::Value::Null,
        "crate_root": serde_json::Value::Null,
        "backend": "rust_analyzer",
        "container_name": symbol.container_name,
    })
}

#[cfg(feature = "rust-support")]
fn filter_rust_symbols_for_qualified_query(
    hints: &RustQueryHints,
    file_evidence: &HashMap<String, RustFileEvidence>,
    symbols: Vec<RustIndexedSymbolRecord>,
) -> Vec<RustIndexedSymbolRecord> {
    if hints.module_hint.is_none() {
        return symbols;
    }
    let filtered = symbols
        .iter()
        .filter(|symbol| {
            let rank = rust_file_evidence_rank(symbol, hints, file_evidence);
            rank.0 == 0 || rank.2 == 0 || rank.3 == 0
        })
        .cloned()
        .collect::<Vec<_>>();
    if filtered.is_empty() {
        return symbols;
    }
    filtered
}

#[cfg(feature = "rust-support")]
fn rust_file_evidence_rank(
    symbol: &RustIndexedSymbolRecord,
    hints: &RustQueryHints,
    file_evidence: &HashMap<String, RustFileEvidence>,
) -> (u8, u8, u8, u8) {
    let symbol_name = symbol.name.to_ascii_lowercase();
    let module_path = symbol
        .module_path
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let crate_name = symbol
        .crate_name
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let owner_name = symbol
        .owner_name
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let trait_name = symbol
        .trait_name
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let file = symbol.file.replace('\\', "/").to_ascii_lowercase();
    let evidence = file_evidence.get(&symbol.file);

    let qualified_path_bias = match hints.module_hint.as_deref() {
        Some(module_hint)
            if module_path == module_hint
                || module_path.ends_with(&format!("::{module_hint}"))
                || symbol_name == hints.normalized_query
                || symbol_name.starts_with(&format!("{}::", hints.normalized_query)) =>
        {
            0
        }
        Some(module_hint) if module_path.contains(module_hint) || symbol_name.contains(module_hint) => 1,
        Some(_) => 2,
        None => 1,
    };
    let crate_bias = match hints.crate_hint.as_deref() {
        Some(crate_hint) if crate_name == crate_hint => 0,
        Some(_) => 1,
        None => 1,
    };
    let import_bias = match evidence {
        Some(evidence)
            if evidence.import_paths.iter().any(|path| {
                path == &hints.normalized_query
                    || path.ends_with(&format!("::{}", hints.leaf))
                    || (!owner_name.is_empty() && path.ends_with(&format!("::{}", owner_name)))
                    || (!trait_name.is_empty() && path.ends_with(&format!("::{}", trait_name)))
            }) || evidence.import_aliases.iter().any(|alias| alias == &hints.leaf)
                || evidence.import_leafs.iter().any(|leaf| {
                    leaf == &hints.leaf || (!trait_name.is_empty() && leaf == &trait_name)
                }) =>
        {
            0
        }
        Some(evidence)
            if evidence.import_paths.iter().any(|path| {
                path.contains(&hints.leaf)
                    || hints
                        .module_hint
                        .as_deref()
                        .map(|module_hint| path.contains(module_hint))
                        .unwrap_or(false)
            }) || evidence.module_names.iter().any(|module| hints.segments.iter().any(|segment| segment == module)) =>
        {
            1
        }
        _ => 2,
    };
    let resolved_path_bias = match evidence {
        Some(evidence) if evidence.resolved_paths.iter().any(|path| path == &file) => 0,
        Some(evidence)
            if evidence
                .resolved_paths
                .iter()
                .any(|path| path.ends_with(&format!("/{}.rs", hints.leaf)) || path.contains(&hints.leaf)) =>
        {
            1
        }
        _ => 2,
    };
    (qualified_path_bias, crate_bias, import_bias, resolved_path_bias)
}

#[cfg(feature = "rust-support")]
fn rust_indexed_symbol_rank(
    symbol: &RustIndexedSymbolRecord,
    normalized_query: &str,
) -> (u8, u8, u8, u8, String, String, u32) {
    let name = symbol.name.to_ascii_lowercase();
    let owner = symbol
        .owner_name
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let trait_name = symbol
        .trait_name
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let exact = if name == normalized_query { 0 } else { 1 };
    let ownership = if owner == normalized_query
        || trait_name == normalized_query
        || name.starts_with(&format!("{normalized_query}::"))
        || name == format!("impl {normalized_query}")
        || name.ends_with(&format!(" for {normalized_query}"))
    {
        0
    } else if name.contains(normalized_query) {
        1
    } else {
        2
    };
    let rust_kind_bias = match symbol.kind.as_str() {
        "struct" | "enum" | "trait" => 0,
        "impl_block" => 1,
        "method" => 2,
        "function" => 3,
        "module" => 4,
        _ => 5,
    };
    let symbol_type_bias = match symbol.symbol_type {
        SymbolType::Class => 0,
        SymbolType::Function => 1,
        SymbolType::Import => 2,
    };
    (
        exact,
        ownership,
        rust_kind_bias,
        symbol_type_bias,
        symbol.crate_name.clone().unwrap_or_default(),
        symbol.file.clone(),
        symbol.start_line,
    )
}

#[cfg(feature = "rust-support")]
fn rust_indexed_symbol_matches_query(
    symbol: &RustIndexedSymbolRecord,
    normalized_query: &str,
) -> bool {
    let hints = parse_rust_query_hints(normalized_query);
    let name = symbol.name.to_ascii_lowercase();
    let owner = symbol
        .owner_name
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let trait_name = symbol
        .trait_name
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let module_path = symbol
        .module_path
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let crate_name = symbol
        .crate_name
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let direct_match = name == normalized_query
        || name.contains(normalized_query)
        || owner == normalized_query
        || trait_name == normalized_query
        || module_path.contains(normalized_query)
        || name.starts_with(&format!("{normalized_query}::"))
        || name == format!("impl {normalized_query}")
        || name.ends_with(&format!(" for {normalized_query}"));
    if direct_match {
        return true;
    }

    let leaf_match = name == hints.leaf
        || owner == hints.leaf
        || trait_name == hints.leaf
        || name.contains(&hints.leaf);
    if !leaf_match {
        return false;
    }

    let module_match = hints
        .module_hint
        .as_deref()
        .map(|module_hint| {
            module_path == module_hint
                || module_path.ends_with(&format!("::{module_hint}"))
                || module_path.contains(module_hint)
        })
        .unwrap_or(true);
    let crate_match = hints
        .crate_hint
        .as_deref()
        .map(|crate_hint| crate_name == crate_hint || module_path.starts_with(crate_hint))
        .unwrap_or(true);
    module_match || crate_match
}

#[cfg(feature = "rust-support")]
fn preferred_crate_match(
    symbol: &RustIndexedSymbolRecord,
    preferred_crate: Option<&str>,
) -> bool {
    match preferred_crate {
        Some(crate_name) => symbol.crate_name.as_deref() == Some(crate_name),
        None => true,
    }
}

#[cfg(feature = "rust-support")]
fn is_rust_definition_candidate_indexed(
    symbol: &RustIndexedSymbolRecord,
    query: &str,
) -> bool {
    let hints = parse_rust_query_hints(query);
    let lower = symbol.name.to_ascii_lowercase();
    let module_path = symbol
        .module_path
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let crate_name = symbol
        .crate_name
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    matches!(symbol.kind.as_str(), "struct" | "enum" | "trait" | "function")
        && lower == hints.leaf
        && hints
            .module_hint
            .as_deref()
            .map(|module_hint| {
                module_path == module_hint
                    || module_path.ends_with(&format!("::{module_hint}"))
                    || module_path.contains(module_hint)
            })
            .unwrap_or(true)
        && (hints.module_hint.is_some()
            || hints
                .crate_hint
                .as_deref()
                .map(|crate_hint| {
                    crate_name.is_empty() || crate_name == crate_hint || module_path.starts_with(crate_hint)
                })
                .unwrap_or(true))
}

#[cfg(feature = "rust-support")]
fn rust_related_impls_from_metadata(
    query: &str,
    definitions: &[RustIndexedSymbolRecord],
    all_symbols: &[RustIndexedSymbolRecord],
    preferred_crate: Option<&str>,
) -> Vec<RustIndexedSymbolRecord> {
    let hints = parse_rust_query_hints(query);
    let target_names = definitions
        .iter()
        .map(|symbol| symbol.name.clone())
        .collect::<HashSet<_>>();
    let mut impls = all_symbols
        .iter()
        .filter(|symbol| symbol.kind == "impl_block")
        .filter(|symbol| preferred_crate_match(symbol, preferred_crate))
        .filter(|symbol| {
            symbol.owner_name.as_deref() == Some(hints.leaf.as_str())
                || symbol.trait_name.as_deref() == Some(hints.leaf.as_str())
                || symbol
                    .owner_name
                    .as_deref()
                    .map(|owner| target_names.contains(owner))
                    .unwrap_or(false)
                || symbol
                    .trait_name
                    .as_deref()
                    .map(|trait_name| target_names.contains(trait_name))
                    .unwrap_or(false)
        })
        .cloned()
        .collect::<Vec<_>>();
    impls.sort_by(|a, b| a.file.cmp(&b.file).then_with(|| a.start_line.cmp(&b.start_line)));
    impls.dedup_by(|a, b| a.name == b.name && a.file == b.file && a.start_line == b.start_line);
    impls
}

#[cfg(feature = "rust-support")]
fn rust_related_methods_from_metadata(
    query: &str,
    definitions: &[RustIndexedSymbolRecord],
    impls: &[RustIndexedSymbolRecord],
    all_symbols: &[RustIndexedSymbolRecord],
    preferred_crate: Option<&str>,
) -> Vec<RustIndexedSymbolRecord> {
    let hints = parse_rust_query_hints(query);
    let mut owners = definitions
        .iter()
        .map(|symbol| symbol.name.clone())
        .collect::<HashSet<_>>();
    owners.insert(hints.leaf.clone());
    for symbol in impls {
        if let Some(owner) = symbol.owner_name.as_deref() {
            owners.insert(owner.to_string());
        }
    }
    let mut methods = all_symbols
        .iter()
        .filter(|symbol| symbol.kind == "method")
        .filter(|symbol| preferred_crate_match(symbol, preferred_crate))
        .filter(|symbol| {
            symbol
                .owner_name
                .as_deref()
                .map(|owner| owners.contains(owner))
                .unwrap_or(false)
                || symbol
                    .trait_name
                    .as_deref()
                    .map(|trait_name| trait_name.eq_ignore_ascii_case(&hints.leaf))
                    .unwrap_or(false)
                || symbol.name.to_ascii_lowercase().contains(&hints.leaf)
        })
        .cloned()
        .collect::<Vec<_>>();
    methods.sort_by(|a, b| a.file.cmp(&b.file).then_with(|| a.start_line.cmp(&b.start_line)));
    methods.dedup_by(|a, b| a.name == b.name && a.file == b.file && a.start_line == b.start_line);
    methods
}

#[cfg(feature = "rust-support")]
fn rust_modules_for_symbols<'a>(
    symbols: impl Iterator<Item = &'a RustIndexedSymbolRecord>,
) -> Vec<String> {
    let mut modules = symbols
        .map(|symbol| {
            symbol
                .module_path
                .clone()
                .unwrap_or_else(|| rust_module_for_file(&symbol.file))
        })
        .filter(|module| !module.is_empty())
        .collect::<Vec<_>>();
    modules.sort();
    modules.dedup();
    modules
}

#[cfg(feature = "rust-support")]
fn rust_module_for_file(file: &str) -> String {
    let normalized = file.replace('\\', "/");
    let path = std::path::Path::new(&normalized);
    let mut parts = Vec::new();
    for component in path.components() {
        let value = component.as_os_str().to_string_lossy();
        if value.ends_with(".rs") {
            let stem = value.trim_end_matches(".rs");
            if !matches!(stem, "lib" | "main" | "mod") {
                parts.push(stem.to_string());
            }
            break;
        }
        if !value.is_empty() {
            parts.push(value.to_string());
        }
    }
    parts.join("::")
}

#[cfg(feature = "rust-support")]
fn rust_indexed_search_match_json(symbol: RustIndexedSymbolRecord) -> serde_json::Value {
    json!({
        "name": symbol.name,
        "kind": symbol.kind,
        "file": symbol.file,
        "start_line": symbol.start_line,
        "end_line": symbol.end_line,
        "summary": symbol.summary,
        "signature": symbol.signature,
        "owner_name": symbol.owner_name,
        "trait_name": symbol.trait_name,
        "module_path": symbol.module_path,
        "crate_name": symbol.crate_name,
        "crate_root": symbol.crate_root,
    })
}

#[cfg(feature = "rust-support")]
fn rust_indexed_context_item_json(
    repo_root: &Path,
    symbol: &RustIndexedSymbolRecord,
    max_tokens: usize,
    prefer_header_only: bool,
) -> serde_json::Value {
    let (start_line, end_line, code, span_mode) =
        rust_indexed_symbol_snippet(repo_root, symbol, max_tokens, prefer_header_only);
    json!({
        "name": symbol.name,
        "kind": symbol.kind,
        "file": symbol.file,
        "start_line": symbol.start_line,
        "end_line": symbol.end_line,
        "signature": symbol.signature,
        "summary": symbol.summary,
        "owner_name": symbol.owner_name,
        "trait_name": symbol.trait_name,
        "module_path": symbol.module_path,
        "crate_name": symbol.crate_name,
        "crate_root": symbol.crate_root,
        "span_mode": span_mode,
        "context_start_line": start_line,
        "context_end_line": end_line,
        "code": code,
    })
}

#[cfg(feature = "rust-support")]
fn rust_indexed_symbol_snippet(
    repo_root: &Path,
    symbol: &RustIndexedSymbolRecord,
    max_tokens: usize,
    prefer_header_only: bool,
) -> (u32, u32, String, &'static str) {
    let file_path = repo_root.join(&symbol.file);
    let Ok(raw) = std::fs::read_to_string(file_path) else {
        return (symbol.start_line, symbol.end_line, String::new(), "unavailable");
    };
    let lines = raw.lines().collect::<Vec<_>>();
    if lines.is_empty() {
        return (symbol.start_line, symbol.end_line, String::new(), "empty");
    }

    let header_only = prefer_header_only || symbol.name.starts_with("impl ");
    let default_line_budget = if max_tokens <= 280 {
        6
    } else if max_tokens <= 600 {
        14
    } else {
        28
    };
    let start = symbol.start_line.max(1);
    let mut end = symbol.end_line.max(start);
    if header_only {
        end = start;
    } else {
        end = end.min(start.saturating_add(default_line_budget - 1));
    }
    let start_idx = start.saturating_sub(1) as usize;
    let end_idx = end.min(lines.len() as u32) as usize;
    let code = lines
        .iter()
        .skip(start_idx)
        .take(end_idx.saturating_sub(start_idx))
        .copied()
        .collect::<Vec<_>>()
        .join("\n");
    (
        start,
        end,
        code,
        if header_only {
            "header_only"
        } else if end < symbol.end_line {
            "truncated_exact_span"
        } else {
            "exact_span"
        },
    )
}

fn build_filesystem_architecture_map(repo_root: &Path) -> serde_json::Value {
    let mut modules = Vec::new();
    let entries = std::fs::read_dir(repo_root)
        .ok()
        .into_iter()
        .flat_map(|items| items.filter_map(|item| item.ok()))
        .collect::<Vec<_>>();
    let mut sorted = entries;
    sorted.sort_by_key(|entry| entry.path());

    for entry in sorted {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if should_skip_architecture_dir(&name) {
            continue;
        }
        let module_path = path
            .strip_prefix(repo_root)
            .ok()
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default();
        let files = sample_filesystem_architecture_files(repo_root, &path);
        let entry_points = files
            .iter()
            .filter_map(|file| architecture_entry_point_name(file))
            .collect::<Vec<_>>();
        let signals = cold_start_signals(&name, &files, !entry_points.is_empty());
        modules.push(json!({
            "name": name,
            "path": module_path.clone(),
            "support_level": "filesystem_heuristic",
            "importance": importance_for_signals(&signals),
            "signals": signals,
            "entry_points": entry_points,
            "files": files
                .iter()
                .map(|file| short_file_label(file, &module_path))
                .collect::<Vec<_>>(),
            "indexed_file_count": 0,
            "source_file_count": files.len(),
            "fan_out": 0,
        }));
    }

    let mut modules = filter_fixture_dominant_architecture_modules_in_place(modules);
    sort_architecture_modules(&mut modules);
    let discovered_module_count = modules.len();
    let visible_module_count = modules.len();
    let high_priority_modules = architecture_high_priority_summary(&modules);
    let priority_modules = architecture_priority_modules(&modules);
    let priority_focus_mode = architecture_priority_focus_mode(&priority_modules);
    let priority_focus_reason =
        architecture_priority_focus_reason(&priority_modules, priority_focus_mode);
    let priority_focus_targets =
        architecture_priority_focus_targets(&priority_modules, priority_focus_mode);
    let priority_focus_entries =
        architecture_priority_focus_entries(&priority_modules, &priority_focus_targets);
    let priority_focus_trust = architecture_priority_focus_trust(&priority_focus_entries);
    let priority_focus_commands =
        architecture_priority_focus_commands(&priority_focus_entries);
    let priority_focus_follow_up_operations =
        architecture_priority_focus_follow_up_operations(&priority_focus_entries);
    let priority_focus_primary_entry =
        architecture_priority_focus_primary_entry(&priority_focus_entries);
    let priority_focus_primary_trust =
        architecture_priority_entry_trust(&priority_focus_primary_entry);
    let priority_focus_secondary_entry =
        architecture_priority_focus_secondary_entry(&priority_focus_entries);
    let priority_focus_secondary_trust =
        if priority_focus_secondary_entry.is_null() {
            serde_json::Value::Null
        } else {
            serde_json::Value::String(
                architecture_priority_entry_trust(&priority_focus_secondary_entry)
                    .to_string(),
            )
        };
    json!({
        "orientation_stage": "filesystem_heuristic",
        "priority_scoring_model": "architecture_priority_v1",
        "priority_scoring_weights": architecture_priority_scoring_weights(),
        "priority_focus_mode": priority_focus_mode,
        "priority_focus_reason": priority_focus_reason,
        "priority_focus_trust": priority_focus_trust,
        "priority_focus_targets": priority_focus_targets,
        "priority_focus_entries": priority_focus_entries,
        "priority_focus_commands": priority_focus_commands,
        "priority_focus_follow_up_operations": priority_focus_follow_up_operations,
        "priority_focus_primary_target": priority_focus_primary_entry.get("name").cloned().unwrap_or(serde_json::Value::Null),
        "priority_focus_primary_path": priority_focus_primary_entry.get("path").cloned().unwrap_or(serde_json::Value::Null),
        "priority_focus_primary_importance": priority_focus_primary_entry.get("importance").cloned().unwrap_or(serde_json::Value::Null),
        "priority_focus_primary_support_level": priority_focus_primary_entry.get("support_level").cloned().unwrap_or(serde_json::Value::Null),
        "priority_focus_primary_actionability": priority_focus_primary_entry.get("actionability").cloned().unwrap_or(serde_json::Value::Null),
        "priority_focus_primary_trust": priority_focus_primary_trust,
        "priority_focus_primary_rank": priority_focus_primary_entry.get("priority_rank").cloned().unwrap_or(serde_json::Value::Null),
        "priority_focus_primary_score": priority_focus_primary_entry.get("priority_score").cloned().unwrap_or(serde_json::Value::Null),
        "priority_focus_primary_score_components": priority_focus_primary_entry.get("priority_score_components").cloned().unwrap_or(serde_json::Value::Null),
        "priority_focus_primary_score_gap_from_previous": priority_focus_primary_entry.get("priority_score_gap_from_previous").cloned().unwrap_or(serde_json::Value::Null),
        "priority_focus_primary_score_gap_to_next": priority_focus_primary_entry.get("priority_score_gap_to_next").cloned().unwrap_or(serde_json::Value::Null),
        "priority_focus_primary_score_separation": priority_focus_primary_entry.get("priority_score_separation").cloned().unwrap_or(serde_json::Value::Null),
        "priority_focus_primary_signals": priority_focus_primary_entry.get("signals").cloned().unwrap_or(serde_json::Value::Null),
        "priority_focus_primary_entry_points": priority_focus_primary_entry.get("entry_points").cloned().unwrap_or(serde_json::Value::Null),
        "priority_focus_primary_files": priority_focus_primary_entry.get("files").cloned().unwrap_or(serde_json::Value::Null),
        "priority_focus_primary_indexed_file_count": priority_focus_primary_entry.get("indexed_file_count").cloned().unwrap_or(serde_json::Value::Null),
        "priority_focus_primary_source_file_count": priority_focus_primary_entry.get("source_file_count").cloned().unwrap_or(serde_json::Value::Null),
        "priority_focus_primary_fan_out": priority_focus_primary_entry.get("fan_out").cloned().unwrap_or(serde_json::Value::Null),
        "priority_focus_primary_open_first_path": priority_focus_primary_entry.get("open_first_path").cloned().unwrap_or(serde_json::Value::Null),
        "priority_focus_primary_next_step_operation": priority_focus_primary_entry.get("next_step_operation").cloned().unwrap_or(serde_json::Value::Null),
        "priority_focus_primary_next_step_target_kind": priority_focus_primary_entry.get("next_step_target_kind").cloned().unwrap_or(serde_json::Value::Null),
        "priority_focus_primary_next_step_target_path": priority_focus_primary_entry.get("next_step_target_path").cloned().unwrap_or(serde_json::Value::Null),
        "priority_focus_primary_command": priority_focus_primary_entry.get("next_step_command").cloned().unwrap_or(serde_json::Value::Null),
        "priority_focus_secondary_target": priority_focus_secondary_entry.get("name").cloned().unwrap_or(serde_json::Value::Null),
        "priority_focus_secondary_path": priority_focus_secondary_entry.get("path").cloned().unwrap_or(serde_json::Value::Null),
        "priority_focus_secondary_importance": priority_focus_secondary_entry.get("importance").cloned().unwrap_or(serde_json::Value::Null),
        "priority_focus_secondary_support_level": priority_focus_secondary_entry.get("support_level").cloned().unwrap_or(serde_json::Value::Null),
        "priority_focus_secondary_actionability": priority_focus_secondary_entry.get("actionability").cloned().unwrap_or(serde_json::Value::Null),
        "priority_focus_secondary_trust": priority_focus_secondary_trust,
        "priority_focus_secondary_rank": priority_focus_secondary_entry.get("priority_rank").cloned().unwrap_or(serde_json::Value::Null),
        "priority_focus_secondary_score": priority_focus_secondary_entry.get("priority_score").cloned().unwrap_or(serde_json::Value::Null),
        "priority_focus_secondary_score_components": priority_focus_secondary_entry.get("priority_score_components").cloned().unwrap_or(serde_json::Value::Null),
        "priority_focus_secondary_score_gap_from_previous": priority_focus_secondary_entry.get("priority_score_gap_from_previous").cloned().unwrap_or(serde_json::Value::Null),
        "priority_focus_secondary_score_gap_to_next": priority_focus_secondary_entry.get("priority_score_gap_to_next").cloned().unwrap_or(serde_json::Value::Null),
        "priority_focus_secondary_score_separation": priority_focus_secondary_entry.get("priority_score_separation").cloned().unwrap_or(serde_json::Value::Null),
        "priority_focus_secondary_signals": priority_focus_secondary_entry.get("signals").cloned().unwrap_or(serde_json::Value::Null),
        "priority_focus_secondary_entry_points": priority_focus_secondary_entry.get("entry_points").cloned().unwrap_or(serde_json::Value::Null),
        "priority_focus_secondary_files": priority_focus_secondary_entry.get("files").cloned().unwrap_or(serde_json::Value::Null),
        "priority_focus_secondary_indexed_file_count": priority_focus_secondary_entry.get("indexed_file_count").cloned().unwrap_or(serde_json::Value::Null),
        "priority_focus_secondary_source_file_count": priority_focus_secondary_entry.get("source_file_count").cloned().unwrap_or(serde_json::Value::Null),
        "priority_focus_secondary_fan_out": priority_focus_secondary_entry.get("fan_out").cloned().unwrap_or(serde_json::Value::Null),
        "priority_focus_secondary_open_first_path": priority_focus_secondary_entry.get("open_first_path").cloned().unwrap_or(serde_json::Value::Null),
        "priority_focus_secondary_next_step_operation": priority_focus_secondary_entry.get("next_step_operation").cloned().unwrap_or(serde_json::Value::Null),
        "priority_focus_secondary_next_step_target_kind": priority_focus_secondary_entry.get("next_step_target_kind").cloned().unwrap_or(serde_json::Value::Null),
        "priority_focus_secondary_next_step_target_path": priority_focus_secondary_entry.get("next_step_target_path").cloned().unwrap_or(serde_json::Value::Null),
        "priority_focus_secondary_command": priority_focus_secondary_entry.get("next_step_command").cloned().unwrap_or(serde_json::Value::Null),
        "summary": format!("Cold-start orientation from {discovered_module_count} top-level folder(s)"),
        "discovered_module_count": discovered_module_count,
        "visible_module_count": visible_module_count,
        "grouped_hidden_module_count": 0,
        "modules": modules,
        "high_priority_modules": high_priority_modules,
        "priority_modules": priority_modules,
    })
}

fn architecture_high_priority_summary(modules: &[serde_json::Value]) -> String {
    architecture_priority_source_modules(modules)
        .into_iter()
        .take(3)
        .map(|module| {
            let name = module.get("name").and_then(|v| v.as_str()).unwrap_or("?");
            let importance = module
                .get("importance")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let support_level = module
                .get("support_level")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let actionability = architecture_priority_actionability(module);
            if let Some(trust) = architecture_priority_trust_suffix(support_level, actionability) {
                format!("{name} [{importance}, {trust}]")
            } else {
                format!("{name} [{importance}]")
            }
        })
        .collect::<Vec<_>>()
        .join(" | ")
}

fn architecture_priority_modules(modules: &[serde_json::Value]) -> Vec<serde_json::Value> {
    let priority_source_modules = architecture_priority_source_modules(modules)
        .into_iter()
        .take(3)
        .map(|module| {
            let path = module.get("path").and_then(|v| v.as_str());
            let open_first_path = architecture_open_first_path(module);
            let actionability = architecture_priority_actionability(module);
            let priority_score_components = architecture_priority_score_components(module);
            let priority_score = architecture_priority_score(&priority_score_components);
            let (next_step_operation, next_step_target_path) =
                architecture_priority_next_step(module, actionability, open_first_path.as_deref());
            let next_step_target_kind =
                architecture_priority_next_step_target_kind(next_step_operation, next_step_target_path.as_deref());
            let next_step_command = architecture_priority_next_step_command(
                next_step_operation,
                next_step_target_path.as_deref(),
            );
            json!({
                "priority_score": priority_score,
                "priority_score_components": {
                    "importance": priority_score_components.importance,
                    "support": priority_score_components.support,
                    "signals": priority_score_components.signals,
                },
                "name": module.get("name").cloned().unwrap_or(serde_json::Value::Null),
                "path": path.map(|value| serde_json::Value::String(value.to_string())).unwrap_or(serde_json::Value::Null),
                "importance": module.get("importance").cloned().unwrap_or(serde_json::Value::Null),
                "support_level": module.get("support_level").cloned().unwrap_or(serde_json::Value::Null),
                "actionability": serde_json::Value::String(actionability.to_string()),
                "signals": module.get("signals").cloned().unwrap_or(serde_json::Value::Null),
                "entry_points": module.get("entry_points").cloned().unwrap_or(serde_json::Value::Null),
                "files": module.get("files").cloned().unwrap_or(serde_json::Value::Null),
                "indexed_file_count": module.get("indexed_file_count").cloned().unwrap_or(serde_json::Value::Null),
                "source_file_count": module.get("source_file_count").cloned().unwrap_or(serde_json::Value::Null),
                "fan_out": module.get("fan_out").cloned().unwrap_or(serde_json::Value::Null),
                "open_first_path": open_first_path.map(serde_json::Value::String).unwrap_or(serde_json::Value::Null),
                "next_step_operation": serde_json::Value::String(next_step_operation.to_string()),
                "next_step_target_kind": serde_json::Value::String(next_step_target_kind.to_string()),
                "next_step_target_path": next_step_target_path.map(serde_json::Value::String).unwrap_or(serde_json::Value::Null),
                "next_step_command": serde_json::Value::String(next_step_command),
            })
        })
        .collect::<Vec<_>>()
        ;
    architecture_priority_with_gaps(priority_source_modules)
}

fn architecture_priority_source_modules(modules: &[serde_json::Value]) -> Vec<&serde_json::Value> {
    let mut concrete = modules
        .iter()
        .filter(|module| {
            module.get("support_level").and_then(|v| v.as_str())
                != Some("unsupported_source_group")
        })
        .collect::<Vec<_>>();
    if concrete.len() >= 3 {
        concrete.truncate(3);
        return concrete;
    }
    let mut combined = concrete;
    combined.extend(modules.iter().filter(|module| {
        module.get("support_level").and_then(|v| v.as_str())
            == Some("unsupported_source_group")
    }));
    combined.truncate(3);
    combined
}

fn architecture_priority_actionability(module: &serde_json::Value) -> &'static str {
    match module
        .get("support_level")
        .and_then(|v| v.as_str())
        .unwrap_or("filesystem_heuristic")
    {
        "indexed" => "semantic_precise",
        "unsupported_source" => "orientation_only",
        "unsupported_source_group" => "grouped_overview",
        _ => "filesystem_heuristic",
    }
}

fn architecture_priority_trust_suffix(
    support_level: &str,
    actionability: &str,
) -> Option<String> {
    if support_level == "indexed" && actionability == "semantic_precise" {
        return None;
    }
    if actionability == "unknown" || actionability == support_level {
        return Some(support_level.to_string());
    }
    Some(format!("{support_level}, {actionability}"))
}

fn architecture_priority_next_step(
    module: &serde_json::Value,
    actionability: &str,
    open_first_path: Option<&str>,
) -> (&'static str, Option<String>) {
    let module_path = module.get("path").and_then(|v| v.as_str()).filter(|path| !path.is_empty() && *path != "(grouped)");
    match actionability {
        "semantic_precise" | "orientation_only" | "filesystem_heuristic" => {
            if let Some(path) = open_first_path {
                ("get_file_brief", Some(path.to_string()))
            } else if let Some(path) = module_path {
                ("get_directory_brief", Some(path.to_string()))
            } else {
                ("get_directory_brief", None)
            }
        }
        "grouped_overview" => ("get_directory_brief", module_path.map(|path| path.to_string())),
        _ => {
            if let Some(path) = open_first_path {
                ("get_file_brief", Some(path.to_string()))
            } else {
                ("get_directory_brief", module_path.map(|path| path.to_string()))
            }
        }
    }
}

fn architecture_priority_next_step_command(
    operation: &str,
    target_path: Option<&str>,
) -> String {
    match target_path {
        Some(path) if !path.is_empty() => {
            let target_flag = match operation {
                "get_file_brief" => "--file",
                _ => "--path",
            };
            format!(
                "semantic --repo . retrieve --op {operation} {target_flag} {} --output text",
                shell_quote_argument(path)
            )
        }
        _ => format!("semantic --repo . retrieve --op {operation} --output text"),
    }
}

fn architecture_priority_next_step_target_kind(
    operation: &str,
    target_path: Option<&str>,
) -> &'static str {
    match operation {
        "get_file_brief" => "file",
        "get_directory_brief" if target_path.is_some() => "directory",
        _ => "workspace",
    }
}

struct ArchitecturePriorityScoreComponents {
    importance: usize,
    support: usize,
    signals: usize,
}

fn architecture_priority_score_components(
    module: &serde_json::Value,
) -> ArchitecturePriorityScoreComponents {
    let importance = architecture_importance_rank(module) * 100;
    let support = match architecture_support_rank(module) {
        0 => 30,
        1 => 20,
        2 => 10,
        _ => 0,
    };
    let signals = architecture_signal_count(module);
    ArchitecturePriorityScoreComponents {
        importance,
        support,
        signals,
    }
}

fn architecture_priority_score(components: &ArchitecturePriorityScoreComponents) -> usize {
    components.importance + components.support + components.signals
}

fn architecture_priority_with_gaps(
    modules: Vec<serde_json::Value>,
) -> Vec<serde_json::Value> {
    let scores = modules
        .iter()
        .map(|module| {
            module
                .get("priority_score")
                .and_then(|value| value.as_u64())
                .unwrap_or(0)
        })
        .collect::<Vec<_>>();
    modules
        .into_iter()
        .enumerate()
        .map(|(index, mut module)| {
            let previous_gap = if index == 0 {
                None
            } else {
                Some(scores[index - 1].saturating_sub(scores[index]))
            };
            let next_gap = if index + 1 >= scores.len() {
                None
            } else {
                Some(scores[index].saturating_sub(scores[index + 1]))
            };
            if let Some(obj) = module.as_object_mut() {
                let separation = architecture_priority_separation(previous_gap, next_gap);
                obj.insert("priority_rank".to_string(), json!(index + 1));
                obj.insert(
                    "priority_score_gap_from_previous".to_string(),
                    previous_gap.map(serde_json::Value::from).unwrap_or(serde_json::Value::Null),
                );
                obj.insert(
                    "priority_score_gap_to_next".to_string(),
                    next_gap.map(serde_json::Value::from).unwrap_or(serde_json::Value::Null),
                );
                obj.insert(
                    "priority_score_separation".to_string(),
                    serde_json::Value::String(separation.to_string()),
                );
            }
            module
        })
        .collect()
}

fn architecture_priority_separation(
    previous_gap: Option<u64>,
    next_gap: Option<u64>,
) -> &'static str {
    match (previous_gap, next_gap) {
        (None, None) => "solo",
        (_, Some(gap)) if gap >= 5 => "clear_lead",
        (Some(gap), None) if gap >= 5 => "clear_trailing_gap",
        (Some(_), None) => "close_trailing_gap",
        _ => "close_cluster",
    }
}

fn architecture_priority_scoring_weights() -> serde_json::Value {
    json!({
        "importance_multiplier": 100,
        "support_weights": {
            "indexed": 30,
            "unsupported_source": 20,
            "unsupported_source_group": 10,
            "filesystem_heuristic": 0
        },
        "signal_increment": 1
    })
}

fn architecture_priority_focus_mode(priority_modules: &[serde_json::Value]) -> &'static str {
    match priority_modules.len() {
        0 => "no_priority_targets",
        1 => "single_focus",
        _ => {
            let first = priority_modules.first();
            let second = priority_modules.get(1);
            let first_sep = first
                .and_then(|module| module.get("priority_score_separation"))
                .and_then(|value| value.as_str())
                .unwrap_or("close_cluster");
            let first_gap = first
                .and_then(|module| module.get("priority_score_gap_to_next"))
                .and_then(|value| value.as_u64())
                .unwrap_or(0);
            let second_sep = second
                .and_then(|module| module.get("priority_score_separation"))
                .and_then(|value| value.as_str())
                .unwrap_or("close_cluster");

            if first_sep == "clear_lead" && first_gap >= 5 {
                "single_focus"
            } else if second_sep == "close_cluster" || first_gap <= 2 {
                "compare_top_two"
            } else {
                "review_priority_cluster"
            }
        }
    }
}

fn architecture_priority_focus_reason(
    priority_modules: &[serde_json::Value],
    focus_mode: &str,
) -> String {
    match focus_mode {
        "no_priority_targets" => "no priority modules available".to_string(),
        "single_focus" => {
            if priority_modules.len() <= 1 {
                return "only one priority module is available".to_string();
            }
            let lead = priority_modules
                .first()
                .and_then(|module| module.get("name"))
                .and_then(|value| value.as_str())
                .unwrap_or("top module");
            let gap = priority_modules
                .first()
                .and_then(|module| module.get("priority_score_gap_to_next"))
                .and_then(|value| value.as_u64())
                .unwrap_or(0);
            format!("{lead} has a clear lead over the next candidate (gap={gap})")
        }
        "compare_top_two" => {
            let first = priority_modules
                .first()
                .and_then(|module| module.get("name"))
                .and_then(|value| value.as_str())
                .unwrap_or("first");
            let second = priority_modules
                .get(1)
                .and_then(|module| module.get("name"))
                .and_then(|value| value.as_str())
                .unwrap_or("second");
            let gap = priority_modules
                .first()
                .and_then(|module| module.get("priority_score_gap_to_next"))
                .and_then(|value| value.as_u64())
                .unwrap_or(0);
            format!("{first} and {second} are close enough to compare first (gap={gap})")
        }
        "review_priority_cluster" => {
            let cluster = priority_modules
                .iter()
                .take(3)
                .filter_map(|module| module.get("name").and_then(|value| value.as_str()))
                .collect::<Vec<_>>();
            if cluster.is_empty() {
                "top priority cluster needs review".to_string()
            } else {
                format!("top priority cluster is still competitive: {}", cluster.join(", "))
            }
        }
        _ => "priority focus decided from current ranking gaps".to_string(),
    }
}

fn architecture_priority_focus_targets(
    priority_modules: &[serde_json::Value],
    focus_mode: &str,
) -> Vec<String> {
    let take_count = match focus_mode {
        "compare_top_two" => 2,
        "review_priority_cluster" => 3,
        "single_focus" => 1,
        _ => 0,
    };
    priority_modules
        .iter()
        .take(take_count)
        .filter_map(|module| {
            module
                .get("name")
                .and_then(|value| value.as_str())
                .map(|value| value.to_string())
        })
        .collect()
}

fn architecture_priority_focus_commands(
    focus_entries: &[serde_json::Value],
) -> Vec<String> {
    focus_entries
        .iter()
        .filter_map(|entry| {
            let name = entry.get("name").and_then(|value| value.as_str())?;
            let command = entry
                .get("next_step_command")
                .and_then(|value| value.as_str())?;
            Some(format!("{name} -> {command}"))
        })
        .collect()
}

fn architecture_priority_focus_follow_up_operations(
    focus_entries: &[serde_json::Value],
) -> Vec<serde_json::Value> {
    focus_entries
        .iter()
        .map(|entry| {
            json!({
                "target": entry.get("name").cloned().unwrap_or(serde_json::Value::Null),
                "path": entry.get("path").cloned().unwrap_or(serde_json::Value::Null),
                "support_level": entry.get("support_level").cloned().unwrap_or(serde_json::Value::Null),
                "actionability": entry.get("actionability").cloned().unwrap_or(serde_json::Value::Null),
                "trust": architecture_priority_entry_trust(entry),
                "operation": entry.get("next_step_operation").cloned().unwrap_or(serde_json::Value::Null),
                "target_kind": entry.get("next_step_target_kind").cloned().unwrap_or(serde_json::Value::Null),
                "target_path": entry.get("next_step_target_path").cloned().unwrap_or(serde_json::Value::Null),
                "command": entry.get("next_step_command").cloned().unwrap_or(serde_json::Value::Null),
            })
        })
        .collect()
}

fn architecture_priority_focus_entries(
    priority_modules: &[serde_json::Value],
    focus_targets: &[String],
) -> Vec<serde_json::Value> {
    priority_modules
        .iter()
        .filter(|module| {
            module
                .get("name")
                .and_then(|value| value.as_str())
                .map(|name| focus_targets.iter().any(|target| target == name))
                .unwrap_or(false)
        })
        .map(|module| {
            json!({
                "name": module.get("name").cloned().unwrap_or(serde_json::Value::Null),
                "path": module.get("path").cloned().unwrap_or(serde_json::Value::Null),
                "importance": module.get("importance").cloned().unwrap_or(serde_json::Value::Null),
                "support_level": module.get("support_level").cloned().unwrap_or(serde_json::Value::Null),
                "actionability": module.get("actionability").cloned().unwrap_or(serde_json::Value::Null),
                "priority_rank": module.get("priority_rank").cloned().unwrap_or(serde_json::Value::Null),
                "priority_score": module.get("priority_score").cloned().unwrap_or(serde_json::Value::Null),
                "priority_score_components": module.get("priority_score_components").cloned().unwrap_or(serde_json::Value::Null),
                "priority_score_gap_from_previous": module.get("priority_score_gap_from_previous").cloned().unwrap_or(serde_json::Value::Null),
                "priority_score_gap_to_next": module.get("priority_score_gap_to_next").cloned().unwrap_or(serde_json::Value::Null),
                "priority_score_separation": module.get("priority_score_separation").cloned().unwrap_or(serde_json::Value::Null),
                "signals": module.get("signals").cloned().unwrap_or(serde_json::Value::Null),
                "entry_points": module.get("entry_points").cloned().unwrap_or(serde_json::Value::Null),
                "files": module.get("files").cloned().unwrap_or(serde_json::Value::Null),
                "indexed_file_count": module.get("indexed_file_count").cloned().unwrap_or(serde_json::Value::Null),
                "source_file_count": module.get("source_file_count").cloned().unwrap_or(serde_json::Value::Null),
                "fan_out": module.get("fan_out").cloned().unwrap_or(serde_json::Value::Null),
                "open_first_path": module.get("open_first_path").cloned().unwrap_or(serde_json::Value::Null),
                "next_step_operation": module.get("next_step_operation").cloned().unwrap_or(serde_json::Value::Null),
                "next_step_target_kind": module.get("next_step_target_kind").cloned().unwrap_or(serde_json::Value::Null),
                "next_step_target_path": module.get("next_step_target_path").cloned().unwrap_or(serde_json::Value::Null),
                "next_step_command": module.get("next_step_command").cloned().unwrap_or(serde_json::Value::Null),
            })
        })
        .collect()
}

fn architecture_priority_focus_trust(focus_entries: &[serde_json::Value]) -> &'static str {
    if focus_entries.is_empty() {
        return "unknown";
    }
    let mut has_precise = false;
    let mut has_non_precise = false;
    for entry in focus_entries {
        let support_level = entry
            .get("support_level")
            .and_then(|value| value.as_str())
            .unwrap_or("unknown");
        let actionability = entry
            .get("actionability")
            .and_then(|value| value.as_str())
            .unwrap_or("unknown");
        if support_level == "indexed" && actionability == "semantic_precise" {
            has_precise = true;
        } else {
            has_non_precise = true;
        }
    }
    match (has_precise, has_non_precise) {
        (true, false) => "semantic_precise",
        (false, true) => "orientation_only",
        (true, true) => "mixed",
        _ => "unknown",
    }
}

fn architecture_priority_entry_trust(entry: &serde_json::Value) -> &'static str {
    let support_level = entry
        .get("support_level")
        .and_then(|value| value.as_str())
        .unwrap_or("unknown");
    let actionability = entry
        .get("actionability")
        .and_then(|value| value.as_str())
        .unwrap_or("unknown");
    if support_level == "indexed" && actionability == "semantic_precise" {
        "semantic_precise"
    } else if actionability == "orientation_only" || actionability == "filesystem_heuristic" {
        "orientation_only"
    } else {
        "unknown"
    }
}

fn architecture_priority_focus_primary_entry(
    focus_entries: &[serde_json::Value],
) -> serde_json::Value {
    focus_entries
        .first()
        .cloned()
        .unwrap_or(serde_json::Value::Null)
}

fn architecture_priority_focus_secondary_entry(
    focus_entries: &[serde_json::Value],
) -> serde_json::Value {
    focus_entries
        .get(1)
        .cloned()
        .unwrap_or(serde_json::Value::Null)
}

fn shell_quote_argument(value: &str) -> String {
    let escaped = value.replace('"', "\\\"");
    format!("\"{escaped}\"")
}

fn architecture_open_first_path(module: &serde_json::Value) -> Option<String> {
    let path = module.get("path").and_then(|v| v.as_str())?;
    if path.is_empty() || path == "(grouped)" {
        return None;
    }
    let files = module
        .get("files")
        .and_then(|v| v.as_array())
        .map(|items| {
            items.iter()
                .filter_map(|item| item.as_str().map(|value| value.to_string()))
                .collect::<Vec<_>>()
        })?;
    let entry_points = module
        .get("entry_points")
        .and_then(|v| v.as_array())
        .map(|items| {
            items.iter()
                .filter_map(|item| item.as_str().map(|value| value.to_string()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let selected_file = select_priority_open_first_file(&files, &entry_points)?;
    Some(join_architecture_module_file_path(path, selected_file))
}

fn join_architecture_module_file_path(module_path: &str, file: &str) -> String {
    if file.contains('/') {
        format!(
            "{}/{}",
            module_path.trim_matches('/'),
            file.trim_matches('/')
        )
    } else {
        format!("{}/{}", module_path.trim_matches('/'), file)
    }
}

fn select_priority_open_first_file<'a>(
    files: &'a [String],
    entry_points: &[String],
) -> Option<&'a str> {
    let best_match = files
        .iter()
        .enumerate()
        .filter_map(|(index, file)| {
            score_priority_open_first_file(file, entry_points)
                .map(|score| (score, std::cmp::Reverse(index), file.as_str()))
        })
        .max_by(|left, right| left.cmp(right))
        .map(|(_, _, file)| file);
    best_match.or_else(|| files.first().map(|file| file.as_str()))
}

fn score_priority_open_first_file(file: &str, entry_points: &[String]) -> Option<usize> {
    let file_name = Path::new(file).file_name().and_then(|name| name.to_str())?;
    let stem = strip_extension(file_name);
    let stem_normalized = normalize_architecture_match_token(&stem);
    if stem_normalized.is_empty() {
        return None;
    }
    let mut best = 0usize;
    for entry_point in entry_points {
        let entry_normalized = normalize_architecture_match_token(entry_point);
        if entry_normalized.is_empty() {
            continue;
        }
        let score = if stem_normalized == entry_normalized {
            4
        } else if stem_normalized.contains(&entry_normalized) {
            3
        } else if entry_normalized.contains(&stem_normalized) {
            2
        } else {
            0
        };
        best = best.max(score);
    }
    if best > 0 { Some(best) } else { None }
}

fn normalize_architecture_match_token(value: &str) -> String {
    value
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(|ch| ch.to_lowercase())
        .collect()
}

fn sample_filesystem_architecture_files(repo_root: &Path, module_dir: &Path) -> Vec<String> {
    WalkDir::new(module_dir)
        .min_depth(1)
        .max_depth(3)
        .sort_by_file_name()
        .into_iter()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_file())
        .filter_map(|entry| {
            let relative = entry.path().strip_prefix(repo_root).ok()?;
            let relative = relative.to_string_lossy().replace('\\', "/");
            if should_skip_status_source_scan_path(&relative) {
                return None;
            }
            Some(relative)
        })
        .take(6)
        .collect()
}

fn derive_modules_from_files(files: &[String]) -> BTreeMap<String, Vec<String>> {
    let mut groups = BTreeMap::<String, Vec<String>>::new();
    for file in files {
        let key = derive_module_key(file);
        groups.entry(key).or_default().push(file.clone());
    }
    groups
}

fn derive_module_key(file: &str) -> String {
    let parts = file.split('/').collect::<Vec<_>>();
    if parts.len() >= 3 && matches!(parts[0], "src" | "lib" | "app") {
        format!("{}/{}", parts[0], parts[1])
    } else if parts.len() >= 3 && matches!(parts[0], "packages" | "apps" | "services") {
        format!("{}/{}", parts[0], parts[1])
    } else {
        parts.first().copied().unwrap_or("root").to_string()
    }
}

fn rank_recent_modules(repo_root: &Path, modules: &[(String, String, Vec<String>)]) -> HashSet<String> {
    let mut ranked = modules
        .iter()
        .map(|(name, _path, files)| {
            let latest = files
                .iter()
                .filter_map(|file| {
                    std::fs::metadata(repo_root.join(file))
                        .ok()?
                        .modified()
                        .ok()?
                        .duration_since(UNIX_EPOCH)
                        .ok()
                        .map(|value| value.as_secs())
                })
                .max()
                .unwrap_or(0);
            (name.clone(), latest)
        })
        .collect::<Vec<_>>();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    ranked.into_iter().take(3).map(|(name, _)| name).collect()
}

fn summarize_architecture_module(
    name: &str,
    path: &str,
    files: &[String],
    entry_points: Option<&BTreeSet<String>>,
    fan_out: usize,
    high_activity: bool,
) -> serde_json::Value {
    let mut signals = module_risk_signals(name, path);
    if high_activity {
        signals.push("high_activity".to_string());
    }
    if fan_out >= 2 {
        signals.push("high_fanout".to_string());
    }
    let entry_points = entry_points
        .map(|items| items.iter().take(4).cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    if !entry_points.is_empty() {
        signals.push("entry_points".to_string());
    }
    if files.len() >= 2 && !files.iter().any(|file| is_test_like_path(file)) {
        signals.push("weak_test_coverage".to_string());
    }
    dedupe_strings(&mut signals);
    let sample_files = files
        .iter()
        .take(6)
        .map(|file| short_file_label(file, path))
        .collect::<Vec<_>>();
    json!({
        "name": module_display_name(name),
        "path": path,
        "support_level": "indexed",
        "importance": importance_for_signals(&signals),
        "signals": signals,
        "entry_points": entry_points,
        "files": sample_files,
        "indexed_file_count": files.len(),
        "source_file_count": files.len(),
        "fan_out": fan_out,
    })
}

fn build_unsupported_source_architecture_modules(
    repo_root: &Path,
    existing_modules: &[serde_json::Value],
) -> Vec<serde_json::Value> {
    let summary = summarize_repo_source_boundary(repo_root);
    if summary.unsupported_source_files.is_empty() {
        return Vec::new();
    }

    let existing_paths = existing_modules
        .iter()
        .filter_map(|module| module.get("path").and_then(|value| value.as_str()))
        .map(|path| path.to_ascii_lowercase())
        .collect::<HashSet<_>>();
    let grouped = derive_modules_from_files(&summary.unsupported_source_files);
    let recent_modules = rank_recent_modules_for_files(repo_root, &grouped);

    grouped
        .into_iter()
        .filter(|(_name, files)| !files.is_empty())
        .filter(|(path, _files)| !existing_paths.contains(&path.to_ascii_lowercase()))
        .map(|(path, files)| {
            summarize_unsupported_source_architecture_module(
                &path,
                &files,
                recent_modules.contains(&path),
            )
        })
        .collect()
}

fn summarize_unsupported_source_architecture_module(
    path: &str,
    files: &[String],
    high_activity: bool,
) -> serde_json::Value {
    let name = module_display_name(path);
    let mut signals = module_risk_signals(&name, path);
    signals.push("outside_parser_support".to_string());
    if high_activity {
        signals.push("high_activity".to_string());
    }
    let entry_points = files
        .iter()
        .filter_map(|file| architecture_entry_point_name(file))
        .take(4)
        .collect::<Vec<_>>();
    if !entry_points.is_empty() {
        signals.push("entry_points".to_string());
    }
    if files.len() >= 2 && !files.iter().any(|file| is_test_like_path(file)) {
        signals.push("weak_test_coverage".to_string());
    }
    dedupe_strings(&mut signals);
    let sample_files = files
        .iter()
        .take(6)
        .map(|file| short_file_label(file, path))
        .collect::<Vec<_>>();
    json!({
        "name": name,
        "path": path,
        "support_level": "unsupported_source",
        "importance": importance_for_signals(&signals),
        "signals": signals,
        "entry_points": entry_points,
        "files": sample_files,
        "indexed_file_count": 0,
        "source_file_count": files.len(),
        "fan_out": 0,
    })
}

fn should_skip_architecture_dir(name: &str) -> bool {
    matches!(
        name,
        ".git" | ".semantic" | ".claude" | "node_modules" | "target" | "dist" | "build" | "coverage"
    )
}

fn should_skip_architecture_path(path: &str) -> bool {
    let normalized = path.replace('\\', "/");
    normalized == ".semantic"
        || normalized == ".claude"
        || normalized.starts_with(".semantic/")
        || normalized.starts_with(".claude/")
}

fn cold_start_signals(name: &str, files: &[String], has_entry_points: bool) -> Vec<String> {
    let mut signals = module_risk_signals(name, name);
    if has_entry_points {
        signals.push("entry_points".to_string());
    }
    if files.len() >= 2 && !files.iter().any(|file| is_test_like_path(file)) {
        signals.push("weak_test_coverage".to_string());
    }
    dedupe_strings(&mut signals);
    signals
}

fn module_risk_signals(name: &str, path: &str) -> Vec<String> {
    let lower = format!("{}/{}", name, path).to_ascii_lowercase();
    let mut signals = Vec::new();
    if ["auth", "login", "jwt", "token", "security"]
        .iter()
        .any(|needle| lower.contains(needle))
    {
        signals.push("security".to_string());
    }
    if ["billing", "payment", "invoice", "stripe"]
        .iter()
        .any(|needle| lower.contains(needle))
    {
        signals.push("payments".to_string());
    }
    if ["infra", "db", "database", "migration", "deploy", "config"]
        .iter()
        .any(|needle| lower.contains(needle))
    {
        signals.push("infrastructure".to_string());
    }
    signals
}

fn filter_fixture_dominant_architecture_modules(modules: &mut Vec<serde_json::Value>) {
    let filtered = filter_fixture_dominant_architecture_modules_in_place(std::mem::take(modules));
    *modules = filtered;
}

fn filter_fixture_dominant_architecture_modules_in_place(
    modules: Vec<serde_json::Value>,
) -> Vec<serde_json::Value> {
    let has_non_fixture = modules.iter().any(|module| !is_fixture_dominant_module(module));
    if !has_non_fixture {
        return modules;
    }
    let filtered = modules
        .into_iter()
        .filter(|module| !is_fixture_dominant_module(module))
        .collect::<Vec<_>>();
    if filtered.is_empty() {
        return Vec::new();
    }
    filtered
}

fn is_fixture_dominant_module(module: &serde_json::Value) -> bool {
    let name = module.get("name").and_then(|v| v.as_str()).unwrap_or_default();
    let path = module.get("path").and_then(|v| v.as_str()).unwrap_or_default();
    let lower = format!("{name}/{path}").to_ascii_lowercase();
    lower.contains("test_fixture")
        || lower.contains("test_fixtures")
        || lower.contains("/fixtures")
        || lower.starts_with("fixtures")
        || lower.contains("fixture")
}

fn importance_for_signals(signals: &[String]) -> &'static str {
    if signals.iter().any(|signal| {
        matches!(
            signal.as_str(),
            "security" | "payments" | "infrastructure" | "high_fanout" | "entry_points"
        )
    }) {
        "high"
    } else if signals.is_empty() {
        "low"
    } else {
        "medium"
    }
}

fn architecture_map_summary(
    discovered_module_count: usize,
    visible_module_count: usize,
    indexed_module_count: usize,
    unsupported_module_count: usize,
) -> String {
    if unsupported_module_count == 0 && discovered_module_count == visible_module_count {
        return format!("Orientation map for {indexed_module_count} indexed module(s)");
    }
    if discovered_module_count == visible_module_count {
        return format!(
            "Orientation map for {discovered_module_count} module(s) ({indexed_module_count} indexed, {unsupported_module_count} outside parser coverage)"
        );
    }
    format!(
        "Orientation map for {discovered_module_count} discovered module(s), showing {visible_module_count} ({indexed_module_count} indexed, {unsupported_module_count} outside parser coverage)"
    )
}

fn compress_architecture_modules(modules: Vec<serde_json::Value>) -> Vec<serde_json::Value> {
    let mut supported = Vec::new();
    let mut unsupported = Vec::new();
    let mut other = Vec::new();

    for module in modules {
        match module.get("support_level").and_then(|v| v.as_str()) {
            Some("unsupported_source") => unsupported.push(module),
            Some("indexed" | "filesystem_heuristic") => supported.push(module),
            _ => other.push(module),
        }
    }

    if unsupported.len() <= MAX_UNSUPPORTED_SOURCE_MODULES {
        let mut combined = supported;
        combined.extend(unsupported);
        combined.extend(other);
        sort_architecture_modules(&mut combined);
        return combined;
    }

    let remainder = unsupported.split_off(MAX_UNSUPPORTED_SOURCE_MODULES);
    let aggregate = summarize_grouped_unsupported_source_modules(&remainder);

    let mut combined = supported;
    combined.extend(unsupported);
    combined.push(aggregate);
    combined.extend(other);
    sort_architecture_modules(&mut combined);
    combined
}

fn summarize_grouped_unsupported_source_modules(
    modules: &[serde_json::Value],
) -> serde_json::Value {
    let grouped_names = modules
        .iter()
        .filter_map(|module| module.get("name").and_then(|v| v.as_str()))
        .take(8)
        .map(|name| name.to_string())
        .collect::<Vec<_>>();
    let grouped_file_count = modules
        .iter()
        .map(|module| {
            module
                .get("source_file_count")
                .and_then(|v| v.as_u64())
                .unwrap_or(0)
        })
        .sum::<u64>() as usize;
    let max_importance = modules
        .iter()
        .map(architecture_importance_rank)
        .max()
        .unwrap_or(2);
    let importance = match max_importance {
        3 => "high",
        2 => "medium",
        _ => "low",
    };
    json!({
        "name": "other_unsupported_sources",
        "path": "(grouped)",
        "support_level": "unsupported_source_group",
        "importance": importance,
        "signals": ["outside_parser_support", "grouped_modules"],
        "entry_points": [],
        "files": grouped_names,
        "grouped_module_count": modules.len(),
        "indexed_file_count": 0,
        "source_file_count": grouped_file_count,
        "fan_out": 0,
    })
}

fn grouped_hidden_module_count(modules: &[serde_json::Value]) -> usize {
    modules
        .iter()
        .map(|module| {
            module
                .get("grouped_module_count")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as usize
        })
        .sum()
}

fn sort_architecture_modules(modules: &mut [serde_json::Value]) {
    modules.sort_by(|left, right| {
        let left_importance = architecture_importance_rank(left);
        let right_importance = architecture_importance_rank(right);
        right_importance
            .cmp(&left_importance)
            .then_with(|| architecture_support_rank(left).cmp(&architecture_support_rank(right)))
            .then_with(|| architecture_signal_count(right).cmp(&architecture_signal_count(left)))
            .then_with(|| architecture_name(left).cmp(architecture_name(right)))
    });
}

fn architecture_importance_rank(module: &serde_json::Value) -> usize {
    match module
        .get("importance")
        .and_then(|value| value.as_str())
        .unwrap_or("low")
    {
        "high" => 3,
        "medium" => 2,
        _ => 1,
    }
}

fn architecture_support_rank(module: &serde_json::Value) -> usize {
    match module
        .get("support_level")
        .and_then(|value| value.as_str())
        .unwrap_or("filesystem_heuristic")
    {
        "indexed" => 0,
        "unsupported_source" => 1,
        "unsupported_source_group" => 2,
        _ => 3,
    }
}

fn architecture_signal_count(module: &serde_json::Value) -> usize {
    module
        .get("signals")
        .and_then(|value| value.as_array())
        .map(|signals| signals.len())
        .unwrap_or(0)
}

fn architecture_name(module: &serde_json::Value) -> &str {
    module
        .get("name")
        .and_then(|value| value.as_str())
        .unwrap_or("")
}

fn rank_recent_modules_for_files(
    repo_root: &Path,
    modules: &BTreeMap<String, Vec<String>>,
) -> HashSet<String> {
    let items = modules
        .iter()
        .map(|(name, files)| (name.clone(), name.clone(), files.clone()))
        .collect::<Vec<_>>();
    rank_recent_modules(repo_root, &items)
}

fn short_file_label(file: &str, module_path: &str) -> String {
    let prefix = format!("{}/", module_path.trim_matches('/'));
    file.strip_prefix(&prefix)
        .or_else(|| Path::new(file).file_name().and_then(|name| name.to_str()))
        .unwrap_or(file)
        .to_string()
}

fn module_display_name(name: &str) -> String {
    name.rsplit('/').next().unwrap_or(name).to_string()
}

fn strip_extension(file: &str) -> String {
    Path::new(file)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or(file)
        .to_string()
}

fn architecture_entry_point_name(file: &str) -> Option<String> {
    let normalized = file.replace('\\', "/");
    let lower = normalized.to_ascii_lowercase();
    let file_name = Path::new(&normalized)
        .file_name()
        .and_then(|name| name.to_str())?;
    if is_probable_entry_file(file_name) {
        return Some(strip_extension(file_name));
    }
    let in_bin_dir = lower.starts_with("bin/")
        || lower.contains("/bin/")
        || lower.starts_with("src/bin/")
        || lower.contains("/src/bin/");
    if in_bin_dir {
        return Some(strip_extension(file_name));
    }
    None
}

fn is_probable_entry_file(file: &str) -> bool {
    let lower = file.to_ascii_lowercase();
    [
        "index.ts",
        "index.tsx",
        "index.js",
        "index.jsx",
        "index.py",
        "main.ts",
        "main.tsx",
        "main.js",
        "main.jsx",
        "main.rs",
        "main.py",
        "app.ts",
        "app.tsx",
        "app.js",
        "app.jsx",
        "app.py",
        "server.ts",
        "server.tsx",
        "server.js",
        "server.jsx",
        "server.py",
        "cli.ts",
        "cli.tsx",
        "cli.js",
        "cli.jsx",
        "cli.py",
        "manage.py",
        "__main__.py",
        "wsgi.py",
        "asgi.py",
    ]
    .iter()
    .any(|candidate| lower == *candidate)
}

fn is_test_like_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.contains("test") || lower.contains("spec")
}

fn dedupe_strings(values: &mut Vec<String>) {
    let mut seen = BTreeSet::new();
    values.retain(|value| seen.insert(value.clone()));
}

fn compute_index_coverage(
    indexed_files: &[String],
    file_hint: Option<&str>,
    path_hint: Option<&str>,
) -> (&'static str, Option<String>) {
    if indexed_files.is_empty() {
        if let Some(target) = file_hint.or(path_hint) {
            let normalized = normalize_coverage_path(target);
            if parser_support_for_target_path(&normalized) == "unsupported" {
                return ("unsupported_target", Some(normalized));
            }
            return ("unindexed_repo", Some(normalized));
        }
        return ("unindexed_repo", None);
    }
    if let Some(file) = file_hint {
        let normalized = normalize_coverage_path(file);
        if parser_support_for_target_path(&normalized) == "unsupported" {
            return ("unsupported_target", Some(normalized));
        }
        let resolved = resolve_indexed_target_alias(indexed_files, &normalized)
            .unwrap_or_else(|| normalized.clone());
        let indexed = indexed_files.iter().any(|item| item == &resolved);
        return (
            if indexed { "indexed_target" } else { "unindexed_target" },
            Some(resolved),
        );
    }
    if let Some(path) = path_hint {
        let normalized = normalize_coverage_path(path);
        if looks_like_file_path(&normalized)
            && parser_support_for_target_path(&normalized) == "unsupported"
        {
            return ("unsupported_target", Some(normalized));
        }
        let indexed = if looks_like_file_path(&normalized) {
            let resolved = resolve_indexed_target_alias(indexed_files, &normalized)
                .unwrap_or_else(|| normalized.clone());
            return (
                if indexed_files.iter().any(|item| item == &resolved) {
                    "indexed_target"
                } else {
                    "unindexed_target"
                },
                Some(resolved),
            );
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

fn exact_unsupported_target(file_hint: Option<&str>, path_hint: Option<&str>) -> Option<String> {
    let target = file_hint.or(path_hint)?;
    let normalized = normalize_coverage_path(target);
    if !looks_like_file_path(&normalized) {
        return None;
    }
    if parser_support_for_target_path(&normalized) == "unsupported" {
        Some(normalized)
    } else {
        None
    }
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
    if coverage == "unsupported_target" {
        "unsupported_target"
    } else if indexed_file_count == 0 || coverage == "unindexed_repo" {
        "unindexed_repo"
    } else if coverage == "indexed_target" {
        "target_ready"
    } else if coverage == "unindexed_target" {
        "partial_index_missing_target"
    } else {
        "indexed_repo"
    }
}

fn index_recovery_mode(auto_index_requested: bool, coverage: &str) -> &'static str {
    if coverage == "unsupported_target" {
        "unsupported_target"
    } else if auto_index_requested && coverage == "unindexed_target" {
        "auto_index_attempted_no_change"
    } else if coverage == "unindexed_target" {
        "suggest_only"
    } else {
        "none"
    }
}

fn index_recovery_target_kind(target: &str) -> &'static str {
    if looks_like_file_path(target) {
        "file"
    } else {
        "directory"
    }
}

#[cfg(test)]
mod tests {
    use super::{
        architecture_priority_focus_commands, architecture_priority_focus_entries,
        architecture_priority_focus_follow_up_operations,
        architecture_priority_focus_mode, architecture_priority_focus_reason,
        architecture_priority_focus_primary_entry, architecture_priority_focus_secondary_entry,
        architecture_priority_focus_targets,
        architecture_priority_focus_trust, architecture_priority_entry_trust,
        architecture_priority_modules,
        compute_index_coverage, exact_unsupported_target, index_readiness,
        index_recovery_mode, index_recovery_target_kind, retrieve_coverage_resolved,
        suggested_index_command,
        MAX_UNSUPPORTED_SOURCE_MODULES,
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
    fn compute_index_coverage_resolves_indexed_sibling_extension_for_exact_file() {
        let indexed = vec!["src/app.tsx".to_string()];
        let (coverage, target) = compute_index_coverage(&indexed, Some("src/app.ts"), None);
        assert_eq!(coverage, "indexed_target");
        assert_eq!(target.as_deref(), Some("src/app.tsx"));
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
    fn compute_index_coverage_marks_unsupported_exact_file_target() {
        let indexed = vec!["src/auth/session.ts".to_string()];
        let (coverage, target) = compute_index_coverage(&indexed, None, Some("src/main.rs"));
        assert_eq!(coverage, "unsupported_target");
        assert_eq!(target.as_deref(), Some("src/main.rs"));
    }

    #[test]
    fn exact_unsupported_target_only_matches_unsupported_files() {
        assert_eq!(
            exact_unsupported_target(None, Some("src/main.rs")),
            Some("src/main.rs".to_string())
        );
        assert_eq!(exact_unsupported_target(None, Some("src/worker")), None);
        assert_eq!(exact_unsupported_target(None, Some("src/main.ts")), None);
    }

    #[test]
    fn retrieve_returns_raw_preview_for_indexed_unsupported_target_path() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join("src")).expect("mkdir src");
        fs::write(
            repo.join("src").join("main.rs"),
            "pub fn load_db() {\n    println!(\"db\");\n}\n",
        )
        .expect("write rust");

        let runtime = AppRuntime::bootstrap(
            repo,
            RuntimeOptions {
                start_watcher: false,
                ensure_config: true,
                bootstrap_index_policy: crate::BootstrapIndexPolicy::ReuseExistingOrCreate,
            },
        )
        .expect("bootstrap runtime");

        let value = runtime.handle_retrieve(crate::models::RetrieveRequestBody {
            request: engine::RetrievalRequest {
                operation: engine::Operation::GetPlannedContext,
                path: Some("src/main.rs".to_string()),
                ..Default::default()
            },
            semantic_enabled: None,
            input_compressed: None,
            original_query: None,
            single_file_fast_path: Some(true),
            reference_only: Some(true),
            mapping_mode: None,
            max_footprint_items: None,
            reuse_session_context: Some(true),
            session_id: None,
            raw_expansion_mode: None,
            auto_index_target: None,
        });

        let result = value.get("result").expect("result");
        assert_ne!(
            result.get("index_readiness").and_then(|v| v.as_str()),
            Some("unsupported_target")
        );
        assert_ne!(
            result.get("index_coverage").and_then(|v| v.as_str()),
            Some("unsupported_target")
        );
        assert_eq!(
            result
                .get("path_target_fallback")
                .and_then(|v| v.as_bool()),
            Some(true)
        );
        assert!(
            result
                .get("code_span")
                .and_then(|v| v.get("code"))
                .and_then(|v| v.as_str())
                .map(|code| code.contains("load_db"))
                .unwrap_or(false)
        );
    }

    #[test]
    fn suggested_index_command_is_emitted_for_unindexed_target() {
        assert_eq!(
            suggested_index_command("unindexed_target", Some("src/worker")),
            Some("semantic index --path src/worker".to_string())
        );
        assert_eq!(suggested_index_command("indexed_target", Some("src/worker")), None);
        assert_eq!(
            suggested_index_command("unsupported_target", Some("src/main.rs")),
            None
        );
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
                "index_recovery_mode": "none",
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
        assert_eq!(index_readiness(1, "unsupported_target"), "unsupported_target");
    }

    #[test]
    fn index_recovery_mode_distinguishes_suggest_and_attempted() {
        assert_eq!(index_recovery_mode(false, "indexed_target"), "none");
        assert_eq!(index_recovery_mode(false, "unindexed_target"), "suggest_only");
        assert_eq!(
            index_recovery_mode(true, "unindexed_target"),
            "auto_index_attempted_no_change"
        );
        assert_eq!(
            index_recovery_mode(false, "unsupported_target"),
            "unsupported_target"
        );
    }

    #[test]
    fn index_recovery_target_kind_distinguishes_file_and_directory() {
        assert_eq!(index_recovery_target_kind("src/worker/job.ts"), "file");
        assert_eq!(index_recovery_target_kind("src/worker"), "directory");
    }

    #[cfg(feature = "rust-support")]
    #[test]
    fn retrieve_search_rust_symbol_uses_indexed_records() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join(".semantic")).expect("mkdir .semantic");
        fs::create_dir_all(repo.join("src")).expect("mkdir src");
        fs::write(
            repo.join(".semantic").join("rust.toml"),
            "enabled = true\nsmall_project_mode = true\n",
        )
        .expect("write rust config");
        fs::write(
            repo.join("src").join("user.rs"),
            "pub struct User;\nimpl User { pub fn new() -> Self { Self } }\n",
        )
        .expect("write rust");

        let runtime = AppRuntime::bootstrap(
            repo,
            RuntimeOptions {
                start_watcher: false,
                ensure_config: true,
                bootstrap_index_policy: crate::BootstrapIndexPolicy::ReuseExistingOrCreate,
            },
        )
        .expect("bootstrap runtime");

        let value = runtime.handle_retrieve(crate::models::RetrieveRequestBody {
            request: RetrievalRequest {
                operation: Operation::SearchRustSymbol,
                query: Some("User".to_string()),
                limit: Some(10),
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
            auto_index_target: None,
        });

        let matches = value
            .get("result")
            .and_then(|v| v.get("matches"))
            .and_then(|v| v.as_array())
            .expect("matches");
        assert!(!matches.is_empty());
        assert_eq!(
            matches
                .first()
                .and_then(|item| item.get("name"))
                .and_then(|v| v.as_str()),
            Some("User")
        );
        assert!(
            matches
                .iter()
                .any(|item| item.get("name").and_then(|v| v.as_str()) == Some("impl User"))
        );
        assert!(
            matches
                .iter()
                .any(|item| item.get("name").and_then(|v| v.as_str()) == Some("User::new"))
        );
    }

    #[cfg(feature = "rust-support")]
    #[test]
    fn retrieve_get_rust_context_groups_defs_impls_and_methods_from_index() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join(".semantic")).expect("mkdir .semantic");
        fs::create_dir_all(repo.join("src")).expect("mkdir src");
        fs::write(
            repo.join(".semantic").join("rust.toml"),
            "enabled = true\nsmall_project_mode = true\n",
        )
        .expect("write rust config");
        fs::write(
            repo.join("src").join("user.rs"),
            concat!(
                "pub struct User;\n\n",
                "impl User {\n",
                "    pub fn new() -> Self { Self }\n",
                "    pub fn rename(&mut self) {}\n",
                "}\n",
            ),
        )
        .expect("write rust");

        let runtime = AppRuntime::bootstrap(
            repo,
            RuntimeOptions {
                start_watcher: false,
                ensure_config: true,
                bootstrap_index_policy: crate::BootstrapIndexPolicy::ReuseExistingOrCreate,
            },
        )
        .expect("bootstrap runtime");

        let value = runtime.handle_retrieve(crate::models::RetrieveRequestBody {
            request: RetrievalRequest {
                operation: Operation::GetRustContext,
                query: Some("User".to_string()),
                max_tokens: Some(300),
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
            auto_index_target: None,
        });

        let result = value.get("result").expect("result");
        assert_eq!(
            result.get("strategy").and_then(|v| v.as_str()),
            Some("indexed_rust_grouped_context")
        );
        assert!(
            result
                .get("definitions")
                .and_then(|v| v.as_array())
                .map(|items| items.iter().any(|item| item.get("name").and_then(|v| v.as_str()) == Some("User")))
                .unwrap_or(false)
        );
        assert!(
            result
                .get("impl_blocks")
                .and_then(|v| v.as_array())
                .map(|items| items.iter().any(|item| item.get("name").and_then(|v| v.as_str()) == Some("impl User")))
                .unwrap_or(false)
        );
        assert!(
            result
                .get("associated_items")
                .and_then(|v| v.as_array())
                .map(|items| items.iter().any(|item| item.get("name").and_then(|v| v.as_str()) == Some("User::new")))
                .unwrap_or(false)
        );
        assert!(
            result
                .get("impl_blocks")
                .and_then(|v| v.as_array())
                .and_then(|items| items.first())
                .and_then(|item| item.get("span_mode"))
                .and_then(|v| v.as_str())
                == Some("header_only")
        );
    }

    #[cfg(feature = "rust-support")]
    #[test]
    fn retrieve_get_rust_context_recovers_cross_file_trait_impls() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join(".semantic")).expect("mkdir .semantic");
        fs::create_dir_all(repo.join("src")).expect("mkdir src");
        fs::write(
            repo.join(".semantic").join("rust.toml"),
            "enabled = true\nsmall_project_mode = true\n",
        )
        .expect("write rust config");
        fs::write(
            repo.join("src").join("traits.rs"),
            "pub trait Displayable { fn display(&self) -> String; }\n",
        )
        .expect("write trait");
        fs::write(repo.join("src").join("user.rs"), "pub struct User;\n").expect("write user");
        fs::write(
            repo.join("src").join("impls.rs"),
            concat!(
                "use crate::traits::Displayable;\n",
                "use crate::user::User;\n\n",
                "impl Displayable for User {\n",
                "    fn display(&self) -> String { \"user\".to_string() }\n",
                "}\n",
            ),
        )
        .expect("write impl");

        let runtime = AppRuntime::bootstrap(
            repo,
            RuntimeOptions {
                start_watcher: false,
                ensure_config: true,
                bootstrap_index_policy: crate::BootstrapIndexPolicy::ReuseExistingOrCreate,
            },
        )
        .expect("bootstrap runtime");

        let value = runtime.handle_retrieve(crate::models::RetrieveRequestBody {
            request: RetrievalRequest {
                operation: Operation::GetRustContext,
                query: Some("Displayable".to_string()),
                max_tokens: Some(450),
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
            auto_index_target: None,
        });

        let result = value.get("result").expect("result");
        assert!(
            result
                .get("definitions")
                .and_then(|v| v.as_array())
                .map(|items| items.iter().any(|item| item.get("name").and_then(|v| v.as_str()) == Some("Displayable")))
                .unwrap_or(false)
        );
        assert!(
            result
                .get("impl_blocks")
                .and_then(|v| v.as_array())
                .map(|items| items.iter().any(|item| item.get("name").and_then(|v| v.as_str()) == Some("impl Displayable for User")))
                .unwrap_or(false)
        );
        assert!(
            result
                .get("associated_items")
                .and_then(|v| v.as_array())
                .map(|items| items.iter().any(|item| item.get("name").and_then(|v| v.as_str()) == Some("User::display")))
                .unwrap_or(false)
        );
    }

    #[cfg(feature = "rust-support")]
    #[test]
    fn retrieve_get_rust_context_prefers_single_crate_when_duplicates_exist() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join(".semantic")).expect("mkdir .semantic");
        fs::write(
            repo.join(".semantic").join("rust.toml"),
            "enabled = true\nsmall_project_mode = true\n",
        )
        .expect("write rust config");
        fs::write(
            repo.join("Cargo.toml"),
            "[workspace]\nmembers = [\"api\", \"worker\"]\n",
        )
        .expect("write workspace cargo");

        for crate_name in ["api", "worker"] {
            fs::create_dir_all(repo.join(crate_name).join("src")).expect("mkdir rust crate");
            fs::write(
                repo.join(crate_name).join("Cargo.toml"),
                format!("[package]\nname = \"{crate_name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"),
            )
            .expect("write crate cargo");
        }

        fs::write(
            repo.join("api").join("src").join("lib.rs"),
            concat!(
                "pub struct User;\n",
                "impl User { pub fn from_api() -> Self { Self } }\n",
            ),
        )
        .expect("write api crate");
        fs::write(
            repo.join("worker").join("src").join("lib.rs"),
            concat!(
                "pub struct User;\n",
                "impl User { pub fn from_worker() -> Self { Self } }\n",
            ),
        )
        .expect("write worker crate");

        let runtime = AppRuntime::bootstrap(
            repo,
            RuntimeOptions {
                start_watcher: false,
                ensure_config: true,
                bootstrap_index_policy: crate::BootstrapIndexPolicy::ReuseExistingOrCreate,
            },
        )
        .expect("bootstrap runtime");

        let value = runtime.handle_retrieve(crate::models::RetrieveRequestBody {
            request: RetrievalRequest {
                operation: Operation::GetRustContext,
                query: Some("User".to_string()),
                max_tokens: Some(400),
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
            auto_index_target: None,
        });

        let result = value.get("result").expect("result");
        let preferred_crate = result
            .get("preferred_crate")
            .and_then(|v| v.as_str())
            .expect("preferred crate");
        assert!(preferred_crate == "api" || preferred_crate == "worker");
        let associated = result
            .get("associated_items")
            .and_then(|v| v.as_array())
            .expect("associated items");
        let wrong_method = if preferred_crate == "api" {
            "User::from_worker"
        } else {
            "User::from_api"
        };
        assert!(
            associated.iter().all(|item| item.get("crate_name").and_then(|v| v.as_str()) == Some(preferred_crate))
        );
        assert!(
            associated
                .iter()
                .all(|item| item.get("name").and_then(|v| v.as_str()) != Some(wrong_method))
        );
    }

    #[cfg(feature = "rust-support")]
    #[test]
    fn retrieve_get_rust_context_handles_qualified_trait_queries() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join(".semantic")).expect("mkdir .semantic");
        fs::write(
            repo.join(".semantic").join("rust.toml"),
            "enabled = true\nsmall_project_mode = true\n",
        )
        .expect("write rust config");
        fs::write(
            repo.join("Cargo.toml"),
            "[package]\nname = \"qualified\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .expect("write cargo");
        fs::create_dir_all(repo.join("src")).expect("mkdir src");
        fs::write(
            repo.join("src").join("lib.rs"),
            concat!(
                "pub mod api;\n",
                "pub mod domain;\n",
                "pub mod user;\n",
                "pub mod api_impl;\n",
                "pub mod domain_impl;\n",
            ),
        )
        .expect("write lib");
        fs::write(
            repo.join("src").join("api.rs"),
            "pub trait Serialize { fn encode(&self) -> &'static str; }\n",
        )
        .expect("write api trait");
        fs::write(
            repo.join("src").join("domain.rs"),
            "pub trait Serialize { fn save(&self) -> &'static str; }\n",
        )
        .expect("write domain trait");
        fs::write(repo.join("src").join("user.rs"), "pub struct User;\n").expect("write user");
        fs::write(
            repo.join("src").join("api_impl.rs"),
            concat!(
                "use crate::api::Serialize;\n",
                "use crate::user::User;\n",
                "impl Serialize for User {\n",
                "    fn encode(&self) -> &'static str { \"api\" }\n",
                "}\n",
            ),
        )
        .expect("write api impl");
        fs::write(
            repo.join("src").join("domain_impl.rs"),
            concat!(
                "use crate::domain::Serialize;\n",
                "use crate::user::User;\n",
                "impl Serialize for User {\n",
                "    fn save(&self) -> &'static str { \"domain\" }\n",
                "}\n",
            ),
        )
        .expect("write domain impl");

        let runtime = AppRuntime::bootstrap(
            repo,
            RuntimeOptions {
                start_watcher: false,
                ensure_config: true,
                bootstrap_index_policy: crate::BootstrapIndexPolicy::ReuseExistingOrCreate,
            },
        )
        .expect("bootstrap runtime");

        let value = runtime.handle_retrieve(crate::models::RetrieveRequestBody {
            request: RetrievalRequest {
                operation: Operation::GetRustContext,
                query: Some("api::Serialize".to_string()),
                max_tokens: Some(320),
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
            auto_index_target: None,
        });

        let result = value.get("result").expect("result");
        let definitions = result
            .get("definitions")
            .and_then(|v| v.as_array())
            .expect("definitions");
        assert!(definitions.iter().any(|item| {
            item.get("name").and_then(|v| v.as_str()) == Some("Serialize")
                && item
                    .get("module_path")
                    .and_then(|v| v.as_str())
                    .map(|module| module == "api" || module.ends_with("::api"))
                    .unwrap_or(false)
        }));

        let impls = result
            .get("impl_blocks")
            .and_then(|v| v.as_array())
            .expect("impl blocks");
        assert!(impls.iter().any(|item| {
            item.get("file").and_then(|v| v.as_str()) == Some("src/api_impl.rs")
        }));

        let associated = result
            .get("associated_items")
            .and_then(|v| v.as_array())
            .expect("associated items");
        assert!(associated.iter().any(|item| {
            item.get("name").and_then(|v| v.as_str()) == Some("User::encode")
        }));
        assert!(associated.iter().all(|item| {
            item.get("name").and_then(|v| v.as_str()) != Some("User::save")
        }));
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
        assert_eq!(
            value.get("index_recovery_target_kind").and_then(|v| v.as_str()),
            Some("file")
        );
        assert_eq!(
            value.get("parser_target_support").and_then(|v| v.as_str()),
            Some("supported")
        );
        assert_eq!(
            value.get("index_region_status").and_then(|v| v.as_str()),
            Some("targeted_partial")
        );
        assert_eq!(
            value.get("index_recovery_delta")
                .and_then(|v| v.get("added_file_count"))
                .and_then(|v| v.as_u64()),
            Some(1)
        );
        assert_eq!(
            value.get("index_recovery_delta")
                .and_then(|v| v.get("changed_files"))
                .and_then(|v| v.as_array())
                .and_then(|items| items.first())
                .and_then(|v| v.as_str()),
            Some("src/worker/job.ts")
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
            result.get("index_recovery_mode").and_then(|v| v.as_str()),
            Some("auto_index_applied")
        );
        assert_eq!(
            result
                .get("parser_target_support")
                .and_then(|v| v.as_str()),
            Some("supported")
        );
        assert_eq!(
            result
                .get("index_recovery_target_kind")
                .and_then(|v| v.as_str()),
            Some("file")
        );
        assert_eq!(
            result.get("index_region_status").and_then(|v| v.as_str()),
            Some("targeted_partial")
        );
        assert_eq!(
            result
                .get("index_recovery_delta")
                .and_then(|v| v.get("added_file_count"))
                .and_then(|v| v.as_u64()),
            Some(1)
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
            result.get("index_recovery_mode").and_then(|v| v.as_str()),
            Some("auto_index_applied")
        );
        assert_eq!(
            result.get("index_coverage_target").and_then(|v| v.as_str()),
            Some("src/worker/job.ts")
        );
    }

    #[test]
    fn retrieve_marks_unsupported_exact_file_target() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join("src").join("auth")).expect("mkdir auth");
        fs::write(
            repo.join("src").join("auth").join("session.ts"),
            "export function buildSession(){ return 1; }\n",
        )
        .expect("write auth");
        fs::write(repo.join("src").join("main.rs"), "fn main() {}\n").expect("write rust");

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
                name: Some("main".to_string()),
                path: Some("src/main.rs".to_string()),
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

        let result = value.get("result").expect("result");
        assert_eq!(
            value.get("operation").and_then(|v| v.as_str()),
            Some("search_symbol")
        );
        assert_eq!(
            result.get("message").and_then(|v| v.as_str()),
            Some("returned raw file preview for parser-unsupported target")
        );
        assert_ne!(
            result.get("index_readiness").and_then(|v| v.as_str()),
            Some("unsupported_target")
        );
        assert_ne!(
            result.get("index_recovery_mode").and_then(|v| v.as_str()),
            Some("unsupported_target")
        );
        assert_eq!(
            result
                .get("parser_target_support")
                .and_then(|v| v.as_str()),
            Some("unsupported")
        );
        assert_ne!(
            result.get("index_coverage").and_then(|v| v.as_str()),
            Some("unsupported_target")
        );
        assert_eq!(
            result.get("index_coverage_target").and_then(|v| v.as_str()),
            Some("src/main.rs")
        );
        assert_eq!(
            result.get("path_target_fallback").and_then(|v| v.as_bool()),
            Some(true)
        );
        assert_eq!(result.get("suggested_index_command"), None);
        assert_eq!(value.get("auto_index_applied"), None);
        assert!(
            result
                .get("code_span")
                .and_then(|v| v.get("code"))
                .and_then(|v| v.as_str())
                .map(|code| code.contains("fn main"))
                .unwrap_or(false)
        );
    }

    #[test]
    fn retrieve_returns_architecture_map_from_indexed_modules() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join("src").join("auth")).expect("mkdir auth");
        fs::create_dir_all(repo.join("src").join("billing")).expect("mkdir billing");
        fs::write(
            repo.join("src").join("auth").join("login.ts"),
            "export function login(){ return true; }\n",
        )
        .expect("write auth");
        fs::write(
            repo.join("src").join("billing").join("invoice.ts"),
            "export function chargeInvoice(){ return 1; }\n",
        )
        .expect("write billing");

        let runtime = AppRuntime::bootstrap(
            repo.clone(),
            RuntimeOptions {
                start_watcher: false,
                ensure_config: true,
                bootstrap_index_policy: crate::BootstrapIndexPolicy::ReuseExistingOrCreate,
            },
        )
        .expect("bootstrap runtime");

        let value = runtime.handle_retrieve(crate::models::RetrieveRequestBody {
            request: RetrievalRequest {
                operation: Operation::GetArchitectureMap,
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
            auto_index_target: None,
        });

        assert_eq!(
            value.get("operation").and_then(|v| v.as_str()),
            Some("get_architecture_map")
        );
        let result = value.get("result").expect("result");
        assert_eq!(
            result.get("orientation_stage").and_then(|v| v.as_str()),
            Some("indexed_modules")
        );
        let modules = result
            .get("modules")
            .and_then(|v| v.as_array())
            .expect("modules");
        assert!(modules.iter().any(|module| module.get("name").and_then(|v| v.as_str()) == Some("auth")));
        assert!(modules.iter().any(|module| module.get("name").and_then(|v| v.as_str()) == Some("billing")));
    }

    #[test]
    fn retrieve_architecture_map_filters_fixture_dominant_modules_when_real_modules_exist() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join("src").join("auth")).expect("mkdir auth");
        fs::create_dir_all(repo.join("test_fixtures").join("sample")).expect("mkdir fixtures");
        fs::write(
            repo.join("src").join("auth").join("login.ts"),
            "export function login(){ return true; }\n",
        )
        .expect("write auth");
        fs::write(
            repo.join("test_fixtures").join("sample").join("fixture.ts"),
            "export function fixture(){ return true; }\n",
        )
        .expect("write fixture");

        let runtime = AppRuntime::bootstrap(
            repo.clone(),
            RuntimeOptions {
                start_watcher: false,
                ensure_config: true,
                bootstrap_index_policy: crate::BootstrapIndexPolicy::ReuseExistingOrCreate,
            },
        )
        .expect("bootstrap runtime");

        let value = runtime.handle_retrieve(crate::models::RetrieveRequestBody {
            request: RetrievalRequest {
                operation: Operation::GetArchitectureMap,
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
            auto_index_target: None,
        });

        let modules = value
            .get("result")
            .and_then(|v| v.get("modules"))
            .and_then(|v| v.as_array())
            .expect("modules");
        assert!(modules.iter().any(|module| module.get("name").and_then(|v| v.as_str()) == Some("auth")));
        assert!(!modules.iter().any(|module| module.get("name").and_then(|v| v.as_str()) == Some("test_fixtures")));
    }

    #[test]
    fn retrieve_architecture_map_surfaces_unsupported_source_modules_in_mixed_repo() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join("src").join("auth")).expect("mkdir auth");
        fs::create_dir_all(repo.join("semantic_app").join("src")).expect("mkdir rust crate");
        fs::write(
            repo.join("src").join("auth").join("login.ts"),
            "export function login(){ return true; }\n",
        )
        .expect("write auth");
        fs::write(
            repo.join("semantic_app").join("src").join("main.rs"),
            "fn main() { println!(\"hi\"); }\n",
        )
        .expect("write rust");

        let runtime = AppRuntime::bootstrap(
            repo.clone(),
            RuntimeOptions {
                start_watcher: false,
                ensure_config: true,
                bootstrap_index_policy: crate::BootstrapIndexPolicy::ReuseExistingOrCreate,
            },
        )
        .expect("bootstrap runtime");

        let value = runtime.handle_retrieve(crate::models::RetrieveRequestBody {
            request: RetrievalRequest {
                operation: Operation::GetArchitectureMap,
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
            auto_index_target: None,
        });

        let modules = value
            .get("result")
            .and_then(|v| v.get("modules"))
            .and_then(|v| v.as_array())
            .expect("modules");
        assert_eq!(
            value.get("result")
                .and_then(|v| v.get("priority_scoring_model"))
                .and_then(|v| v.as_str()),
            Some("architecture_priority_v1")
        );
        assert_eq!(
            value.get("result")
                .and_then(|v| v.get("priority_scoring_weights"))
                .and_then(|v| v.get("importance_multiplier"))
                .and_then(|v| v.as_u64()),
            Some(100)
        );
        assert_eq!(
            value.get("result")
                .and_then(|v| v.get("priority_scoring_weights"))
                .and_then(|v| v.get("support_weights"))
                .and_then(|v| v.get("indexed"))
                .and_then(|v| v.as_u64()),
            Some(30)
        );
        assert_eq!(
            value.get("result")
                .and_then(|v| v.get("priority_scoring_weights"))
                .and_then(|v| v.get("signal_increment"))
                .and_then(|v| v.as_u64()),
            Some(1)
        );
        assert_eq!(
            value.get("result")
                .and_then(|v| v.get("priority_focus_mode"))
                .and_then(|v| v.as_str()),
            Some("single_focus")
        );
        assert_eq!(
            value.get("result")
                .and_then(|v| v.get("priority_focus_reason"))
                .and_then(|v| v.as_str()),
            Some("auth has a clear lead over the next candidate (gap=10)")
        );
        assert_eq!(
            value.get("result")
                .and_then(|v| v.get("priority_focus_trust"))
                .and_then(|v| v.as_str()),
            Some("semantic_precise")
        );
        assert_eq!(
            value.get("result")
                .and_then(|v| v.get("priority_focus_targets"))
                .and_then(|v| v.as_array())
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| item.as_str())
                        .collect::<Vec<_>>()
                }),
            Some(vec!["auth"])
        );
        assert_eq!(
            value.get("result")
                .and_then(|v| v.get("priority_focus_primary_target"))
                .and_then(|v| v.as_str()),
            Some("auth")
        );
        assert_eq!(
            value.get("result")
                .and_then(|v| v.get("priority_focus_primary_path"))
                .and_then(|v| v.as_str()),
            Some("src/auth")
        );
        assert_eq!(
            value.get("result")
                .and_then(|v| v.get("priority_focus_primary_importance"))
                .and_then(|v| v.as_str()),
            Some("high")
        );
        assert_eq!(
            value.get("result")
                .and_then(|v| v.get("priority_focus_primary_support_level"))
                .and_then(|v| v.as_str()),
            Some("indexed")
        );
        assert_eq!(
            value.get("result")
                .and_then(|v| v.get("priority_focus_primary_actionability"))
                .and_then(|v| v.as_str()),
            Some("semantic_precise")
        );
        assert_eq!(
            value.get("result")
                .and_then(|v| v.get("priority_focus_primary_trust"))
                .and_then(|v| v.as_str()),
            Some("semantic_precise")
        );
        assert_eq!(
            value.get("result")
                .and_then(|v| v.get("priority_focus_primary_rank"))
                .and_then(|v| v.as_u64()),
            Some(1)
        );
        assert_eq!(
            value.get("result")
                .and_then(|v| v.get("priority_focus_primary_score"))
                .and_then(|v| v.as_u64()),
            Some(333)
        );
        assert_eq!(
            value.get("result")
                .and_then(|v| v.get("priority_focus_primary_score_components"))
                .and_then(|v| v.get("importance"))
                .and_then(|v| v.as_u64()),
            Some(300)
        );
        assert_eq!(
            value.get("result")
                .and_then(|v| v.get("priority_focus_primary_score_components"))
                .and_then(|v| v.get("support"))
                .and_then(|v| v.as_u64()),
            Some(30)
        );
        assert_eq!(
            value.get("result")
                .and_then(|v| v.get("priority_focus_primary_score_components"))
                .and_then(|v| v.get("signals"))
                .and_then(|v| v.as_u64()),
            Some(3)
        );
        assert_eq!(
            value.get("result")
                .and_then(|v| v.get("priority_focus_primary_score_gap_from_previous"))
                .and_then(|v| v.as_u64()),
            None
        );
        assert_eq!(
            value.get("result")
                .and_then(|v| v.get("priority_focus_primary_score_gap_to_next"))
                .and_then(|v| v.as_u64()),
            Some(10)
        );
        assert_eq!(
            value.get("result")
                .and_then(|v| v.get("priority_focus_primary_score_separation"))
                .and_then(|v| v.as_str()),
            Some("clear_lead")
        );
        assert_eq!(
            value.get("result")
                .and_then(|v| v.get("priority_focus_primary_signals"))
                .and_then(|v| v.as_array())
                .map(|items| items.iter().filter_map(|item| item.as_str()).collect::<Vec<_>>()),
            Some(vec!["security", "high_activity", "entry_points"])
        );
        assert_eq!(
            value.get("result")
                .and_then(|v| v.get("priority_focus_primary_entry_points"))
                .and_then(|v| v.as_array())
                .map(|items| items.iter().filter_map(|item| item.as_str()).collect::<Vec<_>>()),
            Some(vec!["login"])
        );
        assert_eq!(
            value.get("result")
                .and_then(|v| v.get("priority_focus_primary_files"))
                .and_then(|v| v.as_array())
                .map(|items| items.iter().filter_map(|item| item.as_str()).collect::<Vec<_>>()),
            Some(vec!["login.ts"])
        );
        assert_eq!(
            value.get("result")
                .and_then(|v| v.get("priority_focus_primary_indexed_file_count"))
                .and_then(|v| v.as_u64()),
            Some(1)
        );
        assert_eq!(
            value.get("result")
                .and_then(|v| v.get("priority_focus_primary_source_file_count"))
                .and_then(|v| v.as_u64()),
            Some(1)
        );
        assert_eq!(
            value.get("result")
                .and_then(|v| v.get("priority_focus_primary_fan_out"))
                .and_then(|v| v.as_u64()),
            Some(0)
        );
        assert_eq!(
            value.get("result")
                .and_then(|v| v.get("priority_focus_primary_open_first_path"))
                .and_then(|v| v.as_str()),
            Some("src/auth/login.ts")
        );
        assert_eq!(
            value.get("result")
                .and_then(|v| v.get("priority_focus_primary_next_step_operation"))
                .and_then(|v| v.as_str()),
            Some("get_file_brief")
        );
        assert_eq!(
            value.get("result")
                .and_then(|v| v.get("priority_focus_primary_next_step_target_kind"))
                .and_then(|v| v.as_str()),
            Some("file")
        );
        assert_eq!(
            value.get("result")
                .and_then(|v| v.get("priority_focus_primary_next_step_target_path"))
                .and_then(|v| v.as_str()),
            Some("src/auth/login.ts")
        );
        assert_eq!(
            value.get("result")
                .and_then(|v| v.get("priority_focus_primary_command"))
                .and_then(|v| v.as_str()),
            Some("semantic --repo . retrieve --op get_file_brief --file \"src/auth/login.ts\" --output text")
        );
        assert_eq!(
            value.get("result")
                .and_then(|v| v.get("priority_focus_commands"))
                .and_then(|v| v.as_array())
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| item.as_str())
                        .collect::<Vec<_>>()
                }),
            Some(vec![
                "auth -> semantic --repo . retrieve --op get_file_brief --file \"src/auth/login.ts\" --output text"
            ])
        );
        assert_eq!(
            value.get("result")
                .and_then(|v| v.get("priority_focus_follow_up_operations"))
                .and_then(|v| v.as_array())
                .and_then(|items| items.first())
                .and_then(|item| item.get("target"))
                .and_then(|v| v.as_str()),
            Some("auth")
        );
        assert_eq!(
            value.get("result")
                .and_then(|v| v.get("priority_focus_follow_up_operations"))
                .and_then(|v| v.as_array())
                .and_then(|items| items.first())
                .and_then(|item| item.get("trust"))
                .and_then(|v| v.as_str()),
            Some("semantic_precise")
        );
        assert_eq!(
            value.get("result")
                .and_then(|v| v.get("priority_focus_entries"))
                .and_then(|v| v.as_array())
                .and_then(|items| items.first())
                .and_then(|item| item.get("support_level"))
                .and_then(|v| v.as_str()),
            Some("indexed")
        );
        assert_eq!(
            value.get("result")
                .and_then(|v| v.get("priority_focus_entries"))
                .and_then(|v| v.as_array())
                .and_then(|items| items.first())
                .and_then(|item| item.get("open_first_path"))
                .and_then(|v| v.as_str()),
            Some("src/auth/login.ts")
        );
        assert_eq!(
            value.get("result")
                .and_then(|v| v.get("priority_focus_entries"))
                .and_then(|v| v.as_array())
                .and_then(|items| items.first())
                .and_then(|item| item.get("next_step_target_kind"))
                .and_then(|v| v.as_str()),
            Some("file")
        );
        assert_eq!(
            value.get("result")
                .and_then(|v| v.get("summary"))
                .and_then(|v| v.as_str()),
            Some("Orientation map for 2 module(s) (1 indexed, 1 outside parser coverage)")
        );
        assert_eq!(
            value.get("result")
                .and_then(|v| v.get("high_priority_modules"))
                .and_then(|v| v.as_str()),
            Some("auth [high] | semantic_app [high, unsupported_source, orientation_only]")
        );
        assert_eq!(
            value.get("result")
                .and_then(|v| v.get("discovered_module_count"))
                .and_then(|v| v.as_u64()),
            Some(2)
        );
        assert_eq!(
            value.get("result")
                .and_then(|v| v.get("visible_module_count"))
                .and_then(|v| v.as_u64()),
            Some(2)
        );
        assert_eq!(
            value.get("result")
                .and_then(|v| v.get("priority_modules"))
                .and_then(|v| v.as_array())
                .map(|items| items.len()),
            Some(2)
        );
        assert!(
            value.get("result")
                .and_then(|v| v.get("priority_modules"))
                .and_then(|v| v.as_array())
                .and_then(|items| items.first())
                .and_then(|item| item.get("signals"))
                .and_then(|v| v.as_array())
                .map(|signals| signals.iter().any(|signal| signal.as_str() == Some("entry_points")))
                .unwrap_or(false)
        );
        assert!(
            value.get("result")
                .and_then(|v| v.get("priority_modules"))
                .and_then(|v| v.as_array())
                .and_then(|items| items.first())
                .and_then(|item| item.get("files"))
                .and_then(|v| v.as_array())
                .map(|files| files.iter().any(|file| file.as_str() == Some("login.ts")))
                .unwrap_or(false)
        );
        assert_eq!(
            value.get("result")
                .and_then(|v| v.get("priority_modules"))
                .and_then(|v| v.as_array())
                .and_then(|items| items.first())
                .and_then(|item| item.get("priority_rank"))
                .and_then(|v| v.as_u64()),
            Some(1)
        );
        assert_eq!(
            value.get("result")
                .and_then(|v| v.get("priority_modules"))
                .and_then(|v| v.as_array())
                .and_then(|items| items.first())
                .and_then(|item| item.get("priority_score_gap_from_previous"))
                .and_then(|v| v.as_u64()),
            None
        );
        assert_eq!(
            value.get("result")
                .and_then(|v| v.get("priority_modules"))
                .and_then(|v| v.as_array())
                .and_then(|items| items.first())
                .and_then(|item| item.get("priority_score_gap_to_next"))
                .and_then(|v| v.as_u64()),
            Some(10)
        );
        assert_eq!(
            value.get("result")
                .and_then(|v| v.get("priority_modules"))
                .and_then(|v| v.as_array())
                .and_then(|items| items.first())
                .and_then(|item| item.get("priority_score_separation"))
                .and_then(|v| v.as_str()),
            Some("clear_lead")
        );
        assert_eq!(
            value.get("result")
                .and_then(|v| v.get("priority_modules"))
                .and_then(|v| v.as_array())
                .and_then(|items| items.first())
                .and_then(|item| item.get("priority_score"))
                .and_then(|v| v.as_u64()),
            Some(333)
        );
        assert_eq!(
            value.get("result")
                .and_then(|v| v.get("priority_modules"))
                .and_then(|v| v.as_array())
                .and_then(|items| items.first())
                .and_then(|item| item.get("priority_score_components"))
                .and_then(|v| v.get("importance"))
                .and_then(|v| v.as_u64()),
            Some(300)
        );
        assert_eq!(
            value.get("result")
                .and_then(|v| v.get("priority_modules"))
                .and_then(|v| v.as_array())
                .and_then(|items| items.first())
                .and_then(|item| item.get("priority_score_components"))
                .and_then(|v| v.get("support"))
                .and_then(|v| v.as_u64()),
            Some(30)
        );
        assert_eq!(
            value.get("result")
                .and_then(|v| v.get("priority_modules"))
                .and_then(|v| v.as_array())
                .and_then(|items| items.first())
                .and_then(|item| item.get("priority_score_components"))
                .and_then(|v| v.get("signals"))
                .and_then(|v| v.as_u64()),
            Some(3)
        );
        assert_eq!(
            value.get("result")
                .and_then(|v| v.get("priority_modules"))
                .and_then(|v| v.as_array())
                .and_then(|items| items.get(1))
                .and_then(|item| item.get("priority_rank"))
                .and_then(|v| v.as_u64()),
            Some(2)
        );
        assert_eq!(
            value.get("result")
                .and_then(|v| v.get("priority_modules"))
                .and_then(|v| v.as_array())
                .and_then(|items| items.get(1))
                .and_then(|item| item.get("priority_score_gap_from_previous"))
                .and_then(|v| v.as_u64()),
            Some(10)
        );
        assert_eq!(
            value.get("result")
                .and_then(|v| v.get("priority_modules"))
                .and_then(|v| v.as_array())
                .and_then(|items| items.get(1))
                .and_then(|item| item.get("priority_score"))
                .and_then(|v| v.as_u64()),
            Some(323)
        );
        assert_eq!(
            value.get("result")
                .and_then(|v| v.get("priority_modules"))
                .and_then(|v| v.as_array())
                .and_then(|items| items.get(1))
                .and_then(|item| item.get("priority_score_gap_to_next"))
                .and_then(|v| v.as_u64()),
            None
        );
        assert_eq!(
            value.get("result")
                .and_then(|v| v.get("priority_modules"))
                .and_then(|v| v.as_array())
                .and_then(|items| items.get(1))
                .and_then(|item| item.get("priority_score_separation"))
                .and_then(|v| v.as_str()),
            Some("clear_trailing_gap")
        );
        assert_eq!(
            value.get("result")
                .and_then(|v| v.get("priority_modules"))
                .and_then(|v| v.as_array())
                .and_then(|items| items.first())
                .and_then(|item| item.get("actionability"))
                .and_then(|v| v.as_str()),
            Some("semantic_precise")
        );
        assert_eq!(
            value.get("result")
                .and_then(|v| v.get("priority_modules"))
                .and_then(|v| v.as_array())
                .and_then(|items| items.first())
                .and_then(|item| item.get("next_step_operation"))
                .and_then(|v| v.as_str()),
            Some("get_file_brief")
        );
        assert_eq!(
            value.get("result")
                .and_then(|v| v.get("priority_modules"))
                .and_then(|v| v.as_array())
                .and_then(|items| items.first())
                .and_then(|item| item.get("next_step_target_kind"))
                .and_then(|v| v.as_str()),
            Some("file")
        );
        assert_eq!(
            value.get("result")
                .and_then(|v| v.get("priority_modules"))
                .and_then(|v| v.as_array())
                .and_then(|items| items.first())
                .and_then(|item| item.get("next_step_command"))
                .and_then(|v| v.as_str()),
            Some(
                "semantic --repo . retrieve --op get_file_brief --file \"src/auth/login.ts\" --output text"
            )
        );
        assert_eq!(
            value.get("result")
                .and_then(|v| v.get("priority_modules"))
                .and_then(|v| v.as_array())
                .and_then(|items| items.get(1))
                .and_then(|item| item.get("actionability"))
                .and_then(|v| v.as_str()),
            Some("orientation_only")
        );
        assert_eq!(
            value.get("result")
                .and_then(|v| v.get("priority_modules"))
                .and_then(|v| v.as_array())
                .and_then(|items| items.get(1))
                .and_then(|item| item.get("next_step_target_kind"))
                .and_then(|v| v.as_str()),
            Some("file")
        );
        assert_eq!(
            value.get("result")
                .and_then(|v| v.get("priority_modules"))
                .and_then(|v| v.as_array())
                .and_then(|items| items.get(1))
                .and_then(|item| item.get("next_step_target_path"))
                .and_then(|v| v.as_str()),
            Some("semantic_app/src/main.rs")
        );
        assert_eq!(
            value.get("result")
                .and_then(|v| v.get("priority_modules"))
                .and_then(|v| v.as_array())
                .and_then(|items| items.first())
                .and_then(|item| item.get("open_first_path"))
                .and_then(|v| v.as_str()),
            Some("src/auth/login.ts")
        );
        let auth = modules
            .iter()
            .find(|module| module.get("name").and_then(|v| v.as_str()) == Some("auth"))
            .expect("auth module");
        assert_eq!(
            auth.get("support_level").and_then(|v| v.as_str()),
            Some("indexed")
        );

        let rust = modules
            .iter()
            .find(|module| module.get("name").and_then(|v| v.as_str()) == Some("semantic_app"))
            .expect("semantic_app module");
        assert_eq!(
            rust.get("support_level").and_then(|v| v.as_str()),
            Some("unsupported_source")
        );
        assert!(
            rust.get("signals")
                .and_then(|v| v.as_array())
                .map(|signals| signals.iter().any(|item| item.as_str() == Some("outside_parser_support")))
                .unwrap_or(false)
        );

        let api_like = modules
            .iter()
            .find(|module| module.get("name").and_then(|v| v.as_str()) == Some("semantic_app"))
            .expect("semantic_app module");
        assert!(
            api_like
                .get("entry_points")
                .and_then(|v| v.as_array())
                .map(|items| items.iter().any(|item| item.as_str() == Some("main")))
                .unwrap_or(false)
        );
    }

    #[test]
    fn retrieve_architecture_map_prefers_entry_point_matching_file_for_open_first_path() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join("frontend")).expect("mkdir frontend");
        fs::write(
            repo.join("frontend").join("a_helpers.ts"),
            "export function helperThing(){ return true; }\n",
        )
        .expect("write helper");
        fs::write(
            repo.join("frontend").join("client.ts"),
            "export function ApiClient(){ return true; }\n",
        )
        .expect("write client");

        let runtime = AppRuntime::bootstrap(
            repo.clone(),
            RuntimeOptions {
                start_watcher: false,
                ensure_config: true,
                bootstrap_index_policy: crate::BootstrapIndexPolicy::ReuseExistingOrCreate,
            },
        )
        .expect("bootstrap runtime");

        let value = runtime.handle_retrieve(crate::models::RetrieveRequestBody {
            request: RetrievalRequest {
                operation: Operation::GetArchitectureMap,
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
            auto_index_target: None,
        });

        assert_eq!(
            value.get("result")
                .and_then(|v| v.get("priority_modules"))
                .and_then(|v| v.as_array())
                .and_then(|items| items.first())
                .and_then(|item| item.get("open_first_path"))
                .and_then(|v| v.as_str()),
            Some("frontend/client.ts")
        );
    }

    #[test]
    fn retrieve_architecture_map_marks_rust_main_modules_as_entry_points() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join("api").join("src")).expect("mkdir api");
        fs::create_dir_all(repo.join("engine").join("src")).expect("mkdir engine");
        fs::write(repo.join("api").join("src").join("main.rs"), "fn main() {}\n")
            .expect("write api");
        fs::write(repo.join("engine").join("src").join("lib.rs"), "pub fn run() {}\n")
            .expect("write engine");

        let runtime = AppRuntime::bootstrap(
            repo.clone(),
            RuntimeOptions {
                start_watcher: false,
                ensure_config: true,
                bootstrap_index_policy: crate::BootstrapIndexPolicy::Skip,
            },
        )
        .expect("bootstrap runtime");

        let value = runtime.handle_retrieve(crate::models::RetrieveRequestBody {
            request: RetrievalRequest {
                operation: Operation::GetArchitectureMap,
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
            auto_index_target: None,
        });

        let modules = value
            .get("result")
            .and_then(|v| v.get("modules"))
            .and_then(|v| v.as_array())
            .expect("modules");
        let api = modules
            .iter()
            .find(|module| module.get("name").and_then(|v| v.as_str()) == Some("api"))
            .expect("api module");
        assert_eq!(
            api.get("entry_points")
                .and_then(|v| v.as_array())
                .map(|items| items.iter().filter_map(|item| item.as_str()).collect::<Vec<_>>()),
            Some(vec!["main"])
        );
        assert!(
            api.get("signals")
                .and_then(|v| v.as_array())
                .map(|items| items.iter().any(|item| item.as_str() == Some("entry_points")))
                .unwrap_or(false)
        );
    }

    #[test]
    fn retrieve_architecture_map_marks_rust_bin_modules_as_entry_points() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join("cli").join("src").join("bin")).expect("mkdir cli");
        fs::create_dir_all(repo.join("engine").join("src")).expect("mkdir engine");
        fs::write(
            repo.join("cli").join("src").join("bin").join("admin.rs"),
            "fn main() {}\n",
        )
        .expect("write cli bin");
        fs::write(repo.join("engine").join("src").join("lib.rs"), "pub fn run() {}\n")
            .expect("write engine");

        let runtime = AppRuntime::bootstrap(
            repo.clone(),
            RuntimeOptions {
                start_watcher: false,
                ensure_config: true,
                bootstrap_index_policy: crate::BootstrapIndexPolicy::Skip,
            },
        )
        .expect("bootstrap runtime");

        let value = runtime.handle_retrieve(crate::models::RetrieveRequestBody {
            request: RetrievalRequest {
                operation: Operation::GetArchitectureMap,
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
            auto_index_target: None,
        });

        let modules = value
            .get("result")
            .and_then(|v| v.get("modules"))
            .and_then(|v| v.as_array())
            .expect("modules");
        let cli = modules
            .iter()
            .find(|module| module.get("name").and_then(|v| v.as_str()) == Some("cli"))
            .expect("cli module");
        assert_eq!(
            cli.get("entry_points")
                .and_then(|v| v.as_array())
                .map(|items| items.iter().filter_map(|item| item.as_str()).collect::<Vec<_>>()),
            Some(vec!["admin"])
        );
        assert!(
            cli.get("signals")
                .and_then(|v| v.as_array())
                .map(|items| items.iter().any(|item| item.as_str() == Some("entry_points")))
                .unwrap_or(false)
        );
    }

    #[test]
    fn retrieve_architecture_map_marks_python_and_server_bootstraps_as_entry_points() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join("backend")).expect("mkdir backend");
        fs::create_dir_all(repo.join("frontend")).expect("mkdir frontend");
        fs::write(repo.join("backend").join("manage.py"), "print('ok')\n")
            .expect("write manage");
        fs::write(
            repo.join("frontend").join("server.ts"),
            "export function startServer() { return true; }\n",
        )
        .expect("write server");

        let runtime = AppRuntime::bootstrap(
            repo.clone(),
            RuntimeOptions {
                start_watcher: false,
                ensure_config: true,
                bootstrap_index_policy: crate::BootstrapIndexPolicy::Skip,
            },
        )
        .expect("bootstrap runtime");

        let value = runtime.handle_retrieve(crate::models::RetrieveRequestBody {
            request: RetrievalRequest {
                operation: Operation::GetArchitectureMap,
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
            auto_index_target: None,
        });

        let modules = value
            .get("result")
            .and_then(|v| v.get("modules"))
            .and_then(|v| v.as_array())
            .expect("modules");
        let backend = modules
            .iter()
            .find(|module| module.get("name").and_then(|v| v.as_str()) == Some("backend"))
            .expect("backend module");
        assert!(
            backend
                .get("entry_points")
                .and_then(|v| v.as_array())
                .map(|items| items.iter().any(|item| item.as_str() == Some("manage")))
                .unwrap_or(false)
        );
        let frontend = modules
            .iter()
            .find(|module| module.get("name").and_then(|v| v.as_str()) == Some("frontend"))
            .expect("frontend module");
        assert!(
            frontend
                .get("entry_points")
                .and_then(|v| v.as_array())
                .map(|items| items.iter().any(|item| item.as_str() == Some("server")))
                .unwrap_or(false)
        );
    }

    #[test]
    fn retrieve_architecture_map_groups_excess_unsupported_source_modules() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join("src").join("auth")).expect("mkdir auth");
        fs::write(
            repo.join("src").join("auth").join("login.ts"),
            "export function login(){ return true; }\n",
        )
        .expect("write auth");

        for crate_name in [
            "semantic_app",
            "engine",
            "retrieval",
            "api",
            "parser",
            "storage",
            "watcher",
                "planner",
        ] {
            fs::create_dir_all(repo.join(crate_name).join("src")).expect("mkdir rust crate");
            fs::write(
                repo.join(crate_name).join("src").join("lib.rs"),
                "pub fn marker() {}\n",
            )
            .expect("write rust crate");
        }
        fs::create_dir_all(repo.join("cli").join("src").join("bin")).expect("mkdir cli bin");
        fs::write(
            repo.join("cli").join("src").join("bin").join("admin.rs"),
            "fn main() {}\n",
        )
        .expect("write cli bin");

        let runtime = AppRuntime::bootstrap(
            repo.clone(),
            RuntimeOptions {
                start_watcher: false,
                ensure_config: true,
                bootstrap_index_policy: crate::BootstrapIndexPolicy::ReuseExistingOrCreate,
            },
        )
        .expect("bootstrap runtime");

        let value = runtime.handle_retrieve(crate::models::RetrieveRequestBody {
            request: RetrievalRequest {
                operation: Operation::GetArchitectureMap,
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
            auto_index_target: None,
        });

        let modules = value
            .get("result")
            .and_then(|v| v.get("modules"))
            .and_then(|v| v.as_array())
            .expect("modules");
        let priority_modules = value
            .get("result")
            .and_then(|v| v.get("priority_modules"))
            .and_then(|v| v.as_array())
            .expect("priority modules");
        let cli = modules
            .iter()
            .find(|module| module.get("name").and_then(|v| v.as_str()) == Some("cli"))
            .expect("cli module should stay visible");
        assert!(
            cli.get("entry_points")
                .and_then(|v| v.as_array())
                .map(|items| items.iter().any(|item| item.as_str() == Some("admin")))
                .unwrap_or(false)
        );
        assert!(
            priority_modules
                .iter()
                .any(|module| module.get("name").and_then(|v| v.as_str()) == Some("cli"))
        );
        assert!(
            !priority_modules.iter().any(|module| {
                module.get("support_level").and_then(|v| v.as_str())
                    == Some("unsupported_source_group")
            })
        );
        assert!(
            !value
                .get("result")
                .and_then(|v| v.get("high_priority_modules"))
                .and_then(|v| v.as_str())
                .map(|summary| summary.contains("other_unsupported_sources"))
                .unwrap_or(false)
        );
        let standalone_unsupported = modules
            .iter()
            .filter(|module| {
                module.get("support_level").and_then(|v| v.as_str()) == Some("unsupported_source")
            })
            .count();
        assert!(standalone_unsupported <= MAX_UNSUPPORTED_SOURCE_MODULES);
        let grouped = modules
            .iter()
            .find(|module| {
                module.get("support_level").and_then(|v| v.as_str())
                    == Some("unsupported_source_group")
            })
            .expect("grouped unsupported module");
        assert_eq!(
            grouped.get("grouped_module_count").and_then(|v| v.as_u64()),
            Some(3)
        );
        assert_eq!(
            value.get("result")
                .and_then(|v| v.get("grouped_hidden_module_count"))
                .and_then(|v| v.as_u64()),
            Some(3)
        );
        assert_eq!(
            value.get("result")
                .and_then(|v| v.get("summary"))
                .and_then(|v| v.as_str()),
            Some(
                "Orientation map for 10 discovered module(s), showing 8 (1 indexed, 9 outside parser coverage)"
            )
        );
    }

    #[test]
    fn retrieve_returns_architecture_map_from_filesystem_when_unindexed() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join("frontend")).expect("mkdir frontend");
        fs::write(repo.join("frontend").join("a.ts"), "export const helper = 1;\n")
            .expect("write helper");
        fs::write(repo.join("frontend").join("index.tsx"), "export const App = 1;\n")
            .expect("write frontend");

        let runtime = AppRuntime::bootstrap(
            repo.clone(),
            RuntimeOptions {
                start_watcher: false,
                ensure_config: true,
                bootstrap_index_policy: crate::BootstrapIndexPolicy::Skip,
            },
        )
        .expect("bootstrap runtime");

        let value = runtime.handle_retrieve(crate::models::RetrieveRequestBody {
            request: RetrievalRequest {
                operation: Operation::GetArchitectureMap,
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
            auto_index_target: None,
        });

        let result = value.get("result").expect("result");
        assert_eq!(
            result.get("orientation_stage").and_then(|v| v.as_str()),
            Some("filesystem_heuristic")
        );
        assert_eq!(
            result.get("priority_scoring_model").and_then(|v| v.as_str()),
            Some("architecture_priority_v1")
        );
        assert_eq!(
            result
                .get("priority_scoring_weights")
                .and_then(|v| v.get("importance_multiplier"))
                .and_then(|v| v.as_u64()),
            Some(100)
        );
        assert_eq!(
            result
                .get("priority_scoring_weights")
                .and_then(|v| v.get("support_weights"))
                .and_then(|v| v.get("filesystem_heuristic"))
                .and_then(|v| v.as_u64()),
            Some(0)
        );
        assert_eq!(
            result
                .get("priority_focus_mode")
                .and_then(|v| v.as_str()),
            Some("single_focus")
        );
        assert_eq!(
            result
                .get("priority_focus_reason")
                .and_then(|v| v.as_str()),
            Some("only one priority module is available")
        );
        assert_eq!(
            result
                .get("priority_focus_trust")
                .and_then(|v| v.as_str()),
            Some("orientation_only")
        );
        assert_eq!(
            result
                .get("priority_focus_targets")
                .and_then(|v| v.as_array())
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| item.as_str())
                        .collect::<Vec<_>>()
                }),
            Some(vec!["frontend"])
        );
        assert_eq!(
            result
                .get("priority_focus_primary_target")
                .and_then(|v| v.as_str()),
            Some("frontend")
        );
        assert_eq!(
            result
                .get("priority_focus_primary_path")
                .and_then(|v| v.as_str()),
            Some("frontend")
        );
        assert_eq!(
            result
                .get("priority_focus_primary_importance")
                .and_then(|v| v.as_str()),
            Some("high")
        );
        assert_eq!(
            result
                .get("priority_focus_primary_support_level")
                .and_then(|v| v.as_str()),
            Some("filesystem_heuristic")
        );
        assert_eq!(
            result
                .get("priority_focus_primary_actionability")
                .and_then(|v| v.as_str()),
            Some("filesystem_heuristic")
        );
        assert_eq!(
            result
                .get("priority_focus_primary_trust")
                .and_then(|v| v.as_str()),
            Some("orientation_only")
        );
        assert_eq!(
            result
                .get("priority_focus_primary_rank")
                .and_then(|v| v.as_u64()),
            Some(1)
        );
        assert_eq!(
            result
                .get("priority_focus_primary_score")
                .and_then(|v| v.as_u64()),
            Some(302)
        );
        assert_eq!(
            result
                .get("priority_focus_primary_score_components")
                .and_then(|v| v.get("importance"))
                .and_then(|v| v.as_u64()),
            Some(300)
        );
        assert_eq!(
            result
                .get("priority_focus_primary_score_components")
                .and_then(|v| v.get("support"))
                .and_then(|v| v.as_u64()),
            Some(0)
        );
        assert_eq!(
            result
                .get("priority_focus_primary_score_components")
                .and_then(|v| v.get("signals"))
                .and_then(|v| v.as_u64()),
            Some(2)
        );
        assert_eq!(
            result
                .get("priority_focus_primary_score_gap_from_previous")
                .and_then(|v| v.as_u64()),
            None
        );
        assert_eq!(
            result
                .get("priority_focus_primary_score_gap_to_next")
                .and_then(|v| v.as_u64()),
            None
        );
        assert_eq!(
            result
                .get("priority_focus_primary_score_separation")
                .and_then(|v| v.as_str()),
            Some("solo")
        );
        assert_eq!(
            result
                .get("priority_focus_primary_signals")
                .and_then(|v| v.as_array())
                .map(|items| items.iter().filter_map(|item| item.as_str()).collect::<Vec<_>>()),
            Some(vec!["entry_points", "weak_test_coverage"])
        );
        assert_eq!(
            result
                .get("priority_focus_primary_entry_points")
                .and_then(|v| v.as_array())
                .map(|items| items.iter().filter_map(|item| item.as_str()).collect::<Vec<_>>()),
            Some(vec!["index"])
        );
        assert_eq!(
            result
                .get("priority_focus_primary_files")
                .and_then(|v| v.as_array())
                .map(|items| items.iter().filter_map(|item| item.as_str()).collect::<Vec<_>>()),
            Some(vec!["a.ts", "index.tsx"])
        );
        assert_eq!(
            result
                .get("priority_focus_primary_indexed_file_count")
                .and_then(|v| v.as_u64()),
            Some(0)
        );
        assert_eq!(
            result
                .get("priority_focus_primary_source_file_count")
                .and_then(|v| v.as_u64()),
            Some(2)
        );
        assert_eq!(
            result
                .get("priority_focus_primary_fan_out")
                .and_then(|v| v.as_u64()),
            Some(0)
        );
        assert_eq!(
            result
                .get("priority_focus_primary_open_first_path")
                .and_then(|v| v.as_str()),
            Some("frontend/index.tsx")
        );
        assert_eq!(
            result
                .get("priority_focus_primary_next_step_operation")
                .and_then(|v| v.as_str()),
            Some("get_file_brief")
        );
        assert_eq!(
            result
                .get("priority_focus_primary_next_step_target_kind")
                .and_then(|v| v.as_str()),
            Some("file")
        );
        assert_eq!(
            result
                .get("priority_focus_primary_next_step_target_path")
                .and_then(|v| v.as_str()),
            Some("frontend/index.tsx")
        );
        assert_eq!(
            result
                .get("priority_focus_primary_command")
                .and_then(|v| v.as_str()),
            Some("semantic --repo . retrieve --op get_file_brief --file \"frontend/index.tsx\" --output text")
        );
        assert_eq!(
            result
                .get("priority_focus_commands")
                .and_then(|v| v.as_array())
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| item.as_str())
                        .collect::<Vec<_>>()
                }),
            Some(vec![
                "frontend -> semantic --repo . retrieve --op get_file_brief --file \"frontend/index.tsx\" --output text"
            ])
        );
        assert_eq!(
            result
                .get("priority_focus_follow_up_operations")
                .and_then(|v| v.as_array())
                .and_then(|items| items.first())
                .and_then(|item| item.get("target"))
                .and_then(|v| v.as_str()),
            Some("frontend")
        );
        assert_eq!(
            result
                .get("priority_focus_follow_up_operations")
                .and_then(|v| v.as_array())
                .and_then(|items| items.first())
                .and_then(|item| item.get("trust"))
                .and_then(|v| v.as_str()),
            Some("orientation_only")
        );
        assert_eq!(
            result
                .get("priority_focus_entries")
                .and_then(|v| v.as_array())
                .and_then(|items| items.first())
                .and_then(|item| item.get("support_level"))
                .and_then(|v| v.as_str()),
            Some("filesystem_heuristic")
        );
        assert_eq!(
            result
                .get("priority_focus_entries")
                .and_then(|v| v.as_array())
                .and_then(|items| items.first())
                .and_then(|item| item.get("open_first_path"))
                .and_then(|v| v.as_str()),
            Some("frontend/index.tsx")
        );
        assert_eq!(
            result
                .get("priority_focus_entries")
                .and_then(|v| v.as_array())
                .and_then(|items| items.first())
                .and_then(|item| item.get("next_step_target_kind"))
                .and_then(|v| v.as_str()),
            Some("file")
        );
        assert_eq!(
            result
                .get("discovered_module_count")
                .and_then(|v| v.as_u64()),
            Some(1)
        );
        assert_eq!(
            result.get("visible_module_count").and_then(|v| v.as_u64()),
            Some(1)
        );
        assert_eq!(
            result
                .get("grouped_hidden_module_count")
                .and_then(|v| v.as_u64()),
            Some(0)
        );
        assert_eq!(
            result
                .get("high_priority_modules")
                .and_then(|v| v.as_str()),
            Some("frontend [high, filesystem_heuristic]")
        );
        assert_eq!(
            result
                .get("priority_modules")
                .and_then(|v| v.as_array())
                .map(|items| items.len()),
            Some(1)
        );
        assert!(
            result
                .get("priority_modules")
                .and_then(|v| v.as_array())
                .and_then(|items| items.first())
                .and_then(|item| item.get("entry_points"))
                .and_then(|v| v.as_array())
                .map(|items| items.iter().any(|item| item.as_str() == Some("index")))
                .unwrap_or(false)
        );
        assert!(
            result
                .get("priority_modules")
                .and_then(|v| v.as_array())
                .and_then(|items| items.first())
                .and_then(|item| item.get("files"))
                .and_then(|v| v.as_array())
                .map(|items| items.iter().any(|item| item.as_str() == Some("index.tsx")))
                .unwrap_or(false)
        );
        assert_eq!(
            result
                .get("priority_modules")
                .and_then(|v| v.as_array())
                .and_then(|items| items.first())
                .and_then(|item| item.get("priority_rank"))
                .and_then(|v| v.as_u64()),
            Some(1)
        );
        assert_eq!(
            result
                .get("priority_modules")
                .and_then(|v| v.as_array())
                .and_then(|items| items.first())
                .and_then(|item| item.get("priority_score_gap_from_previous"))
                .and_then(|v| v.as_u64()),
            None
        );
        assert_eq!(
            result
                .get("priority_modules")
                .and_then(|v| v.as_array())
                .and_then(|items| items.first())
                .and_then(|item| item.get("priority_score_gap_to_next"))
                .and_then(|v| v.as_u64()),
            None
        );
        assert_eq!(
            result
                .get("priority_modules")
                .and_then(|v| v.as_array())
                .and_then(|items| items.first())
                .and_then(|item| item.get("priority_score_separation"))
                .and_then(|v| v.as_str()),
            Some("solo")
        );
        assert_eq!(
            result
                .get("priority_modules")
                .and_then(|v| v.as_array())
                .and_then(|items| items.first())
                .and_then(|item| item.get("priority_score"))
                .and_then(|v| v.as_u64()),
            Some(302)
        );
        assert_eq!(
            result
                .get("priority_modules")
                .and_then(|v| v.as_array())
                .and_then(|items| items.first())
                .and_then(|item| item.get("priority_score_components"))
                .and_then(|v| v.get("importance"))
                .and_then(|v| v.as_u64()),
            Some(300)
        );
        assert_eq!(
            result
                .get("priority_modules")
                .and_then(|v| v.as_array())
                .and_then(|items| items.first())
                .and_then(|item| item.get("priority_score_components"))
                .and_then(|v| v.get("support"))
                .and_then(|v| v.as_u64()),
            Some(0)
        );
        assert_eq!(
            result
                .get("priority_modules")
                .and_then(|v| v.as_array())
                .and_then(|items| items.first())
                .and_then(|item| item.get("priority_score_components"))
                .and_then(|v| v.get("signals"))
                .and_then(|v| v.as_u64()),
            Some(2)
        );
        assert_eq!(
            result
                .get("priority_modules")
                .and_then(|v| v.as_array())
                .and_then(|items| items.first())
                .and_then(|item| item.get("actionability"))
                .and_then(|v| v.as_str()),
            Some("filesystem_heuristic")
        );
        assert_eq!(
            result
                .get("priority_modules")
                .and_then(|v| v.as_array())
                .and_then(|items| items.first())
                .and_then(|item| item.get("next_step_operation"))
                .and_then(|v| v.as_str()),
            Some("get_file_brief")
        );
        assert_eq!(
            result
                .get("priority_modules")
                .and_then(|v| v.as_array())
                .and_then(|items| items.first())
                .and_then(|item| item.get("next_step_target_kind"))
                .and_then(|v| v.as_str()),
            Some("file")
        );
        assert_eq!(
            result
                .get("priority_modules")
                .and_then(|v| v.as_array())
                .and_then(|items| items.first())
                .and_then(|item| item.get("next_step_command"))
                .and_then(|v| v.as_str()),
            Some(
                "semantic --repo . retrieve --op get_file_brief --file \"frontend/index.tsx\" --output text"
            )
        );
        assert_eq!(
            result
                .get("priority_modules")
                .and_then(|v| v.as_array())
                .and_then(|items| items.first())
                .and_then(|item| item.get("open_first_path"))
                .and_then(|v| v.as_str()),
            Some("frontend/index.tsx")
        );
        assert!(
            result
                .get("modules")
                .and_then(|v| v.as_array())
                .map(|items| items.iter().any(|item| item.get("name").and_then(|v| v.as_str()) == Some("frontend")))
                .unwrap_or(false)
        );
        let frontend = result
            .get("modules")
            .and_then(|v| v.as_array())
            .and_then(|items| {
                items.iter()
                    .find(|item| item.get("name").and_then(|v| v.as_str()) == Some("frontend"))
            })
            .expect("frontend module");
        assert_eq!(
            frontend.get("support_level").and_then(|v| v.as_str()),
            Some("filesystem_heuristic")
        );
    }

    #[test]
    fn retrieve_architecture_map_uses_directory_target_kind_when_open_file_hint_is_missing() {
        let module = serde_json::json!({
            "name": "docs",
            "path": "docs",
            "importance": "medium",
            "support_level": "unsupported_source_group",
            "signals": ["grouped_modules"],
            "entry_points": [],
            "files": []
        });

        let priority_modules = architecture_priority_modules(&[module]);
        let first = priority_modules.first().expect("priority module");
        assert_eq!(
            first.get("next_step_operation").and_then(|v| v.as_str()),
            Some("get_directory_brief")
        );
        assert_eq!(
            first.get("priority_score").and_then(|v| v.as_u64()),
            Some(211)
        );
        assert_eq!(
            first.get("priority_score_gap_from_previous")
                .and_then(|v| v.as_u64()),
            None
        );
        assert_eq!(
            first.get("priority_score_gap_to_next")
                .and_then(|v| v.as_u64()),
            None
        );
        assert_eq!(
            first.get("priority_score_separation")
                .and_then(|v| v.as_str()),
            Some("solo")
        );
        assert_eq!(
            first.get("priority_score_components")
                .and_then(|v| v.get("importance"))
                .and_then(|v| v.as_u64()),
            Some(200)
        );
        assert_eq!(
            first.get("priority_score_components")
                .and_then(|v| v.get("support"))
                .and_then(|v| v.as_u64()),
            Some(10)
        );
        assert_eq!(
            first.get("priority_score_components")
                .and_then(|v| v.get("signals"))
                .and_then(|v| v.as_u64()),
            Some(1)
        );
        assert_eq!(
            first.get("next_step_target_kind").and_then(|v| v.as_str()),
            Some("directory")
        );
        assert_eq!(
            first.get("next_step_target_path").and_then(|v| v.as_str()),
            Some("docs")
        );
        assert_eq!(
            first.get("next_step_command").and_then(|v| v.as_str()),
            Some(
                "semantic --repo . retrieve --op get_directory_brief --path \"docs\" --output text"
            )
        );
    }

    #[test]
    fn architecture_priority_focus_mode_prefers_compare_top_two_for_close_cluster() {
        let priority_modules = vec![
            serde_json::json!({
                "name": "module_a",
                "priority_score_separation": "close_cluster",
                "priority_score_gap_to_next": 1
            }),
            serde_json::json!({
                "name": "module_b",
                "priority_score_separation": "close_cluster",
                "priority_score_gap_to_next": 1
            }),
        ];

        assert_eq!(
            architecture_priority_focus_mode(&priority_modules),
            "compare_top_two"
        );
        assert_eq!(
            architecture_priority_focus_reason(&priority_modules, "compare_top_two"),
            "module_a and module_b are close enough to compare first (gap=1)"
        );
        assert_eq!(
            architecture_priority_focus_targets(&priority_modules, "compare_top_two"),
            vec!["module_a".to_string(), "module_b".to_string()]
        );
        let focus_entries = architecture_priority_focus_entries(
            &[
                serde_json::json!({
                    "name": "module_a",
                    "importance": "medium",
                    "support_level": "indexed",
                    "actionability": "semantic_precise",
                    "open_first_path": "src/module_a.ts",
                    "next_step_operation": "get_file_brief",
                    "next_step_target_kind": "file",
                    "next_step_target_path": "src/module_a.ts",
                    "next_step_command": "semantic --repo . retrieve --op get_file_brief --file \"src/module_a.ts\" --output text"
                }),
                serde_json::json!({
                    "name": "module_b",
                    "importance": "medium",
                    "support_level": "unsupported_source",
                    "actionability": "orientation_only",
                    "open_first_path": "src/module_b.ts",
                    "next_step_operation": "get_file_brief",
                    "next_step_target_kind": "file",
                    "next_step_target_path": "src/module_b.ts",
                    "next_step_command": "semantic --repo . retrieve --op get_file_brief --file \"src/module_b.ts\" --output text"
                }),
            ],
            &["module_a".to_string(), "module_b".to_string()]
        );
        assert_eq!(
            architecture_priority_focus_commands(
                &focus_entries
            ),
            vec![
                "module_a -> semantic --repo . retrieve --op get_file_brief --file \"src/module_a.ts\" --output text".to_string(),
                "module_b -> semantic --repo . retrieve --op get_file_brief --file \"src/module_b.ts\" --output text".to_string(),
            ]
        );
        assert_eq!(
            architecture_priority_focus_follow_up_operations(&focus_entries)
                .get(1)
                .and_then(|item| item.get("trust"))
                .and_then(|v| v.as_str()),
            Some("orientation_only")
        );
        assert_eq!(architecture_priority_focus_trust(&focus_entries), "mixed");
        let primary_entry = architecture_priority_focus_primary_entry(&focus_entries);
        let secondary_entry = architecture_priority_focus_secondary_entry(&focus_entries);
        assert_eq!(architecture_priority_entry_trust(&primary_entry), "semantic_precise");
        assert_eq!(architecture_priority_entry_trust(&secondary_entry), "orientation_only");
        assert_eq!(
            primary_entry.get("name").and_then(|v| v.as_str()),
            Some("module_a")
        );
        assert_eq!(
            primary_entry.get("importance").and_then(|v| v.as_str()),
            Some("medium")
        );
        assert_eq!(
            primary_entry.get("next_step_target_path").and_then(|v| v.as_str()),
            Some("src/module_a.ts")
        );
        assert_eq!(
            secondary_entry.get("name").and_then(|v| v.as_str()),
            Some("module_b")
        );
        assert_eq!(
            secondary_entry.get("importance").and_then(|v| v.as_str()),
            Some("medium")
        );
        assert_eq!(
            secondary_entry.get("next_step_target_path").and_then(|v| v.as_str()),
            Some("src/module_b.ts")
        );
        assert_eq!(
            focus_entries
                .first()
                .and_then(|item| item.get("open_first_path"))
                .and_then(|v| v.as_str()),
            Some("src/module_a.ts")
        );
        assert_eq!(
            focus_entries
                .get(1)
                .and_then(|item| item.get("next_step_target_path"))
                .and_then(|v| v.as_str()),
            Some("src/module_b.ts")
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
