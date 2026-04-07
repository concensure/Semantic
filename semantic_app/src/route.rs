use crate::models::IdeAutoRouteRequest;
use crate::runtime::summarize_indexed_path_hints;
use crate::runtime::AppRuntime;
use crate::session::{
    apply_session_context_reuse, apply_session_raw_expansion_controls, auto_session_id,
    context_delta, fnv1a_hash, refs_from_result, touch_or_create_session, RawExpansionMode,
};
use engine::{Operation, RetrievalRequest};
use impact_analysis::ImpactAnalyzer;
use llm_router::LLMTask;
use serde_json::json;
use std::collections::HashSet;

const AUTO_SUMMARY_FILE_THRESHOLD: usize = 50;

impl AppRuntime {
    pub fn handle_autoroute(&self, body: IdeAutoRouteRequest) -> serde_json::Value {
        let retry_body_template = body.clone();
        if let Some(action) = body.action.as_deref() {
            return self
                .handle_route_action(action, body.action_input.unwrap_or_else(|| json!({})));
        }

        let task = body.task.clone().unwrap_or_default();
        let intent = detect_ide_intent(&task);
        let retrieval_query = retrieval_query_for_task(&task);
        let effective_session_id = body.session_id.clone().unwrap_or_else(auto_session_id);

        let max_tokens = body.max_tokens.unwrap_or_else(|| match intent {
            "debug" => 1600,
            "refactor" => 2000,
            "implement" => 1400,
            _ => 800,
        });
        let single_file_fast_path = body.single_file_fast_path.unwrap_or(true);
        let reference_only = body.reference_only.unwrap_or(intent != "understand");
        let mapping_mode = body.mapping_mode.clone();
        let max_footprint_items = body.max_footprint_items;
        let reuse_session_context = body.reuse_session_context.unwrap_or(true);
        let include_summary = body.include_summary.unwrap_or(false);
        let auto_minimal_raw = body.auto_minimal_raw.unwrap_or(true);
        let raw_expansion_mode = RawExpansionMode::parse(body.raw_expansion_mode.as_deref());

        let (primary_operation, primary_tool_name) = if intent == "refactor" {
            (
                Operation::GetHybridRankedContext,
                "get_hybrid_ranked_context",
            )
        } else {
            (Operation::GetPlannedContext, "get_planned_context")
        };

        let planned_request = RetrievalRequest {
            operation: primary_operation,
            query: Some(retrieval_query.clone()),
            max_tokens: Some(max_tokens),
            ..Default::default()
        };
        let planned = self.retrieval().lock().handle_with_options_ext(
            planned_request,
            Some(single_file_fast_path),
            Some(!reference_only),
            mapping_mode.as_deref(),
            max_footprint_items,
        );

        let (selected_tool, mut result) = match planned {
            Ok(response) => (primary_tool_name, response.result),
            Err(_) => {
                let fallback = RetrievalRequest {
                    operation: Operation::SearchSemanticSymbol,
                    query: Some(retrieval_query.clone()),
                    limit: Some(8),
                    ..Default::default()
                };
                match self.retrieval().lock().handle(fallback) {
                    Ok(response) => ("search_semantic_symbol", response.result),
                    Err(err) => {
                        return json!({
                            "ok": false,
                            "session_id": effective_session_id,
                            "intent": intent,
                            "selected_tool": "none",
                            "error": err.to_string()
                        });
                    }
                }
            }
        };

        let context_refs_count = count_result_context_refs(&result);
        let mut escalation_applied = false;
        if context_refs_count == 0 && selected_tool != "search_semantic_symbol" {
            let escalation_req = RetrievalRequest {
                operation: Operation::SearchSemanticSymbol,
                query: Some(retrieval_query.clone()),
                limit: Some(6),
                ..Default::default()
            };
            if let Ok(response) = self.retrieval().lock().handle(escalation_req) {
                if let Some(obj) = result.as_object_mut() {
                    obj.insert("escalated_context".to_string(), response.result);
                    obj.insert(
                        "escalation_reason".to_string(),
                        json!("planned_context_returned_no_refs"),
                    );
                    escalation_applied = true;
                }
            }
        }

        let debug_candidates = if intent == "debug" {
            self.retrieval()
                .lock()
                .get_root_cause_candidates()
                .ok()
                .filter(|value| {
                    value
                        .get("root_cause_candidates")
                        .and_then(|v| v.as_array())
                        .map(|items| !items.is_empty())
                        .unwrap_or(false)
                })
        } else {
            None
        };

        let mut reused_context_count = 0usize;
        let mut auto_summary = None;
        let mut refs_unchanged = false;
        let mut context_delta_value = None;
        let mut context_delta_mode = false;
        let raw_outcome;

        {
            let index_revision = self.retrieval().lock().index_revision();
            let middleware = self.middleware();
            let mut middleware = middleware.lock();
            let session =
                touch_or_create_session(&mut middleware, &effective_session_id, index_revision);
            if reuse_session_context {
                reused_context_count = apply_session_context_reuse(&mut result, session);
            }
            raw_outcome =
                apply_session_raw_expansion_controls(&mut result, session, raw_expansion_mode);

            let current_symbol = extract_result_symbol_name(&result).unwrap_or_default();

            if !current_symbol.is_empty() {
                session
                    .last_target_symbols
                    .push_back(current_symbol.clone());
                while session.last_target_symbols.len() > 32 {
                    session.last_target_symbols.pop_front();
                }
                session
                    .intent_symbol_cache
                    .insert(task.to_lowercase(), current_symbol.clone());
            }

            let current_refs_json = result
                .get("context")
                .map(|value| serde_json::to_string(value).unwrap_or_default())
                .unwrap_or_default();
            let current_refs_hash = fnv1a_hash(&current_refs_json);
            if !current_symbol.is_empty() {
                if let Some(previous_hash) = session.last_refs_hash.get(&current_symbol) {
                    refs_unchanged =
                        *previous_hash == current_refs_hash && !current_refs_json.is_empty();
                }
                session
                    .last_refs_hash
                    .insert(current_symbol.clone(), current_refs_hash);
            }

            if !current_symbol.is_empty() && !refs_unchanged {
                let current_keys = refs_from_result(&result);
                let (delta_mode, delta_value) = context_delta(
                    session.last_context_keys.get(&current_symbol),
                    &current_keys,
                );
                context_delta_mode = delta_mode;
                context_delta_value = delta_value;
                session
                    .last_context_keys
                    .insert(current_symbol.clone(), current_keys);
            }

            if !include_summary && !session.summary_delivered {
                let file_count = self
                    .retrieval()
                    .lock()
                    .with_storage(|storage| storage.list_files().map(|files| files.len()))
                    .unwrap_or(0);
                if file_count >= AUTO_SUMMARY_FILE_THRESHOLD {
                    auto_summary = self.build_auto_summary(session, &current_symbol, intent);
                } else {
                    session.summary_delivered = true;
                }
            }
        }

        let confidence_score = result
            .get("confidence_score")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.55);
        let mut minimal_raw_seed_added = false;
        let mut low_confidence_raw_context_added = false;
        if reference_only && auto_minimal_raw {
            if (0.50..0.75).contains(&confidence_score) {
                if let Some(seed) = self.build_minimal_raw_seed(&result) {
                    if let Some(obj) = result.as_object_mut() {
                        obj.insert("minimal_raw_seed".to_string(), seed);
                        minimal_raw_seed_added = true;
                    }
                }
            } else if confidence_score < 0.50 {
                if let Some(raw) = self.build_low_confidence_raw_context(&result, 2) {
                    if let Some(obj) = result.as_object_mut() {
                        obj.insert("low_confidence_raw_context".to_string(), raw);
                        low_confidence_raw_context_added = true;
                    }
                }
            }
        }

        if reference_only
            && intent == "debug"
            && !minimal_raw_seed_added
            && !low_confidence_raw_context_added
        {
            self.inline_small_spans(&mut result);
        }

        let llm_task = match intent {
            "debug" | "refactor" => LLMTask::Planning,
            "implement" => LLMTask::CodeExecution,
            _ => LLMTask::InteractiveChat,
        };
        let route_decision = self.llm_router().and_then(|router| router.route(llm_task));
        let recommended_provider = route_decision.as_ref().map(|d| d.provider.as_str());
        let recommended_endpoint = route_decision.as_ref().map(|d| d.endpoint.as_str());

        let current_symbol_for_addons = result
            .get("symbol")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let current_top_file_for_addons = result
            .get("context")
            .and_then(|v| v.as_array())
            .and_then(|items| items.first())
            .and_then(|item| item.get("file"))
            .and_then(|v| v.as_str())
            .map(|value| value.to_string())
            .or_else(|| {
                result
                    .get("ranked_context")
                    .and_then(|v| v.as_array())
                    .and_then(|items| items.first())
                    .and_then(|item| item.get("file"))
                    .and_then(|v| v.as_str())
                    .map(|value| value.to_string())
            })
            .or_else(|| {
                result
                    .get("dependency_spans")
                    .and_then(|v| v.as_array())
                    .and_then(|items| items.first())
                    .and_then(|item| item.get("file"))
                    .and_then(|v| v.as_str())
                    .map(|value| value.to_string())
            });
        let indexed_files = self
            .retrieval()
            .lock()
            .with_storage(|storage| storage.list_files())
            .unwrap_or_default();
        let route_path_hint = extract_path_hint(&task);
        let (index_coverage, index_coverage_target) = compute_route_index_coverage(
            &indexed_files,
            current_top_file_for_addons.as_deref(),
            route_path_hint.as_deref(),
        );
        let route_index_readiness = index_readiness(indexed_files.len(), index_coverage);
        let auto_index_requested = body.auto_index_target.unwrap_or(false);
        if body.auto_index_target.unwrap_or(false) && index_coverage == "unindexed_target" {
            if let Some(target) = index_coverage_target.as_deref() {
                if self
                    .indexer()
                    .lock()
                    .index_paths(self.repo_root(), &[target.to_string()])
                    .is_ok()
                {
                    let mut retry_body = retry_body_template;
                    retry_body.auto_index_target = Some(false);
                    let mut retried = self.handle_autoroute(retry_body);
                    if route_coverage_resolved(&retried, target) {
                        let indexed_files = self
                            .retrieval()
                            .lock()
                            .with_storage(|storage| storage.list_files())
                            .unwrap_or_default();
                        if let Some(obj) = retried.as_object_mut() {
                            obj.insert("auto_index_applied".to_string(), json!(true));
                            obj.insert("auto_index_target".to_string(), json!(target));
                            obj.insert("indexed_file_count".to_string(), json!(indexed_files.len()));
                            obj.insert(
                                "indexed_path_hints".to_string(),
                                json!(summarize_indexed_path_hints(&indexed_files)),
                            );
                            obj.insert("index_readiness".to_string(), json!("target_ready"));
                            obj.insert("index_recovery_mode".to_string(), json!("auto_index_applied"));
                            if let Some(verification) =
                                obj.get_mut("verification").and_then(|v| v.as_object_mut())
                            {
                                verification.insert(
                                    "index_recovery_mode".to_string(),
                                    json!("auto_index_applied"),
                                );
                            }
                            if let Some(result) =
                                obj.get_mut("result").and_then(|v| v.as_object_mut())
                            {
                                result.insert(
                                    "index_recovery_mode".to_string(),
                                    json!("auto_index_applied"),
                                );
                            }
                        }
                    } else if let Some(obj) = retried.as_object_mut() {
                        obj.insert(
                            "index_recovery_mode".to_string(),
                            json!("auto_index_attempted_no_change"),
                        );
                        if let Some(verification) =
                            obj.get_mut("verification").and_then(|v| v.as_object_mut())
                        {
                            verification.insert(
                                "index_recovery_mode".to_string(),
                                json!("auto_index_attempted_no_change"),
                            );
                        }
                        if let Some(result) =
                            obj.get_mut("result").and_then(|v| v.as_object_mut())
                        {
                            result.insert(
                                "index_recovery_mode".to_string(),
                                json!("auto_index_attempted_no_change"),
                            );
                        }
                    }
                    return retried;
                }
            }
        }
        let impact_scope = if !current_symbol_for_addons.is_empty()
            && matches!(intent, "debug" | "refactor" | "implement")
        {
            let retrieval = self.retrieval();
            let guard = retrieval.lock();
            let impact = if let Some(file) = current_top_file_for_addons.as_deref() {
                ImpactAnalyzer::analyze_in_file(
                    guard.storage_ref(),
                    &current_symbol_for_addons,
                    file,
                )
            } else {
                ImpactAnalyzer::analyze(guard.storage_ref(), &current_symbol_for_addons)
            };
            match impact {
                Ok(report) => json!({
                    "anchor_symbol": report.changed_symbol,
                    "anchor_file": current_top_file_for_addons,
                    "impacted_files": report.impacted_files.iter().take(10).collect::<Vec<_>>(),
                    "impacted_symbols": report.impacted_symbols.iter().take(15).collect::<Vec<_>>(),
                    "has_test_impact": !report.impacted_tests.is_empty()
                }),
                Err(_) => serde_json::Value::Null,
            }
        } else {
            serde_json::Value::Null
        };

        let test_coverage_suppressed =
            if matches!(intent, "understand" | "debug") && !current_symbol_for_addons.is_empty() {
                let retrieval = self.retrieval();
                let guard = retrieval.lock();
                let symbol_has_gap = test_coverage::TestCoverageAnalyzer::has_gap_for_symbol(
                    guard.storage_ref(),
                    &current_symbol_for_addons,
                )
                .unwrap_or(true);
                drop(guard);
                if !symbol_has_gap {
                    if let Some(ctx) = result.get_mut("context").and_then(|v| v.as_array_mut()) {
                        ctx.retain(|item| {
                            let file = item
                                .get("file")
                                .and_then(|v| v.as_str())
                                .unwrap_or_default()
                                .to_lowercase();
                            !(file.contains("test")
                                || file.contains("spec")
                                || file.contains("__tests__")
                                || file.contains("_test."))
                        });
                    }
                    true
                } else {
                    false
                }
            } else {
                false
            };

        let repo_name = self
            .repo_root()
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        let knowledge_hints = match self.knowledge_graph().lock().list() {
            Ok(entries) => json!(entries
                .iter()
                .rev()
                .filter(|e| e.repository.is_empty()
                    || e.repository == "*"
                    || e.repository == repo_name)
                .take(5)
                .map(|e| json!({"category": e.category, "title": e.title, "details": e.details}))
                .collect::<Vec<_>>()),
            Err(_) => json!([]),
        };
        let (
            exact_target_in_top_context,
            exact_target_span_in_top_context,
            exact_dependencies_in_reported_files,
            exact_impact_scope_alignment,
            exact_impact_scope_graph_alignment,
            exact_impact_scope_target_anchor,
            exact_impact_scope_graph_complete,
            impact_scope_graph_details,
        ) = self.compute_exact_verification_signals(
            &result,
            &impact_scope,
            matches!(intent, "implement" | "refactor"),
        );
        let workspace_boundary_alignment = verify_workspace_boundary_alignment(&result);
        let mut verification = build_verification_summary(
            intent,
            &result,
            selected_tool,
            context_refs_count,
            confidence_score,
            reference_only,
            escalation_applied,
            minimal_raw_seed_added,
            low_confidence_raw_context_added,
            exact_target_in_top_context,
            exact_target_span_in_top_context,
            exact_dependencies_in_reported_files,
            exact_impact_scope_alignment,
            exact_impact_scope_graph_alignment,
            exact_impact_scope_target_anchor,
            exact_impact_scope_graph_complete,
            workspace_boundary_alignment,
        );
        let include_impact_scope_graph_details =
            exact_impact_scope_graph_alignment == Some(false)
                || exact_impact_scope_graph_complete == Some(false);
        if include_impact_scope_graph_details {
            if let Some(details) = impact_scope_graph_details {
                if let Some(obj) = verification.as_object_mut() {
                    obj.insert("impact_scope_graph_details".to_string(), details);
                }
            }
        }
        if let Some(obj) = verification.as_object_mut() {
            obj.insert("index_readiness".to_string(), json!(route_index_readiness));
            obj.insert(
                "index_recovery_mode".to_string(),
                json!(index_recovery_mode(auto_index_requested, index_coverage)),
            );
            obj.insert("index_coverage".to_string(), json!(index_coverage));
            if let Some(target) = index_coverage_target.clone() {
                obj.insert("index_coverage_target".to_string(), json!(target));
                if let Some(command) =
                    suggested_index_command(index_coverage, Some(target.as_str()))
                {
                    obj.insert("suggested_index_command".to_string(), json!(command));
                }
            }
            if index_coverage == "unindexed_target" {
                let mut issues = obj
                    .get("issues")
                    .and_then(|value| value.as_array())
                    .cloned()
                    .unwrap_or_default();
                let has_issue = issues
                    .iter()
                    .any(|item| item.as_str() == Some("target_path_not_indexed"));
                if !has_issue {
                    issues.push(json!("target_path_not_indexed"));
                    obj.insert("issues".to_string(), serde_json::Value::Array(issues));
                }
            }
        }
        self.maybe_recover_mutation_route(&mut verification, &mut result);
        trim_healthy_read_only_verification_payload(&mut verification, intent, reference_only);
        trim_healthy_mutation_verification_payload(&mut verification, intent, reference_only);
        trim_empty_code_fields(&mut result);
        trim_healthy_reference_only_mutation_payload(
            &mut result,
            intent,
            reference_only,
            &verification,
        );
        trim_healthy_reference_only_understand_payload(
            &mut result,
            intent,
            reference_only,
            &verification,
        );
        trim_healthy_reference_only_implement_payload(
            &mut result,
            intent,
            reference_only,
            &verification,
        );
        trim_healthy_reference_only_debug_payload(
            &mut result,
            intent,
            reference_only,
            &verification,
        );
        if let Some(obj) = result.as_object_mut() {
            obj.insert(
                "raw_expansion_mode".to_string(),
                json!(raw_expansion_mode.label()),
            );
            obj.insert(
                "already_opened_refs".to_string(),
                json!(raw_outcome.already_opened_hits),
            );
            obj.insert(
                "raw_budget_exhausted".to_string(),
                json!(raw_outcome.budget_exhausted),
            );
            obj.insert(
                "content_kind".to_string(),
                json!(route_content_kind(
                    current_top_file_for_addons.as_deref(),
                    &task,
                    intent
                )),
            );
            obj.insert("index_readiness".to_string(), json!(route_index_readiness));
            obj.insert(
                "index_recovery_mode".to_string(),
                json!(index_recovery_mode(auto_index_requested, index_coverage)),
            );
            obj.insert("index_coverage".to_string(), json!(index_coverage));
            if let Some(target) = index_coverage_target.clone() {
                obj.insert("index_coverage_target".to_string(), json!(target));
                if let Some(command) =
                    suggested_index_command(index_coverage, Some(target.as_str()))
                {
                    obj.insert("suggested_index_command".to_string(), json!(command));
                }
            }
        }

        let mut response = json!({
            "ok": true,
            "session_id": effective_session_id,
            "intent": intent,
            "selected_tool": selected_tool,
            "max_tokens": max_tokens,
            "single_file_fast_path": single_file_fast_path,
            "reference_only": reference_only,
            "mapping_mode": mapping_mode.unwrap_or_else(|| "footprint_first".to_string()),
            "reuse_session_context": reuse_session_context,
            "reused_context_count": reused_context_count,
            "already_opened_refs": raw_outcome.already_opened_hits,
            "raw_expansion_mode": raw_expansion_mode.label(),
            "raw_budget_exhausted": raw_outcome.budget_exhausted,
            "refs_unchanged": refs_unchanged,
            "context_delta": context_delta_value,
            "context_delta_mode": context_delta_mode,
            "recommended_provider": recommended_provider,
            "recommended_endpoint": recommended_endpoint,
            "impact_scope": impact_scope,
            "knowledge_hints": knowledge_hints,
            "test_coverage_suppressed": test_coverage_suppressed,
            "verification": verification,
            "result": result,
            "debug_candidates": debug_candidates,
            "project_summary": if let Some(summary) = auto_summary {
                Some(summary)
            } else if include_summary {
                self.explicit_summary_for_intent(
                    intent,
                    &current_symbol_for_addons,
                    current_top_file_for_addons.as_deref(),
                    &task,
                )
            } else {
                None
            }
        });
        trim_healthy_read_only_route_response(&mut response, intent, reference_only, &verification);
        trim_healthy_mutation_route_response(&mut response, intent, reference_only, &verification);
        prune_empty_route_response_fields(&mut response);
        response
    }

