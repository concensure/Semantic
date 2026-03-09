use anyhow::{anyhow, Result};
use budgeter::{select_with_budget, ContextBudget, ContextItem};
use engine::{LogicNodeRecord, Operation, RetrievalRequest, RetrievalResponse, SymbolRecord, SymbolType};
use planner::Planner;
use serde_json::json;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

pub struct RetrievalService {
    repo_root: PathBuf,
    storage: storage::Storage,
}

impl RetrievalService {
    pub fn new(repo_root: PathBuf, storage: storage::Storage) -> Self {
        Self { repo_root, storage }
    }

    pub fn repo_root(&self) -> &Path {
        &self.repo_root
    }

    pub fn handle(&self, request: RetrievalRequest) -> Result<RetrievalResponse> {
        let operation = request.operation.clone();
        let result = match operation {
            Operation::GetRepoMap => self.get_repo_map()?,
            Operation::GetFileOutline => {
                let file = request.file.ok_or_else(|| anyhow!("file is required"))?;
                self.get_file_outline(&file)?
            }
            Operation::SearchSymbol => {
                let name = request.name.ok_or_else(|| anyhow!("name is required"))?;
                self.search_symbol(&name, request.limit.unwrap_or(20))?
            }
            Operation::GetFunction => {
                let name = request.name.ok_or_else(|| anyhow!("name is required"))?;
                self.get_symbol_span(&name, SymbolType::Function)?
            }
            Operation::GetClass => {
                let name = request.name.ok_or_else(|| anyhow!("name is required"))?;
                self.get_symbol_span(&name, SymbolType::Class)?
            }
            Operation::GetDependencies => {
                let name = request.name.ok_or_else(|| anyhow!("name is required"))?;
                self.get_dependencies(&name)?
            }
            Operation::GetCodeSpan => {
                let file = request.file.ok_or_else(|| anyhow!("file is required"))?;
                let start = request
                    .start_line
                    .ok_or_else(|| anyhow!("start_line is required"))?;
                let end = request
                    .end_line
                    .ok_or_else(|| anyhow!("end_line is required"))?;
                self.get_code_span(&file, start, end)?
            }
            Operation::GetLogicNodes => {
                let name = request.name.ok_or_else(|| anyhow!("name is required"))?;
                self.get_logic_nodes(&name)?
            }
            Operation::GetLogicNeighborhood => {
                let node_id = request
                    .node_id
                    .ok_or_else(|| anyhow!("node_id is required"))?;
                self.get_logic_neighborhood(node_id, request.radius.unwrap_or(1))?
            }
            Operation::GetLogicSpan => {
                let node_id = request
                    .node_id
                    .ok_or_else(|| anyhow!("node_id is required"))?;
                self.get_logic_span(node_id)?
            }
            Operation::GetDependencyNeighborhood => {
                let name = request.name.ok_or_else(|| anyhow!("name is required"))?;
                let radius = request.radius.ok_or_else(|| anyhow!("radius is required"))?;
                self.get_dependency_neighborhood(&name, radius)?
            }
            Operation::GetSymbolNeighborhood => {
                let name = request.name.ok_or_else(|| anyhow!("name is required"))?;
                let radius = request.radius.ok_or_else(|| anyhow!("radius is required"))?;
                self.get_symbol_neighborhood(&name, radius)?
            }
            Operation::GetReasoningContext => {
                let name = request.name.ok_or_else(|| anyhow!("name is required"))?;
                let logic_radius = request
                    .logic_radius
                    .ok_or_else(|| anyhow!("logic_radius is required"))?;
                let dependency_radius = request
                    .dependency_radius
                    .ok_or_else(|| anyhow!("dependency_radius is required"))?;
                self.get_reasoning_context(&name, logic_radius, dependency_radius)?
            }
            Operation::GetPlannedContext => {
                let query = request.query.ok_or_else(|| anyhow!("query is required"))?;
                let max_tokens = request
                    .max_tokens
                    .ok_or_else(|| anyhow!("max_tokens is required"))?;
                self.get_planned_context(&query, max_tokens)?
            }
            Operation::GetRepoMapHierarchy => self.get_repo_map_hierarchy()?,
            Operation::GetModuleDependencies => self.get_module_dependencies()?,
            Operation::SearchSemanticSymbol => {
                let query = request
                    .query
                    .or(request.name)
                    .ok_or_else(|| anyhow!("query or name is required"))?;
                self.search_semantic_symbol(&query, request.limit.unwrap_or(20))?
            }
            Operation::GetWorkspaceReasoningContext => {
                let query = request.query.ok_or_else(|| anyhow!("query is required"))?;
                let max_tokens = request
                    .max_tokens
                    .ok_or_else(|| anyhow!("max_tokens is required"))?;
                self.get_workspace_reasoning_context(
                    &query,
                    max_tokens,
                    request.workspace_scope.unwrap_or_default(),
                )?
            }
            Operation::PlanSafeEdit => {
                let symbol = request
                    .name
                    .or(request.query)
                    .ok_or_else(|| anyhow!("name or query(symbol) is required"))?;
                let edit_description = request
                    .edit_description
                    .ok_or_else(|| anyhow!("edit_description is required"))?;
                self.plan_safe_edit(
                    &symbol,
                    &edit_description,
                    request.max_tokens.unwrap_or(4000),
                    request.patch_mode,
                    request.run_tests.unwrap_or(false),
                )?
            }
        };

        Ok(RetrievalResponse { operation, result })
    }

    pub fn get_patch_memory(
        &self,
        repository: Option<String>,
        symbol: Option<String>,
        model: Option<String>,
        time_range: Option<(u64, u64)>,
    ) -> Result<serde_json::Value> {
        let memory = patch_memory::PatchMemory::open(&self.repo_root)?;
        let filter = patch_memory::PatchQuery {
            repository,
            symbol,
            model,
            time_range,
        };
        let records = memory.list_records(&filter)?;
        let graph = memory.export_history_graph()?;
        Ok(json!({
            "records": records,
            "graph": graph,
        }))
    }

    pub fn get_patch_stats(
        &self,
        repository: Option<String>,
        symbol: Option<String>,
        model: Option<String>,
        time_range: Option<(u64, u64)>,
    ) -> Result<serde_json::Value> {
        let memory = patch_memory::PatchMemory::open(&self.repo_root)?;
        let filter = patch_memory::PatchQuery {
            repository,
            symbol,
            model,
            time_range,
        };
        let stats = memory.stats(&filter)?;
        Ok(json!({ "stats": stats }))
    }

    pub fn get_model_performance(
        &self,
        repository: Option<String>,
        symbol: Option<String>,
        model: Option<String>,
        time_range: Option<(u64, u64)>,
    ) -> Result<serde_json::Value> {
        let memory = patch_memory::PatchMemory::open(&self.repo_root)?;
        let filter = patch_memory::PatchQuery {
            repository,
            symbol,
            model,
            time_range,
        };
        let performance = memory.model_performance(&filter)?;
        Ok(json!({ "model_performance": performance }))
    }

    pub fn get_evolution_issues(&self, repository: &str) -> Result<serde_json::Value> {
        let code_issues =
            code_health::CodeHealthAnalyzer::analyze(&self.repo_root, repository, &self.storage)?;
        let architecture_issues = architecture_analysis::ArchitectureAnalyzer::analyze(&self.storage)?;
        Ok(json!({
            "repository": repository,
            "code_health_issues": code_issues,
            "architecture_issues": architecture_issues,
        }))
    }

    pub fn get_evolution_plans(&self, repository: &str) -> Result<serde_json::Value> {
        let code_issues =
            code_health::CodeHealthAnalyzer::analyze(&self.repo_root, repository, &self.storage)?;
        let architecture_issues = architecture_analysis::ArchitectureAnalyzer::analyze(&self.storage)?;
        let plans = improvement_planner::ImprovementPlanner::from_issues(&code_issues, &architecture_issues);
        Ok(json!({
            "repository": repository,
            "plans": plans,
        }))
    }

    pub fn generate_evolution_plan(
        &self,
        repository: &str,
        dry_run: bool,
    ) -> Result<serde_json::Value> {
        let code_issues =
            code_health::CodeHealthAnalyzer::analyze(&self.repo_root, repository, &self.storage)?;
        let architecture_issues = architecture_analysis::ArchitectureAnalyzer::analyze(&self.storage)?;
        let plans = improvement_planner::ImprovementPlanner::from_issues(&code_issues, &architecture_issues);
        let graph = evolution_graph::EvolutionGraphBuilder::from_plans(&plans);
        let simulation = evolution_graph::EvolutionGraphBuilder::simulate(&graph)?;
        let risk = estimate_evolution_risk(&self.repo_root, &self.storage, plans.len())?;

        let kg = knowledge_graph::KnowledgeGraph::open(&self.repo_root)?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or_default();
        kg.append(&knowledge_graph::KnowledgeEntry {
            timestamp: now,
            category: "evolution_plan".to_string(),
            repository: repository.to_string(),
            title: format!("Generated evolution plan with {} improvements", plans.len()),
            details: format!("dry_run={dry_run}, estimated_nodes={}", graph.nodes.len()),
        })?;

        Ok(json!({
            "repository": repository,
            "issues": {
                "code_health": code_issues,
                "architecture": architecture_issues,
            },
            "plans": plans,
            "evolution_graph": graph,
            "risk": risk,
            "estimated_changes": {
                "plan_count": plans.len(),
                "node_count": simulation.estimated_node_count,
                "patch_count": simulation.estimated_patch_count,
            },
            "simulation": if dry_run { Some(simulation) } else { None },
            "dry_run": dry_run,
        }))
    }

