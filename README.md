# Semantic Agent Cognitive Layer (MVP)

Local Rust service for deterministic code retrieval by symbol, span, and logic graph.

## IDE Semantic-First Integration

Use the semantic-first integration guide for RooCode/KiloCode/Codex/Claude wiring, middleware policy controls, and end-to-end flow diagrams:

- `docs/IDE_SEMANTIC_FIRST.md`
- `docs/TOOL_CALLING_GUIDE.md` (objective + usage of API and MCP tool callings)
- `docs/AB_TEST_DEV_RESULTS.md` (development benchmark history and latest metrics)

## Modules

- `engine` shared contracts
- `parser` Tree-sitter extraction
- `storage` SQLite + Tantivy
- `indexer` repo indexing orchestration
- `retrieval` operation handlers
- `watcher` incremental file updates
- `api` Axum JSON service

Implemented semantic layers now include logic nodes, persisted control/data-flow edges, semantic node labels, and graph-backed clustering/ranking.

## Run

```bash
cargo run -p api -- ./test_repo
```

Service binds to `$SEMANTIC_API_BASE_URL`.

## Two-Tool MCP Surface

The MCP bridge (`mcp_bridge`) exposes **two primary tools** that cover all use cases:

- **`retrieve`** — unified retrieval. Pass `operation` to select: `GetRepoMap`, `GetFileOutline`, `SearchSymbol`, `GetCodeSpan`, `GetLogicNodes`, `GetControlFlowSlice`, `GetDataFlowSlice`, `GetLogicClusters`, `GetDependencyNeighborhood`, `GetReasoningContext`, `GetPlannedContext`, `PlanSafeEdit`, `GetControlFlowHints`, `GetDataFlowHints`, `GetHybridRankedContext`, `GetDebugGraph`, `GetPipelineGraph`, `GetRootCauseCandidates`, `GetTestGaps`, `GetDeploymentHistory`, `GetPerformanceStats`.
- **`ide_autoroute`** — intent routing (`task`) or action dispatch (`action` + `action_input`). Actions: `debug_failure`, `generate_tests`, `apply_tests`, `analyze_pipeline`.

All 27 legacy named tools remain available for backward compatibility (see `GET /mcp/tools` → `legacy_tools`).

Key API endpoints:

- `POST /retrieve` — all retrieval and graph operations
- `POST /ide_autoroute` — intent routing and action dispatch
- `PATCH /edit` — safe edit planning/execution

Default retrieval behavior:

- `reference_only=true` (structured references first, raw code minimized)
- `single_file_fast_path=true` recommended for obvious single-file edits
- adaptive retrieval breadth to avoid over-fetch on high-fanout symbols

Demo project used by the development A/B suite:

- `test_repo/todo_app/`

## Latest A/B Benchmark Update (2026-03-13)

Recent `POST /ab_test_dev` runs with `provider=openai`, `autoroute_first=true`, and `single_file_fast_path=true`:

- prior baseline: `-18.62%` token savings (`9738` -> `11551`)
- improved run: `+8.07%` token savings (`9365` -> `8609`)
- latest run: `+5.70%` token savings (`9749` -> `9193`)
- task success stayed `11/11` in both control and semantic arms

Core improvements now in the pipeline:

- stronger planner target resolution for natural-language symbol queries
- explicit target-symbol hinting from A/B task metadata into planning
- target-aligned minimal raw seed for autoroute (prevents unrelated seed code)
- per-task context refs in A/B (no cross-task ref starvation)
- escalation routing updated to replacement-style accounting (no additive token double-charge)
- richer A/B diagnostics (`target_match`, `semantic_route`, prompt-char deltas, gating metrics)

Additional retrieval operations:

- `get_logic_nodes`
- `get_logic_neighborhood`
- `get_logic_span`
- `get_dependency_neighborhood`
- `get_symbol_neighborhood`
- `get_reasoning_context`
- `get_planned_context`
- `get_repo_map_hierarchy`
- `get_module_dependencies`
- `search_semantic_symbol`
- `get_workspace_reasoning_context`
- `plan_safe_edit`

## Reasoning Retrieval