fn build_auto_summary(
        &self,
        session: &mut crate::session::SessionContextState,
        current_symbol: &str,
        intent: &str,
    ) -> Option<serde_json::Value> {
        let include_error_hints = intent == "debug";
        let tier = match intent {
            "debug" | "refactor" => project_summariser::SummaryTier::Full,
            _ => project_summariser::SummaryTier::Standard,
        };
        if !session.last_summary_file_set.is_empty() {
            let current_files: HashSet<String> = self
                .retrieval()
                .lock()
                .with_storage(|storage| storage.list_files())
                .unwrap_or_default()
                .into_iter()
                .collect();
            let new_files: Vec<String> = current_files
                .difference(&session.last_summary_file_set)
                .take(10)
                .cloned()
                .collect();
            let removed_files: Vec<String> = session
                .last_summary_file_set
                .difference(&current_files)
                .take(10)
                .cloned()
                .collect();
            let diff_size = new_files.len() + removed_files.len();
            if diff_size == 0 {
                session.last_summary_file_set = current_files;
                session.summary_delivered = true;
                return None;
            }
            if diff_size <= 5 {
                let new_outlines: Vec<_> = new_files
                    .iter()
                    .filter_map(|file| {
                        self.retrieval()
                            .lock()
                            .handle(RetrievalRequest {
                                operation: Operation::GetFileOutline,
                                file: Some(file.clone()),
                                ..Default::default()
                            })
                            .ok()
                            .map(|response| response.result)
                    })
                    .collect();
                session.last_summary_file_set = current_files;
                session.summary_delivered = true;
                return Some(json!({
                    "type": "index_refresh_delta",
                    "new_files": new_outlines,
                    "removed_files": removed_files,
                    "auto_injected": true,
                    "summary_tier": "nano"
                }));
            }
            let summary = self.summary_for_symbol(current_symbol, tier, include_error_hints, true);
            if summary.is_some() {
                session.last_summary_file_set = current_files;
                session.summary_delivered = true;
            }
            return summary;
        }

        let summary = self.summary_for_symbol(current_symbol, tier, include_error_hints, true);
        if summary.is_some() {
            session.last_summary_file_set = self
                .retrieval()
                .lock()
                .with_storage(|storage| storage.list_files())
                .unwrap_or_default()
                .into_iter()
                .collect();
            session.summary_delivered = true;
        }
        summary
    }

    fn maybe_recover_mutation_route(
        &self,
        verification: &mut serde_json::Value,
        _result: &mut serde_json::Value,
    ) {
        let Some(verification_obj) = verification.as_object_mut() else {
            return;
        };
        if verification_obj
            .get("mutation_state")
            .and_then(|v| v.as_str())
            != Some("blocked")
        {
            return;
        }
        let target_symbol = verification_obj
            .get("target_symbol")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        if target_symbol.is_empty() {
            verification_obj.insert(
                "mutation_retry".to_string(),
                json!({
                    "recovered": false,
                    "strategy": "none",
                    "reason": "missing_target_symbol"
                }),
            );
            return;
        }
        let top_context_file = verification_obj
            .get("top_context_file")
            .and_then(|v| v.as_str())
            .map(|value| value.to_string());

        if let Some(file) = top_context_file.clone() {
            if let Some(outline) = self.fetch_outline_if_contains_symbol(&file, &target_symbol) {
                verification_obj.insert(
                    "mutation_retry".to_string(),
                    json!({
                        "recovered": true,
                        "strategy": "top_file_outline",
                        "target_symbol": target_symbol,
                        "file": file,
                    }),
                );
                verification_obj.insert("mutation_recovered_by_retry".to_string(), json!(true));
                verification_obj.insert("mutation_state".to_string(), json!("ready"));
                verification_obj.insert("mutation_block_reason".to_string(), serde_json::Value::Null);
                verification_obj.insert(
                    "mutation_bundle".to_string(),
                    json!({
                        "status": "retry_recovered",
                        "required_checks": [
                            "target_symbol_present",
                            "top_context_present",
                            "candidate_target_aligned",
                            "context_refs_present",
                            "exact_target_in_top_context",
                            "exact_target_span_in_top_context",
                            "exact_dependencies_in_reported_files",
                            "workspace_boundary_alignment"
                        ],
                        "passed_checks": [
                            "target_symbol_present",
                            "top_context_present",
                            "exact_target_in_top_context"
                        ],
                        "failed_checks": [],
                        "missing_checks": [],
                        "ready_without_retry": false
                    }),
                );
                verification_obj.insert(
                    "recommended_action".to_string(),
                    json!("safe to proceed after exact mutation retry"),
                );
                let _ = outline;
                return;
            }
        }

        let preferred_boundary = top_context_file
            .as_deref()
            .and_then(workspace_boundary_prefix);
        let Some((file, outline, ambiguous)) =
            self.search_exact_symbol_outline(&target_symbol, preferred_boundary.as_deref())
        else {
            verification_obj.insert(
                "mutation_retry".to_string(),
                json!({
                    "recovered": false,
                    "strategy": "exact_symbol_search",
                    "reason": "no_exact_symbol_match"
                }),
            );
            return;
        };
        if ambiguous {
            verification_obj.insert(
                "mutation_retry".to_string(),
                json!({
                    "recovered": false,
                    "strategy": "exact_symbol_search",
                    "reason": "ambiguous_exact_symbol_match"
                }),
            );
            return;
        }

        verification_obj.insert(
            "mutation_retry".to_string(),
            json!({
                "recovered": true,
                "strategy": "exact_symbol_search",
                "target_symbol": target_symbol,
                "file": file,
            }),
        );
        verification_obj.insert("mutation_recovered_by_retry".to_string(), json!(true));
        verification_obj.insert("mutation_state".to_string(), json!("ready"));
        verification_obj.insert("mutation_block_reason".to_string(), serde_json::Value::Null);
        verification_obj.insert(
            "mutation_bundle".to_string(),
            json!({
                "status": "retry_recovered",
                "required_checks": [
                    "target_symbol_present",
                    "top_context_present",
                    "candidate_target_aligned",
                    "context_refs_present",
                    "exact_target_in_top_context",
                    "exact_target_span_in_top_context",
                    "exact_dependencies_in_reported_files",
                    "workspace_boundary_alignment"
                ],
                "passed_checks": [
                    "target_symbol_present",
                    "top_context_present",
                    "exact_target_in_top_context",
                    "exact_target_span_in_top_context"
                ],
                "failed_checks": [],
                "missing_checks": [],
                "ready_without_retry": false
            }),
        );
        verification_obj.insert(
            "recommended_action".to_string(),
            json!("safe to proceed after exact mutation retry"),
        );
        verification_obj.insert("top_context_file".to_string(), json!(file));
        verification_obj.insert("top_context_present".to_string(), json!(true));
        verification_obj.insert("exact_target_in_top_context".to_string(), json!(true));
        verification_obj.insert("exact_target_span_in_top_context".to_string(), json!(true));
        let _ = outline;
    }

    fn fetch_outline_if_contains_symbol(
        &self,
        file: &str,
        target_symbol: &str,
    ) -> Option<serde_json::Value> {
        let response = self
            .retrieval()
            .lock()
            .handle(RetrievalRequest {
                operation: Operation::GetFileOutline,
                file: Some(file.to_string()),
                ..Default::default()
            })
            .ok()?;
        if file_outline_contains_symbol(&response.result, target_symbol) {
            Some(response.result)
        } else {
            None
        }
    }

    fn search_exact_symbol_outline(
        &self,
        target_symbol: &str,
        preferred_boundary: Option<&str>,
    ) -> Option<(String, serde_json::Value, bool)> {
        let response = self
            .retrieval()
            .lock()
            .handle(RetrievalRequest {
                operation: Operation::SearchSymbol,
                name: Some(target_symbol.to_string()),
                limit: Some(8),
                ..Default::default()
            })
            .ok()?;
        let mut files = collect_exact_symbol_files(&response.result, target_symbol);
        if let Some(boundary) = preferred_boundary {
            let filtered: Vec<String> = files
                .iter()
                .filter(|file| workspace_boundary_prefix(file) == Some(boundary.to_string()))
                .cloned()
                .collect();
            if !filtered.is_empty() {
                files = filtered;
            }
        }
        files.sort();
        files.dedup();
        let ambiguous = files.len() > 1;
        let file = files.first()?.clone();
        let outline = self.fetch_outline_if_contains_symbol(&file, target_symbol)?;
        Some((file, outline, ambiguous))
    }

    fn summary_for_symbol(
        &self,
        symbol: &str,
        tier: project_summariser::SummaryTier,
        include_error_hints: bool,
        auto_injected: bool,
    ) -> Option<serde_json::Value> {
        self.retrieval().lock().with_storage(|storage| {
            let summariser = project_summariser::ProjectSummariser::new(storage);
            if symbol.is_empty() {
                summariser
                    .build_tiered(tier, include_error_hints)
                    .ok()
                    .map(|doc| {
                        json!({
                            "summary_text": doc.summary_text,
                            "token_estimate": doc.token_estimate,
                            "auto_injected": auto_injected,
                            "summary_tier": format!("{tier:?}").to_lowercase(),
                        })
                    })
            } else {
                summariser
                    .build_with_symbol_filter(tier, symbol, include_error_hints)
                    .ok()
                    .map(|(doc, filtered)| {
                        json!({
                            "summary_text": doc.summary_text,
                            "token_estimate": doc.token_estimate,
                            "auto_injected": auto_injected,
                            "summary_tier": format!("{tier:?}").to_lowercase(),
                            "summary_scope": if filtered { "symbol_filtered" } else { "full" },
                        })
                    })
            }
        })
    }

    fn explicit_summary_for_intent(
        &self,
        intent: &str,
        current_symbol: &str,
        top_context_file: Option<&str>,
        task: &str,
    ) -> Option<serde_json::Value> {
        if current_symbol.is_empty() {
            if let Some(file) = top_context_file {
                if is_document_path(file) {
                    if let Ok(response) = self.retrieval().lock().handle(RetrievalRequest {
                        operation: Operation::GetSectionBrief,
                        file: Some(file.to_string()),
                        ..Default::default()
                    }) {
                        return Some(response.result);
                    }
                }
                if let Ok(response) = self.retrieval().lock().handle(RetrievalRequest {
                    operation: Operation::GetFileBrief,
                    file: Some(file.to_string()),
                    ..Default::default()
                }) {
                    return Some(response.result);
                }
            }
            if let Some(dir_hint) = extract_directory_hint(task) {
                if let Ok(response) = self.retrieval().lock().handle(RetrievalRequest {
                    operation: Operation::GetDirectoryBrief,
                    path: Some(dir_hint),
                    ..Default::default()
                }) {
                    return Some(response.result);
                }
            }
        }
        if matches!(intent, "understand" | "debug") && !current_symbol.is_empty() {
            if let Some(summary) = self.build_symbol_micro_summary(
                current_symbol,
                top_context_file,
                if intent == "debug" {
                    Some("Investigate")
                } else {
                    None
                },
                if intent == "debug" {
                    "symbol_micro_debug"
                } else {
                    "symbol_micro"
                },
            ) {
                return Some(summary);
            }
        }
        let tier = match intent {
            "debug" | "refactor" => project_summariser::SummaryTier::Full,
            _ => project_summariser::SummaryTier::Standard,
        };
        if intent == "understand" && !current_symbol.is_empty() {
            self.summary_for_symbol(current_symbol, tier, false, false)
        } else {
            self.summary_for_symbol("", tier, intent == "debug", false)
        }
    }

    fn build_symbol_micro_summary(
        &self,
        current_symbol: &str,
        top_context_file: Option<&str>,
        prefix: Option<&str>,
        scope: &str,
    ) -> Option<serde_json::Value> {
        let file = top_context_file?;
        let summary_text = self.retrieval().lock().with_storage(|storage| {
            let outline = storage.file_outline(file).ok()?;
            let target = outline
                .iter()
                .find(|symbol| symbol.name.eq_ignore_ascii_case(current_symbol))?;
            let sibling_symbols = outline
                .iter()
                .filter(|symbol| !symbol.name.eq_ignore_ascii_case(current_symbol))
                .take(3)
                .map(|symbol| symbol.name.clone())
                .collect::<Vec<_>>();
            let sibling_text = if sibling_symbols.is_empty() {
                String::new()
            } else {
                format!(" related: {}.", sibling_symbols.join(", "))
            };
            let kind = match target.symbol_type {
                engine::SymbolType::Function => "fn",
                engine::SymbolType::Class => "class",
                engine::SymbolType::Import => "import",
            };
            let prefix = prefix
                .filter(|value| !value.is_empty())
                .map(|value| format!("{value}: "))
                .unwrap_or_default();
            Some(format!(
                "{}`{}` @ `{}` ({} {}-{}).{}",
                prefix,
                target.name,
                file,
                kind,
                target.start_line,
                target.end_line,
                sibling_text
            ))
        })?;
        Some(json!({
            "summary_text": summary_text,
            "summary_scope": scope
        }))
    }

    fn build_minimal_raw_seed(
        &self,
        planned_result: &serde_json::Value,
    ) -> Option<serde_json::Value> {
        let first = planned_result.get("context")?.as_array()?.first()?;
        let file = first.get("file")?.as_str()?.to_string();
        let start = first.get("start")?.as_u64()? as u32;
        let end = first.get("end")?.as_u64()? as u32;
        let clipped_end = start.saturating_add(40).min(end);
        let response = self
            .retrieval()
            .lock()
            .handle(RetrievalRequest {
                operation: Operation::GetCodeSpan,
                file: Some(file.clone()),
                start_line: Some(start),
                end_line: Some(clipped_end),
                ..Default::default()
            })
            .ok()?;
        Some(json!({
            "code_span": response.result
        }))
    }

    fn build_low_confidence_raw_context(
        &self,
        planned_result: &serde_json::Value,
        max_items: usize,
    ) -> Option<serde_json::Value> {
        let ctx = planned_result.get("context")?.as_array()?;
        let mut out = Vec::new();
        for item in ctx.iter().take(max_items) {
            let file = item.get("file")?.as_str()?.to_string();
            let start = item.get("start")?.as_u64()? as u32;
            let end = item.get("end")?.as_u64()? as u32;
            let clipped_end = start.saturating_add(40).min(end);
            let response = self
                .retrieval()
                .lock()
                .handle(RetrievalRequest {
                    operation: Operation::GetCodeSpan,
                    file: Some(file.clone()),
                    start_line: Some(start),
                    end_line: Some(clipped_end),
                    ..Default::default()
                })
                .ok()?;
            out.push(json!({
                "code_span": response.result
            }));
        }
        Some(json!(out))
    }

    fn inline_small_spans(&self, result: &mut serde_json::Value) {
        let mut inlined = 0usize;
        if let Some(context) = result.get_mut("context").and_then(|v| v.as_array_mut()) {
            for item in context.iter_mut() {
                if inlined >= 5 {
                    break;
                }
                let file = item
                    .get("file")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let start = item.get("start").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                let end = item.get("end").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                if file.is_empty()
                    || start == 0
                    || end < start
                    || (end - start) > 25
                    || item.get("code_span").is_some()
                {
                    continue;
                }
                if let Ok(response) = self.retrieval().lock().handle(RetrievalRequest {
                    operation: Operation::GetCodeSpan,
                    file: Some(file),
                    start_line: Some(start),
                    end_line: Some(end),
                    ..Default::default()
                }) {
                    if let Some(code) = response.result.get("code").and_then(|v| v.as_str()) {
                        if let Some(obj) = item.as_object_mut() {
                            obj.insert("code_span".to_string(), json!(code));
                            inlined += 1;
                        }
                    }
                }
            }
        }
    }

    fn compute_exact_verification_signals(
        &self,
        result: &serde_json::Value,
        impact_scope: &serde_json::Value,
        include_mutation_scope_checks: bool,
    ) -> (
        Option<bool>,
        Option<bool>,
        Option<bool>,
        Option<bool>,
        Option<bool>,
        Option<bool>,
        Option<bool>,
        Option<serde_json::Value>,
    ) {
        let Some(target_symbol) = extract_result_symbol_name(result) else {
            return (None, None, None, None, None, None, None, None);
        };
        if target_symbol.is_empty() {
            return (None, None, None, None, None, None, None, None);
        }
        let top_context_file = result
            .get("context")
            .and_then(|v| v.as_array())
            .and_then(|items| items.first())
            .and_then(|item| item.get("file"))
            .and_then(|v| v.as_str())
            .or_else(|| {
                result
                    .get("ranked_context")
                    .and_then(|v| v.as_array())
                    .and_then(|items| items.first())
                    .and_then(|item| item.get("file"))
                    .and_then(|v| v.as_str())
            })
            .or_else(|| {
                result
                    .get("dependency_spans")
                    .and_then(|v| v.as_array())
                    .and_then(|items| items.first())
                    .and_then(|item| item.get("symbol"))
                    .and_then(|v| v.get("file"))
                    .and_then(|v| v.as_str())
            });
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
        let top_file = top_item
            .and_then(|item| item.get("file"))
            .and_then(|v| v.as_str());
        let top_start = top_item
            .and_then(|item| item.get("start"))
            .and_then(|v| v.as_u64())
            .map(|value| value as u32);
        let top_end = top_item
            .and_then(|item| item.get("end"))
            .and_then(|v| v.as_u64())
            .map(|value| value as u32);
        let dependency_spans = result.get("dependency_spans").and_then(|v| v.as_array());
        self.retrieval().lock().with_storage(|storage| {
            let mut outline_cache: std::collections::HashMap<String, Vec<engine::SymbolRecord>> =
                std::collections::HashMap::new();
            let graph_details =
                top_context_file.and_then(|file| impact_scope_graph_details_for_target(
                    storage,
                    &target_symbol,
                    file,
                    impact_scope,
                ));
            let mut get_outline = |file: &str| -> Option<Vec<engine::SymbolRecord>> {
                if !outline_cache.contains_key(file) {
                    let symbols = storage.file_outline(file).ok()?;
                    outline_cache.insert(file.to_string(), symbols);
                }
                outline_cache.get(file).cloned()
            };
            let (
                exact_impact_scope_alignment,
                exact_impact_scope_graph_alignment,
                exact_impact_scope_target_anchor,
                exact_impact_scope_graph_complete,
                graph_details,
            ) = if include_mutation_scope_checks {
                (
                    verify_impact_scope_alignment(impact_scope, top_context_file, &mut get_outline),
                    graph_details.as_ref().and_then(impact_scope_graph_alignment_from_details),
                    verify_impact_scope_target_anchor(impact_scope, &target_symbol, top_context_file),
                    graph_details
                        .as_ref()
                        .and_then(impact_scope_graph_completeness_from_details),
                    graph_details,
                )
            } else {
                (None, None, None, None, None)
            };

            let result_with_dependencies = |
                exact_target_in_top_context: Option<bool>,
                exact_target_span_in_top_context: Option<bool>,
                exact_dependencies_in_reported_files: Option<bool>,
            | {
                (
                    exact_target_in_top_context,
                    exact_target_span_in_top_context,
                    exact_dependencies_in_reported_files,
                    exact_impact_scope_alignment,
                    exact_impact_scope_graph_alignment,
                    exact_impact_scope_target_anchor,
                    exact_impact_scope_graph_complete,
                    graph_details.clone(),
                )
            };

            let exact_target_in_top_context = top_context_file.and_then(|file| {
                let outline = get_outline(file)?;
                Some(
                    outline
                        .iter()
                        .any(|symbol| symbol.name.eq_ignore_ascii_case(&target_symbol)),
                )
            });

            let exact_target_span_in_top_context =
                top_file.and_then(|file| match (top_start, top_end) {
                    (Some(start), Some(end)) => {
                        let outline = get_outline(file)?;
                        outline
                            .iter()
                            .find(|symbol| symbol.name.eq_ignore_ascii_case(&target_symbol))
                            .map(|symbol| {
                                spans_overlap(start, end, symbol.start_line, symbol.end_line)
                            })
                    }
                    _ => None,
                });

            let mut saw_dependency = false;
            let mut exact_dependencies_in_reported_files = None;
            if let Some(items) = dependency_spans {
                for item in items {
                    let Some(file) = item.get("file").and_then(|v| v.as_str()) else {
                        return result_with_dependencies(
                            exact_target_in_top_context,
                            exact_target_span_in_top_context,
                            None,
                        );
                    };
                    let Some(name) = item.get("name").and_then(|v| v.as_str()) else {
                        return result_with_dependencies(
                            exact_target_in_top_context,
                            exact_target_span_in_top_context,
                            None,
                        );
                    };
                    saw_dependency = true;
                    let Some(outline) = get_outline(file) else {
                        return result_with_dependencies(
                            exact_target_in_top_context,
                            exact_target_span_in_top_context,
                            None,
                        );
                    };
                    let matches = outline
                        .iter()
                        .any(|candidate| candidate.name.eq_ignore_ascii_case(name));
                    if !matches {
                        exact_dependencies_in_reported_files = Some(false);
                        break;
                    }
                }
            }
            if saw_dependency && exact_dependencies_in_reported_files.is_none() {
                exact_dependencies_in_reported_files = Some(true);
            }
            result_with_dependencies(
                exact_target_in_top_context,
                exact_target_span_in_top_context,
                exact_dependencies_in_reported_files,
            )
        })
    }
}

fn compute_route_index_coverage(
    indexed_files: &[String],
    file_hint: Option<&str>,
    path_hint: Option<&str>,
) -> (&'static str, Option<String>) {
    if indexed_files.is_empty() {
        if let Some(target) = path_hint.or(file_hint) {
            return ("unindexed_repo", Some(normalize_route_coverage_path(target)));
        }
        return ("unindexed_repo", None);
    }
    if let Some(path) = path_hint {
        let normalized = normalize_route_coverage_path(path);
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
    if let Some(file) = file_hint {
        let normalized = normalize_route_coverage_path(file);
        let indexed = indexed_files.iter().any(|item| item == &normalized);
        return (
            if indexed { "indexed_target" } else { "unindexed_target" },
            Some(normalized),
        );
    }
    ("indexed_repo", None)
}

fn normalize_route_coverage_path(path: &str) -> String {
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

fn route_coverage_resolved(value: &serde_json::Value, target: &str) -> bool {
    let verification = match value.get("verification") {
        Some(verification) => verification,
        None => return false,
    };
    let coverage = verification.get("index_coverage").and_then(|v| v.as_str());
    let current_target = verification
        .get("index_coverage_target")
        .and_then(|v| v.as_str());
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

fn index_recovery_mode(auto_index_requested: bool, coverage: &str) -> &'static str {
    if auto_index_requested && coverage == "unindexed_target" {
        "auto_index_attempted_no_change"
    } else if coverage == "unindexed_target" {
        "suggest_only"
    } else {
        "none"
    }
}

fn route_content_kind(top_context_file: Option<&str>, task: &str, intent: &str) -> &'static str {
    if let Some(file) = top_context_file {
        if is_document_path(file) {
            return "document";
        }
        return "code";
    }
    let lower = task.to_ascii_lowercase();
    if matches!(intent, "understand" | "debug")
        && ["readme", "rules", "skills", "docs", ".md", ".txt"]
            .iter()
            .any(|needle| lower.contains(needle))
    {
        "document"
    } else {
        "code"
    }
}

fn is_document_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.ends_with(".md")
        || lower.ends_with(".txt")
        || lower.ends_with(".rst")
        || lower.ends_with(".adoc")
}

