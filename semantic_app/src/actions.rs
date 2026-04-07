use crate::models::{
    ABTestDevRequest, AnalyzePipelineRequest, ApplyTestsRequest, DebugFailureRequest,
    EditRequestBody, EvolutionPlanRequest, EvolutionQuery, GenerateTestsRequest, MCPSettingsUpdate,
    OrgRefactorRequest, PatchMemoryQuery, ProjectSummaryQuery, SeedTodoRequest,
    SemanticMiddlewareUpdate, SymbolHintsQuery,
};
use crate::retrieve::success;
use crate::runtime::AppRuntime;
use crate::session::{now_epoch_s, SESSION_TTL_SECS};
use engine::{Operation, RetrievalRequest};
use serde_json::json;
use std::thread;
use storage::default_paths;

impl AppRuntime {
    pub fn handle_edit(&self, body: EditRequestBody) -> serde_json::Value {
        {
            let middleware = self.middleware();
            let mut middleware = middleware.lock();
            let now = now_epoch_s();
            middleware
                .sessions
                .retain(|_, value| now.saturating_sub(value.last_seen_epoch_s) <= SESSION_TTL_SECS);
            if middleware.semantic_first_enabled {
                let session_id = body.session_id.as_deref().unwrap_or_default();
                if session_id.is_empty() || !middleware.sessions.contains_key(session_id) {
                    return json!({
                        "ok": false,
                        "error": "semantic-first middleware blocked edit: call /retrieve first with a session_id, then retry /edit with the same session_id."
                    });
                }
            }
        }

        let request = RetrievalRequest {
            operation: Operation::PlanSafeEdit,
            name: Some(body.symbol),
            max_tokens: Some(body.max_tokens.unwrap_or(4000)),
            edit_description: Some(body.edit),
            patch_mode: body.patch_mode,
            run_tests: body.run_tests,
            ..Default::default()
        };
        match self.retrieval().lock().handle(request) {
            Ok(response) => success(response),
            Err(err) => json!({"ok": false, "error": err.to_string()}),
        }
    }

    pub fn get_semantic_middleware(&self) -> serde_json::Value {
        let middleware = self.middleware();
        let mut middleware = middleware.lock();
        let now = now_epoch_s();
        middleware
            .sessions
            .retain(|_, value| now.saturating_sub(value.last_seen_epoch_s) <= SESSION_TTL_SECS);
        json!({
            "ok": true,
            "semantic_first_enabled": middleware.semantic_first_enabled,
            "tracked_sessions": middleware.sessions.len()
        })
    }

    pub fn update_semantic_middleware(&self, body: SemanticMiddlewareUpdate) -> serde_json::Value {
        let middleware = self.middleware();
        let mut middleware = middleware.lock();
        middleware.semantic_first_enabled = body.semantic_first_enabled;
        if !body.semantic_first_enabled {
            middleware.sessions.clear();
        }
        json!({
            "ok": true,
            "semantic_first_enabled": middleware.semantic_first_enabled,
            "tracked_sessions": middleware.sessions.len()
        })
    }

    pub fn get_patch_memory(&self, query: PatchMemoryQuery) -> serde_json::Value {
        wrap_result(self.retrieval().lock().get_patch_memory(
            query.repository,
            query.symbol,
            query.model,
            parse_time_range(&query.time_range),
        ))
    }

    pub fn get_patch_stats(&self, query: PatchMemoryQuery) -> serde_json::Value {
        wrap_result(self.retrieval().lock().get_patch_stats(
            query.repository,
            query.symbol,
            query.model,
            parse_time_range(&query.time_range),
        ))
    }

    pub fn get_model_performance(&self, query: PatchMemoryQuery) -> serde_json::Value {
        wrap_result(self.retrieval().lock().get_model_performance(
            query.repository,
            query.symbol,
            query.model,
            parse_time_range(&query.time_range),
        ))
    }

    pub fn get_refactor_status(&self) -> serde_json::Value {
        match refactor_graph::RefactorExecutor::status(self.repo_root()) {
            Ok(result) => json!({"ok": true, "result": result}),
            Err(err) => json!({"ok": false, "error": err.to_string()}),
        }
    }