    pub fn get_organization_graph(&self) -> Result<serde_json::Value> {
        let graph = org_graph::OrganizationGraphBuilder::build(&self.storage)?;
        let contracts = api_contract_graph::APIContractGraphBuilder::scan(&self.repo_root, &self.storage)?;
        let deps = dependency_intelligence::DependencyIntelligence::analyze(&self.storage, &graph)?;
        Ok(json!({
            "organization_graph": graph,
            "api_contracts": contracts,
            "dependency_intelligence": deps,
        }))
    }

    pub fn get_service_graph(&self) -> Result<serde_json::Value> {
        let graph = service_graph::ServiceGraphBuilder::build(&self.storage)?;
        Ok(json!({ "service_graph": graph }))
    }

    pub fn plan_org_refactor(&self, origin_repo: &str) -> Result<serde_json::Value> {
        let org = org_graph::OrganizationGraphBuilder::build(&self.storage)?;
        let deps = dependency_intelligence::DependencyIntelligence::analyze(&self.storage, &org)?;
        let propagation = change_propagation::ChangePropagationEngine::predict(origin_repo, &deps);
        let plan = organization_planner::OrganizationPlanner::plan(origin_repo, &propagation);
        let multi_repo_graph =
            organization_planner::OrganizationPlanner::build_multi_repo_refactor_graph(&plan);
        let execution = organization_planner::OrganizationPlanner::execute_distributed(
            &self.repo_root,
            &plan,
            &propagation,
        )?;
        let telemetry = organization_planner::OrganizationPlanner::read_telemetry(&self.repo_root)?;
        Ok(json!({
            "plan": plan,
            "propagation": propagation,
            "multi_repo_refactor_graph": multi_repo_graph,
            "execution": execution,
            "telemetry": telemetry,
        }))
    }

    pub fn get_org_refactor_status(&self) -> Result<serde_json::Value> {
        let status = organization_planner::OrganizationPlanner::read_status(&self.repo_root)?;
        let telemetry = organization_planner::OrganizationPlanner::read_telemetry(&self.repo_root)?;
        Ok(json!({
            "org_refactor_status": status,
            "telemetry": telemetry,
        }))
    }

    pub fn debug_failure(&self, event: debug_graph::FailureEvent) -> Result<serde_json::Value> {
        let analysis = debug_graph::DebugGraphEngine::analyze_failure(
            &self.repo_root,
            &self.storage,
            event,
        )?;
        Ok(json!({
            "debug_graph": analysis.debug_graph,
            "root_cause_candidates": analysis.candidates,
            "patch_suggestion": analysis.patch_suggestion,
            "failure_event": analysis.last_failure,
        }))
    }

    pub fn get_debug_graph(&self) -> Result<serde_json::Value> {
        let state = debug_graph::DebugGraphEngine::read_state(&self.repo_root)?;
        Ok(json!({
            "failure_event": state.last_failure,
            "debug_graph": state.debug_graph,
        }))
    }

    pub fn get_root_cause_candidates(&self) -> Result<serde_json::Value> {
        let state = debug_graph::DebugGraphEngine::read_state(&self.repo_root)?;
        Ok(json!({
            "root_cause_candidates": state.candidates,
            "patch_suggestion": state.patch_suggestion,
        }))
    }

    pub fn get_test_gaps(&self) -> Result<serde_json::Value> {
        let gaps = test_coverage::TestCoverageAnalyzer::analyze(&self.storage)?;
        Ok(json!({ "test_gaps": gaps }))
    }

    pub fn generate_tests(
        &self,
        target_symbol: &str,
        framework: &str,
    ) -> Result<serde_json::Value> {
        let symbol = self
            .storage
            .get_symbol_any(target_symbol)?
            .ok_or_else(|| anyhow!("symbol not found: {target_symbol}"))?;
        let code = read_span(
            &self.repo_root,
            &symbol.file,
            symbol.start_line,
            symbol.end_line,
        )?;
        let plan = test_planner::TestPlanner::build_plan(target_symbol, framework);
        let generated = test_planner::TestPlanner::generate_tests(&plan, framework, &code);
        Ok(json!({
            "test_plan": plan,
            "generated_tests": generated,
        }))
    }

    pub fn apply_tests(
        &self,
        repository: &str,
        target_symbol: &str,
        framework: &str,
    ) -> Result<serde_json::Value> {
        let symbol = self
            .storage
            .get_symbol_any(target_symbol)?
            .ok_or_else(|| anyhow!("symbol not found: {target_symbol}"))?;
        let code = read_span(
            &self.repo_root,
            &symbol.file,
            symbol.start_line,
            symbol.end_line,
        )?;
        let plan = test_planner::TestPlanner::build_plan(target_symbol, framework);
        let generated = test_planner::TestPlanner::generate_tests(&plan, framework, &code);
        let applied = test_planner::TestPlanner::apply_tests(
            &self.repo_root,
            repository,
            &plan,
            &generated,
        )?;
        Ok(json!({
            "test_plan": plan,
            "generated_tests": generated,
            "apply_result": applied,
        }))
    }

    pub fn get_pipeline_graph(&self) -> Result<serde_json::Value> {
        let graph = pipeline_graph::PipelineIntelligence::default_graph();
        Ok(json!({ "pipeline_graph": graph }))
    }

    pub fn analyze_pipeline(
        &self,
        request: pipeline_graph::PipelineAnalysisRequest,
    ) -> Result<serde_json::Value> {
        let result = pipeline_graph::PipelineIntelligence::analyze(&self.repo_root, &request)?;
        Ok(json!({
            "pipeline_request": request,
            "analysis": result,
        }))
    }

    pub fn get_deployment_history(&self) -> Result<serde_json::Value> {
        let deployments = pipeline_graph::PipelineIntelligence::list_deployments(&self.repo_root)?;
        Ok(json!({ "deployment_history": deployments }))
    }

    pub fn run_ab_test(
        &self,
        prompt: &str,
        symbol: Option<String>,
        provider: Option<String>,
    ) -> Result<serde_json::Value> {
        load_env_from_file(&self.repo_root);
        let symbol_name = symbol.unwrap_or_else(|| "retryRequest".to_string());
        let lexical = self.storage.search_symbol_by_name(&symbol_name, 1)?;
        let context_snippet = if let Some(sym) = lexical.first() {
            read_span(&self.repo_root, &sym.file, sym.start_line, sym.end_line)?
        } else {
            String::new()
        };

        let routing_cfg =
            std::fs::read_to_string(self.repo_root.join(".semantic").join("llm_routing.toml"))
                .unwrap_or_default();
        let providers_cfg =
            std::fs::read_to_string(self.repo_root.join(".semantic").join("llm_config.toml"))
                .unwrap_or_default();
        let metrics_json =
            std::fs::read_to_string(self.repo_root.join(".semantic").join("model_metrics.json"))
                .unwrap_or_else(|_| "{}".to_string());
        let router = llm_router::LLMRouter::from_files(&providers_cfg, &routing_cfg, &metrics_json)?;
        let route = provider
            .map(|p| llm_router::RouteDecision {
                provider: p,
                endpoint: String::new(),
            })
            .or_else(|| router.route(llm_router::LLMTask::InteractiveChat));

        let provider_settings = parse_provider_settings(&providers_cfg);
        let selected_provider = route
            .as_ref()
            .map(|r| r.provider.clone())
            .unwrap_or_else(|| "ollama".to_string());

        let without_context_prompt = prompt.to_string();
        let with_context_prompt = if context_snippet.is_empty() {
            prompt.to_string()
        } else {
            format!(
                "Project context:\n{}\n\nUser request:\n{}",
                context_snippet, prompt
            )
        };

        let result_a = call_live_llm(
            &selected_provider,
            provider_settings.get(&selected_provider),
            route.as_ref().map(|r| r.endpoint.as_str()),
            &without_context_prompt,
            512,
        );
        let result_b = call_live_llm(
            &selected_provider,
            provider_settings.get(&selected_provider),
            route.as_ref().map(|r| r.endpoint.as_str()),
            &with_context_prompt,
            512,
        );

        let (a_tokens, a_output) = result_a
            .as_ref()
            .map(|r| (r.total_tokens, r.text.clone()))
            .unwrap_or((estimate_tokens(&without_context_prompt), String::new()));
        let (b_tokens, b_output) = result_b
            .as_ref()
            .map(|r| (r.total_tokens, r.text.clone()))
            .unwrap_or((estimate_tokens(&with_context_prompt), String::new()));
        let savings_pct = if a_tokens == 0 {
            0.0
        } else {
            ((a_tokens as f32 - b_tokens as f32) / a_tokens as f32) * 100.0
        };

        append_ab_test_csv(
            &self.repo_root,
            &ABTestRow {
                timestamp: current_ts(),
                provider: selected_provider.clone(),
                symbol: symbol_name.clone(),
                tokens_without_project: a_tokens,
                tokens_with_project: b_tokens,
                savings_pct,
            },
        )?;

        Ok(json!({
            "provider": selected_provider,
            "symbol": symbol_name,
            "without_project": {
                "tokens": a_tokens,
                "output": a_output,
            },
            "with_project": {
                "tokens": b_tokens,
                "output": b_output,
            },
            "savings_pct": savings_pct,
            "live_calls": {
                "without_project": result_a.is_some(),
                "with_project": result_b.is_some(),
            }
        }))
    }

    fn get_repo_map(&self) -> Result<serde_json::Value> {
        let files = self.storage.list_files()?;
        Ok(json!({ "files": files }))
    }

    fn get_file_outline(&self, file: &str) -> Result<serde_json::Value> {
        let symbols = self.storage.file_outline(file)?;
        Ok(json!({ "file": file, "symbols": symbols }))
    }