fn extract_path_hint(task: &str) -> Option<String> {
    for token in task.split_whitespace() {
        let normalized =
            token.trim_matches(|ch: char| ch == '"' || ch == '\'' || ch == ',' || ch == '.');
        if normalized.contains('/') || normalized.contains('\\') {
            let path = normalized.replace('\\', "/").trim_matches('/').to_string();
            if path.is_empty() {
                continue;
            }
            return Some(path);
        }
    }
    None
}

fn extract_directory_hint(task: &str) -> Option<String> {
    let path = extract_path_hint(task)?;
    if !looks_like_file_path(&path) {
        return Some(path);
    }
    path.rsplit_once('/')
        .map(|(dir, _)| dir.to_string())
        .filter(|dir| !dir.is_empty())
}

fn trim_empty_code_fields(result: &mut serde_json::Value) {
    for key in ["context", "ranked_context"] {
        let Some(items) = result.get_mut(key).and_then(|value| value.as_array_mut()) else {
            continue;
        };
        for item in items {
            let Some(obj) = item.as_object_mut() else {
                continue;
            };
            let remove_code = obj
                .get("code")
                .and_then(|value| value.as_str())
                .map(|code| code.trim().is_empty())
                .unwrap_or(false);
            if remove_code {
                obj.remove("code");
            }
        }
    }
}

fn trim_healthy_reference_only_mutation_payload(
    result: &mut serde_json::Value,
    intent: &str,
    reference_only: bool,
    verification: &serde_json::Value,
) {
    if !reference_only || !matches!(intent, "implement" | "refactor") {
        return;
    }
    let exact_ready = verification
        .get("mutation_bundle")
        .and_then(|value| value.get("status"))
        .and_then(|value| value.as_str())
        == Some("exact_ready");
    if !exact_ready {
        return;
    }
    let Some(obj) = result.as_object_mut() else {
        return;
    };
    obj.remove("confidence_score");
    obj.remove("query");
    obj.remove("strategy");
    obj.remove("graph_details_available");
    obj.remove("control_flow_hints");
    obj.remove("data_flow_hints");
    obj.remove("logic_clusters");
}

fn trim_healthy_reference_only_understand_payload(
    result: &mut serde_json::Value,
    intent: &str,
    _reference_only: bool,
    verification: &serde_json::Value,
) {
    if intent != "understand" {
        return;
    }
    let has_issues = verification
        .get("issues")
        .and_then(|value| value.as_array())
        .map(|issues| !issues.is_empty())
        .unwrap_or(false);
    if has_issues {
        return;
    }
    let has_context = result
        .get("context")
        .and_then(|value| value.as_array())
        .map(|items| !items.is_empty())
        .unwrap_or(false);
    if !has_context {
        return;
    }
    let Some(obj) = result.as_object_mut() else {
        return;
    };
    obj.remove("confidence_score");
    obj.remove("candidate_files");
    obj.remove("candidate_symbols");
    obj.remove("effective_breadth");
    obj.remove("include_raw_code");
    obj.remove("context_phase");
    obj.remove("mapping_mode");
    obj.remove("small_repo_mode");
    obj.remove("intent");
    obj.remove("retrieval_strategy");
    obj.remove("single_file_fast_path");
    obj.remove("plan");
    if obj
        .get("cache")
        .and_then(|value| value.get("hit"))
        .and_then(|value| value.as_bool())
        == Some(false)
    {
        obj.remove("cache");
    }
}

fn trim_healthy_reference_only_debug_payload(
    result: &mut serde_json::Value,
    intent: &str,
    reference_only: bool,
    verification: &serde_json::Value,
) {
    if !reference_only || intent != "debug" {
        return;
    }
    let has_issues = verification
        .get("issues")
        .and_then(|value| value.as_array())
        .map(|issues| !issues.is_empty())
        .unwrap_or(false);
    if has_issues {
        return;
    }
    let top_context_has_code_span = result
        .get("context")
        .and_then(|value| value.as_array())
        .and_then(|items| items.first())
        .and_then(|item| item.get("code_span"))
        .and_then(|value| value.as_str())
        .map(|code| !code.trim().is_empty())
        .unwrap_or(false);
    let Some(obj) = result.as_object_mut() else {
        return;
    };
    obj.remove("confidence_score");
    obj.remove("candidate_files");
    obj.remove("candidate_symbols");
    obj.remove("effective_breadth");
    obj.remove("include_raw_code");
    obj.remove("context_phase");
    obj.remove("mapping_mode");
    obj.remove("small_repo_mode");
    obj.remove("intent");
    obj.remove("retrieval_strategy");
    obj.remove("single_file_fast_path");
    obj.remove("plan");
    if obj
        .get("cache")
        .and_then(|value| value.get("hit"))
        .and_then(|value| value.as_bool())
        == Some(false)
    {
        obj.remove("cache");
    }
    if top_context_has_code_span {
        obj.remove("minimal_raw_seed");
    }
    let fallback_search_used = verification
        .get("fallback_search_used")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    if fallback_search_used {
        obj.remove("query");
        obj.remove("strategy");
    }
}

fn trim_healthy_read_only_verification_payload(
    verification: &mut serde_json::Value,
    intent: &str,
    reference_only: bool,
) {
    if !(intent == "understand" || (reference_only && intent == "debug")) {
        return;
    }
    let has_issues = verification
        .get("issues")
        .and_then(|value| value.as_array())
        .map(|issues| !issues.is_empty())
        .unwrap_or(false);
    if has_issues {
        return;
    }
    let Some(obj) = verification.as_object_mut() else {
        return;
    };
    if obj.get("mutation_state").and_then(|value| value.as_str()) != Some("not_applicable") {
        return;
    }
    if obj
        .get("issues")
        .and_then(|value| value.as_array())
        .map(|items| items.is_empty())
        .unwrap_or(false)
    {
        obj.remove("issues");
    }
    obj.remove("mutation_intent");
    obj.remove("mutation_bundle");
    obj.remove("mutation_ready_without_retry");
    obj.remove("mutation_block_reason");
    obj.remove("fallback_search_used");
    obj.remove("escalation_applied");
    obj.remove("minimal_raw_seed_added");
    obj.remove("low_confidence_raw_context_added");
    obj.remove("exact_dependencies_in_reported_files");
    obj.remove("exact_impact_scope_alignment");
    obj.remove("exact_impact_scope_graph_alignment");
    obj.remove("exact_impact_scope_target_anchor");
    obj.remove("exact_impact_scope_graph_complete");
    obj.remove("retrieval_strategy");
    if obj
        .get("candidate_target_aligned")
        .and_then(|value| value.as_bool())
        == Some(true)
    {
        obj.remove("candidate_target_aligned");
    }
    if obj
        .get("target_symbol_present")
        .and_then(|value| value.as_bool())
        == Some(true)
    {
        obj.remove("target_symbol_present");
    }
    if obj
        .get("top_context_present")
        .and_then(|value| value.as_bool())
        == Some(true)
    {
        obj.remove("top_context_present");
    }
    if obj
        .get("exact_target_in_top_context")
        .and_then(|value| value.as_bool())
        == Some(true)
    {
        obj.remove("exact_target_in_top_context");
    }
    if obj
        .get("exact_target_span_in_top_context")
        .and_then(|value| value.as_bool())
        == Some(true)
    {
        obj.remove("exact_target_span_in_top_context");
    }
    if obj
        .get("workspace_boundary_alignment")
        .map(|value| value.is_null())
        .unwrap_or(false)
        || obj
            .get("workspace_boundary_alignment")
            .and_then(|value| value.as_bool())
            == Some(true)
    {
        obj.remove("workspace_boundary_alignment");
    }
    if obj
        .get("context_refs_count")
        .and_then(|value| value.as_u64())
        == Some(1)
    {
        obj.remove("context_refs_count");
    }
    obj.remove("selected_tool");
    obj.remove("reference_only");
}

fn trim_healthy_read_only_route_response(
    response: &mut serde_json::Value,
    intent: &str,
    reference_only: bool,
    verification: &serde_json::Value,
) {
    if !(intent == "understand" || (reference_only && intent == "debug")) {
        return;
    }
    let has_issues = verification
        .get("issues")
        .and_then(|value| value.as_array())
        .map(|issues| !issues.is_empty())
        .unwrap_or(false);
    if has_issues {
        return;
    }
    if verification
        .get("mutation_state")
        .and_then(|value| value.as_str())
        != Some("not_applicable")
    {
        return;
    }
    let Some(obj) = response.as_object_mut() else {
        return;
    };
    if obj
        .get("mapping_mode")
        .and_then(|value| value.as_str())
        == Some("footprint_first")
    {
        obj.remove("mapping_mode");
    }
    if obj
        .get("reuse_session_context")
        .and_then(|value| value.as_bool())
        == Some(true)
    {
        obj.remove("reuse_session_context");
    }
    if obj
        .get("reused_context_count")
        .and_then(|value| value.as_u64())
        == Some(0)
    {
        obj.remove("reused_context_count");
    }
    if obj
        .get("refs_unchanged")
        .and_then(|value| value.as_bool())
        == Some(false)
    {
        obj.remove("refs_unchanged");
    }
    if obj
        .get("test_coverage_suppressed")
        .and_then(|value| value.as_bool())
        == Some(false)
    {
        obj.remove("test_coverage_suppressed");
    }
    if obj
        .get("session_id")
        .and_then(|value| value.as_str())
        .map(|value| value.starts_with("auto-"))
        == Some(true)
    {
        obj.remove("session_id");
    }
    if obj
        .get("recommended_provider")
        .and_then(|value| value.as_str())
        == Some("primary")
    {
        obj.remove("recommended_provider");
    }
    if obj
        .get("recommended_endpoint")
        .and_then(|value| value.as_str())
        == Some("<PRIMARY_PROVIDER_BASE_URL>")
    {
        obj.remove("recommended_endpoint");
    }
    obj.remove("impact_scope");
}

fn trim_healthy_mutation_verification_payload(
    verification: &mut serde_json::Value,
    intent: &str,
    reference_only: bool,
) {
    if !reference_only || !matches!(intent, "implement" | "refactor") {
        return;
    }
    let exact_ready = verification
        .get("mutation_bundle")
        .and_then(|value| value.get("status"))
        .and_then(|value| value.as_str())
        == Some("exact_ready");
    if !exact_ready {
        return;
    }
    let Some(obj) = verification.as_object_mut() else {
        return;
    };
    if obj
        .get("exact_dependencies_in_reported_files")
        .map(|value| value.is_null())
        .unwrap_or(false)
    {
        obj.remove("exact_dependencies_in_reported_files");
    }
    if obj
        .get("workspace_boundary_alignment")
        .map(|value| value.is_null())
        .unwrap_or(false)
    {
        obj.remove("workspace_boundary_alignment");
    }
    if obj
        .get("mutation_block_reason")
        .map(|value| value.is_null())
        .unwrap_or(false)
    {
        obj.remove("mutation_block_reason");
    }
    if obj
        .get("issues")
        .and_then(|value| value.as_array())
        .map(|items| items.is_empty())
        .unwrap_or(false)
    {
        obj.remove("issues");
    }
    if obj
        .get("fallback_search_used")
        .and_then(|value| value.as_bool())
        == Some(false)
    {
        obj.remove("fallback_search_used");
    }
    if obj
        .get("escalation_applied")
        .and_then(|value| value.as_bool())
        == Some(false)
    {
        obj.remove("escalation_applied");
    }
    if obj
        .get("low_confidence_raw_context_added")
        .and_then(|value| value.as_bool())
        == Some(false)
    {
        obj.remove("low_confidence_raw_context_added");
    }
    if obj
        .get("candidate_target_aligned")
        .and_then(|value| value.as_bool())
        == Some(true)
    {
        obj.remove("candidate_target_aligned");
    }
    if obj
        .get("target_symbol_present")
        .and_then(|value| value.as_bool())
        == Some(true)
    {
        obj.remove("target_symbol_present");
    }
    if obj
        .get("top_context_present")
        .and_then(|value| value.as_bool())
        == Some(true)
    {
        obj.remove("top_context_present");
    }
    if obj
        .get("context_refs_count")
        .and_then(|value| value.as_u64())
        == Some(1)
    {
        obj.remove("context_refs_count");
    }
    if let Some(bundle) = obj.get_mut("mutation_bundle").and_then(|value| value.as_object_mut()) {
        if bundle
            .get("status")
            .and_then(|value| value.as_str())
            == Some("exact_ready")
        {
            bundle.remove("required_checks");
            bundle.remove("passed_checks");
            bundle.remove("failed_checks");
            bundle.remove("missing_checks");
        }
    }
    obj.remove("mutation_intent");
    obj.remove("selected_tool");
    obj.remove("reference_only");
    obj.remove("retrieval_strategy");
}

fn trim_healthy_mutation_route_response(
    response: &mut serde_json::Value,
    intent: &str,
    reference_only: bool,
    verification: &serde_json::Value,
) {
    if !reference_only || !matches!(intent, "implement" | "refactor") {
        return;
    }
    let exact_ready = verification
        .get("mutation_bundle")
        .and_then(|value| value.get("status"))
        .and_then(|value| value.as_str())
        == Some("exact_ready");
    if !exact_ready {
        return;
    }
    let Some(obj) = response.as_object_mut() else {
        return;
    };
    if obj
        .get("mapping_mode")
        .and_then(|value| value.as_str())
        == Some("footprint_first")
    {
        obj.remove("mapping_mode");
    }
    if obj
        .get("reuse_session_context")
        .and_then(|value| value.as_bool())
        == Some(true)
    {
        obj.remove("reuse_session_context");
    }
    if obj
        .get("reused_context_count")
        .and_then(|value| value.as_u64())
        == Some(0)
    {
        obj.remove("reused_context_count");
    }
    if obj
        .get("refs_unchanged")
        .and_then(|value| value.as_bool())
        == Some(false)
    {
        obj.remove("refs_unchanged");
    }
    if obj
        .get("test_coverage_suppressed")
        .and_then(|value| value.as_bool())
        == Some(false)
    {
        obj.remove("test_coverage_suppressed");
    }
    if obj
        .get("session_id")
        .and_then(|value| value.as_str())
        .map(|value| value.starts_with("auto-"))
        == Some(true)
    {
        obj.remove("session_id");
    }
    if obj
        .get("recommended_provider")
        .and_then(|value| value.as_str())
        == Some("primary")
    {
        obj.remove("recommended_provider");
    }
    if obj
        .get("recommended_endpoint")
        .and_then(|value| value.as_str())
        == Some("<PRIMARY_PROVIDER_BASE_URL>")
    {
        obj.remove("recommended_endpoint");
    }
}

fn trim_healthy_reference_only_implement_payload(
    result: &mut serde_json::Value,
    intent: &str,
    reference_only: bool,
    verification: &serde_json::Value,
) {
    if !reference_only || intent != "implement" {
        return;
    }
    let exact_ready = verification
        .get("mutation_bundle")
        .and_then(|value| value.get("status"))
        .and_then(|value| value.as_str())
        == Some("exact_ready");
    if !exact_ready {
        return;
    }
    let Some(obj) = result.as_object_mut() else {
        return;
    };
    obj.remove("confidence_score");
    obj.remove("candidate_files");
    obj.remove("candidate_symbols");
    obj.remove("effective_breadth");
    obj.remove("include_raw_code");
    obj.remove("context_phase");
    obj.remove("mapping_mode");
    obj.remove("small_repo_mode");
    obj.remove("intent");
    obj.remove("retrieval_strategy");
    obj.remove("single_file_fast_path");
    obj.remove("plan");
    if obj
        .get("cache")
        .and_then(|value| value.get("hit"))
        .and_then(|value| value.as_bool())
        == Some(false)
    {
        obj.remove("cache");
    }
}

fn prune_empty_route_response_fields(response: &mut serde_json::Value) {
    let Some(obj) = response.as_object_mut() else {
        return;
    };
    if obj
        .get("context_delta")
        .map(|value| value.is_null())
        .unwrap_or(false)
    {
        obj.remove("context_delta");
        obj.remove("context_delta_mode");
    }
    if obj
        .get("recommended_provider")
        .map(|value| value.is_null())
        .unwrap_or(false)
    {
        obj.remove("recommended_provider");
    }
    if obj
        .get("recommended_endpoint")
        .map(|value| value.is_null())
        .unwrap_or(false)
    {
        obj.remove("recommended_endpoint");
    }
    if obj
        .get("impact_scope")
        .map(|value| value.is_null())
        .unwrap_or(false)
    {
        obj.remove("impact_scope");
    } else if let Some(impact_scope) = obj.get_mut("impact_scope").and_then(|value| value.as_object_mut()) {
        if impact_scope
            .get("impacted_files")
            .and_then(|value| value.as_array())
            .map(|items| items.is_empty())
            .unwrap_or(false)
        {
            impact_scope.remove("impacted_files");
        }
        if impact_scope
            .get("impacted_symbols")
            .and_then(|value| value.as_array())
            .map(|items| items.is_empty())
            .unwrap_or(false)
        {
            impact_scope.remove("impacted_symbols");
        }
        if impact_scope
            .get("has_test_impact")
            .and_then(|value| value.as_bool())
            == Some(false)
        {
            impact_scope.remove("has_test_impact");
        }
    }
    if obj
        .get("knowledge_hints")
        .and_then(|value| value.as_array())
        .map(|items| items.is_empty())
        .unwrap_or(false)
    {
        obj.remove("knowledge_hints");
    }
    if obj
        .get("debug_candidates")
        .map(|value| value.is_null())
        .unwrap_or(false)
    {
        obj.remove("debug_candidates");
    }
}