    pub fn get_evolution_issues(&self, query: EvolutionQuery) -> serde_json::Value {
        let repository = query.repository.unwrap_or_else(|| "default".to_string());
        wrap_result(self.retrieval().lock().get_evolution_issues(&repository))
    }

    pub fn get_evolution_plans(&self, query: EvolutionQuery) -> serde_json::Value {
        let repository = query.repository.unwrap_or_else(|| "default".to_string());
        wrap_result(self.retrieval().lock().get_evolution_plans(&repository))
    }

    pub fn generate_evolution_plan(&self, body: EvolutionPlanRequest) -> serde_json::Value {
        wrap_result(
            self.retrieval()
                .lock()
                .generate_evolution_plan(&body.repository, body.dry_run.unwrap_or(true)),
        )
    }

    pub fn get_organization_graph(&self) -> serde_json::Value {
        wrap_result(self.retrieval().lock().get_organization_graph())
    }

    pub fn get_service_graph(&self) -> serde_json::Value {
        wrap_result(self.retrieval().lock().get_service_graph())
    }

    pub fn plan_org_refactor(&self, body: OrgRefactorRequest) -> serde_json::Value {
        wrap_result(self.retrieval().lock().plan_org_refactor(&body.origin_repo))
    }

    pub fn get_org_refactor_status(&self) -> serde_json::Value {
        wrap_result(self.retrieval().lock().get_org_refactor_status())
    }

    pub fn debug_failure(&self, body: DebugFailureRequest) -> serde_json::Value {
        let event = debug_graph::FailureEvent {
            event_id: body.event_id,
            repository: body.repository,
            timestamp: body.timestamp,
            failure_type: parse_failure_type(&body.failure_type),
            stack_trace: body.stack_trace,
            error_message: body.error_message,
        };
        wrap_result(self.retrieval().lock().debug_failure(event))
    }

    pub fn get_debug_graph(&self) -> serde_json::Value {
        wrap_result(self.retrieval().lock().get_debug_graph())
    }

    pub fn get_root_cause_candidates(&self) -> serde_json::Value {
        wrap_result(self.retrieval().lock().get_root_cause_candidates())
    }

    pub fn get_test_gaps(&self) -> serde_json::Value {
        wrap_result(self.retrieval().lock().get_test_gaps())
    }

    pub fn generate_tests(&self, body: GenerateTestsRequest) -> serde_json::Value {
        wrap_result(self.retrieval().lock().generate_tests(
            &body.target_symbol,
            &body.framework.unwrap_or_else(|| "rust-test".to_string()),
        ))
    }

    pub fn apply_tests(&self, body: ApplyTestsRequest) -> serde_json::Value {
        wrap_result(self.retrieval().lock().apply_tests(
            &body.repository,
            &body.target_symbol,
            &body.framework.unwrap_or_else(|| "rust-test".to_string()),
        ))
    }

    pub fn get_pipeline_graph(&self) -> serde_json::Value {
        wrap_result(self.retrieval().lock().get_pipeline_graph())
    }

    pub fn analyze_pipeline(&self, body: AnalyzePipelineRequest) -> serde_json::Value {
        wrap_result(self.retrieval().lock().analyze_pipeline(
            pipeline_graph::PipelineAnalysisRequest {
                failure_stage: body.failure_stage,
                failure_message: body.failure_message,
            },
        ))
    }

    pub fn get_deployment_history(&self) -> serde_json::Value {
        wrap_result(self.retrieval().lock().get_deployment_history())
    }

    pub fn get_ab_tests(&self) -> serde_json::Value {
        wrap_result(self.retrieval().lock().get_ab_tests())
    }

    pub fn get_env_check(&self) -> serde_json::Value {
        self.retrieval().lock().load_env();
        let env_path = self.repo_root().join(".semantic").join(".env");
        json!({
            "ok": true,
            "repo_root": self.repo_root(),
            "env_path": env_path,
            "env_exists": env_path.exists(),
            "env": {
                "OPENAI_API_KEY": std::env::var("OPENAI_API_KEY").map(|v| !v.trim().is_empty()).unwrap_or(false),
                "ANTHROPIC_API_KEY": std::env::var("ANTHROPIC_API_KEY").map(|v| !v.trim().is_empty()).unwrap_or(false),
                "OPENROUTER_API_KEY": std::env::var("OPENROUTER_API_KEY").map(|v| !v.trim().is_empty()).unwrap_or(false)
            }
        })
    }