Implemented reasoning retrieval combines logic-node and dependency traversal:

- `get_dependency_neighborhood`: BFS over dependency graph by symbol.
- `get_symbol_neighborhood`: symbol with local logic and dependency neighbors.
- `get_reasoning_context`: hybrid logic + dependency context with deterministic ordering.

## Cognitive Retrieval Pipeline

```text
API -> Planner -> Retrieval Engine -> Budgeter -> Context Assembler
```

`get_planned_context` adds:

- query intent detection
- deterministic retrieval planning
- token-budget-based context selection
- adaptive small-repo bypass (`files < 50`)

## Graph Semantics

Implemented graph semantics now include:

- persisted control-flow edges (`get_control_flow_slice`)
- persisted data-flow edges with variable names (`get_data_flow_slice`)
- semantic labels on logic nodes (`get_logic_nodes`)
- clustered logic regions (`get_logic_clusters`)
- hybrid graph ranking that blends symbol, dependency, and graph-density signals

## Module Graph and Hierarchy

Phase-4.5 adds module-aware indexing and retrieval:

- module detection from `src/` and `lib/` structure
- module dependency inference from symbol dependency edges
- hierarchical repo map retrieval (`modules -> files -> symbols`)
- module-aware planning and budgeting priority

## Workspace Intelligence

Phase-5 adds:

- repository registry and dependency graph
- repo-aware symbol/dependency records
- semantic symbol search fallback
- workspace-level reasoning context retrieval
- AST cache and invalidation engine modules

## Safe Editing and Routing (Phase-6)

New modules:

- `impact_analysis`
- `safe_edit_planner`
- `patch_engine`
- `llm_router`
- `policy_engine`
- `patch_memory`
- `refactor_graph`
- `code_health`
- `architecture_analysis`
- `improvement_planner`
- `evolution_graph`
- `knowledge_graph`

Safe edit pipeline:

```text
Agent/IDE -> Edit Request -> Impact Analysis -> Safe Edit Planner -> LLM Router -> Patch Engine -> Validation/Policy -> Apply
```

Patch representation:

- `ASTTransform` (engine-native edit intent)
- `UnifiedDiff` (preview/apply format)

Config files:

- `.semantic/edit_config.toml`
- `.semantic/llm_config.toml`
- `.semantic/llm_routing.toml`
- `.semantic/model_metrics.json`
- `.semantic/policies.toml`
- `.semantic/validation.toml`

Edit endpoint:

```bash
curl -X PATCH "$SEMANTIC_API_BASE_URL/edit" \
  -H "content-type: application/json" \
  -d '{"symbol":"retryRequest","edit":"add exponential backoff","patch_mode":"preview_only","run_tests":true}'
```

Patch memory endpoints:

```bash
curl "$SEMANTIC_API_BASE_URL/patch_memory?symbol=retryRequest"
curl "$SEMANTIC_API_BASE_URL/patch_stats?repository=my-repo"
curl "$SEMANTIC_API_BASE_URL/model_performance"
```

Refactor status endpoint:

```bash
curl "$SEMANTIC_API_BASE_URL/refactor_status"
```

Refactor snapshots are stored in:

- `.semantic/refactor_snapshots/`

Evolution endpoints:

```bash
curl "$SEMANTIC_API_BASE_URL/evolution_issues?repository=core_api"
curl "$SEMANTIC_API_BASE_URL/evolution_plans?repository=core_api"
curl -X POST "$SEMANTIC_API_BASE_URL/generate_evolution_plan" \
  -H "content-type: application/json" \
  -d '{"repository":"core_api","dry_run":true}'
```

## Example Request

```bash
curl -X POST "$SEMANTIC_API_BASE_URL/retrieve" \
  -H "content-type: application/json" \
  -d '{"operation":"get_function","name":"retryRequest"}'
```

## Notes

- Storage paths: `./.semantic/semantic.db` and `./.semantic/tantivy/`.
- Index performance stats: `./.semantic/index_performance.json`.
- Watcher reindexes changed files incrementally.
- Runtime docs use environment placeholders such as `$SEMANTIC_API_BASE_URL`; do not commit local URLs, API keys, or tokens.