fn build_verification_summary(
    intent: &str,
    result: &serde_json::Value,
    selected_tool: &str,
    context_refs_count: usize,
    confidence_score: f64,
    reference_only: bool,
    escalation_applied: bool,
    minimal_raw_seed_added: bool,
    low_confidence_raw_context_added: bool,
    exact_target_in_top_context: Option<bool>,
    exact_target_span_in_top_context: Option<bool>,
    exact_dependencies_in_reported_files: Option<bool>,
    exact_impact_scope_alignment: Option<bool>,
    exact_impact_scope_graph_alignment: Option<bool>,
    exact_impact_scope_target_anchor: Option<bool>,
    exact_impact_scope_graph_complete: Option<bool>,
    workspace_boundary_alignment: Option<bool>,
) -> serde_json::Value {
    let confidence_band = result
        .get("confidence_band")
        .and_then(|v| v.as_str())
        .map(|value| value.to_string())
        .unwrap_or_else(|| {
            if confidence_score >= 0.75 {
                "high".to_string()
            } else if confidence_score >= 0.50 {
                "medium".to_string()
            } else {
                "low".to_string()
            }
        });
    let target_symbol = extract_result_symbol_name(result).unwrap_or_default();
    let top_context_file = result
        .get("context")
        .and_then(|v| v.as_array())
        .and_then(|items| items.first())
        .and_then(|item| item.get("file"))
        .and_then(|v| v.as_str())
        .map(|value| value.to_string())
        .or_else(|| {
            result
                .get("ranked_context")
                .and_then(|v| v.as_array())
                .and_then(|items| items.first())
                .and_then(|item| item.get("file"))
                .and_then(|v| v.as_str())
                .map(|value| value.to_string())
        })
        .or_else(|| {
            result
                .get("dependency_spans")
                .and_then(|v| v.as_array())
                .and_then(|items| items.first())
                .and_then(|item| item.get("symbol"))
                .and_then(|v| v.get("file"))
                .and_then(|v| v.as_str())
                .map(|value| value.to_string())
        });
    let candidate_symbols: Vec<String> = result
        .get("candidate_symbols")
        .and_then(|v| v.as_array())
        .map(|items| {
            items.iter()
                .filter_map(|item| item.as_str().map(|value| value.to_string()))
                .collect()
        })
        .unwrap_or_default();
    let candidate_target_aligned = target_symbol.is_empty()
        || candidate_symbols.is_empty()
        || candidate_symbols
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(&target_symbol));
    let target_symbol_present = !target_symbol.is_empty();
    let top_context_present = top_context_file.is_some();
    let retrieval_strategy = result
        .get("retrieval_strategy")
        .and_then(|v| v.as_str())
        .or_else(|| result.get("strategy").and_then(|v| v.as_str()))
        .unwrap_or_default()
        .to_string();
    let fallback_search_used = selected_tool == "search_semantic_symbol";
    let mut issues = Vec::new();
    if !target_symbol_present && !fallback_search_used {
        issues.push("missing_target_symbol");
    }
    if !top_context_present && !fallback_search_used && context_refs_count > 0 {
        issues.push("missing_top_context_file");
    }
    if !candidate_target_aligned {
        issues.push("candidate_symbols_do_not_include_target");
    }
    if exact_target_in_top_context == Some(false) {
        issues.push("top_context_file_does_not_contain_target_symbol");
    }
    if exact_target_span_in_top_context == Some(false) {
        issues.push("top_context_span_does_not_overlap_target_symbol");
    }
    if exact_dependencies_in_reported_files == Some(false) {
        issues.push("dependency_file_does_not_contain_reported_symbol");
    }
    if exact_impact_scope_alignment == Some(false) {
        issues.push("impact_scope_misaligned");
    }
    if exact_impact_scope_graph_alignment == Some(false) {
        issues.push("impact_scope_graph_misaligned");
    }
    if exact_impact_scope_target_anchor == Some(false) {
        issues.push("impact_scope_not_anchored_to_target");
    }
    if exact_impact_scope_graph_complete == Some(false) {
        issues.push("impact_scope_graph_incomplete");
    }
    if workspace_boundary_alignment == Some(false) {
        issues.push("context_crosses_workspace_boundary");
    }
    if context_refs_count == 0 && !fallback_search_used {
        issues.push("no_context_refs");
    }
    let status = if fallback_search_used {
        "fallback_search"
    } else if issues.iter().any(|issue| *issue == "no_context_refs") {
        "no_context_refs"
    } else if !issues.is_empty() {
        "needs_review"
    } else if low_confidence_raw_context_added || confidence_score < 0.50 {
        "low_confidence"
    } else if escalation_applied || minimal_raw_seed_added || confidence_score < 0.75 {
        "needs_review"
    } else {
        "high_confidence"
    };
    let recommended_action = match status {
        "fallback_search" => "review fallback hits before editing",
        "no_context_refs" => "request exact symbol or file span before editing",
        "low_confidence" => "inspect low_confidence_raw_context",
        "needs_review" => "review returned spans",
        _ => "safe to proceed with semantic context",
    };
    let recommended_cli_follow_up =
        recommended_cli_follow_up(status, &target_symbol, top_context_file.as_deref());
    let mutation_intent = matches!(intent, "implement" | "refactor");
    let mutation_bundle = build_mutation_verification_bundle(
        mutation_intent,
        fallback_search_used,
        target_symbol_present,
        top_context_present,
        candidate_target_aligned,
        context_refs_count,
        exact_target_in_top_context,
        exact_target_span_in_top_context,
        exact_dependencies_in_reported_files,
        exact_impact_scope_alignment,
        exact_impact_scope_graph_alignment,
        exact_impact_scope_target_anchor,
        exact_impact_scope_graph_complete,
        workspace_boundary_alignment,
    );
    let mutation_ready_without_retry = mutation_bundle
        .get("status")
        .and_then(|v| v.as_str())
        == Some("exact_ready");
    let mutation_state = if !mutation_intent {
        "not_applicable"
    } else if status == "high_confidence" || mutation_ready_without_retry {
        "ready"
    } else {
        "blocked"
    };
    let mutation_block_reason = if mutation_state == "blocked" {
        issues
            .first()
            .map(|issue| issue.to_string())
            .or_else(|| Some(status.to_string()))
    } else {
        None
    };

    json!({
        "status": status,
        "recommended_action": recommended_action,
        "recommended_cli_follow_up": recommended_cli_follow_up,
        "mutation_intent": mutation_intent,
        "mutation_bundle": mutation_bundle,
        "mutation_ready_without_retry": mutation_ready_without_retry,
        "mutation_state": mutation_state,
        "mutation_block_reason": mutation_block_reason,
        "selected_tool": selected_tool,
        "retrieval_strategy": retrieval_strategy,
        "target_symbol": target_symbol,
        "target_symbol_present": target_symbol_present,
        "context_refs_count": context_refs_count,
        "top_context_file": top_context_file,
        "top_context_present": top_context_present,
        "reference_only": reference_only,
        "confidence_score": confidence_score,
        "confidence_band": confidence_band,
        "candidate_target_aligned": candidate_target_aligned,
        "exact_target_in_top_context": exact_target_in_top_context,
        "exact_target_span_in_top_context": exact_target_span_in_top_context,
        "exact_dependencies_in_reported_files": exact_dependencies_in_reported_files,
        "exact_impact_scope_alignment": exact_impact_scope_alignment,
        "exact_impact_scope_graph_alignment": exact_impact_scope_graph_alignment,
        "exact_impact_scope_target_anchor": exact_impact_scope_target_anchor,
        "exact_impact_scope_graph_complete": exact_impact_scope_graph_complete,
        "workspace_boundary_alignment": workspace_boundary_alignment,
        "fallback_search_used": fallback_search_used,
        "escalation_applied": escalation_applied,
        "minimal_raw_seed_added": minimal_raw_seed_added,
        "low_confidence_raw_context_added": low_confidence_raw_context_added,
        "issues": issues
    })
}

fn build_mutation_verification_bundle(
    mutation_intent: bool,
    fallback_search_used: bool,
    target_symbol_present: bool,
    top_context_present: bool,
    candidate_target_aligned: bool,
    context_refs_count: usize,
    exact_target_in_top_context: Option<bool>,
    exact_target_span_in_top_context: Option<bool>,
    exact_dependencies_in_reported_files: Option<bool>,
    exact_impact_scope_alignment: Option<bool>,
    exact_impact_scope_graph_alignment: Option<bool>,
    exact_impact_scope_target_anchor: Option<bool>,
    exact_impact_scope_graph_complete: Option<bool>,
    workspace_boundary_alignment: Option<bool>,
) -> serde_json::Value {
    if !mutation_intent {
        return json!({
            "status": "not_applicable",
            "required_checks": [],
            "passed_checks": [],
            "failed_checks": [],
            "missing_checks": [],
            "ready_without_retry": false
        });
    }

    let mut passed_checks: Vec<String> = Vec::new();
    let mut failed_checks: Vec<String> = Vec::new();
    let mut missing_checks: Vec<String> = Vec::new();
    let mut required_checks: Vec<&str> = vec![
        "target_symbol_present",
        "top_context_present",
        "candidate_target_aligned",
        "context_refs_present",
        "exact_target_in_top_context",
        "exact_target_span_in_top_context",
    ];

    evaluate_mutation_check(
        "target_symbol_present",
        Some(target_symbol_present),
        &mut passed_checks,
        &mut failed_checks,
        &mut missing_checks,
    );
    evaluate_mutation_check(
        "top_context_present",
        Some(top_context_present),
        &mut passed_checks,
        &mut failed_checks,
        &mut missing_checks,
    );
    evaluate_mutation_check(
        "candidate_target_aligned",
        Some(candidate_target_aligned),
        &mut passed_checks,
        &mut failed_checks,
        &mut missing_checks,
    );
    evaluate_mutation_check(
        "context_refs_present",
        Some(context_refs_count > 0),
        &mut passed_checks,
        &mut failed_checks,
        &mut missing_checks,
    );
    evaluate_mutation_check(
        "exact_target_in_top_context",
        exact_target_in_top_context,
        &mut passed_checks,
        &mut failed_checks,
        &mut missing_checks,
    );
    evaluate_mutation_check(
        "exact_target_span_in_top_context",
        exact_target_span_in_top_context,
        &mut passed_checks,
        &mut failed_checks,
        &mut missing_checks,
    );
    if exact_dependencies_in_reported_files.is_some() {
        required_checks.push("exact_dependencies_in_reported_files");
        evaluate_mutation_check(
            "exact_dependencies_in_reported_files",
            exact_dependencies_in_reported_files,
            &mut passed_checks,
            &mut failed_checks,
            &mut missing_checks,
        );
    }
    if exact_impact_scope_alignment.is_some() {
        required_checks.push("exact_impact_scope_alignment");
        evaluate_mutation_check(
            "exact_impact_scope_alignment",
            exact_impact_scope_alignment,
            &mut passed_checks,
            &mut failed_checks,
            &mut missing_checks,
        );
    }
    if exact_impact_scope_graph_alignment.is_some() {
        required_checks.push("exact_impact_scope_graph_alignment");
        evaluate_mutation_check(
            "exact_impact_scope_graph_alignment",
            exact_impact_scope_graph_alignment,
            &mut passed_checks,
            &mut failed_checks,
            &mut missing_checks,
        );
    }
    if exact_impact_scope_target_anchor.is_some() {
        required_checks.push("exact_impact_scope_target_anchor");
        evaluate_mutation_check(
            "exact_impact_scope_target_anchor",
            exact_impact_scope_target_anchor,
            &mut passed_checks,
            &mut failed_checks,
            &mut missing_checks,
        );
    }
    if exact_impact_scope_graph_complete.is_some() {
        required_checks.push("exact_impact_scope_graph_complete");
        evaluate_mutation_check(
            "exact_impact_scope_graph_complete",
            exact_impact_scope_graph_complete,
            &mut passed_checks,
            &mut failed_checks,
            &mut missing_checks,
        );
    }
    if workspace_boundary_alignment.is_some() {
        required_checks.push("workspace_boundary_alignment");
        evaluate_mutation_check(
            "workspace_boundary_alignment",
            workspace_boundary_alignment,
            &mut passed_checks,
            &mut failed_checks,
            &mut missing_checks,
        );
    }

    let status = if fallback_search_used {
        "blocked"
    } else if failed_checks.is_empty() && missing_checks.is_empty() {
        "exact_ready"
    } else if failed_checks.is_empty() {
        "needs_retry"
    } else {
        "blocked"
    };

    json!({
        "status": status,
        "required_checks": required_checks,
        "passed_checks": passed_checks,
        "failed_checks": failed_checks,
        "missing_checks": missing_checks,
        "ready_without_retry": status == "exact_ready"
    })
}