    fn get_repo_map_hierarchy(&self) -> Result<serde_json::Value> {
        let modules = self.storage.list_modules()?;
        let mut out = Vec::new();
        for module in modules {
            let module_id = module.id.unwrap_or_default();
            let files = self.storage.list_module_files(module_id)?;
            let mut file_entries = Vec::new();
            for file in files {
                let symbols = self.storage.file_outline(&file.file_path)?;
                file_entries.push(json!({
                    "file": file.file_path,
                    "symbols": symbols,
                }));
            }
            out.push(json!({
                "module": module.name,
                "path": module.path,
                "files": file_entries,
            }));
        }
        Ok(json!({ "modules": out }))
    }

    fn get_module_dependencies(&self) -> Result<serde_json::Value> {
        let deps = self.storage.list_named_module_dependencies()?;
        let edges: Vec<serde_json::Value> = deps
            .into_iter()
            .map(|(from, to)| json!({ "from": from, "to": to }))
            .collect();
        Ok(json!({ "edges": edges }))
    }

    fn search_symbol(&self, name: &str, limit: usize) -> Result<serde_json::Value> {
        let hits = self.storage.tantivy_search(name, limit)?;
        let fallback = self.storage.search_symbol_by_name(name, limit)?;
        Ok(json!({
            "query": name,
            "tantivy_hits": hits,
            "fallback": fallback,
        }))
    }

    fn search_semantic_symbol(&self, query: &str, limit: usize) -> Result<serde_json::Value> {
        let lexical = self.storage.search_symbol_by_name(query, limit)?;
        if !lexical.is_empty() {
            return Ok(json!({
                "query": query,
                "strategy": "lexical",
                "results": lexical
            }));
        }
        let semantic = semantic_search::SemanticSearcher::search(&self.storage, query, limit)?;
        Ok(json!({
            "query": query,
            "strategy": "semantic_fallback",
            "results": semantic
        }))
    }

    fn get_symbol_span(&self, name: &str, symbol_type: SymbolType) -> Result<serde_json::Value> {
        let sym = self
            .storage
            .get_symbol_exact(name, symbol_type)?
            .ok_or_else(|| anyhow!("symbol not found: {name}"))?;

        let code = read_span(&self.repo_root, &sym.file, sym.start_line, sym.end_line)?;
        Ok(json!({
            "name": sym.name,
            "file": sym.file,
            "start_line": sym.start_line,
            "end_line": sym.end_line,
            "code": code,
        }))
    }

    fn get_dependencies(&self, name: &str) -> Result<serde_json::Value> {
        let deps = self.storage.get_dependencies(name)?;
        Ok(json!({ "name": name, "dependencies": deps }))
    }

    fn get_code_span(&self, file: &str, start_line: u32, end_line: u32) -> Result<serde_json::Value> {
        let code = read_span(&self.repo_root, file, start_line, end_line)?;
        Ok(json!({
            "file": file,
            "start_line": start_line,
            "end_line": end_line,
            "code": code,
        }))
    }

    fn get_logic_nodes(&self, symbol_name: &str) -> Result<serde_json::Value> {
        let symbol = self
            .storage
            .get_symbol_any(symbol_name)?
            .ok_or_else(|| anyhow!("symbol not found: {symbol_name}"))?;
        let symbol_id = symbol.id.ok_or_else(|| anyhow!("symbol id missing"))?;
        let nodes = self.storage.get_logic_nodes(symbol_id)?;

        Ok(json!({
            "symbol": symbol.name,
            "symbol_id": symbol_id,
            "nodes": nodes,
        }))
    }

    fn get_logic_neighborhood(&self, node_id: i64, radius: usize) -> Result<serde_json::Value> {
        let nodes = self.storage.get_logic_neighbors(node_id, radius)?;
        Ok(json!({
            "node_id": node_id,
            "radius": radius,
            "nodes": nodes,
        }))
    }

    fn get_logic_span(&self, node_id: i64) -> Result<serde_json::Value> {
        let node = self
            .storage
            .get_logic_node(node_id)?
            .ok_or_else(|| anyhow!("logic node not found: {node_id}"))?;
        let file = self
            .storage
            .get_logic_node_file(node_id)?
            .ok_or_else(|| anyhow!("logic node file not found: {node_id}"))?;

        let start_line = node.start_line as u32;
        let end_line = node.end_line as u32;
        let code = read_span(&self.repo_root, &file, start_line, end_line)?;
        Ok(json!({
            "node_id": node_id,
            "type": node.node_type,
            "file": file,
            "start_line": start_line,
            "end_line": end_line,
            "code": code,
        }))
    }

    fn get_dependency_neighborhood(&self, symbol_name: &str, radius: usize) -> Result<serde_json::Value> {
        let symbol = self
            .storage
            .get_symbol_any(symbol_name)?
            .ok_or_else(|| anyhow!("symbol not found: {symbol_name}"))?;
        let symbol_id = symbol.id.ok_or_else(|| anyhow!("symbol id missing"))?;

        let mut neighbors = self.storage.get_dependency_neighbors(symbol_id, radius)?;
        neighbors.retain(|s| s.id != Some(symbol_id));
        sort_symbols(&mut neighbors);

        Ok(json!({
            "symbol": symbol,
            "radius": radius,
            "neighbors": neighbors,
        }))
    }

    fn get_symbol_neighborhood(&self, symbol_name: &str, radius: usize) -> Result<serde_json::Value> {
        let symbol = self
            .storage
            .get_symbol_any(symbol_name)?
            .ok_or_else(|| anyhow!("symbol not found: {symbol_name}"))?;
        let symbol_id = symbol.id.ok_or_else(|| anyhow!("symbol id missing"))?;

        let mut logic_nodes = self.storage.get_logic_nodes(symbol_id)?;
        sort_logic_nodes(&mut logic_nodes);

        let mut dependencies = self.storage.get_dependency_neighbors(symbol_id, radius)?;
        dependencies.retain(|s| s.id != Some(symbol_id));
        sort_symbols(&mut dependencies);

        Ok(json!({
            "symbol": symbol,
            "logic_nodes": logic_nodes,
            "dependency_neighbors": dependencies,
            "order": ["symbol", "logic_nodes", "dependency_neighbors"],
        }))
    }

    fn get_reasoning_context(
        &self,
        symbol_name: &str,
        logic_radius: usize,
        dependency_radius: usize,
    ) -> Result<serde_json::Value> {
        let symbol = self
            .storage
            .get_symbol_any(symbol_name)?
            .ok_or_else(|| anyhow!("symbol not found: {symbol_name}"))?;
        let symbol_id = symbol.id.ok_or_else(|| anyhow!("symbol id missing"))?;

        let mut seed_nodes = self.storage.get_logic_nodes(symbol_id)?;
        sort_logic_nodes(&mut seed_nodes);

        let mut logic_context = Vec::new();
        for node in &seed_nodes {
            if let Some(node_id) = node.id {
                let mut neighborhood = self.storage.get_logic_neighbors(node_id, logic_radius)?;
                logic_context.append(&mut neighborhood);
            }
        }
        logic_context.sort_by_key(|n| (n.id.unwrap_or_default(), n.start_line, n.end_line));
        logic_context.dedup_by_key(|n| n.id.unwrap_or_default());
        sort_logic_nodes(&mut logic_context);

        let mut dependency_symbols = self
            .storage
            .get_dependency_neighbors(symbol_id, dependency_radius)?;
        dependency_symbols.retain(|s| s.id != Some(symbol_id));
        sort_symbols(&mut dependency_symbols);

        let mut logic_spans = Vec::new();
        for node in &logic_context {
            if let Some(node_id) = node.id {
                if let Some(file) = self.storage.get_logic_node_file(node_id)? {
                    let code = read_span(
                        &self.repo_root,
                        &file,
                        node.start_line as u32,
                        node.end_line as u32,
                    )?;
                    logic_spans.push(json!({
                        "node_id": node_id,
                        "type": node.node_type,
                        "file": file,
                        "start_line": node.start_line,
                        "end_line": node.end_line,
                        "code": code,
                    }));
                }
            }
        }
        logic_spans.sort_by(|a, b| {
            let af = a.get("file").and_then(|v| v.as_str()).unwrap_or_default();
            let bf = b.get("file").and_then(|v| v.as_str()).unwrap_or_default();
            let as_line = a.get("start_line").and_then(|v| v.as_u64()).unwrap_or_default();
            let bs_line = b.get("start_line").and_then(|v| v.as_u64()).unwrap_or_default();
            af.cmp(bf).then_with(|| as_line.cmp(&bs_line))
        });

        let mut dependency_spans = Vec::new();
        for dep in &dependency_symbols {
            let code = read_span(&self.repo_root, &dep.file, dep.start_line, dep.end_line)?;
            dependency_spans.push(json!({
                "symbol": dep,
                "code": code,
            }));
        }
        dependency_spans.sort_by(|a, b| {
            let af = a
                .get("symbol")
                .and_then(|v| v.get("file"))
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let bf = b
                .get("symbol")
                .and_then(|v| v.get("file"))
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let as_line = a
                .get("symbol")
                .and_then(|v| v.get("start_line"))
                .and_then(|v| v.as_u64())
                .unwrap_or_default();
            let bs_line = b
                .get("symbol")
                .and_then(|v| v.get("start_line"))
                .and_then(|v| v.as_u64())
                .unwrap_or_default();
            af.cmp(bf).then_with(|| as_line.cmp(&bs_line))
        });

        Ok(json!({
            "symbol": symbol,
            "logic_nodes": logic_context,
            "logic_spans": logic_spans,
            "dependencies": dependency_symbols,
            "dependency_spans": dependency_spans,
            "order": ["symbol", "logic_nodes", "dependencies"],
            "logic_radius": logic_radius,
            "dependency_radius": dependency_radius,
        }))
    }

