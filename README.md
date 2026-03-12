# Semantic Agent Cognitive Layer (MVP)

Local Rust service for deterministic code retrieval by symbol/code span.

## IDE Semantic-First Integration

Use the semantic-first integration guide for RooCode/KiloCode/Codex/Claude wiring, middleware policy controls, and end-to-end flow diagrams:

- `docs/IDE_SEMANTIC_FIRST.md`

## Modules

- `engine` shared contracts
- `parser` Tree-sitter extraction
- `storage` SQLite + Tantivy
- `indexer` repo indexing orchestration
- `retrieval` operation handlers
- `watcher` incremental file updates
- `api` Axum JSON service

Phase-2 adds logic-node indexing and retrieval for semantic sub-blocks.

## Run

```bash
cargo run -p api -- ./test_repo
```

Service binds to `127.0.0.1:4317`.

Demo project used by the development A/B suite:

- `test_repo/todo_app/`

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

Phase-3 reasoning retrieval combines logic-node and dependency traversal:

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
curl -X PATCH <SEMANTIC_API_BASE_URL>/edit \
  -H "content-type: application/json" \
  -d '{"symbol":"retryRequest","edit":"add exponential backoff","patch_mode":"preview_only","run_tests":true}'
```

Patch memory endpoints:

```bash
curl "<SEMANTIC_API_BASE_URL>/patch_memory?symbol=retryRequest"
curl "<SEMANTIC_API_BASE_URL>/patch_stats?repository=my-repo"
curl "<SEMANTIC_API_BASE_URL>/model_performance"
```

Refactor status endpoint:

```bash
curl "<SEMANTIC_API_BASE_URL>/refactor_status"
```

Refactor snapshots are stored in:

- `.semantic/refactor_snapshots/`

Evolution endpoints:

```bash
curl "<SEMANTIC_API_BASE_URL>/evolution_issues?repository=core_api"
curl "<SEMANTIC_API_BASE_URL>/evolution_plans?repository=core_api"
curl -X POST "<SEMANTIC_API_BASE_URL>/generate_evolution_plan" \
  -H "content-type: application/json" \
  -d '{"repository":"core_api","dry_run":true}'
```

## Example Request

```bash
curl -X POST <SEMANTIC_API_BASE_URL>/retrieve \
  -H "content-type: application/json" \
  -d '{"operation":"get_function","name":"retryRequest"}'
```

## Notes

- Storage paths: `./.semantic/semantic.db` and `./.semantic/tantivy/`.
- Watcher reindexes changed files incrementally.
- Current parser is symbol-level; logic-node retrieval is designed as next phase.