    pub fn seed_todo_tasks(&self, body: SeedTodoRequest) -> serde_json::Value {
        wrap_result(self.retrieval().lock().seed_todo_tasks(body.tasks))
    }

    pub fn get_todo_tasks(&self) -> serde_json::Value {
        wrap_result(self.retrieval().lock().get_todo_tasks())
    }

    pub fn run_ab_test_dev(&self, body: ABTestDevRequest) -> serde_json::Value {
        wrap_result(self.retrieval().lock().run_ab_test_dev(
            body.feature_request.as_deref(),
            body.provider,
            body.max_context_tokens,
            body.single_file_fast_path.unwrap_or(true),
            body.autoroute_first.unwrap_or(true),
            body.scenario.as_deref(),
        ))
    }

    pub fn get_project_summary(&self, query: ProjectSummaryQuery) -> serde_json::Value {
        let max_tokens = query.max_tokens.unwrap_or(800);
        let result = self.retrieval().lock().with_storage(|storage| {
            project_summariser::ProjectSummariser::new(storage).build(max_tokens)
        });
        match result {
            Ok(doc) => {
                let telemetry = self.retrieval().lock().telemetry();
                let mut event =
                    telemetry.event(None, "project_summary_built", "api", Some("planning"));
                event.metadata = telemetry.sanitize_metadata(json!({
                    "token_estimate": doc.token_estimate,
                    "file_count": doc.file_count,
                    "module_count": doc.module_count,
                    "cache_hit": doc.cache_hit,
                }));
                telemetry.emit(event);
                let want_markdown = query.format.as_deref() == Some("markdown");
                json!({
                    "ok": true,
                    "token_estimate": doc.token_estimate,
                    "cache_hit": doc.cache_hit,
                    "summary": doc.to_json(),
                    "summary_text": if want_markdown { doc.summary_text } else { String::new() },
                })
            }
            Err(err) => json!({"ok": false, "error": err.to_string()}),
        }
    }

    pub fn get_control_flow_hints(&self, query: SymbolHintsQuery) -> serde_json::Value {
        wrap_result(
            self.retrieval()
                .lock()
                .get_control_flow_hints(&query.symbol),
        )
    }

    pub fn get_data_flow_hints(&self, query: SymbolHintsQuery) -> serde_json::Value {
        wrap_result(self.retrieval().lock().get_data_flow_hints(&query.symbol))
    }

    pub fn get_hybrid_ranked_context(
        &self,
        body: crate::models::HybridContextRequest,
    ) -> serde_json::Value {
        wrap_result(self.retrieval().lock().get_hybrid_ranked_context(
            &body.query,
            body.max_tokens.unwrap_or(1400),
            body.single_file_fast_path.unwrap_or(true),
        ))
    }

    pub fn get_llm_tools(&self) -> serde_json::Value {
        json!({"ok": true, "result": self.retrieval().lock().get_llm_tools()})
    }

    pub fn get_mcp_settings_ui(&self) -> String {
        let llm_cfg =
            std::fs::read_to_string(self.repo_root().join(".semantic").join("llm_config.toml"))
                .unwrap_or_default();
        let routing_cfg =
            std::fs::read_to_string(self.repo_root().join(".semantic").join("llm_routing.toml"))
                .unwrap_or_default();
        let metrics = std::fs::read_to_string(
            self.repo_root()
                .join(".semantic")
                .join("model_metrics.json"),
        )
        .unwrap_or_else(|_| "{}".to_string());

        format!(
            r#"<html><head><title>MCP Settings</title></head><body>
<h2>MCP / LLM Settings</h2>
<form method="post" action="/mcp_settings_update">
<label>llm_config.toml</label><br/>
<textarea name="llm_config" rows="16" cols="120">{}</textarea><br/><br/>
<label>llm_routing.toml</label><br/>
<textarea name="llm_routing" rows="12" cols="120">{}</textarea><br/><br/>
<label>model_metrics.json</label><br/>
<textarea name="model_metrics" rows="10" cols="120">{}</textarea><br/><br/>
<p>Secrets are intentionally not shown here. Configure API keys and local bridge credentials through environment variables or private local files only.</p>
<label><input type="checkbox" name="enable_ollama" value="true"/>Add Ollama placeholder</label><br/><br/>
<button type="submit">Save</button>
</form>
</body></html>"#,
            html_escape(&llm_cfg),
            html_escape(&routing_cfg),
            html_escape(&metrics)
        )
    }