    fn get_planned_context(&self, query: &str, max_tokens: usize) -> Result<serde_json::Value> {
        let symbols = self.storage.list_symbols()?;
        let mut symbol_names: Vec<String> = symbols.into_iter().map(|s| s.name).collect();
        symbol_names.sort();
        symbol_names.dedup();

        let module_records = self.storage.list_modules()?;
        let mut file_to_module = std::collections::HashMap::new();
        for module in &module_records {
            let module_id = module.id.unwrap_or_default();
            for mf in self.storage.list_module_files(module_id)? {
                file_to_module.insert(mf.file_path, module.name.clone());
            }
        }
        let named_module_deps = self.storage.list_named_module_dependencies()?;
        let mut symbol_to_module = std::collections::HashMap::new();
        for symbol in self.storage.list_symbols()? {
            if let Some(module_name) = file_to_module.get(&symbol.file) {
                symbol_to_module.insert(symbol.name, module_name.clone());
            }
        }

        let planner = Planner::new();
        let intent = planner.detect_intent(query);
        let plan = planner
            .build_plan_with_modules(query, &symbol_names, &symbol_to_module, &named_module_deps)
            .ok_or_else(|| anyhow!("unable to determine target symbol from query"))?;

        let target_symbol = self
            .storage
            .get_symbol_any(&plan.target_symbol)?
            .ok_or_else(|| anyhow!("symbol not found: {}", plan.target_symbol))?;
        let target_id = target_symbol
            .id
            .ok_or_else(|| anyhow!("target symbol id missing"))?;

        let mut context_items = Vec::new();
        let target_code = read_span(
            &self.repo_root,
            &target_symbol.file,
            target_symbol.start_line,
            target_symbol.end_line,
        )?;
        context_items.push(ContextItem {
            file_path: target_symbol.file.clone(),
            module_name: file_to_module
                .get(&target_symbol.file)
                .cloned()
                .unwrap_or_else(|| "unknown".to_string()),
            module_rank: module_rank_for_file(&file_to_module, &plan.scoped_modules, &target_symbol.file),
            start_line: target_symbol.start_line as usize,
            end_line: target_symbol.end_line as usize,
            priority: 0,
            text: target_code,
        });

        let mut logic_nodes = self.storage.get_logic_nodes(target_id)?;
        sort_logic_nodes(&mut logic_nodes);
        let mut logic_context = Vec::new();
        for node in &logic_nodes {
            if let Some(node_id) = node.id {
                logic_context.append(&mut self.storage.get_logic_neighbors(node_id, plan.logic_radius)?);
            }
        }
        logic_context.sort_by_key(|n| (n.id.unwrap_or_default(), n.start_line, n.end_line));
        logic_context.dedup_by_key(|n| n.id.unwrap_or_default());
        sort_logic_nodes(&mut logic_context);

        for node in logic_context {
            if let Some(node_id) = node.id {
                if let Some(file) = self.storage.get_logic_node_file(node_id)? {
                    let code = read_span(
                        &self.repo_root,
                        &file,
                        node.start_line as u32,
                        node.end_line as u32,
                    )?;
                    context_items.push(ContextItem {
                        file_path: file.clone(),
                        module_name: file_to_module
                            .get(&file)
                            .cloned()
                            .unwrap_or_else(|| "unknown".to_string()),
                        module_rank: module_rank_for_file(&file_to_module, &plan.scoped_modules, &file),
                        start_line: node.start_line,
                        end_line: node.end_line,
                        priority: 1,
                        text: code,
                    });
                }
            }
        }

        let direct_dependencies = self.storage.get_symbol_dependencies(target_id)?;
        let mut direct_ids = HashSet::new();
        for dep in &direct_dependencies {
            if let Some(dep_id) = dep.id {
                direct_ids.insert(dep_id);
                let code = read_span(&self.repo_root, &dep.file, dep.start_line, dep.end_line)?;
                context_items.push(ContextItem {
                    file_path: dep.file.clone(),
                    module_name: file_to_module
                        .get(&dep.file)
                        .cloned()
                        .unwrap_or_else(|| "unknown".to_string()),
                    module_rank: module_rank_for_file(&file_to_module, &plan.scoped_modules, &dep.file),
                    start_line: dep.start_line as usize,
                    end_line: dep.end_line as usize,
                    priority: 2,
                    text: code,
                });
            }
        }

        let neighbors =
            collect_dependency_neighbors(&self.storage, target_id, plan.dependency_radius, plan.include_callers)?;
        for dep in neighbors {
            if let Some(dep_id) = dep.id {
                if dep_id == target_id || direct_ids.contains(&dep_id) {
                    continue;
                }
                let code = read_span(&self.repo_root, &dep.file, dep.start_line, dep.end_line)?;
                context_items.push(ContextItem {
                    file_path: dep.file.clone(),
                    module_name: file_to_module
                        .get(&dep.file)
                        .cloned()
                        .unwrap_or_else(|| "unknown".to_string()),
                    module_rank: module_rank_for_file(&file_to_module, &plan.scoped_modules, &dep.file),
                    start_line: dep.start_line as usize,
                    end_line: dep.end_line as usize,
                    priority: 3,
                    text: code,
                });
            }
        }

        context_items.sort_by(|a, b| {
            a.priority
                .cmp(&b.priority)
                .then_with(|| a.module_rank.cmp(&b.module_rank))
                .then_with(|| a.file_path.cmp(&b.file_path))
                .then_with(|| a.start_line.cmp(&b.start_line))
                .then_with(|| a.end_line.cmp(&b.end_line))
        });
        context_items.dedup_by(|a, b| {
            a.file_path == b.file_path
                && a.start_line == b.start_line
                && a.end_line == b.end_line
                && a.priority == b.priority
        });

        let file_count = self.storage.list_files()?.len();
        let selected = if file_count < 50 {
            context_items.clone()
        } else {
            let budget = ContextBudget {
                max_tokens,
                reserved_prompt: 1000,
            };
            select_with_budget(context_items, &budget)
        };

        let assembled: Vec<serde_json::Value> = selected
            .into_iter()
            .map(|item| {
                json!({
                    "file": item.file_path,
                    "module": item.module_name,
                    "start": item.start_line,
                    "end": item.end_line,
                    "priority": item.priority,
                    "code": item.text,
                })
            })
            .collect();

        Ok(json!({
            "symbol": plan.target_symbol,
            "intent": format!("{intent:?}").to_lowercase(),
            "plan": plan,
            "small_repo_mode": file_count < 50,
            "context": assembled,
        }))
    }

    fn get_workspace_reasoning_context(
        &self,
        query: &str,
        max_tokens: usize,
        workspace_scope: Vec<String>,
    ) -> Result<serde_json::Value> {
        let mut repositories = self.storage.list_repositories()?;
        if !workspace_scope.is_empty() {
            repositories.retain(|r| workspace_scope.iter().any(|s| s == &r.name || s == &r.path));
        }
        let planned = self.get_planned_context(query, max_tokens)?;
        Ok(json!({
            "workspace_repositories": repositories,
            "workspace_scope": workspace_scope,
            "context": planned,
        }))
    }

