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

## Optional Project Summariser Add-On

A companion crate (`project_summariser`) generates a compact, LLM-ready project map at session start — no LLM call required, built entirely from the existing index.

```bash
curl "$SEMANTIC_API_BASE_URL/project_summary?max_tokens=800&format=markdown"
```

Or via MCP `retrieve` tool:

```json
{ "operation": "GetProjectSummary", "max_tokens": 800 }
```

Or prepended automatically on `ide_autoroute` with `include_summary=true`:

```json
{ "task": "add due date to tasks", "include_summary": true }
```

Output (~400–800 tokens): per-file purpose sentence, top symbols, project narrative, module dependency sketch. JSON and markdown formats supported.

See `semantic_project_summariser/PLAN.md` (sibling folder) for the full design.

## Optional Token Tracking Add-On

An optional local companion in this repository can track token usage per task across `retrieve`, `ide_autoroute`, and `edit`.

1. Copy `.semantic/token_tracking.example.toml` to `.semantic/token_tracking.toml`.
2. Set `enabled = true`.
3. Run the core API as usual.
4. Run the dashboard:

```bash
cargo run -p token_tracking -- ./test_repo
```

Telemetry is written as NDJSON to `.semantic/token_tracking/events.ndjson` and ingested into `.semantic/token_tracking/tracker.sqlite`.

Privacy defaults:

- `strict`: metrics only, hashed paths, no prompt bodies
- `balanced`: small redacted snippets
- `debug`: richer local capture

## Two-Tool MCP Surface

The MCP bridge (`mcp_bridge`) exposes **two primary tools** that cover all use cases:

- **`retrieve`** — unified retrieval. Pass `operation` to select: `GetRepoMap`, `GetFileOutline`, `SearchSymbol`, `GetCodeSpan`, `GetLogicNodes`, `GetControlFlowSlice`, `GetDataFlowSlice`, `GetLogicClusters`, `GetDependencyNeighborhood`, `GetReasoningContext`, `GetPlannedContext`, `PlanSafeEdit`, `GetControlFlowHints`, `GetDataFlowHints`, `GetHybridRankedContext`, `GetDebugGraph`, `GetPipelineGraph`, `GetRootCauseCandidates`, `GetTestGaps`, `GetDeploymentHistory`, `GetPerformanceStats`, `GetProjectSummary`.
- **`ide_autoroute`** — intent routing (`task`) or action dispatch (`action` + `action_input`). Actions: `debug_failure`, `generate_tests`, `apply_tests`, `analyze_pipeline`.

All 27 legacy named tools remain available for backward compatibility (see `GET /mcp/tools` → `legacy_tools`).

Key API endpoints:

- `POST /retrieve` — all retrieval and graph operations
- `POST /ide_autoroute` — intent routing and action dispatch
- `PATCH /edit` — safe edit planning/execution

Legacy MCP tool aliases are preserved for compatibility, but they are now routed through `retrieve` or `ide_autoroute` instead of depending on separate primary entrypoints.

Default retrieval behavior:

- `reference_only=true` (structured references first, raw code minimized)
- `single_file_fast_path=true` recommended for obvious single-file edits
- adaptive retrieval breadth to avoid over-fetch on high-fanout symbols

Demo project used by the development A/B suite:

- `test_repo/todo_app/`

## Latest A/B Benchmark Update (2026-03-27)

Run with `autoroute_first=true`, `single_file_fast_path=false`, provider=openai, 11-task core suite:

| Run | tokens_without | tokens_with | token_savings | step_savings | task_success |
|---|---:|---:|---:|---:|---:|
| Baseline (2026-03-13) | 9,738 | 11,551 | -18.62% | — | 11/11 |
| Hardened A (2026-03-13) | 9,365 | 8,609 | +8.07% | — | 11/11 |
| Hardened B (2026-03-13) | 9,749 | 9,193 | +5.70% | — | 11/11 |
| **Enhanced (2026-03-27)** | **9,723** | **10,377** | **-6.73%** | **+27.78%** | **11/11** |

The primary metric is now **step savings** (27.78% fewer estimated developer steps), not token savings per call. See `docs/AB_TEST_DEV_RESULTS.md` for full breakdown.

Test suite enhancements in 2026-03-27 run:
- equalized success thresholds (both arms now require `hits >= 2`, fixing an inflation bias)
- new `retrieval_quality` block: `avg_context_coverage_pct`, `avg_retrieval_ms`, `misdirection_risk_pct`
- new `validated_success_with_pct` (structural plan + keyword hit)
- per-task `retrieval_ms`, `context_coverage`, `misdirection_risk` fields
- local `estimate_tokens` in A/B test aligned with budgeter (`chars/3`)

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
- `get_project_summary`

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

## Phase 5 Maturity

Implemented maturity features now include:

- SQLite-backed planned-context cache with automatic invalidation on index changes
- policy-driven token caps and adaptive retrieval thresholds
- anti-bloat controls for small single-file tasks
- quality-gated A/B evaluation with validated patch/test signals
- p95/p99 latency alerts and cache hit-rate alerts in `GetPerformanceStats`
- full MCP compatibility through the two primary tools: `retrieve` and `ide_autoroute`

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
- Retrieval policy template: `./.semantic/retrieval_policy.example.toml`.
- Watcher reindexes changed files incrementally.
- Runtime docs use environment placeholders such as `$SEMANTIC_API_BASE_URL`; do not commit local URLs, API keys, or tokens.