    pub fn update_mcp_settings(&self, body: MCPSettingsUpdate) -> String {
        let semantic_dir = self.repo_root().join(".semantic");
        let _ = std::fs::create_dir_all(&semantic_dir);

        let mut llm_config = body.llm_config;
        if body.enable_ollama.is_some() && !llm_config.contains("ollama") {
            llm_config.push_str(
                "\n[providers]\nollama = \"<OLLAMA_BASE_URL>\"\n\n[provider_settings.ollama]\nmodel = \"<OLLAMA_MODEL>\"\n",
            );
        }

        let _ = std::fs::write(semantic_dir.join("llm_config.toml"), llm_config);
        let _ = std::fs::write(semantic_dir.join("llm_routing.toml"), body.llm_routing);
        let _ = std::fs::write(semantic_dir.join("model_metrics.json"), body.model_metrics);
        "<html><body><h3>Saved.</h3><a href=\"/mcp_settings_ui\">Back</a></body></html>".to_string()
    }

    pub fn handle_route_action(&self, action: &str, input: serde_json::Value) -> serde_json::Value {
        match action {
            "debug_failure" => {
                let error_hint = {
                    let retrieval = self.retrieval();
                    let guard = retrieval.lock();
                    let repo_root = guard.repo_root().to_path_buf();
                    guard.with_storage(|storage| {
                        let logger = error_log::ErrorLogger::new(storage, &repo_root);
                        let _ = logger.migrate();
                        logger
                            .build_hint_block(
                                input
                                    .get("failure_type")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("runtime_exception"),
                                input
                                    .get("error_message")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or(""),
                            )
                            .ok()
                            .flatten()
                    })
                };
                let body = DebugFailureRequest {
                    event_id: input
                        .get("event_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    repository: input
                        .get("repository")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    timestamp: input.get("timestamp").and_then(|v| v.as_u64()).unwrap_or(0),
                    failure_type: input
                        .get("failure_type")
                        .and_then(|v| v.as_str())
                        .unwrap_or("runtime_exception")
                        .to_string(),
                    stack_trace: input
                        .get("stack_trace")
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|item| item.as_str().map(|s| s.to_string()))
                                .collect()
                        })
                        .unwrap_or_default(),
                    error_message: input
                        .get("error_message")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                };
                let mut result = self.debug_failure(body);
                if let Some(hint) = error_hint {
                    if let Some(obj) = result.get_mut("result").and_then(|v| v.as_object_mut()) {
                        obj.insert("prior_error_context".to_string(), hint);
                    }
                }
                json!({"ok": result.get("ok").and_then(|v| v.as_bool()).unwrap_or(false), "action": action, "result": result.get("result").cloned().unwrap_or(result)})
            }
            "generate_tests" => {
                json!({"ok": true, "action": action, "result": self.generate_tests(GenerateTestsRequest {
                target_symbol: input.get("target_symbol").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                framework: input.get("framework").and_then(|v| v.as_str()).map(|s| s.to_string()),
            }).get("result").cloned().unwrap_or(json!(null))})
            }
            "apply_tests" => {
                json!({"ok": true, "action": action, "result": self.apply_tests(ApplyTestsRequest {
                repository: input.get("repository").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                target_symbol: input.get("target_symbol").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                framework: input.get("framework").and_then(|v| v.as_str()).map(|s| s.to_string()),
            }).get("result").cloned().unwrap_or(json!(null))})
            }
            "analyze_pipeline" => {
                json!({"ok": true, "action": action, "result": self.analyze_pipeline(AnalyzePipelineRequest {
                failure_stage: input.get("failure_stage").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                failure_message: input.get("failure_message").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            }).get("result").cloned().unwrap_or(json!(null))})
            }
            "llm_tools" => {
                json!({"ok": true, "action": action, "result": self.get_llm_tools().get("result").cloned().unwrap_or(json!(null))})
            }
            "semantic_middleware_get" => {
                json!({"ok": true, "action": action, "result": self.get_semantic_middleware()})
            }
            "semantic_middleware_set" => {
                json!({"ok": true, "action": action, "result": self.update_semantic_middleware(SemanticMiddlewareUpdate {
                    semantic_first_enabled: input.get("semantic_first_enabled").and_then(|v| v.as_bool()).unwrap_or(true)
                })})
            }
            "patch_memory" => {
                json!({"ok": true, "action": action, "result": self.get_patch_memory(PatchMemoryQuery {
                repository: input.get("repository").and_then(|v| v.as_str()).map(|s| s.to_string()),
                symbol: input.get("symbol").and_then(|v| v.as_str()).map(|s| s.to_string()),
                model: input.get("model").and_then(|v| v.as_str()).map(|s| s.to_string()),
                time_range: input.get("time_range").and_then(|v| v.as_str()).map(|s| s.to_string()),
            }).get("result").cloned().unwrap_or(json!(null))})
            }
            "patch_stats" => {
                json!({"ok": true, "action": action, "result": self.get_patch_stats(PatchMemoryQuery {
                repository: input.get("repository").and_then(|v| v.as_str()).map(|s| s.to_string()),
                symbol: input.get("symbol").and_then(|v| v.as_str()).map(|s| s.to_string()),
                model: input.get("model").and_then(|v| v.as_str()).map(|s| s.to_string()),
                time_range: input.get("time_range").and_then(|v| v.as_str()).map(|s| s.to_string()),
            }).get("result").cloned().unwrap_or(json!(null))})
            }
            "model_performance" => {
                json!({"ok": true, "action": action, "result": self.get_model_performance(PatchMemoryQuery {
                repository: input.get("repository").and_then(|v| v.as_str()).map(|s| s.to_string()),
                symbol: input.get("symbol").and_then(|v| v.as_str()).map(|s| s.to_string()),
                model: input.get("model").and_then(|v| v.as_str()).map(|s| s.to_string()),
                time_range: input.get("time_range").and_then(|v| v.as_str()).map(|s| s.to_string()),
            }).get("result").cloned().unwrap_or(json!(null))})
            }
            "organization_graph" => {
                json!({"ok": true, "action": action, "result": self.get_organization_graph().get("result").cloned().unwrap_or(json!(null))})
            }
            "service_graph" => {
                json!({"ok": true, "action": action, "result": self.get_service_graph().get("result").cloned().unwrap_or(json!(null))})
            }
            "plan_org_refactor" => {
                json!({"ok": true, "action": action, "result": self.plan_org_refactor(OrgRefactorRequest {
                origin_repo: input.get("origin_repo").and_then(|v| v.as_str()).unwrap_or("").to_string()
            }).get("result").cloned().unwrap_or(json!(null))})
            }
            "org_refactor_status" => {
                json!({"ok": true, "action": action, "result": self.get_org_refactor_status().get("result").cloned().unwrap_or(json!(null))})
            }
            "refactor_status" => {
                json!({"ok": true, "action": action, "result": self.get_refactor_status().get("result").cloned().unwrap_or(json!(null))})
            }
            "evolution_issues" => {
                json!({"ok": true, "action": action, "result": self.get_evolution_issues(EvolutionQuery {
                repository: input.get("repository").and_then(|v| v.as_str()).map(|s| s.to_string())
            }).get("result").cloned().unwrap_or(json!(null))})
            }
            "evolution_plans" => {
                json!({"ok": true, "action": action, "result": self.get_evolution_plans(EvolutionQuery {
                repository: input.get("repository").and_then(|v| v.as_str()).map(|s| s.to_string())
            }).get("result").cloned().unwrap_or(json!(null))})
            }
            "generate_evolution_plan" => {
                json!({"ok": true, "action": action, "result": self.generate_evolution_plan(EvolutionPlanRequest {
                repository: input.get("repository").and_then(|v| v.as_str()).unwrap_or("default").to_string(),
                dry_run: input.get("dry_run").and_then(|v| v.as_bool()),
            }).get("result").cloned().unwrap_or(json!(null))})
            }
            "todo_seed" => {
                json!({"ok": true, "action": action, "result": self.seed_todo_tasks(SeedTodoRequest {
                tasks: input.get("tasks").and_then(|v| serde_json::from_value(v.clone()).ok()).unwrap_or_default()
            }).get("result").cloned().unwrap_or(json!(null))})
            }
            "todo_tasks" => {
                json!({"ok": true, "action": action, "result": self.get_todo_tasks().get("result").cloned().unwrap_or(json!(null))})
            }
            "ab_test_dev" => {
                json!({"ok": true, "action": action, "result": self.run_ab_test_dev(ABTestDevRequest {
                feature_request: input.get("feature_request").and_then(|v| v.as_str()).map(|s| s.to_string()),
                provider: input.get("provider").and_then(|v| v.as_str()).map(|s| s.to_string()),
                max_context_tokens: input.get("max_context_tokens").and_then(|v| v.as_u64()).map(|v| v as usize),
                single_file_fast_path: input.get("single_file_fast_path").and_then(|v| v.as_bool()),
                autoroute_first: input.get("autoroute_first").and_then(|v| v.as_bool()),
                scenario: input.get("scenario").and_then(|v| v.as_str()).map(|s| s.to_string()),
            }).get("result").cloned().unwrap_or(json!(null))})
            }
            "ab_test_dev_results" => {
                json!({"ok": true, "action": action, "result": self.get_ab_tests().get("result").cloned().unwrap_or(json!(null))})
            }
            "env_check" => json!({"ok": true, "action": action, "result": self.get_env_check()}),
            "workspace_mode_get" => {
                let workspace = self.workspace_state();
                let ws = workspace.lock();
                json!({"ok": true, "action": action, "result": {
                    "workspace_mode_enabled": ws.workspace_mode_enabled,
                    "workspace_roots": ws.workspace_roots,
                    "primary_root": ws.primary_root,
                    "note": "Toggle with action=workspace_mode_set, action_input={\"enabled\": true|false}"
                }})
            }
            "workspace_mode_set" => {
                let enabled = input
                    .get("enabled")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let workspace = self.workspace_state();
                let mut ws = workspace.lock();
                let changed = ws.workspace_mode_enabled != enabled;
                ws.workspace_mode_enabled = enabled;
                let roots = ws.workspace_roots.clone();
                let primary = ws.primary_root.clone();
                drop(ws);
                if changed {
                    let retrieval = self.retrieval();
                    let roots_for_thread = roots.clone();
                    thread::spawn(move || {
                        let (db_path, tantivy_path) = default_paths(&primary);
                        let Ok(index_storage) = storage::Storage::open(&db_path, &tantivy_path)
                        else {
                            return;
                        };
                        let mut indexer = indexer::Indexer::new(index_storage);
                        if enabled {
                            let _ = indexer.index_workspace(&primary, &roots_for_thread);
                        } else {
                            let _ = indexer.index_repo(&primary);
                        }
                        let _ = retrieval.lock().index_revision();
                    });
                }
                json!({"ok": true, "action": action, "result": {
                    "workspace_mode_enabled": enabled,
                    "indexing_triggered": changed,
                    "workspace_roots": roots,
                    "note": if enabled {
                        "Workspace indexing started in background. Retrieval will cover all projects once complete."
                    } else {
                        "Reverted to primary-root-only indexing. Re-index started in background."
                    }
                }})
            }
            _ => json!({"ok": false, "error": format!("unknown action '{action}'")}),
        }
    }
}

pub fn parse_time_range(input: &Option<String>) -> Option<(u64, u64)> {
    let value = input.as_ref()?;
    let mut parts = value.split(',');
    let from = parts.next()?.trim().parse::<u64>().ok()?;
    let to = parts.next()?.trim().parse::<u64>().ok()?;
    Some((from, to))
}

fn parse_failure_type(value: &str) -> debug_graph::FailureType {
    match value {
        "test_failure" => debug_graph::FailureType::TestFailure,
        "runtime_exception" => debug_graph::FailureType::RuntimeException,
        "build_failure" => debug_graph::FailureType::BuildFailure,
        "integration_failure" => debug_graph::FailureType::IntegrationFailure,
        _ => debug_graph::FailureType::RuntimeException,
    }
}

fn html_escape(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn wrap_result(result: anyhow::Result<serde_json::Value>) -> serde_json::Value {
    match result {
        Ok(result) => json!({"ok": true, "result": result}),
        Err(err) => json!({"ok": false, "error": err.to_string()}),
    }
}