    fn plan_safe_edit(
        &self,
        symbol: &str,
        edit_description: &str,
        max_tokens: usize,
        patch_mode: Option<engine::PatchApplicationMode>,
        run_tests: bool,
    ) -> Result<serde_json::Value> {
        load_env_from_file(&self.repo_root);
        let impact = impact_analysis::ImpactAnalyzer::analyze(&self.storage, symbol)?;
        let patch_memory = patch_memory::PatchMemory::open(&self.repo_root)?;
        let plan = safe_edit_planner::SafeEditPlanner::plan_with_memory(
            &self.storage,
            symbol,
            edit_description,
            &patch_memory,
        )?;

        let policy_config = std::fs::read_to_string(
            self.repo_root.join(".semantic").join("policies.toml"),
        )
        .unwrap_or_default();
        let policies = policy_engine::PolicyEngine::from_toml(&policy_config)?;
        policies.validate_edit_plan(&plan)?;

        let routing_cfg = std::fs::read_to_string(
            self.repo_root.join(".semantic").join("llm_routing.toml"),
        )
        .unwrap_or_default();
        let providers_cfg = std::fs::read_to_string(
            self.repo_root.join(".semantic").join("llm_config.toml"),
        )
        .unwrap_or_default();
        let metrics_json = std::fs::read_to_string(
            self.repo_root.join(".semantic").join("model_metrics.json"),
        )
        .unwrap_or_else(|_| "{}".to_string());
        let history_perf = patch_memory.model_performance(&patch_memory::PatchQuery::default())?;
        let merged_metrics = merge_metrics_json(&metrics_json, &history_perf);
        let router = llm_router::LLMRouter::from_files(&providers_cfg, &routing_cfg, &merged_metrics)?;
        let route = router.route(llm_router::LLMTask::CodeExecution);
        let provider_settings = parse_provider_settings(&providers_cfg);
        let live_llm_result = route.as_ref().and_then(|selected| {
            let prompt = format!(
                "You are editing symbol '{}'.\nFailure/Task: {}\nReturn concise fix guidance.",
                symbol, edit_description
            );
            call_live_llm(
                &selected.provider,
                provider_settings.get(&selected.provider),
                Some(&selected.endpoint),
                &prompt,
                512,
            )
        });

        let file_path = plan
            .required_context
            .first()
            .map(|c| c.file_path.clone())
            .unwrap_or_default();
        let patch = patch_engine::PatchEngine::generate_ast_patch(
            &file_path,
            &plan.target_symbol,
            engine::ASTTransformation::ReplaceFunctionBody,
        );
        let preview_diff = match &patch.representation {
            engine::PatchRepresentation::ASTTransform(ast_edit) => {
                patch_engine::PatchEngine::ast_to_diff(&patch.file_path, ast_edit)
            }
            engine::PatchRepresentation::UnifiedDiff(diff) => diff.clone(),
        };

        let application_mode = patch_mode.unwrap_or(engine::PatchApplicationMode::Confirm);
        let validation_cfg = std::fs::read_to_string(
            self.repo_root.join(".semantic").join("validation.toml"),
        )
        .unwrap_or_default();

        let (provider, model_used) = route
            .as_ref()
            .map(|r| {
                let model = provider_settings
                    .get(&r.provider)
                    .map(|s| s.model.clone())
                    .unwrap_or_else(|| r.provider.clone());
                (r.provider.clone(), model)
            })
            .unwrap_or_else(|| ("unknown".to_string(), "unknown".to_string()));
        let ast_transform = match &patch.representation {
            engine::PatchRepresentation::ASTTransform(ast_edit) => Some(ast_edit.transformation.clone()),
            engine::PatchRepresentation::UnifiedDiff(_) => None,
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or_default();
        let record = engine::PatchRecord {
            patch_id: patch_memory::PatchMemory::new_record_id(),
            timestamp: now,
            repository: self.repo_root.to_string_lossy().to_string(),
            file_path: patch.file_path.clone(),
            target_symbol: plan.target_symbol.clone(),
            edit_type: plan.edit_type.clone(),
            model_used,
            provider,
            diff: preview_diff.clone(),
            ast_transform,
            impacted_symbols: plan.impacted_symbols.clone(),
            approved_by_user: matches!(application_mode, engine::PatchApplicationMode::AutoApply),
            validation_passed: false,
            tests_passed: false,
            rollback_occurred: false,
            rollback_reason: None,
        };
        patch_memory.append_record(&record)?;

        Ok(json!({
            "impact_report": impact,
            "edit_plan": plan,
            "route": route.map(|r| json!({"provider": r.provider, "endpoint": r.endpoint})),
            "patch_preview": {
                "file_path": patch.file_path,
                "representation": patch.representation,
                "diff": preview_diff,
            },
            "live_llm": live_llm_result.map(|r| json!({
                "model": r.model,
                "total_tokens": r.total_tokens,
                "response_text": r.text,
            })),
            "patch_application_mode": application_mode,
            "run_tests": run_tests,
            "max_tokens": max_tokens,
            "validation_config": validation_cfg,
            "patch_record_id": record.patch_id,
        }))
    }
}

fn collect_dependency_neighbors(
    storage: &storage::Storage,
    symbol_id: i64,
    radius: usize,
    include_callers: bool,
) -> Result<Vec<SymbolRecord>> {
    let mut queue = VecDeque::new();
    let mut visited = HashSet::new();
    queue.push_back((symbol_id, 0usize));
    visited.insert(symbol_id);

    let mut out = Vec::new();
    while let Some((current, depth)) = queue.pop_front() {
        if let Some(symbol) = storage.get_symbol_by_id(current)? {
            out.push(symbol);
        }
        if depth >= radius {
            continue;
        }

        let mut neighbors = storage.get_symbol_dependencies(current)?;
        if include_callers {
            neighbors.extend(storage.get_symbol_callers(current)?);
        }
        sort_symbols(&mut neighbors);
        neighbors.dedup_by_key(|s| s.id.unwrap_or_default());

        for symbol in neighbors {
            if let Some(next_id) = symbol.id {
                if visited.insert(next_id) {
                    queue.push_back((next_id, depth + 1));
                }
            }
        }
    }

    sort_symbols(&mut out);
    Ok(out)
}

fn sort_symbols(symbols: &mut [SymbolRecord]) {
    symbols.sort_by(|a, b| {
        a.file
            .cmp(&b.file)
            .then_with(|| a.start_line.cmp(&b.start_line))
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.id.unwrap_or_default().cmp(&b.id.unwrap_or_default()))
    });
}

fn sort_logic_nodes(nodes: &mut [LogicNodeRecord]) {
    nodes.sort_by_key(|n| (n.start_line, n.end_line, n.id.unwrap_or_default()));
}

#[derive(Debug, Clone)]
struct ProviderSetting {
    model: String,
    api_key_env: Option<String>,
}

#[derive(Debug, Clone)]
struct LLMCallResult {
    model: String,
    total_tokens: usize,
    text: String,
}

#[derive(Debug)]
struct ABTestRow {
    timestamp: u64,
    provider: String,
    symbol: String,
    tokens_without_project: usize,
    tokens_with_project: usize,
    savings_pct: f32,
}

fn parse_provider_settings(config: &str) -> HashMap<String, ProviderSetting> {
    let mut out = HashMap::new();
    let mut current_provider: Option<String> = None;
    let mut current_model: Option<String> = None;
    let mut current_api_key_env: Option<String> = None;

    let flush = |provider: &Option<String>,
                 model: &Option<String>,
                 api_key_env: &Option<String>,
                 out_map: &mut HashMap<String, ProviderSetting>| {
        if let Some(p) = provider {
            out_map.insert(
                p.clone(),
                ProviderSetting {
                    model: model.clone().unwrap_or_else(|| "gpt-4o-mini".to_string()),
                    api_key_env: api_key_env.clone(),
                },
            );
        }
    };

    for raw in config.lines() {
        let line = raw.trim();
        if line.starts_with('[') && line.ends_with(']') {
            flush(&current_provider, &current_model, &current_api_key_env, &mut out);
            current_model = None;
            current_api_key_env = None;
            if line.starts_with("[provider_settings.") && line.ends_with(']') {
                let name = line
                    .trim_start_matches("[provider_settings.")
                    .trim_end_matches(']')
                    .to_string();
                current_provider = Some(name);
            } else {
                current_provider = None;
            }
            continue;
        }
        if current_provider.is_none() {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            let key = key.trim();
            let value = value.trim().trim_matches('"').to_string();
            if key == "model" {
                current_model = Some(value);
            } else if key == "api_key_env" {
                current_api_key_env = Some(value);
            }
        }
    }
    flush(&current_provider, &current_model, &current_api_key_env, &mut out);
    out
}

fn load_env_from_file(repo_root: &Path) {
    let env_path = repo_root.join(".semantic").join(".env");
    if !env_path.exists() {
        return;
    }
    let content = fs::read_to_string(env_path).unwrap_or_default();
    for raw in content.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            let key = k.trim();
            let value = v.trim().trim_matches('"');
            if std::env::var(key).is_err() {
                unsafe {
                    std::env::set_var(key, value);
                }
            }
        }
    }
}

fn call_live_llm(
    provider: &str,
    provider_setting: Option<&ProviderSetting>,
    routed_endpoint: Option<&str>,
    prompt: &str,
    max_tokens: usize,
) -> Option<LLMCallResult> {
    let setting = provider_setting.cloned().unwrap_or(ProviderSetting {
        model: default_model_for_provider(provider).to_string(),
        api_key_env: default_key_env_for_provider(provider).map(|s| s.to_string()),
    });
    let endpoint = routed_endpoint
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| default_endpoint_for_provider(provider).to_string());

    if provider == "ollama" {
        return call_ollama(&endpoint, &setting.model, prompt);
    }

    let api_key_env = setting.api_key_env.clone()?;
    let api_key = std::env::var(api_key_env).ok()?;
    if provider == "anthropic" {
        return call_anthropic(&endpoint, &api_key, &setting.model, prompt, max_tokens);
    }
    if provider == "gemini" {
        return call_gemini(&endpoint, &api_key, &setting.model, prompt, max_tokens);
    }
    call_openai_family(&endpoint, &api_key, &setting.model, prompt, max_tokens)
}

fn call_openai_family(
    endpoint: &str,
    api_key: &str,
    model: &str,
    prompt: &str,
    max_tokens: usize,
) -> Option<LLMCallResult> {
    let url = format!("{}/chat/completions", endpoint.trim_end_matches('/'));
    let client = reqwest::blocking::Client::new();
    let response = client
        .post(url)
        .header("Authorization", format!("Bearer {api_key}"))
        .header("Content-Type", "application/json")
        .json(&json!({
            "model": model,
            "messages": [{"role":"user","content": prompt}],
            "max_tokens": max_tokens
        }))
        .send()
        .ok()?;
    let value: serde_json::Value = response.json().ok()?;
    let text = value
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|arr| arr.first())
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let tokens = value
        .get("usage")
        .and_then(|u| u.get("total_tokens"))
        .and_then(|v| v.as_u64())
        .map(|v| v as usize)
        .unwrap_or_else(|| estimate_tokens(&(prompt.to_string() + &text)));
    Some(LLMCallResult {
        model: model.to_string(),
        total_tokens: tokens,
        text,
    })
}

fn call_anthropic(
    endpoint: &str,
    api_key: &str,
    model: &str,
    prompt: &str,
    max_tokens: usize,
) -> Option<LLMCallResult> {
    let url = format!("{}/messages", endpoint.trim_end_matches('/'));
    let client = reqwest::blocking::Client::new();
    let response = client
        .post(url)
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .header("Content-Type", "application/json")
        .json(&json!({
            "model": model,
            "max_tokens": max_tokens,
            "messages": [{"role":"user","content": prompt}]
        }))
        .send()
        .ok()?;
    let value: serde_json::Value = response.json().ok()?;
    let text = value
        .get("content")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .and_then(|v| v.get("text"))
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let tokens = value
        .get("usage")
        .and_then(|u| u.get("input_tokens"))
        .and_then(|v| v.as_u64())
        .map(|v| v as usize + estimate_tokens(&text))
        .unwrap_or_else(|| estimate_tokens(&(prompt.to_string() + &text)));
    Some(LLMCallResult {
        model: model.to_string(),
        total_tokens: tokens,
        text,
    })
}

