# Detailed Design

## 1. Engine Module

Purpose: central contract for all crates.

Added Phase-2 types:

- `LogicNodeType`:
  - `Loop`, `Conditional`, `Try`, `Catch`, `Finally`, `Return`, `Call`, `Await`, `Assignment`, `Throw`, `Switch`, `Case`
- `LogicNodeRecord`
- `LogicEdgeRecord`

Contract updates:

- `ParsedFile.logic_nodes`
- `RetrievalRequest.node_id`
- `RetrievalRequest.radius`
- new operations: `GetLogicNodes`, `GetLogicNeighborhood`, `GetLogicSpan`

## 2. Parser Module (Tree-sitter)

Purpose: parse source and emit symbols/dependencies/logic nodes.

Language support:

- Python (`.py`)
- JavaScript (`.js`, `.jsx`)
- TypeScript (`.ts`, `.tsx`)

Logic extraction strategy:

1. Detect function/method nodes.
2. Traverse each function subtree.
3. Skip nested function bodies when collecting parent logic nodes.
4. Map AST kinds to `LogicNodeType`.
5. Emit line spans (`start_line`, `end_line`) with temporary `symbol_id` placeholder.

Node kind mapping:

- Loop: `for_statement`, `while_statement`, `for_in_statement`
- Conditional: `if_statement`
- Try: `try_statement`
- Catch: `except_clause`, `catch_clause`
- Finally: `finally_clause`
- Return: `return_statement`
- Call: `call`, `call_expression`
- Await: `await`, `await_expression`
- Assignment: `assignment`, `assignment_expression`, `augmented_assignment`
- Throw: `raise_statement`, `throw_statement`
- Switch: `switch_statement`
- Case: `switch_case`

## 3. Storage Module (SQLite + Tantivy)

Purpose: durable store and deterministic graph queries.

Schema additions:

- `logic_nodes(id, symbol_id, node_type, start_line, end_line)`
- `logic_edges(id, from_node_id, to_node_id)`
- indexes: `idx_logic_symbol`, `idx_logic_from`, `idx_logic_to`

Key methods:

- `replace_file_index(...)` atomic file-scoped replace transaction.
- `insert_logic_nodes(symbol_id, nodes)`
- `insert_logic_edges(edges)`
- `get_logic_nodes(symbol_id)`
- `get_logic_node(node_id)`
- `get_logic_node_file(node_id)`
- `get_logic_neighbors(node_id, radius)` BFS neighborhood.

Design choices:

- SQLite remains source of truth.
- Tantivy remains symbol-only acceleration layer.
- logic neighborhood traversal is deterministic (BFS + sorted output).

## 4. Indexer Module

Purpose: coordinate parsing and atomic persistence.

Phase-2 behavior in `index_file`:

1. checksum short-circuit.
2. parse file for symbols, dependencies, logic nodes.
3. call `replace_file_index(...)` (single transaction).
4. refresh Tantivy.

This preserves backward compatibility while adding logic indexing.

## 5. Retrieval Module

Existing operations are unchanged.

New operations:

- `get_logic_nodes`
  - input: `name` (symbol name)
  - output: nodes sorted by source position.

- `get_logic_neighborhood`
  - input: `node_id`, `radius`
  - output: BFS-reachable nodes within `radius`, sorted deterministically.

- `get_logic_span`
  - input: `node_id`
  - output: minimal code span for that node.

- `get_dependency_neighborhood`
  - input: `name`, `radius`
  - output: BFS dependency neighbors (deterministic).

- `get_symbol_neighborhood`
  - input: `name`, `radius`
  - output: symbol + logic nodes + dependency neighbors.

- `get_reasoning_context`
  - input: `name`, `logic_radius`, `dependency_radius`
  - output: merged deterministic reasoning bundle with logic and dependency spans.

Determinism:

- no randomization or probabilistic ordering.
- stable ordering by `(start_line, end_line, id)`.

Phase-3 ordering contract:

- groups are returned in order: symbol, logic nodes, dependencies.
- within groups, sort by file path then source line.

## 6. Watcher + API Modules

Watcher behavior unchanged except Phase-2 data now flows through the same incremental `index_file` path.

API behavior:

- existing endpoints unchanged.
- `/retrieve` now accepts new operations and request fields.

## 7. Planner Module

Purpose: deterministic intent detection and plan generation for cognitive retrieval.

Types:

- `QueryIntent`: `Debug`, `Refactor`, `Understand`, `LocateSymbol`
- `RetrievalPlan`: `target_symbol`, `logic_radius`, `dependency_radius`, `include_callers`
  - includes `scoped_modules` derived from module graph