fn verify_impact_scope_alignment(
    impact_scope: &serde_json::Value,
    top_context_file: Option<&str>,
    get_outline: &mut impl FnMut(&str) -> Option<Vec<engine::SymbolRecord>>,
) -> Option<bool> {
    let impacted_files = impact_scope.get("impacted_files")?.as_array()?;
    let impacted_symbols = impact_scope
        .get("impacted_symbols")
        .and_then(|v| v.as_array())
        .map(|items| {
            items.iter()
                .filter_map(|item| item.as_str().map(|value| value.to_string()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if impacted_files.is_empty() {
        return Some(true);
    }

    let target_boundary = top_context_file.and_then(workspace_boundary_prefix);
    let mut saw_verifiable_file = false;
    for file in impacted_files.iter().filter_map(|item| item.as_str()) {
        if let (Some(target_boundary), Some(file_boundary)) =
            (target_boundary.as_deref(), workspace_boundary_prefix(file).as_deref())
        {
            if file_boundary != target_boundary {
                return Some(false);
            }
        }

        let Some(outline) = get_outline(file) else {
            return None;
        };
        saw_verifiable_file = true;
        if !impacted_symbols.is_empty()
            && !outline.iter().any(|symbol| {
                impacted_symbols
                    .iter()
                    .any(|name| name.eq_ignore_ascii_case(&symbol.name))
            })
        {
            return Some(false);
        }
    }

    if saw_verifiable_file { Some(true) } else { None }
}

fn impact_scope_graph_alignment_from_details(details: &serde_json::Value) -> Option<bool> {
    details.get("aligned").and_then(|v| v.as_bool())
}

fn verify_impact_scope_target_anchor(
    impact_scope: &serde_json::Value,
    target_symbol: &str,
    top_context_file: Option<&str>,
) -> Option<bool> {
    let anchor_symbol = impact_scope.get("anchor_symbol").and_then(|v| v.as_str())?;
    if !anchor_symbol.eq_ignore_ascii_case(target_symbol) {
        return Some(false);
    }
    match (
        impact_scope.get("anchor_file").and_then(|v| v.as_str()),
        top_context_file,
    ) {
        (Some(anchor_file), Some(top_file)) => Some(anchor_file == top_file),
        (Some(_), None) => Some(false),
        (None, Some(_)) => Some(false),
        (None, None) => Some(true),
    }
}

fn impact_scope_graph_completeness_from_details(details: &serde_json::Value) -> Option<bool> {
    let expected_file_count = details
        .get("expected_file_count")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let expected_symbol_count = details
        .get("expected_symbol_count")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let actual_file_count = details
        .get("actual_file_count")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let actual_symbol_count = details
        .get("actual_symbol_count")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let missing_files = details
        .get("missing_files")
        .and_then(|v| v.as_array())
        .map(|items| !items.is_empty())
        .unwrap_or(false);
    let missing_symbols = details
        .get("missing_symbols")
        .and_then(|v| v.as_array())
        .map(|items| !items.is_empty())
        .unwrap_or(false);
    if missing_files || missing_symbols {
        return Some(false);
    }
    if expected_file_count == 0 && expected_symbol_count == 0 {
        return Some(true);
    }
    Some(actual_file_count > 0 || actual_symbol_count > 0)
}

fn impact_scope_graph_details_for_target(
    storage: &storage::Storage,
    target_symbol: &str,
    top_context_file: &str,
    impact_scope: &serde_json::Value,
) -> Option<serde_json::Value> {
    let impacted_files = impact_scope.get("impacted_files")?.as_array()?;
    let impacted_symbols = impact_scope.get("impacted_symbols")?.as_array()?;

    let target = storage
        .file_outline(top_context_file)
        .ok()?
        .into_iter()
        .find(|symbol| symbol.name.eq_ignore_ascii_case(target_symbol))?;
    let target_id = target.id?;
    let callers = storage.get_symbol_callers(target_id).ok()?;

    let mut expected_files = callers
        .iter()
        .map(|symbol| symbol.file.clone())
        .collect::<Vec<_>>();
    expected_files.sort();
    expected_files.dedup();

    let mut expected_symbols = callers
        .iter()
        .map(|symbol| symbol.name.clone())
        .collect::<Vec<_>>();
    expected_symbols.sort();
    expected_symbols.dedup();

    let mut actual_files = impacted_files
        .iter()
        .filter_map(|item| item.as_str().map(|value| value.to_string()))
        .collect::<Vec<_>>();
    actual_files.sort();
    actual_files.dedup();

    let mut actual_symbols = impacted_symbols
        .iter()
        .filter_map(|item| item.as_str().map(|value| value.to_string()))
        .collect::<Vec<_>>();
    actual_symbols.sort();
    actual_symbols.dedup();

    let missing_files = expected_files
        .iter()
        .filter(|expected| !actual_files.iter().any(|actual| actual == *expected))
        .cloned()
        .collect::<Vec<_>>();
    let extra_files = actual_files
        .iter()
        .filter(|actual| !expected_files.iter().any(|expected| expected == *actual))
        .cloned()
        .collect::<Vec<_>>();
    let missing_symbols = expected_symbols
        .iter()
        .filter(|expected| !actual_symbols.iter().any(|actual| actual.eq_ignore_ascii_case(expected)))
        .cloned()
        .collect::<Vec<_>>();
    let extra_symbols = actual_symbols
        .iter()
        .filter(|actual| !expected_symbols.iter().any(|expected| expected.eq_ignore_ascii_case(actual)))
        .cloned()
        .collect::<Vec<_>>();

    Some(json!({
        "aligned": missing_files.is_empty()
            && extra_files.is_empty()
            && missing_symbols.is_empty()
            && extra_symbols.is_empty(),
        "expected_file_count": expected_files.len(),
        "actual_file_count": actual_files.len(),
        "expected_files": expected_files,
        "actual_files": actual_files,
        "missing_files": missing_files,
        "extra_files": extra_files,
        "expected_symbol_count": expected_symbols.len(),
        "actual_symbol_count": actual_symbols.len(),
        "expected_symbols": expected_symbols,
        "actual_symbols": actual_symbols,
        "missing_symbols": missing_symbols,
        "extra_symbols": extra_symbols
    }))
}

fn evaluate_mutation_check(
    name: &str,
    value: Option<bool>,
    passed_checks: &mut Vec<String>,
    failed_checks: &mut Vec<String>,
    missing_checks: &mut Vec<String>,
) {
    match value {
        Some(true) => passed_checks.push(name.to_string()),
        Some(false) => failed_checks.push(name.to_string()),
        None => missing_checks.push(name.to_string()),
    }
}

fn recommended_cli_follow_up(
    status: &str,
    target_symbol: &str,
    top_context_file: Option<&str>,
) -> Option<String> {
    if status == "high_confidence" {
        return None;
    }
    if let Some(file) = top_context_file.filter(|value| !value.is_empty()) {
        return Some(format!(
            "semantic-cli retrieve --op get_file_outline --file {} --output text",
            file
        ));
    }
    if !target_symbol.is_empty() {
        return Some(format!(
            "semantic-cli retrieve --op search_symbol --query {} --output text",
            target_symbol
        ));
    }
    None
}

fn count_result_context_refs(result: &serde_json::Value) -> usize {
    let context_count = result
        .get("context")
        .and_then(|v| v.as_array())
        .map(|items| items.len())
        .unwrap_or(0);
    let ranked_count = result
        .get("ranked_context")
        .and_then(|v| v.as_array())
        .map(|items| items.len())
        .unwrap_or(0);
    let dependency_count = result
        .get("dependency_spans")
        .and_then(|v| v.as_array())
        .map(|items| items.len())
        .unwrap_or(0);
    context_count.max(ranked_count).max(dependency_count)
}

fn spans_overlap(start_a: u32, end_a: u32, start_b: u32, end_b: u32) -> bool {
    start_a <= end_b && start_b <= end_a
}

fn verify_workspace_boundary_alignment(result: &serde_json::Value) -> Option<bool> {
    let top_file = result
        .get("context")
        .and_then(|v| v.as_array())
        .and_then(|items| items.first())
        .and_then(|item| item.get("file"))
        .and_then(|v| v.as_str())
        .or_else(|| {
            result
                .get("ranked_context")
                .and_then(|v| v.as_array())
                .and_then(|items| items.first())
                .and_then(|item| item.get("file"))
                .and_then(|v| v.as_str())
        })?;
    let boundary = workspace_boundary_prefix(top_file)?;
    let mut saw_boundary_file = false;

    for file in result_context_and_dependency_files(result) {
        if let Some(prefix) = workspace_boundary_prefix(&file) {
            saw_boundary_file = true;
            if prefix != boundary {
                return Some(false);
            }
        }
    }

    if saw_boundary_file { Some(true) } else { None }
}

fn file_outline_contains_symbol(outline: &serde_json::Value, target_symbol: &str) -> bool {
    outline
        .get("symbols")
        .and_then(|v| v.as_array())
        .map(|symbols| {
            symbols.iter().any(|symbol| {
                symbol
                    .get("name")
                    .and_then(|v| v.as_str())
                    .map(|name| name.eq_ignore_ascii_case(target_symbol))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

fn collect_exact_symbol_files(search_result: &serde_json::Value, target_symbol: &str) -> Vec<String> {
    let mut files = HashSet::new();
    for key in ["fallback", "results"] {
        if let Some(items) = search_result.get(key).and_then(|v| v.as_array()) {
            for item in items {
                let matches_name = item
                    .get("name")
                    .and_then(|v| v.as_str())
                    .map(|name| name.eq_ignore_ascii_case(target_symbol))
                    .unwrap_or(false);
                let file = item
                    .get("file")
                    .and_then(|v| v.as_str())
                    .map(|value| value.to_string());
                if matches_name {
                    if let Some(file) = file {
                        files.insert(file);
                    }
                }
            }
        }
    }
    let mut files = files.into_iter().collect::<Vec<_>>();
    files.sort();
    files
}

fn workspace_boundary_prefix(file: &str) -> Option<String> {
    let mut parts = file.split('/');
    let first = parts.next()?;
    let second = parts.next()?;
    if first == "packages" && !second.is_empty() {
        Some(format!("{first}/{second}"))
    } else {
        None
    }
}

fn result_context_and_dependency_files(result: &serde_json::Value) -> Vec<String> {
    let mut files = Vec::new();
    if let Some(items) = result.get("context").and_then(|v| v.as_array()) {
        files.extend(items.iter().filter_map(|item| {
            item.get("file")
                .and_then(|v| v.as_str())
                .map(|value| value.to_string())
        }));
    }
    if let Some(items) = result.get("ranked_context").and_then(|v| v.as_array()) {
        files.extend(items.iter().filter_map(|item| {
            item.get("file")
                .and_then(|v| v.as_str())
                .map(|value| value.to_string())
        }));
    }
    if let Some(items) = result.get("dependency_spans").and_then(|v| v.as_array()) {
        files.extend(items.iter().filter_map(|item| {
            item.get("file")
                .and_then(|v| v.as_str())
                .map(|value| value.to_string())
        }));
    }
    files
}

fn extract_result_symbol_name(result: &serde_json::Value) -> Option<String> {
    result
        .get("symbol")
        .and_then(|v| v.as_str())
        .map(|value| value.to_string())
        .or_else(|| {
            result
                .get("symbol")
                .and_then(|v| v.get("name"))
                .and_then(|v| v.as_str())
                .map(|value| value.to_string())
        })
}

fn detect_ide_intent(task: &str) -> &'static str {
    let normalized = task.to_lowercase();
    let tokens = intent_tokens(&normalized);
    let mut debug_score = 0u32;
    let mut refactor_score = 0u32;
    let mut implement_score = 0u32;
    let mut understand_score = 0u32;

        for token in &tokens {
            match token.as_str() {
              "fix" | "bug" | "error" | "failure" | "failing" | "broken" | "issue" | "crash"
              | "crashes" | "fails" | "failed" | "debug" | "diagnose" | "investigate"
              | "repair" | "fault" | "wrong" => debug_score += 3,
            "trace" | "triage" | "stuck" | "regression" => debug_score += 2,
            "refactor" | "rewrite" | "restructure" | "cleanup" | "simplify" | "extract"
            | "rename" | "migrate" | "modularize" => refactor_score += 3,
            "optimize" | "improve" | "harden" | "stabilize" => refactor_score += 2,
            "add" | "implement" | "create" | "build" | "introduce" | "support" | "enable"
            | "allow" | "update" | "extend" => implement_score += 3,
            "change" | "wire" | "generate" | "make" => implement_score += 2,
            "explain" | "understand" | "describe" | "document" | "clarify" | "summarize"
            | "overview" | "inspect" => understand_score += 3,
            "what" | "why" | "how" | "show" => understand_score += 2,
            _ => {}
        }
    }

    let failure_language = normalized.contains("fail")
        || normalized.contains("failure")
        || normalized.contains("error")
        || normalized.contains("bug")
        || normalized.contains("crash")
        || normalized.contains("broken");

    if normalized.contains("root cause") || normalized.contains("why is") {
        debug_score += 3;
    }
    if normalized.contains("explain why") && failure_language {
        debug_score += 4;
    }
    if normalized.contains("why") && failure_language {
        debug_score += 2;
    }
    if normalized.contains("how does") || normalized.contains("what does") {
        understand_score += 3;
    }
    if normalized.contains("fix and explain")
        || normalized.contains("fix and refactor")
        || normalized.contains("debug and refactor")
        || normalized.contains("diagnose and rewrite")
        || normalized.contains("diagnose and refactor")
    {
        debug_score += 2;
    }
    if normalized.contains("refactor and optimize") || normalized.contains("rewrite for reuse") {
        refactor_score += 2;
    }

    let mut scored = [
        ("debug", debug_score),
        ("refactor", refactor_score),
        ("implement", implement_score),
        ("understand", understand_score),
    ];
    scored.sort_by(|a, b| {
        b.1.cmp(&a.1).then_with(|| intent_priority(a.0).cmp(&intent_priority(b.0)))
    });

    if scored[0].1 == 0 {
        "understand"
    } else {
        scored[0].0
    }
}

fn intent_tokens(task: &str) -> Vec<String> {
    task.split(|ch: char| !ch.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(|token| token.to_string())
        .collect()
}

fn intent_priority(intent: &str) -> u8 {
    match intent {
        "debug" => 0,
        "refactor" => 1,
        "implement" => 2,
        _ => 3,
    }
}

fn retrieval_query_for_task(task: &str) -> String {
    let trimmed = task.trim();
    for separator in [", not ", " but not ", " rather than "] {
        if let Some((head, _)) = trimmed.split_once(separator) {
            let head = head.trim();
            if !head.is_empty() {
                return head.to_string();
            }
        }
    }
    trimmed.to_string()
}

#[cfg(test)]
mod tests {
    use super::{detect_ide_intent, retrieval_query_for_task};
    use crate::{AppRuntime, BootstrapIndexPolicy, RuntimeOptions};
    use crate::models::IdeAutoRouteRequest;
    use serde_json::json;
    use std::fs;
    use std::time::Instant;
    use test_support::{
        estimate_tokens_json, materialize_quality_fixture, summarize_route_reports,
        MaterializedFixture, RouteCase, RouteCaseReport,
    };

    fn fixture_runtime() -> (MaterializedFixture, AppRuntime) {
        let fixture = materialize_quality_fixture("cross_stack_app").expect("fixture");
        let runtime = AppRuntime::bootstrap(
            fixture.repo_root().to_path_buf(),
            RuntimeOptions {
                start_watcher: false,
                ensure_config: true,
                bootstrap_index_policy: BootstrapIndexPolicy::ReuseExistingOrCreate,
            },
        )
        .expect("bootstrap runtime");
        (fixture, runtime)
    }

    fn fixture_runtime_for(name: &str) -> (MaterializedFixture, AppRuntime) {
        let fixture = materialize_quality_fixture(name).expect("fixture");
        let runtime = AppRuntime::bootstrap(
            fixture.repo_root().to_path_buf(),
            RuntimeOptions {
                start_watcher: false,
                ensure_config: true,
                bootstrap_index_policy: BootstrapIndexPolicy::ReuseExistingOrCreate,
            },
        )
        .expect("bootstrap runtime");
        (fixture, runtime)
    }

    #[test]
    fn autoroute_marks_unindexed_target_paths_in_verification() {
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
                bootstrap_index_policy: BootstrapIndexPolicy::Skip,
            },
        )
        .expect("bootstrap runtime");
        runtime
            .indexer()
            .lock()
            .index_paths(runtime.repo_root(), &[String::from("src/auth")])
            .expect("targeted index");

        let value = runtime.handle_autoroute(IdeAutoRouteRequest {
            task: Some("understand src/worker job.ts".to_string()),
            include_summary: Some(true),
            raw_expansion_mode: None,
            auto_index_target: None,
            action: None,
            action_input: None,
            session_id: None,
            max_tokens: Some(800),
            single_file_fast_path: Some(true),
            reference_only: Some(true),
            mapping_mode: None,
            max_footprint_items: None,
            reuse_session_context: Some(true),
            auto_minimal_raw: Some(true),
        });

        let verification = value.get("verification").expect("verification");
        assert_eq!(
            verification
                .get("index_readiness")
                .and_then(|v| v.as_str()),
            Some("partial_index_missing_target")
        );
        assert_eq!(
            verification
                .get("index_recovery_mode")
                .and_then(|v| v.as_str()),
            Some("suggest_only")
        );
        assert_eq!(
            verification
                .get("index_coverage")
                .and_then(|v| v.as_str()),
            Some("unindexed_target")
        );
        assert_eq!(
            verification
                .get("index_coverage_target")
                .and_then(|v| v.as_str()),
            Some("src/worker")
        );
        assert_eq!(
            verification
                .get("suggested_index_command")
                .and_then(|v| v.as_str()),
            Some("semantic index --path src/worker")
        );
        assert!(
            verification
                .get("issues")
                .and_then(|v| v.as_array())
                .map(|items| items.iter().any(|item| item.as_str() == Some("target_path_not_indexed")))
                .unwrap_or(false)
        );
    }

    #[test]
    fn autoroute_can_auto_index_unindexed_target_path() {
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
                bootstrap_index_policy: BootstrapIndexPolicy::Skip,
            },
        )
        .expect("bootstrap runtime");
        runtime
            .indexer()
            .lock()
            .index_paths(runtime.repo_root(), &[String::from("src/auth")])
            .expect("targeted index");

        let value = runtime.handle_autoroute(IdeAutoRouteRequest {
            task: Some("understand src/worker job.ts".to_string()),
            include_summary: Some(true),
            raw_expansion_mode: None,
            auto_index_target: Some(true),
            action: None,
            action_input: None,
            session_id: None,
            max_tokens: Some(800),
            single_file_fast_path: Some(true),
            reference_only: Some(true),
            mapping_mode: None,
            max_footprint_items: None,
            reuse_session_context: Some(true),
            auto_minimal_raw: Some(true),
        });

        assert_eq!(
            value.get("auto_index_applied").and_then(|v| v.as_bool()),
            Some(true)
        );
        assert_eq!(
            value.get("auto_index_target").and_then(|v| v.as_str()),
            Some("src/worker")
        );
        assert_eq!(
            value.get("index_recovery_mode").and_then(|v| v.as_str()),
            Some("auto_index_applied")
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
        let verification = value.get("verification").expect("verification");
        assert_eq!(
            verification
                .get("index_readiness")
                .and_then(|v| v.as_str()),
            Some("target_ready")
        );
        assert_eq!(
            verification
                .get("index_recovery_mode")
                .and_then(|v| v.as_str()),
            Some("auto_index_attempted_no_change")
        );
        assert_eq!(
            verification
                .get("index_coverage")
                .and_then(|v| v.as_str()),
            Some("indexed_target")
        );
        assert_eq!(
            verification
                .get("index_coverage_target")
                .and_then(|v| v.as_str()),
            Some("src/worker")
        );
        assert!(
            !verification
                .get("issues")
                .and_then(|v| v.as_array())
                .map(|items| items.iter().any(|item| item.as_str() == Some("target_path_not_indexed")))
                .unwrap_or(false)
        );
    }

    #[test]
    fn autoroute_prefers_exact_file_target_for_auto_index() {
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
                bootstrap_index_policy: BootstrapIndexPolicy::Skip,
            },
        )
        .expect("bootstrap runtime");
        runtime
            .indexer()
            .lock()
            .index_paths(runtime.repo_root(), &[String::from("src/auth")])
            .expect("targeted index");

        let value = runtime.handle_autoroute(IdeAutoRouteRequest {
            task: Some("understand src/worker/job.ts".to_string()),
            include_summary: Some(true),
            raw_expansion_mode: None,
            auto_index_target: Some(true),
            action: None,
            action_input: None,
            session_id: None,
            max_tokens: Some(800),
            single_file_fast_path: Some(true),
            reference_only: Some(true),
            mapping_mode: None,
            max_footprint_items: None,
            reuse_session_context: Some(true),
            auto_minimal_raw: Some(true),
        });

        assert_eq!(
            value.get("auto_index_target").and_then(|v| v.as_str()),
            Some("src/worker/job.ts")
        );
        assert_eq!(
            value.get("index_recovery_mode").and_then(|v| v.as_str()),
            Some("auto_index_applied")
        );
        assert_eq!(
            value.get("indexed_file_count").and_then(|v| v.as_u64()),
            Some(2)
        );
        let verification = value.get("verification").expect("verification");
        assert_eq!(
            verification
                .get("index_readiness")
                .and_then(|v| v.as_str()),
            Some("target_ready")
        );
        assert_eq!(
            verification
                .get("index_recovery_mode")
                .and_then(|v| v.as_str()),
            Some("auto_index_attempted_no_change")
        );
        assert_eq!(
            verification
                .get("index_coverage_target")
                .and_then(|v| v.as_str()),
            Some("src/worker/job.ts")
        );

        let indexed_files = runtime
            .retrieval()
            .lock()
            .with_storage(|storage| storage.list_files())
            .expect("list files");
        assert!(indexed_files.iter().any(|item| item == "src/worker/job.ts"));
    }

    #[test]
    fn autoroute_does_not_claim_auto_index_applied_when_target_stays_unindexed() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join("src").join("auth")).expect("mkdir auth");
        fs::write(
            repo.join("src").join("auth").join("session.ts"),
            "export function buildSession(){ return 1; }\n",
        )
        .expect("write auth");

        let runtime = AppRuntime::bootstrap(
            repo.clone(),
            RuntimeOptions {
                start_watcher: false,
                ensure_config: true,
                bootstrap_index_policy: BootstrapIndexPolicy::Skip,
            },
        )
        .expect("bootstrap runtime");
        runtime
            .indexer()
            .lock()
            .index_paths(runtime.repo_root(), &[String::from("src/auth")])
            .expect("targeted index");

        let value = runtime.handle_autoroute(IdeAutoRouteRequest {
            task: Some("understand src/worker job.ts".to_string()),
            include_summary: Some(true),
            raw_expansion_mode: None,
            auto_index_target: Some(true),
            action: None,
            action_input: None,
            session_id: None,
            max_tokens: Some(800),
            single_file_fast_path: Some(true),
            reference_only: Some(true),
            mapping_mode: None,
            max_footprint_items: None,
            reuse_session_context: Some(true),
            auto_minimal_raw: Some(true),
        });

        assert_eq!(value.get("auto_index_applied"), None);
        assert_eq!(
            value.get("index_recovery_mode").and_then(|v| v.as_str()),
            Some("auto_index_attempted_no_change")
        );
        let verification = value.get("verification").expect("verification");
        assert_eq!(
            verification
                .get("index_readiness")
                .and_then(|v| v.as_str()),
            Some("partial_index_missing_target")
        );
        assert_eq!(
            verification
                .get("index_recovery_mode")
                .and_then(|v| v.as_str()),
            Some("auto_index_attempted_no_change")
        );
        assert_eq!(
            verification
                .get("index_coverage")
                .and_then(|v| v.as_str()),
            Some("unindexed_target")
        );
    }

    fn run_route_case(runtime: &AppRuntime, case: &RouteCase) -> serde_json::Value {
        runtime.handle_autoroute(IdeAutoRouteRequest {
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
        })
    }

    fn assert_route_case(value: &serde_json::Value, case: &RouteCase) {
        assert_eq!(value.get("ok").and_then(|v| v.as_bool()), Some(true));
        if let Some(expected_intent) = &case.expected_intent {
            assert_eq!(
                value.get("intent").and_then(|v| v.as_str()),
                Some(expected_intent.as_str())
            );
        }
        if let Some(expected_selected_tool) = &case.expected_selected_tool {
            assert_eq!(
                value.get("selected_tool").and_then(|v| v.as_str()),
                Some(expected_selected_tool.as_str())
            );
        }
        if let Some(expected_max_tokens) = case.expected_max_tokens {
            assert_eq!(
                value.get("max_tokens").and_then(|v| v.as_u64()),
                Some(expected_max_tokens as u64)
            );
        }
        if let Some(expected_reference_only) = case.expected_reference_only {
            assert_eq!(
                value.get("reference_only").and_then(|v| v.as_bool()),
                Some(expected_reference_only)
            );
        }
        if let Some(expected_single_file_fast_path) = case.expected_single_file_fast_path {
            assert_eq!(
                value.get("single_file_fast_path").and_then(|v| v.as_bool()),
                Some(expected_single_file_fast_path)
            );
        }
        if let Some(expected_result_symbol) = &case.expected_result_symbol {
            assert_eq!(
                value.get("result")
                    .and_then(|v| v.get("symbol"))
                    .and_then(|v| v.as_str()),
                Some(expected_result_symbol.as_str())
            );
        }
        if let Some(expected_project_summary) = case.expected_project_summary {
            assert_eq!(
                value.get("project_summary").map(|v| !v.is_null()),
                Some(expected_project_summary)
            );
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
                .map(|expected| {
                    value.get("selected_tool").and_then(|v| v.as_str()) == Some(expected)
                })
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

    #[test]
    fn intent_detection_remains_keyword_deterministic() {
        assert_eq!(detect_ide_intent("fix fetchData null error"), "debug");
        assert_eq!(detect_ide_intent("refactor fetchData for reuse"), "refactor");
        assert_eq!(detect_ide_intent("add retry handling to fetchData"), "implement");
        assert_eq!(detect_ide_intent("explain fetchData"), "understand");
        assert_eq!(detect_ide_intent("diagnose root cause for fetchData crash"), "debug");
        assert_eq!(detect_ide_intent("rewrite auth flow to simplify dependencies"), "refactor");
        assert_eq!(detect_ide_intent("enable oauth support for login flow"), "implement");
        assert_eq!(detect_ide_intent("how does fetchData retry work"), "understand");
        assert_eq!(detect_ide_intent("fix and explain fetchData bug"), "debug");
        assert_eq!(
            detect_ide_intent("explain why fetchData fails and add retry support"),
            "debug"
        );
        assert_eq!(
            detect_ide_intent("diagnose and rewrite processJob auth failure"),
            "debug"
        );
        assert_eq!(
            retrieval_query_for_task("fix worker runWorker init bug, not api handleRequest"),
            "fix worker runWorker init bug"
        );
        assert_eq!(
            retrieval_query_for_task("explain web renderApp service rather than api buildClient"),
            "explain web renderApp service"
        );
    }

    #[test]
    fn manifest_route_cases_hold_for_cross_stack_fixture() {
        let (fixture, runtime) = fixture_runtime();
        for case in &fixture.manifest().route_cases {
            let value = run_route_case(&runtime, case);
            assert_route_case(&value, case);
        }
    }

    #[test]
    fn manifest_route_cases_hold_for_workspace_duplicate_symbols_fixture() {
        let (fixture, runtime) = fixture_runtime_for("workspace_duplicate_symbols");
        for case in &fixture.manifest().route_cases {
            let value = run_route_case(&runtime, case);
            assert_route_case(&value, case);
        }
    }

    #[test]
    fn route_quality_summary_meets_current_fixture_bar() {
        let (fixture, runtime) = fixture_runtime();
        let mut reports = Vec::new();

        for case in &fixture.manifest().route_cases {
            let started = Instant::now();
            let value = run_route_case(&runtime, case);
            reports.push(route_case_report(
                &value,
                case,
                started.elapsed().as_secs_f64() * 1000.0,
            ));
        }

        let summary = summarize_route_reports(reports);
        assert_eq!(summary.case_count, fixture.manifest().route_cases.len());
        assert!(
            summary.intent_match_rate >= 1.0,
            "route intent summary too low: {:?}",
            summary
        );
        assert!(
            summary.selected_tool_match_rate >= 1.0,
            "route selected-tool summary too low: {:?}",
            summary
        );
        assert!(
            summary.budget_match_rate >= 1.0,
            "route budget summary too low: {:?}",
            summary
        );
        assert!(
            summary.reference_only_match_rate >= 1.0,
            "route reference-only summary too low: {:?}",
            summary
        );
        assert!(
            summary.single_file_fast_path_match_rate >= 1.0,
            "route fast-path summary too low: {:?}",
            summary
        );
        assert!(
            summary.result_symbol_match_rate >= 1.0,
            "route result-symbol summary too low: {:?}",
            summary
        );
        assert!(
            summary.project_summary_match_rate >= 1.0,
            "route project-summary summary too low: {:?}",
            summary
        );
        assert!(
            summary.reviewable_or_better_rate >= 0.80,
            "route verification reviewable rate too low: {:?}",
            summary
        );
        let debug_bucket = summary
            .intent_breakdown
            .iter()
            .find(|bucket| bucket.intent == "debug")
            .expect("debug bucket");
        assert!(
            debug_bucket.reviewable_or_better_rate >= 1.0,
            "debug routes should stay reviewable or better: {:?}",
            summary
        );
        assert!(summary.avg_latency_ms >= 0.0);
        assert!(summary.avg_tokens > 0.0);
    }

    #[test]
    fn autoroute_surfaces_verification_metadata() {
        let (_fixture, runtime) = fixture_runtime();
        let value = runtime.handle_autoroute(IdeAutoRouteRequest {
            task: Some("explain fetch data retry flow".to_string()),
            include_summary: Some(true),
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
            reuse_session_context: Some(false),
            auto_minimal_raw: Some(true),
        });

        let verification = value.get("verification").expect("verification block");
        assert!(verification.get("status").and_then(|v| v.as_str()).is_some());
        assert!(verification
            .get("recommended_action")
            .and_then(|v| v.as_str())
            .is_some());
        assert!(verification
            .get("recommended_cli_follow_up")
            .and_then(|v| v.as_str())
            .is_some());
        assert!(verification
            .get("mutation_state")
            .and_then(|v| v.as_str())
            .is_some());
        assert!(verification
            .get("confidence_score")
            .and_then(|v| v.as_f64())
            .is_some());
        assert!(verification
            .get("confidence_band")
            .and_then(|v| v.as_str())
            .is_some());
    }

    #[test]
    fn understand_route_summary_is_symbol_micro_when_target_is_known() {
        let (fixture, runtime) = fixture_runtime_for("multi_hop_export_star");
        let case = fixture
            .manifest()
            .route_cases
            .iter()
            .find(|case| case.name == "understand_render_theme_flow")
            .expect("understand case");

        let value = runtime.handle_autoroute(IdeAutoRouteRequest {
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
            reuse_session_context: Some(false),
            auto_minimal_raw: Some(true),
        });

        let summary = value
            .get("project_summary")
            .expect("project summary should exist");
        assert_eq!(
            summary.get("summary_scope").and_then(|v| v.as_str()),
            Some("symbol_micro")
        );
        assert!(summary
            .get("summary_text")
            .and_then(|v| v.as_str())
            .map(|text| !text.is_empty())
            .unwrap_or(false));
        assert!(summary.get("token_estimate").is_none());
        assert!(summary.get("auto_injected").is_none());
        assert!(summary.get("summary_tier").is_none());
    }

    #[test]
    fn debug_route_summary_is_symbol_micro_when_target_is_known() {
        let (fixture, runtime) = fixture_runtime_for("workspace_mixed_module_noise");
        let case = fixture
            .manifest()
            .route_cases
            .iter()
            .find(|case| case.name == "debug_worker_init_auth_under_workspace_mixed_noise")
            .expect("debug case");

        let value = runtime.handle_autoroute(IdeAutoRouteRequest {
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
            reuse_session_context: Some(false),
            auto_minimal_raw: Some(true),
        });

        let summary = value
            .get("project_summary")
            .expect("project summary should exist");
        assert_eq!(
            summary.get("summary_scope").and_then(|v| v.as_str()),
            Some("symbol_micro_debug")
        );
        assert!(summary
            .get("summary_text")
            .and_then(|v| v.as_str())
            .map(|text| !text.is_empty() && text.contains("initAuth"))
            .unwrap_or(false));
        assert!(summary.get("token_estimate").is_none());
        assert!(summary.get("auto_injected").is_none());
        assert!(summary.get("summary_tier").is_none());
    }

    #[test]
    fn refactor_route_can_now_be_exact_ready_without_retry() {
        let (_fixture, runtime) = fixture_runtime();
        let value = runtime.handle_autoroute(IdeAutoRouteRequest {
            task: Some("rewrite fetchData to simplify retry".to_string()),
            include_summary: Some(false),
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
            reuse_session_context: Some(false),
            auto_minimal_raw: Some(true),
        });

        let verification = value.get("verification").expect("verification block");
        assert_eq!(
            verification.get("mutation_state").and_then(|v| v.as_str()),
            Some("ready")
        );
        assert_eq!(
            verification
                .get("mutation_bundle")
                .and_then(|v| v.get("status"))
                .and_then(|v| v.as_str()),
            Some("exact_ready")
        );
        assert_eq!(
            verification
                .get("mutation_ready_without_retry")
                .and_then(|v| v.as_bool()),
            Some(true)
        );
        assert_eq!(
            verification
                .get("mutation_recovered_by_retry")
                .and_then(|v| v.as_bool()),
            None
        );
    }

    #[test]
    fn blocked_mutation_bundle_can_still_recover_via_exact_retry() {
        let (_fixture, runtime) = fixture_runtime();
        let mut verification = json!({
            "mutation_state": "blocked",
            "target_symbol": "fetchData",
            "top_context_file": "src/api/client.ts",
            "mutation_bundle": {
                "status": "blocked",
                "failed_checks": ["exact_target_in_top_context"],
                "missing_checks": [],
                "ready_without_retry": false
            }
        });
        let mut result = json!({});

        runtime.maybe_recover_mutation_route(&mut verification, &mut result);

        assert_eq!(
            verification.get("mutation_state").and_then(|v| v.as_str()),
            Some("ready")
        );
        assert_eq!(
            verification
                .get("mutation_recovered_by_retry")
                .and_then(|v| v.as_bool()),
            Some(true)
        );
        assert_eq!(
            verification
                .get("mutation_bundle")
                .and_then(|v| v.get("status"))
                .and_then(|v| v.as_str()),
            Some("retry_recovered")
        );
    }

    #[test]
    fn verification_requires_internal_target_context_alignment_for_high_confidence() {
        let result = json!({
            "symbol": "init_auth",
            "candidate_symbols": ["load_config", "other_symbol"],
            "context": [{
                "file": "packages/api/auth_flow.py",
                "start": 1,
                "end": 5
            }],
            "confidence_score": 0.92,
            "confidence_band": "high",
            "retrieval_strategy": "two_stage_rank_then_span_fetch"
        });

        let verification = super::build_verification_summary(
            "implement",
            &result,
            "get_planned_context",
            1,
            0.92,
            true,
            false,
            false,
            false,
            Some(false),
            Some(false),
            Some(true),
            None,
            None,
            None,
            None,
            Some(true),
        );

        assert_eq!(
            verification.get("status").and_then(|v| v.as_str()),
            Some("needs_review")
        );
        assert_eq!(
            verification.get("mutation_state").and_then(|v| v.as_str()),
            Some("blocked")
        );
        assert_eq!(
            verification
                .get("mutation_block_reason")
                .and_then(|v| v.as_str()),
            Some("candidate_symbols_do_not_include_target")
        );
        assert_eq!(
            verification
                .get("candidate_target_aligned")
                .and_then(|v| v.as_bool()),
            Some(false)
        );
        assert!(verification
            .get("issues")
            .and_then(|v| v.as_array())
            .map(|issues| issues.iter().any(|issue| issue.as_str() == Some("candidate_symbols_do_not_include_target")))
            .unwrap_or(false));
        assert_eq!(
            verification
                .get("exact_target_in_top_context")
                .and_then(|v| v.as_bool()),
            Some(false)
        );
        assert_eq!(
            verification
                .get("exact_target_span_in_top_context")
                .and_then(|v| v.as_bool()),
            Some(false)
        );
    }

    #[test]
    fn verification_marks_dependency_file_symbol_mismatches_for_review() {
        let result = json!({
            "symbol": "init_auth",
            "candidate_symbols": ["init_auth", "load_config"],
            "context": [{
                "file": "packages/api/auth_flow.py",
                "start": 1,
                "end": 5
            }],
            "dependency_spans": [{
                "symbol": {
                    "name": "load_config",
                    "file": "packages/api/missing_config.py"
                }
            }],
            "confidence_score": 0.91,
            "confidence_band": "high",
            "retrieval_strategy": "two_stage_rank_then_span_fetch"
        });

        let verification = super::build_verification_summary(
            "refactor",
            &result,
            "get_reasoning_context",
            1,
            0.91,
            true,
            false,
            false,
            false,
            Some(true),
            Some(true),
            Some(false),
            None,
            None,
            None,
            None,
            Some(true),
        );

        assert_eq!(
            verification.get("status").and_then(|v| v.as_str()),
            Some("needs_review")
        );
        assert_eq!(
            verification.get("mutation_state").and_then(|v| v.as_str()),
            Some("blocked")
        );
        assert_eq!(
            verification
                .get("exact_dependencies_in_reported_files")
                .and_then(|v| v.as_bool()),
            Some(false)
        );
        assert!(verification
            .get("issues")
            .and_then(|v| v.as_array())
            .map(|issues| issues.iter().any(|issue| issue.as_str() == Some("dependency_file_does_not_contain_reported_symbol")))
            .unwrap_or(false));
    }

    #[test]
    fn verification_marks_target_span_mismatches_for_review() {
        let result = json!({
            "symbol": "init_auth",
            "candidate_symbols": ["init_auth", "load_config"],
            "context": [{
                "file": "packages/api/auth_flow.py",
                "start": 40,
                "end": 50
            }],
            "confidence_score": 0.93,
            "confidence_band": "high",
            "retrieval_strategy": "two_stage_rank_then_span_fetch"
        });

        let verification = super::build_verification_summary(
            "implement",
            &result,
            "get_planned_context",
            1,
            0.93,
            true,
            false,
            false,
            false,
            Some(true),
            Some(false),
            Some(true),
            None,
            None,
            None,
            None,
            Some(true),
        );

        assert_eq!(
            verification.get("status").and_then(|v| v.as_str()),
            Some("needs_review")
        );
        assert_eq!(
            verification.get("mutation_state").and_then(|v| v.as_str()),
            Some("blocked")
        );
        assert_eq!(
            verification
                .get("exact_target_span_in_top_context")
                .and_then(|v| v.as_bool()),
            Some(false)
        );
        assert!(verification
            .get("issues")
            .and_then(|v| v.as_array())
            .map(|issues| issues.iter().any(|issue| issue.as_str() == Some("top_context_span_does_not_overlap_target_symbol")))
            .unwrap_or(false));
    }

    #[test]
    fn verification_marks_workspace_boundary_crossing_for_review() {
        let result = json!({
            "symbol": "initAuth",
            "candidate_symbols": ["initAuth", "loadConfig"],
            "context": [{
                "file": "packages/api/src/auth/flow.ts",
                "start": 10,
                "end": 20
            }],
            "dependency_spans": [{
                "symbol": {
                    "name": "loadConfig",
                    "file": "packages/worker/src/shared/config.ts"
                }
            }],
            "confidence_score": 0.94,
            "confidence_band": "high",
            "retrieval_strategy": "two_stage_rank_then_span_fetch"
        });

        let verification = super::build_verification_summary(
            "refactor",
            &result,
            "get_reasoning_context",
            1,
            0.94,
            true,
            false,
            false,
            false,
            Some(true),
            Some(true),
            Some(true),
            None,
            None,
            None,
            None,
            Some(false),
        );

        assert_eq!(
            verification.get("status").and_then(|v| v.as_str()),
            Some("needs_review")
        );
        assert_eq!(
            verification.get("mutation_state").and_then(|v| v.as_str()),
            Some("blocked")
        );
        assert_eq!(
            verification
                .get("workspace_boundary_alignment")
                .and_then(|v| v.as_bool()),
            Some(false)
        );
        assert!(verification
            .get("issues")
            .and_then(|v| v.as_array())
            .map(|issues| issues.iter().any(|issue| issue.as_str() == Some("context_crosses_workspace_boundary")))
            .unwrap_or(false));
    }

    #[test]
    fn verification_marks_high_confidence_mutation_routes_ready() {
        let result = json!({
            "symbol": "load_config",
            "candidate_symbols": ["load_config"],
            "context": [{
                "file": "packages/api/config.py",
                "start": 10,
                "end": 20
            }],
            "confidence_score": 0.92,
            "confidence_band": "high",
            "retrieval_strategy": "two_stage_rank_then_span_fetch"
        });

        let verification = super::build_verification_summary(
            "implement",
            &result,
            "get_planned_context",
            1,
            0.92,
            true,
            false,
            false,
            false,
            Some(true),
            Some(true),
            Some(true),
            None,
            None,
            Some(true),
            Some(true),
            Some(true),
        );

        assert_eq!(
            verification.get("status").and_then(|v| v.as_str()),
            Some("high_confidence")
        );
        assert_eq!(
            verification.get("mutation_state").and_then(|v| v.as_str()),
            Some("ready")
        );
        assert_eq!(
            verification
                .get("mutation_ready_without_retry")
                .and_then(|v| v.as_bool()),
            Some(true)
        );
        assert_eq!(
            verification
                .get("mutation_bundle")
                .and_then(|v| v.get("status"))
                .and_then(|v| v.as_str()),
            Some("exact_ready")
        );
        assert_eq!(
            verification
                .get("mutation_block_reason")
                .and_then(|v| v.as_str()),
            None
        );
    }

    #[test]
    fn exact_mutation_checks_can_mark_route_ready_without_high_confidence_status() {
        let result = json!({
            "symbol": "load_config",
            "candidate_symbols": ["load_config"],
            "context": [{
                "file": "packages/api/config.py",
                "start": 10,
                "end": 20
            }],
            "confidence_score": 0.62,
            "confidence_band": "medium",
            "retrieval_strategy": "two_stage_rank_then_span_fetch"
        });

        let verification = super::build_verification_summary(
            "implement",
            &result,
            "get_planned_context",
            1,
            0.62,
            true,
            false,
            false,
            false,
            Some(true),
            Some(true),
            Some(true),
            None,
            None,
            Some(true),
            Some(true),
            Some(true),
        );

        assert_eq!(
            verification.get("status").and_then(|v| v.as_str()),
            Some("needs_review")
        );
        assert_eq!(
            verification
                .get("mutation_ready_without_retry")
                .and_then(|v| v.as_bool()),
            Some(true)
        );
        assert_eq!(
            verification
                .get("mutation_bundle")
                .and_then(|v| v.get("status"))
                .and_then(|v| v.as_str()),
            Some("exact_ready")
        );
        assert_eq!(
            verification.get("mutation_state").and_then(|v| v.as_str()),
            Some("ready")
        );
    }

    #[test]
    fn ranked_context_counts_as_real_context_for_exact_mutation_readiness() {
        let result = json!({
            "symbol": "load_config",
            "candidate_symbols": ["load_config"],
            "ranked_context": [{
                "file": "packages/api/config.py",
                "start": 10,
                "end": 20
            }],
            "confidence_score": 0.62,
            "confidence_band": "medium",
            "retrieval_strategy": "two_stage_rank_then_span_fetch"
        });

        let verification = super::build_verification_summary(
            "refactor",
            &result,
            "get_hybrid_ranked_context",
            super::count_result_context_refs(&result),
            0.62,
            true,
            false,
            false,
            false,
            Some(true),
            Some(true),
            Some(true),
            None,
            None,
            Some(true),
            Some(true),
            Some(true),
        );

        assert_eq!(
            verification.get("status").and_then(|v| v.as_str()),
            Some("needs_review")
        );
        assert_eq!(
            verification
                .get("mutation_bundle")
                .and_then(|v| v.get("status"))
                .and_then(|v| v.as_str()),
            Some("exact_ready")
        );
        assert_eq!(
            verification
                .get("mutation_ready_without_retry")
                .and_then(|v| v.as_bool()),
            Some(true)
        );
        assert_eq!(
            verification.get("mutation_state").and_then(|v| v.as_str()),
            Some("ready")
        );
    }

    #[test]
    fn mutation_bundle_reports_failed_and_missing_checks() {
        let result = json!({
            "symbol": "init_auth",
            "candidate_symbols": ["init_auth"],
            "context": [{
                "file": "packages/api/auth_flow.py",
                "start": 10,
                "end": 20
            }],
            "confidence_score": 0.61,
            "confidence_band": "medium",
            "retrieval_strategy": "two_stage_rank_then_span_fetch"
        });

        let verification = super::build_verification_summary(
            "refactor",
            &result,
            "get_hybrid_ranked_context",
            1,
            0.61,
            true,
            false,
            false,
            false,
            Some(true),
            Some(false),
            None,
            None,
            None,
            None,
            None,
            Some(true),
        );

        assert_eq!(
            verification
                .get("mutation_bundle")
                .and_then(|v| v.get("status"))
                .and_then(|v| v.as_str()),
            Some("blocked")
        );
        assert!(verification
            .get("mutation_bundle")
            .and_then(|v| v.get("failed_checks"))
            .and_then(|v| v.as_array())
            .map(|items| items.iter().any(|item| item.as_str() == Some("exact_target_span_in_top_context")))
            .unwrap_or(false));
        assert_eq!(
            verification
                .get("mutation_bundle")
                .and_then(|v| v.get("missing_checks"))
                .and_then(|v| v.as_array())
                .map(|items| items.len()),
            Some(0)
        );
    }

    #[test]
    fn verification_marks_impact_scope_misalignment_for_review() {
        let result = json!({
            "symbol": "initAuth",
            "candidate_symbols": ["initAuth", "loadConfig"],
            "context": [{
                "file": "packages/api/src/auth/flow.ts",
                "start": 10,
                "end": 20
            }],
            "confidence_score": 0.94,
            "confidence_band": "high",
            "retrieval_strategy": "two_stage_rank_then_span_fetch"
        });

        let verification = super::build_verification_summary(
            "refactor",
            &result,
            "get_hybrid_ranked_context",
            1,
            0.94,
            true,
            false,
            false,
            false,
            Some(true),
            Some(true),
            Some(true),
            Some(false),
            Some(false),
            Some(true),
            Some(true),
            Some(true),
        );

        assert_eq!(
            verification.get("status").and_then(|v| v.as_str()),
            Some("needs_review")
        );
        assert_eq!(
            verification
                .get("exact_impact_scope_alignment")
                .and_then(|v| v.as_bool()),
            Some(false)
        );
        assert_eq!(
            verification
                .get("exact_impact_scope_graph_alignment")
                .and_then(|v| v.as_bool()),
            Some(false)
        );
        assert!(verification
            .get("issues")
            .and_then(|v| v.as_array())
            .map(|issues| issues.iter().any(|issue| issue.as_str() == Some("impact_scope_misaligned")))
            .unwrap_or(false));
        assert!(verification
            .get("issues")
            .and_then(|v| v.as_array())
            .map(|issues| issues.iter().any(|issue| issue.as_str() == Some("impact_scope_graph_misaligned")))
            .unwrap_or(false));
        assert!(verification
            .get("mutation_bundle")
            .and_then(|v| v.get("failed_checks"))
            .and_then(|v| v.as_array())
            .map(|items| items.iter().any(|item| item.as_str() == Some("exact_impact_scope_alignment")))
            .unwrap_or(false));
        assert!(verification
            .get("mutation_bundle")
            .and_then(|v| v.get("failed_checks"))
            .and_then(|v| v.as_array())
            .map(|items| items.iter().any(|item| item.as_str() == Some("exact_impact_scope_graph_alignment")))
            .unwrap_or(false));
    }

    #[test]
    fn verification_marks_unanchored_impact_scope_for_review() {
        let result = json!({
            "symbol": "initAuth",
            "candidate_symbols": ["initAuth", "loadConfig"],
            "context": [{
                "file": "packages/api/src/auth/flow.ts",
                "start": 10,
                "end": 20
            }],
            "confidence_score": 0.94,
            "confidence_band": "high",
            "retrieval_strategy": "two_stage_rank_then_span_fetch"
        });

        let verification = super::build_verification_summary(
            "refactor",
            &result,
            "get_hybrid_ranked_context",
            1,
            0.94,
            true,
            false,
            false,
            false,
            Some(true),
            Some(true),
            Some(true),
            Some(true),
            Some(true),
            Some(false),
            Some(false),
            Some(true),
        );

        assert_eq!(
            verification.get("status").and_then(|v| v.as_str()),
            Some("needs_review")
        );
        assert_eq!(
            verification
                .get("exact_impact_scope_target_anchor")
                .and_then(|v| v.as_bool()),
            Some(false)
        );
        assert!(verification
            .get("issues")
            .and_then(|v| v.as_array())
            .map(|issues| issues.iter().any(|issue| issue.as_str() == Some("impact_scope_not_anchored_to_target")))
            .unwrap_or(false));
        assert!(verification
            .get("mutation_bundle")
            .and_then(|v| v.get("failed_checks"))
            .and_then(|v| v.as_array())
            .map(|items| items.iter().any(|item| item.as_str() == Some("exact_impact_scope_target_anchor")))
            .unwrap_or(false));
    }

    #[test]
    fn verification_marks_incomplete_impact_scope_for_review() {
        let result = json!({
            "symbol": "initAuth",
            "candidate_symbols": ["initAuth", "loadConfig"],
            "context": [{
                "file": "packages/api/src/auth/flow.ts",
                "start": 10,
                "end": 20
            }],
            "confidence_score": 0.94,
            "confidence_band": "high",
            "retrieval_strategy": "two_stage_rank_then_span_fetch"
        });

        let verification = super::build_verification_summary(
            "refactor",
            &result,
            "get_hybrid_ranked_context",
            1,
            0.94,
            true,
            false,
            false,
            false,
            Some(true),
            Some(true),
            Some(true),
            Some(true),
            Some(true),
            Some(true),
            Some(false),
            Some(true),
        );

        assert_eq!(
            verification.get("status").and_then(|v| v.as_str()),
            Some("needs_review")
        );
        assert_eq!(
            verification
                .get("exact_impact_scope_graph_complete")
                .and_then(|v| v.as_bool()),
            Some(false)
        );
        assert!(verification
            .get("issues")
            .and_then(|v| v.as_array())
            .map(|issues| issues.iter().any(|issue| issue.as_str() == Some("impact_scope_graph_incomplete")))
            .unwrap_or(false));
        assert!(verification
            .get("mutation_bundle")
            .and_then(|v| v.get("failed_checks"))
            .and_then(|v| v.as_array())
            .map(|items| items.iter().any(|item| item.as_str() == Some("exact_impact_scope_graph_complete")))
            .unwrap_or(false));
    }

    #[test]
    fn manifest_route_cases_hold_for_auth_distractor_fixture() {
        let (fixture, runtime) = fixture_runtime_for("auth_distractor_app");
        for case in &fixture.manifest().route_cases {
            let value = run_route_case(&runtime, case);
            assert_route_case(&value, case);
        }
    }

    #[test]
    fn manifest_route_cases_hold_for_workspace_path_collisions_fixture() {
        let (fixture, runtime) = fixture_runtime_for("workspace_path_collisions");
        for case in &fixture.manifest().route_cases {
            let value = run_route_case(&runtime, case);
            assert_route_case(&value, case);
        }
    }

    #[test]
    fn manifest_route_cases_hold_for_cross_file_import_duplicates_fixture() {
        let (fixture, runtime) = fixture_runtime_for("cross_file_import_duplicates");
        for case in &fixture.manifest().route_cases {
            let value = run_route_case(&runtime, case);
            assert_route_case(&value, case);
        }
    }

    #[test]
    fn manifest_route_cases_hold_for_import_alias_reexports_fixture() {
        let (fixture, runtime) = fixture_runtime_for("import_alias_reexports");
        for case in &fixture.manifest().route_cases {
            let value = run_route_case(&runtime, case);
            assert_route_case(&value, case);
        }
    }

    #[test]
    fn manifest_route_cases_hold_for_multi_hop_export_star_fixture() {
        let (fixture, runtime) = fixture_runtime_for("multi_hop_export_star");
        for case in &fixture.manifest().route_cases {
            let value = run_route_case(&runtime, case);
            assert_route_case(&value, case);
        }
    }

    #[test]
    fn manifest_route_cases_hold_for_default_export_aliases_fixture() {
        let (fixture, runtime) = fixture_runtime_for("default_export_aliases");
        for case in &fixture.manifest().route_cases {
            let value = run_route_case(&runtime, case);
            assert_route_case(&value, case);
        }
    }

    #[test]
    fn manifest_route_cases_hold_for_unsupported_default_boundary_fixture() {
        let (fixture, runtime) = fixture_runtime_for("unsupported_default_boundary");
        for case in &fixture.manifest().route_cases {
            let value = run_route_case(&runtime, case);
            assert_route_case(&value, case);
        }
    }

    #[test]
    fn manifest_route_cases_hold_for_unsupported_default_barrel_boundary_fixture() {
        let (fixture, runtime) = fixture_runtime_for("unsupported_default_barrel_boundary");
        for case in &fixture.manifest().route_cases {
            let value = run_route_case(&runtime, case);
            assert_route_case(&value, case);
        }
    }

    #[test]
    fn manifest_route_cases_hold_for_unsupported_commonjs_boundary_fixture() {
        let (fixture, runtime) = fixture_runtime_for("unsupported_commonjs_boundary");
        for case in &fixture.manifest().route_cases {
            let value = run_route_case(&runtime, case);
            assert_route_case(&value, case);
        }
    }

    #[test]
    fn manifest_route_cases_hold_for_unsupported_namespace_export_boundary_fixture() {
        let (fixture, runtime) = fixture_runtime_for("unsupported_namespace_export_boundary");
        for case in &fixture.manifest().route_cases {
            let value = run_route_case(&runtime, case);
            assert_route_case(&value, case);
        }
    }

    #[test]
    fn manifest_route_cases_hold_for_unsupported_commonjs_destructure_boundary_fixture() {
        let (fixture, runtime) = fixture_runtime_for("unsupported_commonjs_destructure_boundary");
        for case in &fixture.manifest().route_cases {
            let value = run_route_case(&runtime, case);
            assert_route_case(&value, case);
        }
    }

    #[test]
    fn manifest_route_cases_hold_for_unsupported_commonjs_object_boundary_fixture() {
        let (fixture, runtime) = fixture_runtime_for("unsupported_commonjs_object_boundary");
        for case in &fixture.manifest().route_cases {
            let value = run_route_case(&runtime, case);
            assert_route_case(&value, case);
        }
    }

    #[test]
    fn manifest_route_cases_hold_for_mixed_module_pattern_noise_fixture() {
        let (fixture, runtime) = fixture_runtime_for("mixed_module_pattern_noise");
        for case in &fixture.manifest().route_cases {
            let value = run_route_case(&runtime, case);
            assert_route_case(&value, case);
        }
    }

    #[test]
    fn manifest_route_cases_hold_for_workspace_mixed_module_noise_fixture() {
        let (fixture, runtime) = fixture_runtime_for("workspace_mixed_module_noise");
        for case in &fixture.manifest().route_cases {
            let value = run_route_case(&runtime, case);
            assert_route_case(&value, case);
        }
    }

    #[test]
    fn manifest_route_cases_hold_for_workspace_mixed_module_with_tests_fixture() {
        let (fixture, runtime) = fixture_runtime_for("workspace_mixed_module_with_tests");
        for case in &fixture.manifest().route_cases {
            let value = run_route_case(&runtime, case);
            assert_route_case(&value, case);
        }
    }

    #[test]
    fn healthy_reference_only_refactor_routes_trim_heavy_graph_payloads() {
        let (fixture, runtime) = fixture_runtime_for("cross_stack_app");
        let case = fixture
            .manifest()
            .route_cases
            .iter()
            .find(|case| case.name == "refactor_fetch_data")
            .expect("refactor case");
        let value = run_route_case(&runtime, case);

        let verification = value.get("verification").expect("verification");
        assert_eq!(
            verification
                .get("mutation_bundle")
                .and_then(|value| value.get("status"))
                .and_then(|value| value.as_str()),
            Some("exact_ready")
        );

        let result = value.get("result").expect("result");
        assert!(
            result.get("control_flow_hints").is_none(),
            "healthy reference-only refactor routes should omit bulky control-flow hints"
        );
        assert!(
            result.get("data_flow_hints").is_none(),
            "healthy reference-only refactor routes should omit bulky data-flow hints"
        );
        assert!(
            result.get("logic_clusters").is_none(),
            "healthy reference-only refactor routes should omit bulky logic clusters"
        );
        assert!(result.get("confidence_score").is_none());
        assert!(result.get("query").is_none());
        assert!(result.get("strategy").is_none());
        assert!(result.get("graph_details_available").is_none());
        assert!(
            result.get("graph_rank_signals").is_some(),
            "compact graph rank signals should remain available"
        );
        assert!(
            result
                .get("ranked_context")
                .and_then(|value| value.as_array())
                .map(|items| items.iter().all(|item| item.get("code").is_none()))
                .unwrap_or(false),
            "empty code fields should be trimmed from ranked context items"
        );
    }

    #[test]
    fn healthy_reference_only_debug_routes_trim_duplicate_seed_and_planning_bookkeeping() {
        let (fixture, runtime) = fixture_runtime_for("auth_distractor_app");
        let case = fixture
            .manifest()
            .route_cases
            .iter()
            .find(|case| case.name == "diagnose_validate_token_root_cause")
            .expect("debug case");
        let value = run_route_case(&runtime, case);

        let verification = value.get("verification").expect("verification");
        assert!(verification.get("issues").is_none());

        let result = value.get("result").expect("result");
        assert!(
            result.get("candidate_files").is_none(),
            "healthy debug routes should omit candidate file bookkeeping"
        );
        assert!(
            result.get("candidate_symbols").is_none(),
            "healthy debug routes should omit candidate symbol bookkeeping"
        );
        assert!(
            result.get("effective_breadth").is_none(),
            "healthy debug routes should omit breadth bookkeeping"
        );
        assert!(
            result.get("retrieval_strategy").is_none(),
            "healthy debug routes should omit duplicate retrieval strategy from result payload"
        );
        assert!(
            result.get("single_file_fast_path").is_none(),
            "healthy debug routes should omit duplicate fast-path flags from result payload"
        );
        assert!(result.get("confidence_score").is_none());
        let top_context_has_code_span = result
            .get("context")
            .and_then(|value| value.as_array())
            .and_then(|items| items.first())
            .and_then(|item| item.get("code_span"))
            .and_then(|value| value.as_str())
            .map(|code| !code.is_empty())
            .unwrap_or(false);
        if top_context_has_code_span {
            assert!(
                result.get("minimal_raw_seed").is_none(),
                "healthy debug routes should omit duplicate minimal raw seed when context already has code"
            );
        }
        let has_inline_code_span = result
            .get("context")
            .and_then(|value| value.as_array())
            .and_then(|items| items.first())
            .and_then(|item| item.get("code_span"))
            .and_then(|value| value.as_str())
            .map(|code| !code.is_empty())
            .unwrap_or(false);
        let has_raw_fallback = result
            .get("low_confidence_raw_context")
            .and_then(|value| value.as_array())
            .map(|items| !items.is_empty())
            .unwrap_or(false)
            || result.get("minimal_raw_seed").is_some();
        assert!(
            has_inline_code_span || has_raw_fallback,
            "healthy debug routes should still keep usable code evidence"
        );
        assert!(
            value.get("session_id").is_none(),
            "healthy read-only routes should omit auto-generated session ids"
        );
        assert!(
            value.get("recommended_provider").is_none(),
            "healthy read-only routes should omit default provider placeholders"
        );
        assert!(
            value.get("recommended_endpoint").is_none(),
            "healthy read-only routes should omit default endpoint placeholders"
        );
        assert!(verification.get("selected_tool").is_none());
        assert!(verification.get("reference_only").is_none());
        assert!(verification.get("fallback_search_used").is_none());
        assert!(verification.get("escalation_applied").is_none());
        assert!(verification.get("minimal_raw_seed_added").is_none());
        assert!(verification.get("low_confidence_raw_context_added").is_none());
    }

    #[test]
    fn healthy_reference_only_implement_routes_trim_planning_bookkeeping() {
        let (fixture, runtime) = fixture_runtime_for("auth_distractor_app");
        let case = fixture
            .manifest()
            .route_cases
            .iter()
            .find(|case| case.name == "enable_build_session_refresh_support")
            .expect("implement case");
        let value = run_route_case(&runtime, case);

        let verification = value.get("verification").expect("verification");
        assert_eq!(
            verification
                .get("mutation_bundle")
                .and_then(|value| value.get("status"))
                .and_then(|value| value.as_str()),
            Some("exact_ready")
        );

        let result = value.get("result").expect("result");
        assert!(result.get("candidate_files").is_none());
        assert!(result.get("candidate_symbols").is_none());
        assert!(result.get("effective_breadth").is_none());
        assert!(result.get("retrieval_strategy").is_none());
        assert!(result.get("single_file_fast_path").is_none());
        assert!(result.get("plan").is_none());
        assert!(result.get("confidence_score").is_none());
        assert!(
            result.get("minimal_raw_seed").is_some(),
            "healthy implement routes should keep the raw edit seed"
        );
        assert_eq!(
            result.get("symbol").and_then(|value| value.as_str()),
            Some("buildSession")
        );
        assert!(value.get("mapping_mode").is_none());
        assert!(value.get("session_id").is_none());
        assert!(value.get("reuse_session_context").is_none());
        assert!(value.get("reused_context_count").is_none());
        assert!(value.get("refs_unchanged").is_none());
        assert!(value.get("test_coverage_suppressed").is_none());
        assert!(value.get("recommended_provider").is_none());
        assert!(value.get("recommended_endpoint").is_none());
        assert!(
            verification.get("exact_dependencies_in_reported_files").is_none(),
            "healthy implement routes should omit null dependency-file checks"
        );
        assert!(
            verification.get("workspace_boundary_alignment").is_none(),
            "healthy implement routes should omit null workspace-boundary checks"
        );
        assert!(
            verification.get("mutation_block_reason").is_none(),
            "healthy implement routes should omit null mutation block reasons"
        );
        assert!(
            verification.get("retrieval_strategy").is_none(),
            "healthy implement routes should omit duplicate verification retrieval strategy"
        );
        assert!(verification.get("issues").is_none());
        assert!(verification.get("fallback_search_used").is_none());
        assert!(verification.get("escalation_applied").is_none());
        assert!(verification.get("low_confidence_raw_context_added").is_none());
        assert!(verification.get("candidate_target_aligned").is_none());
        assert!(verification.get("target_symbol_present").is_none());
        assert!(verification.get("top_context_present").is_none());
        assert!(verification.get("context_refs_count").is_none());
        assert!(verification.get("mutation_intent").is_none());
        assert!(verification.get("selected_tool").is_none());
        assert!(verification.get("reference_only").is_none());
        let bundle = verification.get("mutation_bundle").expect("mutation bundle");
        assert_eq!(
            bundle.get("status").and_then(|value| value.as_str()),
            Some("exact_ready")
        );
        assert!(bundle.get("required_checks").is_none());
        assert!(bundle.get("passed_checks").is_none());
        assert!(bundle.get("failed_checks").is_none());
        assert!(bundle.get("missing_checks").is_none());
    }

    #[test]
    fn healthy_reference_only_understand_routes_trim_planning_bookkeeping() {
        let (fixture, runtime) = fixture_runtime_for("auth_distractor_app");
        let case = fixture
            .manifest()
            .route_cases
            .iter()
            .find(|case| case.name == "understand_validate_token")
            .expect("understand case");
        let value = run_route_case(&runtime, case);

        let verification = value.get("verification").expect("verification");
        assert!(verification.get("issues").is_none());

        let result = value.get("result").expect("result");
        assert!(result.get("candidate_files").is_none());
        assert!(result.get("candidate_symbols").is_none());
        assert!(result.get("effective_breadth").is_none());
        assert!(result.get("retrieval_strategy").is_none());
        assert!(result.get("single_file_fast_path").is_none());
        assert!(result.get("plan").is_none());
        assert!(result.get("confidence_score").is_none());
        assert!(value.get("impact_scope").is_none());
        assert!(value.get("mapping_mode").is_none());
        assert!(value.get("reuse_session_context").is_none());
        assert!(value.get("reused_context_count").is_none());
        assert!(value.get("refs_unchanged").is_none());
        assert!(value.get("test_coverage_suppressed").is_none());
        assert_eq!(
            result.get("symbol").and_then(|value| value.as_str()),
            Some("validateToken")
        );
        assert!(
            result
                .get("context")
                .and_then(|value| value.as_array())
                .map(|items| !items.is_empty())
                .unwrap_or(false),
            "healthy understand routes should keep actual context"
        );
        assert!(
            result
                .get("context")
                .and_then(|value| value.as_array())
                .map(|items| items.iter().all(|item| item.get("raw_included").is_none()))
                .unwrap_or(false),
            "healthy understand routes should omit redundant raw_included flags when code is already present"
        );
        assert!(
            value.get("project_summary").is_some(),
            "healthy understand routes should keep project summary contract"
        );
        assert!(
            verification.get("mutation_bundle").is_none(),
            "healthy understand routes should omit empty mutation bundle metadata"
        );
        assert!(
            verification.get("mutation_ready_without_retry").is_none(),
            "healthy understand routes should omit empty mutation readiness metadata"
        );
        assert!(
            verification.get("retrieval_strategy").is_none(),
            "healthy understand routes should omit duplicate verification retrieval strategy"
        );
        assert!(
            verification.get("workspace_boundary_alignment").is_none(),
            "healthy understand routes should omit null workspace-boundary checks"
        );
        assert!(verification.get("selected_tool").is_none());
        assert!(verification.get("reference_only").is_none());
        assert!(verification.get("fallback_search_used").is_none());
        assert!(verification.get("escalation_applied").is_none());
        assert!(verification.get("minimal_raw_seed_added").is_none());
        assert!(verification.get("low_confidence_raw_context_added").is_none());
    }

    #[test]
    fn healthy_routes_omit_empty_top_level_placeholders() {
        let (fixture, runtime) = fixture_runtime_for("auth_distractor_app");
        let case = fixture
            .manifest()
            .route_cases
            .iter()
            .find(|case| case.name == "enable_build_session_refresh_support")
            .expect("implement case");
        let value = run_route_case(&runtime, case);

        assert!(value.get("context_delta").is_none());
        assert!(value.get("context_delta_mode").is_none());
        assert!(value.get("knowledge_hints").is_none());
        assert!(value.get("debug_candidates").is_none());
        assert!(value.get("project_summary").is_some());
        let provider = value.get("recommended_provider");
        let endpoint = value.get("recommended_endpoint");
        assert_eq!(
            provider.is_some(),
            endpoint.is_some(),
            "provider/endpoint recommendations should appear or disappear together"
        );
    }

    #[test]
    fn debug_routes_skip_duplicate_inline_span_when_raw_fallback_exists() {
        let (fixture, runtime) = fixture_runtime_for("workspace_shared_file_noise");
        let case = fixture
            .manifest()
            .route_cases
            .iter()
            .find(|case| case.name == "diagnose_worker_process_job_root_cause")
            .expect("low-confidence debug case");
        let value = run_route_case(&runtime, case);

        let result = value.get("result").expect("result");
        assert!(
            result.get("low_confidence_raw_context").is_some()
                || result.get("minimal_raw_seed").is_some(),
            "debug route should carry an explicit raw fallback for this confidence band"
        );
        assert!(
            result
                .get("context")
                .and_then(|value| value.as_array())
                .map(|items| items.iter().all(|item| item.get("code_span").is_none()))
                .unwrap_or(false),
            "debug route should not also inline duplicate code spans when a raw fallback is already present"
        );
    }

    #[test]
    fn healthy_read_only_routes_trim_empty_mutation_verification_fields() {
        let (fixture, runtime) = fixture_runtime_for("python_workspace_noise");
        let case = fixture
            .manifest()
            .route_cases
            .iter()
            .find(|case| case.name == "debug_worker_python_init_auth_with_api_noise")
            .expect("debug case");
        let value = run_route_case(&runtime, case);

        let verification = value.get("verification").expect("verification");
        assert_eq!(
            verification.get("mutation_state").and_then(|v| v.as_str()),
            Some("not_applicable")
        );
        assert!(
            verification.get("mutation_bundle").is_none(),
            "healthy read-only routes should omit empty mutation bundle metadata"
        );
        assert!(
            verification.get("mutation_ready_without_retry").is_none(),
            "healthy read-only routes should omit empty mutation readiness metadata"
        );
        assert!(
            verification.get("mutation_block_reason").is_none(),
            "healthy read-only routes should omit null mutation block reasons"
        );
        assert!(
            verification.get("exact_impact_scope_alignment").is_none(),
            "healthy read-only routes should omit null impact-scope alignment checks"
        );
        assert!(
            verification.get("exact_impact_scope_graph_complete").is_none(),
            "healthy read-only routes should omit null impact-scope completeness checks"
        );
        assert!(
            verification.get("retrieval_strategy").is_none(),
            "healthy read-only routes should omit duplicate verification retrieval strategy"
        );
    }

    #[test]
    fn healthy_read_only_routes_trim_default_top_level_response_fields() {
        let (fixture, runtime) = fixture_runtime_for("python_workspace_noise");
        let case = fixture
            .manifest()
            .route_cases
            .iter()
            .find(|case| case.name == "debug_worker_python_init_auth_with_api_noise")
            .expect("debug case");
        let value = run_route_case(&runtime, case);

        assert!(
            value.get("mapping_mode").is_none(),
            "healthy read-only routes should omit default mapping mode"
        );
        assert!(
            value.get("reuse_session_context").is_none(),
            "healthy read-only routes should omit default session reuse flag"
        );
        assert!(
            value.get("reused_context_count").is_none(),
            "healthy read-only routes should omit zero reused-context counts"
        );
        assert!(
            value.get("refs_unchanged").is_none(),
            "healthy read-only routes should omit false refs_unchanged markers"
        );
        assert!(
            value.get("test_coverage_suppressed").is_none(),
            "healthy read-only routes should omit false test-coverage suppression markers"
        );
        assert!(
            value.get("impact_scope").is_none(),
            "healthy read-only routes should omit top-level impact scope when mutation checks are not applicable"
        );
    }

    #[test]
    fn manifest_route_cases_hold_for_python_workspace_noise_fixture() {
        let (fixture, runtime) = fixture_runtime_for("python_workspace_noise");
        for case in &fixture.manifest().route_cases {
            let value = run_route_case(&runtime, case);
            assert_route_case(&value, case);
        }
    }

    #[test]
    fn autoroute_keeps_api_python_context_clean_after_config_rename() {
        let (fixture, runtime) = fixture_runtime_for("python_workspace_noise");
        let repo = fixture.repo_root().to_path_buf();

        fs::rename(
            repo.join("packages").join("api").join("config.py"),
            repo.join("packages").join("api").join("runtime_config.py"),
        )
        .expect("rename api config file");
        fs::write(
            repo.join("packages").join("api").join("auth_flow.py"),
            concat!(
                "from runtime_config import load_config\n\n\n",
                "def init_auth():\n",
                "    return load_config()\n",
            ),
        )
        .expect("rewrite api auth flow");
        fs::write(
            repo.join("packages").join("api").join("test_auth.py"),
            concat!(
                "from auth_flow import init_auth\n\n\n",
                "def test_init_auth_uses_api_config():\n",
                "    assert init_auth()[\"source\"] == \"api\"\n",
            ),
        )
        .expect("rewrite api auth test");

        let indexer = runtime.indexer();
        let mut indexer = indexer.lock();
        indexer
            .delete_file("packages/api/config.py")
            .expect("delete old api config path from index");
        indexer
            .index_file(&repo, "packages/api/runtime_config.py")
            .expect("index renamed api config file");
        indexer
            .index_file(&repo, "packages/api/auth_flow.py")
            .expect("reindex api auth flow");
        indexer
            .index_file(&repo, "packages/api/test_auth.py")
            .expect("reindex api auth test");
        drop(indexer);

        let value = runtime.handle_autoroute(IdeAutoRouteRequest {
            task: Some("debug api init auth config failure".to_string()),
            include_summary: Some(true),
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
            reuse_session_context: Some(false),
            auto_minimal_raw: None,
        });

        let context = value
            .get("result")
            .and_then(|v| v.get("context"))
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        assert!(
            context.iter().all(|item| {
                item.get("file").and_then(|v| v.as_str()) != Some("packages/api/config.py")
            }),
            "autoroute should not leak stale api python config file path after rename"
        );
        assert!(
            context.iter().any(|item| {
                item.get("file").and_then(|v| v.as_str()) == Some("packages/api/auth_flow.py")
            }),
            "autoroute should stay anchored to the api python auth flow after rename"
        );
    }

    #[test]
    fn mutation_route_stays_exact_ready_after_api_python_config_rename() {
        let (fixture, runtime) = fixture_runtime_for("python_workspace_noise");
        let repo = fixture.repo_root().to_path_buf();

        fs::rename(
            repo.join("packages").join("api").join("config.py"),
            repo.join("packages").join("api").join("runtime_config.py"),
        )
        .expect("rename api config file");
        fs::write(
            repo.join("packages").join("api").join("auth_flow.py"),
            concat!(
                "from runtime_config import load_config\n\n\n",
                "def init_auth():\n",
                "    return load_config()\n",
            ),
        )
        .expect("rewrite api auth flow");
        fs::write(
            repo.join("packages").join("api").join("test_auth.py"),
            concat!(
                "from auth_flow import init_auth\n\n\n",
                "def test_init_auth_uses_api_config():\n",
                "    assert init_auth()[\"source\"] == \"api\"\n",
            ),
        )
        .expect("rewrite api auth test");

        let indexer = runtime.indexer();
        let mut indexer = indexer.lock();
        indexer
            .delete_file("packages/api/config.py")
            .expect("delete old api config path from index");
        indexer
            .index_file(&repo, "packages/api/runtime_config.py")
            .expect("index renamed api config file");
        indexer
            .index_file(&repo, "packages/api/auth_flow.py")
            .expect("reindex api auth flow");
        indexer
            .index_file(&repo, "packages/api/test_auth.py")
            .expect("reindex api auth test");
        drop(indexer);

        let value = runtime.handle_autoroute(IdeAutoRouteRequest {
            task: Some("implement stronger api init auth config validation".to_string()),
            include_summary: Some(false),
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
            reuse_session_context: Some(false),
            auto_minimal_raw: None,
        });

        let verification = value.get("verification").expect("verification");
        assert_eq!(
            verification.get("mutation_state").and_then(|v| v.as_str()),
            Some("ready")
        );
        assert_eq!(
            verification
                .get("mutation_bundle")
                .and_then(|v| v.get("status"))
                .and_then(|v| v.as_str()),
            Some("exact_ready")
        );
        assert_eq!(
            verification
                .get("mutation_ready_without_retry")
                .and_then(|v| v.as_bool()),
            Some(true)
        );
        assert_eq!(
            verification
                .get("exact_impact_scope_alignment")
                .and_then(|v| v.as_bool()),
            Some(true)
        );
        assert_eq!(
            verification
                .get("exact_impact_scope_graph_alignment")
                .and_then(|v| v.as_bool()),
            Some(true)
        );
        assert_eq!(
            verification
                .get("impact_scope_graph_details")
                .and_then(|v| v.as_object())
                .map(|_| true),
            None
        );
        let context = value
            .get("result")
            .and_then(|v| v.get("context"))
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        assert!(
            context.iter().all(|item| {
                item.get("file").and_then(|v| v.as_str()) != Some("packages/api/config.py")
            }),
            "mutation route should not leak stale api python config path after rename"
        );
    }

    #[test]
    fn autoroute_keeps_worker_python_context_clean_after_config_rename() {
        let (fixture, runtime) = fixture_runtime_for("python_workspace_noise");
        let repo = fixture.repo_root().to_path_buf();

        fs::rename(
            repo.join("packages").join("worker").join("config.py"),
            repo.join("packages").join("worker").join("runtime_config.py"),
        )
        .expect("rename worker config file");
        fs::write(
            repo.join("packages").join("worker").join("auth_flow.py"),
            concat!(
                "from runtime_config import load_config\n\n\n",
                "def init_auth():\n",
                "    return load_config()\n",
            ),
        )
        .expect("rewrite worker auth flow");
        fs::write(
            repo.join("packages").join("worker").join("test_auth.py"),
            concat!(
                "from auth_flow import init_auth\n\n\n",
                "def test_init_auth_uses_worker_config():\n",
                "    assert init_auth()[\"source\"] == \"worker\"\n",
            ),
        )
        .expect("rewrite worker auth test");

        let indexer = runtime.indexer();
        let mut indexer = indexer.lock();
        indexer
            .delete_file("packages/worker/config.py")
            .expect("delete old worker config path from index");
        indexer
            .index_file(&repo, "packages/worker/runtime_config.py")
            .expect("index renamed worker config file");
        indexer
            .index_file(&repo, "packages/worker/auth_flow.py")
            .expect("reindex worker auth flow");
        indexer
            .index_file(&repo, "packages/worker/test_auth.py")
            .expect("reindex worker auth test");
        drop(indexer);

        let value = runtime.handle_autoroute(IdeAutoRouteRequest {
            task: Some("debug worker init auth config failure".to_string()),
            include_summary: Some(true),
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
            reuse_session_context: Some(false),
            auto_minimal_raw: None,
        });

        let context = value
            .get("result")
            .and_then(|v| v.get("context"))
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        assert!(
            context.iter().all(|item| {
                item.get("file").and_then(|v| v.as_str()) != Some("packages/worker/config.py")
            }),
            "autoroute should not leak stale worker python config file path after rename"
        );
        assert!(
            context.iter().all(|item| {
                item.get("file").and_then(|v| v.as_str()) != Some("packages/api/config.py")
            }),
            "autoroute should not drift into api python config path after worker rename"
        );
        assert!(
            context.iter().any(|item| {
                item.get("file").and_then(|v| v.as_str()) == Some("packages/worker/auth_flow.py")
            }),
            "autoroute should stay anchored to the worker python auth flow after rename"
        );
    }

    #[test]
    fn autoroute_drops_stale_workspace_paths_after_package_file_rename() {
        let (fixture, runtime) = fixture_runtime_for("workspace_path_collisions");
        let repo = fixture.repo_root().to_path_buf();

        fs::rename(
            repo.join("packages").join("worker").join("src").join("auth").join("init.ts"),
            repo.join("packages")
                .join("worker")
                .join("src")
                .join("auth")
                .join("bootstrap.ts"),
        )
        .expect("rename worker auth file");
        fs::write(
            repo.join("packages")
                .join("worker")
                .join("tests")
                .join("auth.spec.ts"),
            concat!(
                "import { runWorker } from \"../src/auth/bootstrap\";\n\n",
                "describe(\"runWorker\", () => {\n",
                "  it(\"accepts worker job ids\", () => {\n",
                "    expect(runWorker(\"job_123\")).toBeTruthy();\n",
                "  });\n",
                "});\n",
            ),
        )
        .expect("rewrite worker test import");

        let indexer = runtime.indexer();
        let mut indexer = indexer.lock();
        indexer
            .delete_file("packages/worker/src/auth/init.ts")
            .expect("delete old worker auth file from index");
        indexer
            .index_file(&repo, "packages/worker/src/auth/bootstrap.ts")
            .expect("index renamed worker auth file");
        indexer
            .index_file(&repo, "packages/worker/tests/auth.spec.ts")
            .expect("reindex worker test");
        drop(indexer);

        let value = runtime.handle_autoroute(IdeAutoRouteRequest {
            task: Some("fix worker runWorker queue init bug".to_string()),
            include_summary: None,
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
            reuse_session_context: Some(false),
            auto_minimal_raw: None,
        });

        assert_eq!(value.get("ok").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(
            value.get("intent").and_then(|v| v.as_str()),
            Some("debug")
        );
        assert_eq!(
            value.get("result")
                .and_then(|v| v.get("symbol"))
                .and_then(|v| v.as_str()),
            Some("runWorker")
        );

        let context = value
            .get("result")
            .and_then(|v| v.get("context"))
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        assert!(
            context.iter().any(|item| {
                item.get("file").and_then(|v| v.as_str())
                    == Some("packages/worker/src/auth/bootstrap.ts")
            }),
            "autoroute context should point to renamed worker auth file"
        );
        assert!(
            context.iter().all(|item| {
                item.get("file").and_then(|v| v.as_str())
                    != Some("packages/worker/src/auth/init.ts")
            }),
            "autoroute context should not leak stale worker auth path"
        );
    }

    #[test]
    fn mutation_route_stays_exact_ready_after_workspace_auth_rename() {
        let (fixture, runtime) = fixture_runtime_for("workspace_path_collisions");
        let repo = fixture.repo_root().to_path_buf();

        fs::rename(
            repo.join("packages").join("worker").join("src").join("auth").join("init.ts"),
            repo.join("packages")
                .join("worker")
                .join("src")
                .join("auth")
                .join("bootstrap.ts"),
        )
        .expect("rename worker auth file");
        fs::write(
            repo.join("packages")
                .join("worker")
                .join("tests")
                .join("auth.spec.ts"),
            concat!(
                "import { runWorker } from \"../src/auth/bootstrap\";\n\n",
                "describe(\"runWorker\", () => {\n",
                "  it(\"accepts worker job ids\", () => {\n",
                "    expect(runWorker(\"job_123\")).toBeTruthy();\n",
                "  });\n",
                "});\n",
            ),
        )
        .expect("rewrite worker test import");

        let indexer = runtime.indexer();
        let mut indexer = indexer.lock();
        indexer
            .delete_file("packages/worker/src/auth/init.ts")
            .expect("delete old worker auth file from index");
        indexer
            .index_file(&repo, "packages/worker/src/auth/bootstrap.ts")
            .expect("index renamed worker auth file");
        indexer
            .index_file(&repo, "packages/worker/tests/auth.spec.ts")
            .expect("reindex worker test");
        drop(indexer);

        let value = runtime.handle_autoroute(IdeAutoRouteRequest {
            task: Some("implement stronger worker runWorker init validation".to_string()),
            include_summary: Some(false),
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
            reuse_session_context: Some(false),
            auto_minimal_raw: None,
        });

        assert_eq!(
            value.get("result")
                .and_then(|v| v.get("symbol"))
                .and_then(|v| v.as_str()),
            Some("runWorker")
        );
        let verification = value.get("verification").expect("verification");
        assert_eq!(
            verification.get("mutation_state").and_then(|v| v.as_str()),
            Some("ready")
        );
        assert_eq!(
            verification
                .get("mutation_bundle")
                .and_then(|v| v.get("status"))
                .and_then(|v| v.as_str()),
            Some("exact_ready")
        );
        assert_eq!(
            verification
                .get("mutation_ready_without_retry")
                .and_then(|v| v.as_bool()),
            Some(true)
        );
        assert_eq!(
            verification
                .get("exact_impact_scope_alignment")
                .and_then(|v| v.as_bool()),
            Some(true)
        );
        assert_eq!(
            verification
                .get("exact_impact_scope_graph_alignment")
                .and_then(|v| v.as_bool()),
            Some(true)
        );
        assert_eq!(
            verification
                .get("top_context_file")
                .and_then(|v| v.as_str()),
            Some("packages/worker/src/auth/bootstrap.ts")
        );
    }

    #[test]
    fn autoroute_keeps_worker_mixed_module_context_clean_after_commonjs_rename() {
        let (fixture, runtime) = fixture_runtime_for("workspace_mixed_module_noise");
        let repo = fixture.repo_root().to_path_buf();

        fs::rename(
            repo.join("packages")
                .join("worker")
                .join("src")
                .join("shared")
                .join("commonjsConfig.js"),
            repo.join("packages")
                .join("worker")
                .join("src")
                .join("shared")
                .join("runtimeConfig.js"),
        )
        .expect("rename worker commonjs file");
        fs::write(
            repo.join("packages")
                .join("worker")
                .join("src")
                .join("auth")
                .join("flow.ts"),
            concat!(
                "const configModule = require(\"../shared/runtimeConfig\");\n\n",
                "export function initAuth() {\n",
                "  return configModule.loadConfig();\n",
                "}\n",
            ),
        )
        .expect("rewrite worker auth flow");

        let indexer = runtime.indexer();
        let mut indexer = indexer.lock();
        indexer
            .delete_file("packages/worker/src/shared/commonjsConfig.js")
            .expect("delete old worker commonjs path from index");
        indexer
            .index_file(&repo, "packages/worker/src/shared/runtimeConfig.js")
            .expect("index renamed worker commonjs file");
        indexer
            .index_file(&repo, "packages/worker/src/auth/flow.ts")
            .expect("reindex worker auth flow");
        drop(indexer);

        let value = runtime.handle_autoroute(IdeAutoRouteRequest {
            task: Some("debug worker initAuth commonjs config failure".to_string()),
            include_summary: Some(true),
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
            reuse_session_context: Some(false),
            auto_minimal_raw: None,
        });

        assert_eq!(value.get("ok").and_then(|v| v.as_bool()), Some(true));

        let context = value
            .get("result")
            .and_then(|v| v.get("context"))
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        assert!(
            context.iter().all(|item| {
                item.get("file").and_then(|v| v.as_str())
                    != Some("packages/worker/src/shared/commonjsConfig.js")
            }),
            "autoroute should not leak stale worker commonjs file path after rename"
        );
        assert!(
            context.iter().all(|item| {
                item.get("file").and_then(|v| v.as_str())
                    != Some("packages/api/src/auth/flow.ts")
            }),
            "autoroute should keep worker debug route out of api package after rename"
        );
    }

    #[test]
    fn mutation_route_stays_exact_ready_after_worker_commonjs_rename() {
        let (fixture, runtime) = fixture_runtime_for("workspace_mixed_module_noise");
        let repo = fixture.repo_root().to_path_buf();

        fs::rename(
            repo.join("packages")
                .join("worker")
                .join("src")
                .join("shared")
                .join("commonjsConfig.js"),
            repo.join("packages")
                .join("worker")
                .join("src")
                .join("shared")
                .join("runtimeConfig.js"),
        )
        .expect("rename worker commonjs file");
        fs::write(
            repo.join("packages")
                .join("worker")
                .join("src")
                .join("auth")
                .join("flow.ts"),
            concat!(
                "const configModule = require(\"../shared/runtimeConfig\");\n\n",
                "export function initAuth() {\n",
                "  return configModule.loadConfig();\n",
                "}\n",
            ),
        )
        .expect("rewrite worker auth flow");

        let indexer = runtime.indexer();
        let mut indexer = indexer.lock();
        indexer
            .delete_file("packages/worker/src/shared/commonjsConfig.js")
            .expect("delete old worker commonjs path from index");
        indexer
            .index_file(&repo, "packages/worker/src/shared/runtimeConfig.js")
            .expect("index renamed worker commonjs file");
        indexer
            .index_file(&repo, "packages/worker/src/auth/flow.ts")
            .expect("reindex worker auth flow");
        drop(indexer);

        let value = runtime.handle_autoroute(IdeAutoRouteRequest {
            task: Some("refactor worker initAuth commonjs config flow".to_string()),
            include_summary: Some(false),
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
            reuse_session_context: Some(false),
            auto_minimal_raw: None,
        });

        assert_eq!(
            value.get("result")
                .and_then(|v| v.get("symbol"))
                .and_then(|v| v.as_str()),
            Some("initAuth")
        );
        let verification = value.get("verification").expect("verification");
        assert_eq!(
            verification.get("mutation_state").and_then(|v| v.as_str()),
            Some("ready")
        );
        assert_eq!(
            verification
                .get("mutation_bundle")
                .and_then(|v| v.get("status"))
                .and_then(|v| v.as_str()),
            Some("exact_ready")
        );
        assert_eq!(
            verification
                .get("mutation_ready_without_retry")
                .and_then(|v| v.as_bool()),
            Some(true)
        );
        assert_eq!(
            verification
                .get("exact_impact_scope_alignment")
                .and_then(|v| v.as_bool()),
            Some(true)
        );
        assert_eq!(
            verification
                .get("exact_impact_scope_graph_alignment")
                .and_then(|v| v.as_bool()),
            Some(true)
        );
        assert_eq!(
            verification
                .get("impact_scope_graph_details")
                .and_then(|v| v.as_object())
                .map(|_| true),
            None
        );
        let context = value
            .get("result")
            .and_then(|v| v.get("context"))
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        assert!(
            context.iter().all(|item| {
                item.get("file").and_then(|v| v.as_str())
                    != Some("packages/worker/src/shared/commonjsConfig.js")
            }),
            "mutation route should not leak stale worker commonjs file path after rename"
        );
    }

    #[test]
    fn autoroute_keeps_api_test_config_context_clean_after_config_rename() {
        let (fixture, runtime) = fixture_runtime_for("workspace_mixed_module_with_tests");
        let repo = fixture.repo_root().to_path_buf();

        fs::rename(
            repo.join("packages")
                .join("api")
                .join("src")
                .join("shared")
                .join("loadConfig.ts"),
            repo.join("packages")
                .join("api")
                .join("src")
                .join("shared")
                .join("runtimeConfig.ts"),
        )
        .expect("rename api config file");
        fs::write(
            repo.join("packages")
                .join("api")
                .join("src")
                .join("auth")
                .join("flow.ts"),
            concat!(
                "import { loadConfig } from \"../shared/runtimeConfig\";\n\n",
                "export function initAuth() {\n",
                "  return loadConfig();\n",
                "}\n",
            ),
        )
        .expect("rewrite api auth flow");
        fs::write(
            repo.join("packages")
                .join("api")
                .join("tests")
                .join("auth.spec.ts"),
            concat!(
                "import { initAuth } from \"../src/auth/flow\";\n\n",
                "describe(\"api initAuth\", () => {\n",
                "  it(\"uses api config\", () => {\n",
                "    expect(initAuth()).toBeTruthy();\n",
                "  });\n",
                "});\n",
            ),
        )
        .expect("rewrite api auth test");

        let indexer = runtime.indexer();
        let mut indexer = indexer.lock();
        indexer
            .delete_file("packages/api/src/shared/loadConfig.ts")
            .expect("delete old api config path from index");
        indexer
            .index_file(&repo, "packages/api/src/shared/runtimeConfig.ts")
            .expect("index renamed api config file");
        indexer
            .index_file(&repo, "packages/api/src/auth/flow.ts")
            .expect("reindex api auth flow");
        indexer
            .index_file(&repo, "packages/api/tests/auth.spec.ts")
            .expect("reindex api auth test");
        drop(indexer);

        let value = runtime.handle_autoroute(IdeAutoRouteRequest {
            task: Some("explain api initAuth config flow".to_string()),
            include_summary: Some(true),
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
            reuse_session_context: Some(false),
            auto_minimal_raw: None,
        });

        let context = value
            .get("result")
            .and_then(|v| v.get("context"))
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        assert!(
            context.iter().all(|item| {
                item.get("file").and_then(|v| v.as_str())
                    != Some("packages/api/src/shared/loadConfig.ts")
            }),
            "autoroute should not leak stale api config file path after rename"
        );
    }

    #[test]
    fn mutation_route_stays_exact_ready_after_api_test_config_rename() {
        let (fixture, runtime) = fixture_runtime_for("workspace_mixed_module_with_tests");
        let repo = fixture.repo_root().to_path_buf();

        fs::rename(
            repo.join("packages")
                .join("api")
                .join("src")
                .join("shared")
                .join("loadConfig.ts"),
            repo.join("packages")
                .join("api")
                .join("src")
                .join("shared")
                .join("runtimeConfig.ts"),
        )
        .expect("rename api config file");
        fs::write(
            repo.join("packages")
                .join("api")
                .join("src")
                .join("auth")
                .join("flow.ts"),
            concat!(
                "import { loadConfig } from \"../shared/runtimeConfig\";\n\n",
                "export function initAuth() {\n",
                "  return loadConfig();\n",
                "}\n",
            ),
        )
        .expect("rewrite api auth flow");
        fs::write(
            repo.join("packages")
                .join("api")
                .join("tests")
                .join("auth.spec.ts"),
            concat!(
                "import { initAuth } from \"../src/auth/flow\";\n\n",
                "describe(\"api initAuth\", () => {\n",
                "  it(\"uses api config\", () => {\n",
                "    expect(initAuth()).toBeTruthy();\n",
                "  });\n",
                "});\n",
            ),
        )
        .expect("rewrite api auth test");

        let indexer = runtime.indexer();
        let mut indexer = indexer.lock();
        indexer
            .delete_file("packages/api/src/shared/loadConfig.ts")
            .expect("delete old api config path from index");
        indexer
            .index_file(&repo, "packages/api/src/shared/runtimeConfig.ts")
            .expect("index renamed api config file");
        indexer
            .index_file(&repo, "packages/api/src/auth/flow.ts")
            .expect("reindex api auth flow");
        indexer
            .index_file(&repo, "packages/api/tests/auth.spec.ts")
            .expect("reindex api auth test");
        drop(indexer);

        let value = runtime.handle_autoroute(IdeAutoRouteRequest {
            task: Some("implement stronger api initAuth auth config validation".to_string()),
            include_summary: Some(false),
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
            reuse_session_context: Some(false),
            auto_minimal_raw: None,
        });

        assert_eq!(
            value.get("result")
                .and_then(|v| v.get("symbol"))
                .and_then(|v| v.as_str()),
            Some("initAuth")
        );
        let verification = value.get("verification").expect("verification");
        assert_eq!(
            verification.get("mutation_state").and_then(|v| v.as_str()),
            Some("ready")
        );
        assert_eq!(
            verification
                .get("mutation_bundle")
                .and_then(|v| v.get("status"))
                .and_then(|v| v.as_str()),
            Some("exact_ready")
        );
        assert_eq!(
            verification
                .get("mutation_ready_without_retry")
                .and_then(|v| v.as_bool()),
            Some(true)
        );
        assert_eq!(
            verification
                .get("exact_impact_scope_alignment")
                .and_then(|v| v.as_bool()),
            Some(true)
        );
        assert_eq!(
            verification
                .get("exact_impact_scope_graph_alignment")
                .and_then(|v| v.as_bool()),
            Some(true)
        );
        let context = value
            .get("result")
            .and_then(|v| v.get("context"))
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        assert!(
            context.iter().all(|item| {
                item.get("file").and_then(|v| v.as_str())
                    != Some("packages/api/src/shared/loadConfig.ts")
            }),
            "mutation route should not leak stale api config file path after rename"
        );
    }

    #[test]
    fn autoroute_keeps_worker_test_config_context_clean_after_commonjs_rename() {
        let (fixture, runtime) = fixture_runtime_for("workspace_mixed_module_with_tests");
        let repo = fixture.repo_root().to_path_buf();

        fs::rename(
            repo.join("packages")
                .join("worker")
                .join("src")
                .join("shared")
                .join("commonjsConfig.js"),
            repo.join("packages")
                .join("worker")
                .join("src")
                .join("shared")
                .join("runtimeCommonjsConfig.js"),
        )
        .expect("rename worker commonjs file");
        fs::write(
            repo.join("packages")
                .join("worker")
                .join("src")
                .join("auth")
                .join("flow.ts"),
            concat!(
                "const configModule = require(\"../shared/runtimeCommonjsConfig\");\n\n",
                "export function initAuth() {\n",
                "  return configModule.loadConfig();\n",
                "}\n",
            ),
        )
        .expect("rewrite worker auth flow");
        fs::write(
            repo.join("packages")
                .join("worker")
                .join("tests")
                .join("auth.spec.ts"),
            concat!(
                "import { initAuth } from \"../src/auth/flow\";\n\n",
                "describe(\"worker initAuth\", () => {\n",
                "  it(\"uses worker config\", () => {\n",
                "    expect(initAuth()).toBeTruthy();\n",
                "  });\n",
                "});\n",
            ),
        )
        .expect("rewrite worker auth test");

        let indexer = runtime.indexer();
        let mut indexer = indexer.lock();
        indexer
            .delete_file("packages/worker/src/shared/commonjsConfig.js")
            .expect("delete old worker commonjs path from index");
        indexer
            .index_file(&repo, "packages/worker/src/shared/runtimeCommonjsConfig.js")
            .expect("index renamed worker commonjs file");
        indexer
            .index_file(&repo, "packages/worker/src/auth/flow.ts")
            .expect("reindex worker auth flow");
        indexer
            .index_file(&repo, "packages/worker/tests/auth.spec.ts")
            .expect("reindex worker auth test");
        drop(indexer);

        let value = runtime.handle_autoroute(IdeAutoRouteRequest {
            task: Some("debug worker initAuth commonjs config failure".to_string()),
            include_summary: Some(true),
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
            reuse_session_context: Some(false),
            auto_minimal_raw: None,
        });

        let context = value
            .get("result")
            .and_then(|v| v.get("context"))
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        assert!(
            context.iter().all(|item| {
                item.get("file").and_then(|v| v.as_str())
                    != Some("packages/worker/src/shared/commonjsConfig.js")
            }),
            "autoroute should not leak stale worker commonjs file path after rename"
        );
    }

    #[test]
    fn manifest_route_cases_hold_for_workspace_shared_file_noise_fixture() {
        let (fixture, runtime) = fixture_runtime_for("workspace_shared_file_noise");
        for case in &fixture.manifest().route_cases {
            let value = run_route_case(&runtime, case);
            assert_route_case(&value, case);
        }
    }
}