fn call_gemini(
    endpoint: &str,
    api_key: &str,
    model: &str,
    prompt: &str,
    max_tokens: usize,
) -> Option<LLMCallResult> {
    let base = endpoint.trim_end_matches('/');
    let url = format!(
        "{}/v1beta/models/{}:generateContent?key={}",
        base, model, api_key
    );
    let client = reqwest::blocking::Client::new();
    let response = client
        .post(url)
        .header("Content-Type", "application/json")
        .json(&json!({
            "contents": [{"parts": [{"text": prompt}]}],
            "generationConfig": {"maxOutputTokens": max_tokens}
        }))
        .send()
        .ok()?;
    let value: serde_json::Value = response.json().ok()?;
    let text = value
        .get("candidates")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .and_then(|v| v.get("content"))
        .and_then(|v| v.get("parts"))
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .and_then(|v| v.get("text"))
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let tokens = value
        .get("usageMetadata")
        .and_then(|u| u.get("totalTokenCount"))
        .and_then(|v| v.as_u64())
        .map(|v| v as usize)
        .unwrap_or_else(|| estimate_tokens(&(prompt.to_string() + &text)));
    Some(LLMCallResult {
        model: model.to_string(),
        total_tokens: tokens,
        text,
    })
}

fn call_ollama(endpoint: &str, model: &str, prompt: &str) -> Option<LLMCallResult> {
    let url = format!("{}/api/generate", endpoint.trim_end_matches('/'));
    let client = reqwest::blocking::Client::new();
    let response = client
        .post(url)
        .header("Content-Type", "application/json")
        .json(&json!({
            "model": model,
            "prompt": prompt,
            "stream": false
        }))
        .send()
        .ok()?;
    let value: serde_json::Value = response.json().ok()?;
    let text = value
        .get("response")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    Some(LLMCallResult {
        model: model.to_string(),
        total_tokens: estimate_tokens(&(prompt.to_string() + &text)),
        text,
    })
}

fn default_endpoint_for_provider(provider: &str) -> &'static str {
    match provider {
        "anthropic" => "https://api.anthropic.com/v1",
        "gemini" => "https://generativelanguage.googleapis.com",
        "openrouter" => "https://openrouter.ai/api/v1",
        "together" => "https://api.together.xyz/v1",
        "ollama" => "http://127.0.0.1:11434",
        _ => "https://api.openai.com/v1",
    }
}

fn default_model_for_provider(provider: &str) -> &'static str {
    match provider {
        "anthropic" => "claude-3-5-sonnet-latest",
        "gemini" => "gemini-1.5-pro",
        "openrouter" => "openai/gpt-4o-mini",
        "together" => "meta-llama/Meta-Llama-3.1-70B-Instruct-Turbo",
        "ollama" => "llama3.1:8b",
        _ => "gpt-4o-mini",
    }
}

fn default_key_env_for_provider(provider: &str) -> Option<&'static str> {
    match provider {
        "anthropic" => Some("ANTHROPIC_API_KEY"),
        "gemini" => Some("GEMINI_API_KEY"),
        "openrouter" => Some("OPENROUTER_API_KEY"),
        "together" => Some("TOGETHER_API_KEY"),
        "ollama" => None,
        _ => Some("OPENAI_API_KEY"),
    }
}

fn estimate_tokens(text: &str) -> usize {
    ((text.len() as f32) / 4.0).ceil() as usize
}

fn append_ab_test_csv(repo_root: &Path, row: &ABTestRow) -> Result<()> {
    let path = repo_root.join(".semantic").join("ab_test_results.csv");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    if file.metadata()?.len() == 0 {
        writeln!(
            file,
            "timestamp,provider,symbol,tokens_without_project,tokens_with_project,savings_pct"
        )?;
    }
    writeln!(
        file,
        "{},{},{},{},{},{:.2}",
        row.timestamp,
        row.provider,
        row.symbol,
        row.tokens_without_project,
        row.tokens_with_project,
        row.savings_pct
    )?;
    Ok(())
}

fn current_ts() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

fn merge_metrics_json(
    base_metrics_json: &str,
    performance: &[engine::ModelPerformance],
) -> String {
    let mut base: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str::<serde_json::Value>(base_metrics_json)
            .ok()
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default();

    for perf in performance {
        let entry = base
            .entry(perf.model.clone())
            .or_insert_with(|| json!({}));
        if let Some(obj) = entry.as_object_mut() {
            obj.insert("success_rate".to_string(), json!(perf.success_rate));
            if !obj.contains_key("latency_ms") {
                obj.insert("latency_ms".to_string(), json!(perf.avg_latency_ms));
            }
            if !obj.contains_key("token_cost") {
                obj.insert("token_cost".to_string(), json!(perf.avg_cost));
            }
        }
    }

    serde_json::to_string(&base).unwrap_or_else(|_| "{}".to_string())
}

fn estimate_evolution_risk(
    repo_root: &Path,
    storage: &storage::Storage,
    impacted_plan_count: usize,
) -> Result<engine::EvolutionRisk> {
    let memory = patch_memory::PatchMemory::open(repo_root)?;
    let stats = memory.stats(&patch_memory::PatchQuery::default())?;
    let failure_rate = 1.0f32 - stats.success_rate;
    let impacted_files = impacted_plan_count as f32;
    let file_count = storage.list_files()?.len();
    let test_files = storage
        .list_files()?
        .into_iter()
        .filter(|f| {
            let l = f.to_lowercase();
            l.contains("test") || l.contains("spec")
        })
        .count();
    let coverage_signal = if file_count == 0 {
        0.0
    } else {
        1.0 - (test_files as f32 / file_count as f32)
    };

    let risk_score = (failure_rate * 0.5) + ((impacted_files / 100.0).min(1.0) * 0.3) + (coverage_signal * 0.2);
    Ok(engine::EvolutionRisk {
        risk_score,
        reasoning: format!(
            "failure_rate={:.2}, impacted_plans={}, low_test_signal={:.2}",
            failure_rate, impacted_plan_count, coverage_signal
        ),
    })
}

fn module_rank_for_file(
    file_to_module: &std::collections::HashMap<String, String>,
    scoped_modules: &[String],
    file_path: &str,
) -> u8 {
    let Some(module_name) = file_to_module.get(file_path) else {
        return 2;
    };
    if let Some(pos) = scoped_modules.iter().position(|m| m == module_name) {
        if pos == 0 {
            0
        } else {
            1
        }
    } else {
        2
    }
}