Heuristics:

- `fix|bug|error` -> `Debug`
- `refactor|rewrite|optimize` -> `Refactor`
- `how does|explain` -> `Understand`

Intent effects:

- `Debug`: `logic_radius=1`, `dependency_radius=1`
- `Refactor`: `logic_radius=1`, `dependency_radius=2`
- `Understand`: `logic_radius=2`, `dependency_radius=1`

## 8. Budgeter Module

Purpose: select context items under token budget.

Types:

- `ContextBudget { max_tokens, reserved_prompt }`
- `ContextItem { file_path, module_name, module_rank, start_line, end_line, priority, text }`

Priority:

- `0`: target symbol
- `1`: logic nodes
- `2`: direct dependencies
- `3`: dependency neighbors

Token estimation:

- `tokens ~= chars / 4`

Selection algorithm:

1. sort items by priority, module rank, file path, start line
2. include item if estimated tokens fit remaining budget
3. stop at first overflow

Small-repo mode:

- if indexed files `< 50`, budget step is skipped.

## 9. Module Graph and Hierarchy (Phase-4.5)

Storage schema additions:

- `modules`
- `module_files`
- `module_dependencies`

Indexer integration:

1. detect modules from directory structure (`src/<module>`, `lib/<module>`)
2. map indexed files to module ids
3. infer inter-module edges from symbol dependency edges
4. persist module graph after indexing updates

New operations:

- `get_repo_map_hierarchy`: modules -> files -> symbols
- `get_module_dependencies`: module edge list

Planner integration:

- target symbol module identified from symbol/file membership
- scoped modules = target module + dependency modules

Budgeter integration:

- module rank prioritization:
  - target module
  - dependency modules
  - other modules

## 10. Tests and Fixture

Fixture extended with async logic-rich TS function:

- `test_repo/src/client.ts::fetchData`

Expected extracted nodes include:

- `Conditional`
- `Throw`
- `Await`
- `Return`

Coverage focus:

- parser logic-node extraction
- storage logic-node persistence
- retrieval logic operations
- backward compatibility for existing retrieval operations
- planner intent and plan generation
- planner module scoping
- budget truncation and deterministic ordering
- module detection, membership, and dependency inference

## 11. Workspace Intelligence (Phase-5)

New engine models:

- `Workspace`
- `RepositoryRecord`
- `RepoDependency`

Schema additions:

- `repositories`
- `repo_dependencies`
- `repo_id` on `symbols` and `dependencies`

New modules:

- `workspace`: repository registration/list/get.
- `repo_registry`: registry access wrapper.
- `symbol_similarity`: deterministic string similarity.
- `semantic_search`: lexical -> semantic fallback symbol lookup.
- `invalidation_engine`: dependency-driven stale symbol expansion.
- `ast_cache`: checksum-keyed AST cache.

New retrieval operations:

- `search_semantic_symbol`
- `get_workspace_reasoning_context`

## 12. Safe Editing + Multi-Model Routing (Phase-6)

New engine models:

- `ImpactReport`
- `SignatureImpact`
- `EditPlan`
- `EditType`
- `PatchApplicationMode`
- `PatchRepresentation`
- `ASTEdit`
- `ASTTransformation`
- `CodePatch`

New retrieval operation:

- `plan_safe_edit`

### 12.1 Impact Analysis Module (`impact_analysis`)

Purpose:

- compute deterministic edit blast radius from existing graph data.

Inputs:

- changed symbol name
- symbol dependency graph
- invalidation expansion graph

Outputs:

- impacted symbols
- impacted files
- impacted tests
- signature impact hints

Traversal:

- BFS-based stale expansion via invalidation engine.

### 12.2 Safe Edit Planner Module (`safe_edit_planner`)

Purpose:

- construct safe edit plan before generating modifications.

Pipeline:

1. run impact analysis.
2. classify edit type from edit description.
3. collect required context spans for target + impacted symbols.
4. emit deterministic `EditPlan`.

### 12.3 Patch Engine Module (`patch_engine`)

Purpose:

- represent edits as AST transforms and convert to previewable unified diff.

Patch model:

- `PatchRepresentation::ASTTransform(ASTEdit)`
- `PatchRepresentation::UnifiedDiff(String)`

Flow:

1. generate AST transform.
2. convert AST transform to unified-diff preview.
3. validate patch preconditions (syntax parse guard).
4. return preview + apply metadata.

### 12.4 LLM Router Module (`llm_router`)

Purpose:

- deterministic provider routing by task and performance metadata.

