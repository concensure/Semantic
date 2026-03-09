# Agent Cognitive Layer Architecture

## Goal
Provide deterministic, minimal-span code retrieval for agents using symbol, dependency, and logic-node layers.

## Component Map

- `engine`: shared DTOs and operation contract.
- `parser`: Tree-sitter parsing for Python/JavaScript/TypeScript.
- `storage`: SQLite (source of truth) + Tantivy (symbol acceleration).
- `indexer`: full scan + incremental file indexing.
- `retrieval`: operation dispatcher and deterministic responses.
- `planner`: deterministic intent detection and retrieval plan generation.
- `budgeter`: token-aware context selection by priority.
- `impact_analysis`: dependency/logic/module-aware impact traversal for edits.
- `safe_edit_planner`: edit-type classification + context-complete edit plan construction.
- `patch_engine`: AST-transform patch representation, diff preview conversion, validation.
- `patch_memory`: patch lifecycle logging, stats aggregation, model performance memory, risk scoring.
- `refactor_graph`: multi-file refactor graph planning, Kahn scheduling, transactional execution, rollback.
- `code_health`: static health-signal analyzer.
- `architecture_analysis`: graph-driven architecture issue detector.
- `improvement_planner`: converts issues into actionable improvement plans.
- `evolution_graph`: translates improvement plans into evolution/refactor graphs.
- `knowledge_graph`: persists long-term design and evolution knowledge.
- `llm_router`: task-based, performance-aware provider routing.
- `policy_engine`: user-defined safety/approval enforcement.
- `watcher`: file event handler for incremental refresh.
- `api`: local JSON server on `127.0.0.1:4317`.

## Retrieval Layers

- Symbol layer: functions/classes/imports.
- Dependency layer: caller -> callee adjacency list.
- Logic layer: semantic sub-blocks inside functions/methods (`logic_nodes`, `logic_edges`).
- Cognitive layer: plan + budget pipeline for minimal high-value context.
- Module layer: module graph + hierarchical repository map.
- Workspace layer: repository registry and cross-repository context planning.

## Phase-4 Pipeline Diagram

```text
API
  |
Planner
  |
Retrieval Engine
  |
Budgeter
  |
Context Assembler
```

## Phase-6 Safe Edit Pipeline

```text
Agent / IDE
  |
Edit Request
  |
Impact Analysis
  |
Safe Edit Planner
  |
LLM Router
  |
Patch Engine (AST transform -> unified diff preview)
  |
Patch Memory (record + aggregate)
  |
Policy + Validation Gates
  |
Patch Application
```

## Refactor Graph Pipeline

```text
High-Level Refactor Request
  |
Refactor Graph Builder
  |
Dependency Scheduler (Kahn)
  |
Executor (sequential node execution)
  |
Patch Memory + Status Tracking
  |
Commit / Rollback (snapshots)
```

## Evolution Pipeline (Phase-8)

```text
Workspace Scan
  |
Code Health Analysis
  |
Architecture Analysis
  |
Improvement Planning
  |
Evolution Graph Construction
  |
Proposal + Risk Estimation
  |
Simulation (dry-run, no edits)
```

## Deterministic Contract

Supported operations:

- `get_repo_map`
- `get_file_outline`
- `search_symbol`
- `get_function`
- `get_class`
- `get_dependencies`
- `get_code_span`
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

Determinism rules:

- explicit required-input validation per operation.
- stable sorting by source position / id.
- no probabilistic ranking in logic retrieval.

## Storage Architecture

SQLite tables:

- `files`
- `symbols`
- `dependencies`
- `logic_nodes`
- `logic_edges`
- `rules`
- `skills`

Tantivy fields:

- `name`
- `file`
- `language`
- `type`

## Indexing Flow

1. repo scan + extension filter.
2. parse AST.
3. extract symbols.
4. extract dependencies.
5. extract logic nodes from function/method subtrees.
6. build sequential logic edges per symbol.
7. atomic file replace in SQLite transaction.
8. refresh Tantivy index.

## Incremental Updates

On create/modify:

- checksum compare.
- parse changed file only.
- replace file-scoped symbols/dependencies/logic_nodes/logic_edges.

On delete:

- remove file-scoped symbols/dependencies/logic graph and file metadata.

## Extensibility Path

Reserved for next phases:

- CFG/data-flow edges.
- logic clustering.
- semantic labels.
- hybrid ranking over symbol + dependency + logic graphs.

## Reasoning Retrieval

Phase-3 adds deterministic reasoning-level retrieval by combining:

- symbol-level entrypoint lookup
- logic-node neighborhood traversal
- dependency neighborhood traversal

`get_reasoning_context` merges symbol, logic spans, and dependency spans in fixed order:

1. symbol
2. logic nodes
3. dependencies

## Planned Context Retrieval

`get_planned_context` flow:

1. detect intent from query heuristics
2. build deterministic retrieval plan
3. fetch reasoning context (logic + dependency)
4. apply token budget unless small-repo mode (`files < 50`)
5. return assembled context items sorted by priority, file, line

## Module Graph and Hierarchy

Phase-4.5 adds:

- module detection from top-level directories under `src/` and `lib/`
- file-to-module membership
- module dependency graph inferred from symbol dependency edges
- hierarchy retrieval (`modules -> files -> symbols`)

Planner and budgeter integration:

- planner scopes search to target module + dependency modules
- budgeter prioritizes scoped modules before other modules

## Workspace Intelligence (Phase-5)

New modules:

- `workspace`
- `repo_registry`
- `symbol_similarity`
- `semantic_search`
- `invalidation_engine`
- `ast_cache`

New graph path:

`Workspace -> Repositories -> Modules -> Files -> Symbols -> Logic Nodes`

## Safe Editing (Phase-6)

Additive capabilities:

- impact reports with symbol/file/test and signature propagation hints.
- edit planning with explicit `EditType` and required context bundle.
- AST-aware patch representation:
  - `PatchRepresentation::ASTTransform(ASTEdit)`
  - `PatchRepresentation::UnifiedDiff(String)`
- configurable patch mode:
  - `confirm`
  - `auto_apply`
  - `preview_only`
- LLM provider routing by task and observed metrics.
- patch-memory-aware routing (historical model success blended with latency/cost).
- planner feedback loop from edit-type risk scores.
- policy enforcement for protected paths and mandatory confirmations.