fn read_span(repo_root: &Path, relative_file: &str, start_line: u32, end_line: u32) -> Result<String> {
    let full_path = repo_root.join(relative_file);
    let content = fs::read_to_string(full_path)?;
    let mut out = Vec::new();

    for (idx, line) in content.lines().enumerate() {
        let line_no = idx as u32 + 1;
        if line_no >= start_line && line_no <= end_line {
            out.push(line.to_string());
        }
    }

    Ok(out.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::RetrievalService;
    use engine::{LogicNodeRecord, LogicNodeType, Operation, RetrievalRequest, SymbolRecord, SymbolType};
    use std::fs;
    use storage::Storage;

    #[test]
    fn returns_code_span_for_function() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join("src")).expect("mkdir src");
        fs::write(
            repo.join("src").join("client.ts"),
            "function retryRequest(){\n  return 1;\n}\n",
        )
        .expect("write file");

        let db = tmp.path().join("semantic.db");
        let idx = tmp.path().join("tantivy");
        let mut storage = Storage::open(&db, &idx).expect("open storage");
        storage
            .upsert_file("src/client.ts", "typescript", "x")
            .expect("upsert file");
        let ids = storage
            .insert_symbols(&[SymbolRecord {
                id: None,
                repo_id: 0,
                name: "retryRequest".into(),
                symbol_type: SymbolType::Function,
                file: "src/client.ts".into(),
                start_line: 1,
                end_line: 3,
                language: "typescript".into(),
                summary: "Function retryRequest".into(),
            }])
            .expect("insert symbol");
        storage
            .insert_logic_nodes(
                ids[0],
                &[LogicNodeRecord {
                    id: None,
                    symbol_id: ids[0],
                    node_type: LogicNodeType::Return,
                    start_line: 2,
                    end_line: 2,
                }],
            )
            .expect("insert logic nodes");

        let service = RetrievalService::new(repo, storage);
        let resp = service
            .handle(RetrievalRequest {
                operation: Operation::GetFunction,
                name: Some("retryRequest".into()),
                query: None,
                file: None,
                start_line: None,
                end_line: None,
                max_tokens: None,
                limit: None,
                node_id: None,
                radius: None,
                logic_radius: None,
                dependency_radius: None,
                workspace_scope: None,
            edit_description: None,
            patch_mode: None,
            run_tests: None,
            })
            .expect("get function");

        let code = resp
            .result
            .get("code")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        assert!(code.contains("retryRequest"));
    }

    #[test]
    fn returns_logic_nodes() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join("src")).expect("mkdir src");
        fs::write(
            repo.join("src").join("client.ts"),
            "function retryRequest(){\n  if (x) { return 1; }\n  return 2;\n}\n",
        )
        .expect("write file");

        let db = tmp.path().join("semantic.db");
        let idx = tmp.path().join("tantivy");
        let mut storage = Storage::open(&db, &idx).expect("open storage");
        storage
            .replace_file_index(
                0,
                "src/client.ts",
                "typescript",
                "x",
                &[SymbolRecord {
                    id: None,
                    repo_id: 0,
                    name: "retryRequest".into(),
                    symbol_type: SymbolType::Function,
                    file: "src/client.ts".into(),
                    start_line: 1,
                    end_line: 4,
                    language: "typescript".into(),
                    summary: "Function retryRequest".into(),
                }],
                &[],
                &[
                    LogicNodeRecord {
                        id: None,
                        symbol_id: 1,
                        node_type: LogicNodeType::Conditional,
                        start_line: 2,
                        end_line: 2,
                    },
                    LogicNodeRecord {
                        id: None,
                        symbol_id: 1,
                        node_type: LogicNodeType::Return,
                        start_line: 3,
                        end_line: 3,
                    },
                ],
            )
            .expect("replace index");

        let service = RetrievalService::new(repo, storage);
        let resp = service
            .handle(RetrievalRequest {
                operation: Operation::GetLogicNodes,
                name: Some("retryRequest".into()),
                query: None,
                file: None,
                start_line: None,
                end_line: None,
                max_tokens: None,
                limit: None,
                node_id: None,
                radius: None,
                logic_radius: None,
                dependency_radius: None,
                workspace_scope: None,
            edit_description: None,
            patch_mode: None,
            run_tests: None,
            })
            .expect("logic nodes");

        let nodes = resp
            .result
            .get("nodes")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        assert_eq!(nodes.len(), 2);
    }

    #[test]
    fn returns_reasoning_context_with_deterministic_groups() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join("src")).expect("mkdir src");
        fs::write(
            repo.join("src").join("flow.ts"),
            "async function a(){ if(true){ throw new Error('x') } await b(); return c(); }\nfunction b(){ return 1; }\nfunction c(){ return b(); }\n",
        )
        .expect("write file");

        let db = tmp.path().join("semantic.db");
        let idx = tmp.path().join("tantivy");
        let mut storage = Storage::open(&db, &idx).expect("open storage");
        storage
            .replace_file_index(
                0,
                "src/flow.ts",
                "typescript",
                "x",
                &[
                    SymbolRecord {
                        id: None,
                        repo_id: 0,
                        name: "a".into(),
                        symbol_type: SymbolType::Function,
                        file: "src/flow.ts".into(),
                        start_line: 1,
                        end_line: 1,
                        language: "typescript".into(),
                        summary: "Function a".into(),
                    },
                    SymbolRecord {
                        id: None,
                        repo_id: 0,
                        name: "b".into(),
                        symbol_type: SymbolType::Function,
                        file: "src/flow.ts".into(),
                        start_line: 2,
                        end_line: 2,
                        language: "typescript".into(),
                        summary: "Function b".into(),
                    },
                    SymbolRecord {
                        id: None,
                        repo_id: 0,
                        name: "c".into(),
                        symbol_type: SymbolType::Function,
                        file: "src/flow.ts".into(),
                        start_line: 3,
                        end_line: 3,
                        language: "typescript".into(),
                        summary: "Function c".into(),
                    },
                ],
                &[
                    engine::DependencyRecord {
                        id: None,
                        repo_id: 0,
                        caller_symbol: "a".into(),
                        callee_symbol: "b".into(),
                        file: "src/flow.ts".into(),
                    },
                    engine::DependencyRecord {
                        id: None,
                        repo_id: 0,
                        caller_symbol: "a".into(),
                        callee_symbol: "c".into(),
                        file: "src/flow.ts".into(),
                    },
                    engine::DependencyRecord {
                        id: None,
                        repo_id: 0,
                        caller_symbol: "c".into(),
                        callee_symbol: "b".into(),
                        file: "src/flow.ts".into(),
                    },
                ],
                &[
                    LogicNodeRecord {
                        id: None,
                        symbol_id: 1,
                        node_type: LogicNodeType::Conditional,
                        start_line: 1,
                        end_line: 1,
                    },
                    LogicNodeRecord {
                        id: None,
                        symbol_id: 1,
                        node_type: LogicNodeType::Throw,
                        start_line: 1,
                        end_line: 1,
                    },
                    LogicNodeRecord {
                        id: None,
                        symbol_id: 1,
                        node_type: LogicNodeType::Await,
                        start_line: 1,
                        end_line: 1,
                    },
                    LogicNodeRecord {
                        id: None,
                        symbol_id: 1,
                        node_type: LogicNodeType::Return,
                        start_line: 1,
                        end_line: 1,
                    },
                ],
            )
            .expect("replace index");

        let service = RetrievalService::new(repo, storage);
        let resp = service
            .handle(RetrievalRequest {
                operation: Operation::GetReasoningContext,
                name: Some("a".into()),
                query: None,
                file: None,
                start_line: None,
                end_line: None,
                max_tokens: None,
                limit: None,
                node_id: None,
                radius: None,
                logic_radius: Some(1),
                dependency_radius: Some(2),
                workspace_scope: None,
            edit_description: None,
            patch_mode: None,
            run_tests: None,
            })
            .expect("reasoning context");

        let order = resp
            .result
            .get("order")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        assert_eq!(order.len(), 3);

        let deps = resp
            .result
            .get("dependencies")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        assert!(!deps.is_empty());
    }

    #[test]
    fn returns_dependency_and_symbol_neighborhood() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join("src")).expect("mkdir src");
        fs::write(
            repo.join("src").join("flow.ts"),
            "function a(){ return b(); }\nfunction b(){ return c(); }\nfunction c(){ return 1; }\n",
        )
        .expect("write file");

        let db = tmp.path().join("semantic.db");
        let idx = tmp.path().join("tantivy");
        let mut storage = Storage::open(&db, &idx).expect("open storage");
        storage
            .replace_file_index(
                0,
                "src/flow.ts",
                "typescript",
                "x",
                &[
                    SymbolRecord {
                        id: None,
                        repo_id: 0,
                        name: "a".into(),
                        symbol_type: SymbolType::Function,
                        file: "src/flow.ts".into(),
                        start_line: 1,
                        end_line: 1,
                        language: "typescript".into(),
                        summary: "Function a".into(),
                    },
                    SymbolRecord {
                        id: None,
                        repo_id: 0,
                        name: "b".into(),
                        symbol_type: SymbolType::Function,
                        file: "src/flow.ts".into(),
                        start_line: 2,
                        end_line: 2,
                        language: "typescript".into(),
                        summary: "Function b".into(),
                    },
                    SymbolRecord {
                        id: None,
                        repo_id: 0,
                        name: "c".into(),
                        symbol_type: SymbolType::Function,
                        file: "src/flow.ts".into(),
                        start_line: 3,
                        end_line: 3,
                        language: "typescript".into(),
                        summary: "Function c".into(),
                    },
                ],
                &[
                    engine::DependencyRecord {
                        id: None,
                        repo_id: 0,
                        caller_symbol: "a".into(),
                        callee_symbol: "b".into(),
                        file: "src/flow.ts".into(),
                    },
                    engine::DependencyRecord {
                        id: None,
                        repo_id: 0,
                        caller_symbol: "b".into(),
                        callee_symbol: "c".into(),
                        file: "src/flow.ts".into(),
                    },
                ],
                &[LogicNodeRecord {
                    id: None,
                    symbol_id: 1,
                    node_type: LogicNodeType::Return,
                    start_line: 1,
                    end_line: 1,
                }],
            )
            .expect("replace index");

        let service = RetrievalService::new(repo, storage);
        let deps_resp = service
            .handle(RetrievalRequest {
                operation: Operation::GetDependencyNeighborhood,
                name: Some("a".into()),
                query: None,
                file: None,
                start_line: None,
                end_line: None,
                max_tokens: None,
                limit: None,
                node_id: None,
                radius: Some(2),
                logic_radius: None,
                dependency_radius: None,
                workspace_scope: None,
            edit_description: None,
            patch_mode: None,
            run_tests: None,
            })
            .expect("dependency neighborhood");
        let deps = deps_resp
            .result
            .get("neighbors")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        assert_eq!(deps.len(), 2);

        let symbol_resp = service
            .handle(RetrievalRequest {
                operation: Operation::GetSymbolNeighborhood,
                name: Some("a".into()),
                query: None,
                file: None,
                start_line: None,
                end_line: None,
                max_tokens: None,
                limit: None,
                node_id: None,
                radius: Some(2),
                logic_radius: None,
                dependency_radius: None,
                workspace_scope: None,
            edit_description: None,
            patch_mode: None,
            run_tests: None,
            })
            .expect("symbol neighborhood");
        let order = symbol_resp
            .result
            .get("order")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        assert_eq!(order.len(), 3);
    }

    #[test]
    fn planned_context_skips_budget_in_small_repo() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join("src")).expect("mkdir src");
        fs::write(
            repo.join("src").join("client.ts"),
            "function fetchData(){ return retryRequest(); }\nfunction retryRequest(){ return 1; }\n",
        )
        .expect("write file");

        let db = tmp.path().join("semantic.db");
        let idx = tmp.path().join("tantivy");
        let mut storage = Storage::open(&db, &idx).expect("open storage");
        storage
            .replace_file_index(
                0,
                "src/client.ts",
                "typescript",
                "x",
                &[
                    SymbolRecord {
                        id: None,
                        repo_id: 0,
                        name: "fetchData".into(),
                        symbol_type: SymbolType::Function,
                        file: "src/client.ts".into(),
                        start_line: 1,
                        end_line: 1,
                        language: "typescript".into(),
                        summary: "Function fetchData".into(),
                    },
                    SymbolRecord {
                        id: None,
                        repo_id: 0,
                        name: "retryRequest".into(),
                        symbol_type: SymbolType::Function,
                        file: "src/client.ts".into(),
                        start_line: 2,
                        end_line: 2,
                        language: "typescript".into(),
                        summary: "Function retryRequest".into(),
                    },
                ],
                &[engine::DependencyRecord {
                    id: None,
                    repo_id: 0,
                    caller_symbol: "fetchData".into(),
                    callee_symbol: "retryRequest".into(),
                    file: "src/client.ts".into(),
                }],
                &[LogicNodeRecord {
                    id: None,
                    symbol_id: 1,
                    node_type: LogicNodeType::Return,
                    start_line: 1,
                    end_line: 1,
                }],
            )
            .expect("replace index");

        let service = RetrievalService::new(repo, storage);
        let resp = service
            .handle(RetrievalRequest {
                operation: Operation::GetPlannedContext,
                name: None,
                query: Some("fix retry logic in fetchData".into()),
                file: None,
                start_line: None,
                end_line: None,
                max_tokens: Some(20),
                limit: None,
                node_id: None,
                radius: None,
                logic_radius: None,
                dependency_radius: None,
                workspace_scope: None,
            edit_description: None,
            patch_mode: None,
            run_tests: None,
            })
            .expect("planned context");

        assert_eq!(
            resp.result
                .get("small_repo_mode")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            true
        );
        let context = resp
            .result
            .get("context")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        assert!(!context.is_empty());
    }

    #[test]
    fn planned_context_applies_budget_in_large_repo() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join("src")).expect("mkdir src");
        fs::write(
            repo.join("src").join("client.ts"),
            "function fetchData(){ return retryRequest(); }\nfunction retryRequest(){ return 1; }\n",
        )
        .expect("write file");

        let db = tmp.path().join("semantic.db");
        let idx = tmp.path().join("tantivy");
        let mut storage = Storage::open(&db, &idx).expect("open storage");
        storage
            .replace_file_index(
                0,
                "src/client.ts",
                "typescript",
                "x",
                &[
                    SymbolRecord {
                        id: None,
                        repo_id: 0,
                        name: "fetchData".into(),
                        symbol_type: SymbolType::Function,
                        file: "src/client.ts".into(),
                        start_line: 1,
                        end_line: 1,
                        language: "typescript".into(),
                        summary: "Function fetchData".into(),
                    },
                    SymbolRecord {
                        id: None,
                        repo_id: 0,
                        name: "retryRequest".into(),
                        symbol_type: SymbolType::Function,
                        file: "src/client.ts".into(),
                        start_line: 2,
                        end_line: 2,
                        language: "typescript".into(),
                        summary: "Function retryRequest".into(),
                    },
                ],
                &[engine::DependencyRecord {
                    id: None,
                    repo_id: 0,
                    caller_symbol: "fetchData".into(),
                    callee_symbol: "retryRequest".into(),
                    file: "src/client.ts".into(),
                }],
                &[LogicNodeRecord {
                    id: None,
                    symbol_id: 1,
                    node_type: LogicNodeType::Return,
                    start_line: 1,
                    end_line: 1,
                }],
            )
            .expect("replace index");

        for i in 0..60 {
            storage
                .upsert_file(&format!("src/dummy_{i}.ts"), "typescript", "z")
                .expect("upsert file");
        }

        let service = RetrievalService::new(repo, storage);
        let resp = service
            .handle(RetrievalRequest {
                operation: Operation::GetPlannedContext,
                name: None,
                query: Some("refactor fetchData".into()),
                file: None,
                start_line: None,
                end_line: None,
                max_tokens: Some(1004),
                limit: None,
                node_id: None,
                radius: None,
                logic_radius: None,
                dependency_radius: None,
                workspace_scope: None,
            edit_description: None,
            patch_mode: None,
            run_tests: None,
            })
            .expect("planned context");

        assert_eq!(
            resp.result
                .get("small_repo_mode")
                .and_then(|v| v.as_bool())
                .unwrap_or(true),
            false
        );
        let context = resp
            .result
            .get("context")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        assert!(context.len() <= 1);
    }

    #[test]
    fn returns_repo_map_hierarchy_and_module_dependencies() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join("src").join("api")).expect("mkdir api");
        fs::create_dir_all(repo.join("src").join("utils")).expect("mkdir utils");
        fs::write(
            repo.join("src").join("utils").join("retry.ts"),
            "export function retryRequest(){ return 1; }\n",
        )
        .expect("write retry");
        fs::write(
            repo.join("src").join("api").join("client.ts"),
            "import { retryRequest } from '../utils/retry';\nexport function fetchData(){ return retryRequest(); }\n",
        )
        .expect("write client");

        let db = tmp.path().join("semantic.db");
        let idx = tmp.path().join("tantivy");
        let mut storage = Storage::open(&db, &idx).expect("open storage");
        storage
            .replace_file_index(
                0,
                "src/utils/retry.ts",
                "typescript",
                "x1",
                &[SymbolRecord {
                    id: None,
                    repo_id: 0,
                    name: "retryRequest".into(),
                    symbol_type: SymbolType::Function,
                    file: "src/utils/retry.ts".into(),
                    start_line: 1,
                    end_line: 1,
                    language: "typescript".into(),
                    summary: "Function retryRequest".into(),
                }],
                &[],
                &[],
            )
            .expect("replace retry");
        storage
            .replace_file_index(
                0,
                "src/api/client.ts",
                "typescript",
                "x2",
                &[SymbolRecord {
                    id: None,
                    repo_id: 0,
                    name: "fetchData".into(),
                    symbol_type: SymbolType::Function,
                    file: "src/api/client.ts".into(),
                    start_line: 2,
                    end_line: 2,
                    language: "typescript".into(),
                    summary: "Function fetchData".into(),
                }],
                &[engine::DependencyRecord {
                    id: None,
                    repo_id: 0,
                    caller_symbol: "fetchData".into(),
                    callee_symbol: "retryRequest".into(),
                    file: "src/api/client.ts".into(),
                }],
                &[],
            )
            .expect("replace client");

        storage.clear_module_graph().expect("clear modules");
        let api_id = storage.insert_module("api", "src/api").expect("insert api");
        let util_id = storage
            .insert_module("utils", "src/utils")
            .expect("insert utils");
        storage
            .insert_module_file(api_id, "src/api/client.ts")
            .expect("insert api file");
        storage
            .insert_module_file(util_id, "src/utils/retry.ts")
            .expect("insert util file");
        storage
            .insert_module_dependency(api_id, util_id)
            .expect("insert module dep");

        let service = RetrievalService::new(repo, storage);
        let hierarchy = service
            .handle(RetrievalRequest {
                operation: Operation::GetRepoMapHierarchy,
                name: None,
                query: None,
                file: None,
                start_line: None,
                end_line: None,
                max_tokens: None,
                limit: None,
                node_id: None,
                radius: None,
                logic_radius: None,
                dependency_radius: None,
                workspace_scope: None,
            edit_description: None,
            patch_mode: None,
            run_tests: None,
            })
            .expect("repo hierarchy");
        let modules = hierarchy
            .result
            .get("modules")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        assert!(modules.len() >= 2);

        let module_deps = service
            .handle(RetrievalRequest {
                operation: Operation::GetModuleDependencies,
                name: None,
                query: None,
                file: None,
                start_line: None,
                end_line: None,
                max_tokens: None,
                limit: None,
                node_id: None,
                radius: None,
                logic_radius: None,
                dependency_radius: None,
                workspace_scope: None,
            edit_description: None,
            patch_mode: None,
            run_tests: None,
            })
            .expect("module dependencies");
        let edges = module_deps
            .result
            .get("edges")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        assert!(edges.iter().any(|e| {
            e.get("from").and_then(|v| v.as_str()) == Some("api")
                && e.get("to").and_then(|v| v.as_str()) == Some("utils")
        }));
    }

    #[test]
    fn semantic_symbol_search_fallback_works() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join("src")).expect("mkdir src");
        fs::write(
            repo.join("src").join("retry.ts"),
            "function retryRequest(){ return 1; }\n",
        )
        .expect("write file");

        let db = tmp.path().join("semantic.db");
        let idx = tmp.path().join("tantivy");
        let mut storage = Storage::open(&db, &idx).expect("open storage");
        storage
            .replace_file_index(
                0,
                "src/retry.ts",
                "typescript",
                "x",
                &[SymbolRecord {
                    id: None,
                    repo_id: 0,
                    name: "retryRequest".into(),
                    symbol_type: SymbolType::Function,
                    file: "src/retry.ts".into(),
                    start_line: 1,
                    end_line: 1,
                    language: "typescript".into(),
                    summary: "Function retryRequest".into(),
                }],
                &[],
                &[],
            )
            .expect("replace");

        let service = RetrievalService::new(repo, storage);
        let resp = service
            .handle(RetrievalRequest {
                operation: Operation::SearchSemanticSymbol,
                name: None,
                query: Some("retry operation".into()),
                file: None,
                start_line: None,
                end_line: None,
                max_tokens: None,
                workspace_scope: None,
                limit: Some(5),
                node_id: None,
                radius: None,
                logic_radius: None,
                dependency_radius: None,
            })
            .expect("semantic search");
        let results = resp
            .result
            .get("results")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        assert!(!results.is_empty());
    }

    #[test]
    fn workspace_reasoning_context_returns_repositories() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join("src")).expect("mkdir src");
        fs::write(
            repo.join("src").join("a.ts"),
            "function fetchData(){ return 1; }\n",
        )
        .expect("write file");

        let db = tmp.path().join("semantic.db");
        let idx = tmp.path().join("tantivy");
        let mut storage = Storage::open(&db, &idx).expect("open storage");
        storage
            .register_repository("repoA", "/workspace/repoA")
            .expect("register repo");
        storage
            .replace_file_index(
                1,
                "src/a.ts",
                "typescript",
                "x",
                &[SymbolRecord {
                    id: None,
                    repo_id: 1,
                    name: "fetchData".into(),
                    symbol_type: SymbolType::Function,
                    file: "src/a.ts".into(),
                    start_line: 1,
                    end_line: 1,
                    language: "typescript".into(),
                    summary: "Function fetchData".into(),
                }],
                &[],
                &[LogicNodeRecord {
                    id: None,
                    symbol_id: 1,
                    node_type: LogicNodeType::Return,
                    start_line: 1,
                    end_line: 1,
                }],
            )
            .expect("replace");

        let service = RetrievalService::new(repo, storage);
        let resp = service
            .handle(RetrievalRequest {
                operation: Operation::GetWorkspaceReasoningContext,
                name: None,
                query: Some("explain fetchData".into()),
                file: None,
                start_line: None,
                end_line: None,
                max_tokens: Some(1500),
                workspace_scope: Some(vec!["repoA".into()]),
                limit: None,
                node_id: None,
                radius: None,
                logic_radius: None,
                dependency_radius: None,
            })
            .expect("workspace context");
        let repos = resp
            .result
            .get("workspace_repositories")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        assert!(!repos.is_empty());
    }
}