Task model:

- `Planning`
- `CodeExecution`
- `InteractiveChat`

Inputs:

- `.semantic/llm_config.toml` (provider endpoints)
- `.semantic/llm_routing.toml` (preferred providers by task)
- `.semantic/model_metrics.json` (latency/cost/failure)

Selection:

- preferred list filtered to configured providers, ranked by metric score.

### 12.5 Policy Engine Module (`policy_engine`)

Purpose:

- enforce user-defined edit safety rules.

Inputs:

- `.semantic/policies.toml`

Rules:

- protected paths are non-editable.
- selected edit types can require confirmation.

### 12.6 Retrieval/API Integration

New endpoint:

- `PATCH /edit`

Request:

- `symbol`
- `edit`
- optional `patch_mode`
- optional `run_tests`
- optional `max_tokens`

`plan_safe_edit` execution path:

1. impact report
2. safe edit plan
3. policy validation
4. LLM route decision
5. patch preview generation
6. patch mode resolution (`confirm` default)
7. optional validation-config resolution

Response payload includes:

- impact report
- edit plan
- route decision
- patch preview
- patch mode + confirmation requirement
- validation configuration summary

## 13. Patch Memory (Phase-6.x)

New crate:

- `patch_memory`

Storage location:

- `.semantic/patch_memory/patch_log.jsonl`
- `.semantic/patch_memory/patch_stats.json`
- `.semantic/patch_memory/model_performance.json`

Primary record:

- `PatchRecord` with patch metadata, approval/validation/test outcomes, and rollback tracking.

Aggregation outputs:

- patch success rate
- rollback frequency
- per-model success rate
- per-edit-type success rate

Router integration:

- merge patch-memory model success into router metrics.
- routing score:
  - `0.6 * success_rate`
  - `0.2 * latency_score`
  - `0.2 * cost_score`

Planner feedback loop:

- safe edit planner queries patch memory for `EditRiskScore`.
- high-risk edit types expand context to include caller contexts.

New API endpoints:

- `GET /patch_memory`
- `GET /patch_stats`
- `GET /model_performance`

Optional filters:

- `repository`
- `symbol`
- `model`
- `time_range` (`from,to` unix seconds)

## 14. Refactor Graph Engine (Phase-7)

New crate:

- `refactor_graph`

Core types:

- `RefactorNode { id, target_symbol, edit_plan, dependencies, repository }`
- `RefactorGraph { nodes }`
- `RefactorStatus { completed_nodes, pending_nodes, failed_nodes }`

Construction:

- high-level request expands into node set:
  - type definitions
  - callers/import updates
  - tests

Scheduling:

- deterministic topological ordering using Kahn's algorithm.

Execution:

1. begin transaction
2. execute nodes sequentially
3. per-node patch generation + validation + apply
4. per-node patch-memory record
5. commit on success, rollback on failure

Transaction storage:

- snapshots in `.semantic/refactor_snapshots/<refactor_id>/`

Progress endpoint:

- `GET /refactor_status`

Risk-based behavior:

- patch memory `EditRiskScore` is consulted before each node.
- high-risk nodes can require confirmation (configurable execution options).

## 15. Autonomous Code Evolution Engine (Phase-8)

New crates:

- `code_health`
- `architecture_analysis`
- `improvement_planner`
- `evolution_graph`
- `knowledge_graph`

Code health issue model:

- `CodeHealthIssue`
- `IssueType`: `DeadCode`, `DuplicateLogic`, `LargeFunction`, `LargeModule`, `CircularDependency`, `UnusedImport`, `DeepNesting`

Architecture issue model:

- `ArchitectureIssue`
- `ArchitectureIssueType`: `LayerViolation`, `TightCoupling`, `MissingInterface`, `CyclicModuleDependency`, `GodModule`

Improvement plan model:

- `ImprovementPlan`
- `ImprovementType`: `RemoveDeadCode`, `ExtractInterface`, `SplitModule`, `SimplifyLogic`, `DeduplicateCode`

Evolution graph model:

- `EvolutionNode`
- `EvolutionGraph`

Knowledge graph persistence:

- `.semantic/knowledge_graph/knowledge.jsonl`

Risk estimation:

- `EvolutionRisk` from:
  - patch memory failure rate
  - impacted plan count
  - test-file presence signal

Phase-8 API endpoints:

- `GET /evolution_issues`
- `GET /evolution_plans`
- `POST /generate_evolution_plan`

Simulation mode:

- `dry_run=true` returns estimated patch/node counts and preview nodes.
- no edits are performed.
