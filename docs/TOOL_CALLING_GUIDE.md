# Semantic Layer Tool Calling Guide

This file explains the objective and usage of semantic layer tool calling, including direct API usage and MCP bridge usage.

## Objective

Use semantic tools to reduce unnecessary context transfer and improve edit accuracy by retrieving only the code and relationships needed for a task.

Primary goals:

- precise symbol/span retrieval for planning and editing
- impact-aware edits across dependent symbols/files
- lower repeated context cost via indexed retrieval and scoped calls

## Entry Points

- Direct API:
  - `POST /retrieve` for retrieval operations
  - `PATCH /edit` for safe edit planning/execution
  - `GET /llm_tools` for discoverable tool metadata
  - `POST /ide_autoroute` for IDE-native semantic-first routing
  - `GET /performance_stats` for runtime hardening metrics
  - `GET /control_flow_hints?symbol=...` for control-flow hints
  - `GET /data_flow_hints?symbol=...` for data-flow hints
  - `POST /hybrid_ranked_context` for hybrid ranked context retrieval
- MCP bridge:
  - `GET /mcp/tools`
  - `POST /mcp/tools/call`

## Core Retrieval Operations (`POST /retrieve`)

Request envelope:

```json
{
  "operation": "get_planned_context",
  "query": "add due date validation",
  "semantic_enabled": true,
  "single_file_fast_path": true,
  "reference_only": true
}
```

Supported operations and when to use them:

- `get_repo_map`: list indexed files
- `get_file_outline`: list symbols in one file (`file` required)
- `search_symbol`: lexical symbol lookup (`name` required)
- `get_function`: return function span by symbol name (`name` required)
- `get_class`: return class span by symbol name (`name` required)
- `get_dependencies`: direct dependencies for a symbol (`name` required)
- `get_code_span`: exact line span retrieval (`file`, `start_line`, `end_line`)
- `get_logic_nodes`: logic node retrieval for symbol (`name`)
- `get_logic_neighborhood`: neighboring logic nodes around node id (`node_id`, `radius`)
- `get_logic_span`: retrieve code for one logic node (`node_id`)
- `get_dependency_neighborhood`: caller/callee traversal (`name`, `radius`)
- `get_symbol_neighborhood`: combined local + dependency neighborhood (`name`, optional radii)
- `get_reasoning_context`: structured context for edit reasoning (`name`, logic/dependency radii)
- `get_planned_context`: budgeted, intent-driven context (`query`, `max_tokens`)
- `get_repo_map_hierarchy`: module -> file -> symbol hierarchy
- `get_module_dependencies`: module dependency edges
- `search_semantic_symbol`: semantic fallback search when lexical misses (`query`)
- `get_workspace_reasoning_context`: cross-repository/workspace context (`query`)
- `plan_safe_edit`: impact-aware patch planning (`name`/`query`, `edit_description`)

## Edit Path (`PATCH /edit`)

Use for safe edit planning and patch preview/apply.

Example:

```json
{
  "symbol": "createTask",
  "edit": "add due date validation",
  "patch_mode": "PreviewOnly",
  "run_tests": false
}
```

Patch modes:

- `PreviewOnly`
- `Confirm`
- `AutoApply`

## Middleware Policy for Semantic-First

- `GET /semantic_middleware`
- `POST /semantic_middleware`

When enabled (`semantic_first_enabled=true`):

- `/edit` requires a prior `/retrieve` in the same `session_id`.
- `/retrieve` stores successful session ids when `session_id` is present.

This enforces semantic retrieval before edit execution.

## MCP Tool Calling

MCP bridge provides tool wrappers for the same retrieval/edit stack.

Discovery:

- `GET /mcp/tools`

Call:

- `POST /mcp/tools/call`
- header: `x-mcp-token: <LOCAL_BRIDGE_TOKEN>`

Common MCP tool names:

- `llm_tools`
- `get_repo_map`
- `get_file_outline`
- `search_symbol`
- `get_code_span`
- `get_logic_nodes`
- `get_dependency_neighborhood`
- `get_reasoning_context`
- `get_planned_context`
- `plan_safe_edit`
- `ab_test_dev`
- `ab_test_dev_results`

## Suggested Call Sequence in IDE Agents

1. For planning: call `get_planned_context`.
2. For pinpointing edits: call `search_symbol` or `get_code_span`.
3. For cross-file risk: call `get_dependency_neighborhood` or `get_reasoning_context`.
4. For patch planning: call `plan_safe_edit` or `/edit`.
5. For obvious single-file edits: set `single_file_fast_path=true`.

IDE-native shortcut:

1. Call `ide_autoroute` first with `{ task, session_id, max_tokens, single_file_fast_path, reference_only }`.
2. Use returned `selected_tool` + `result` as the first semantic context.
3. If editing is needed, use `result.minimal_raw_seed` (auto-filled when `reference_only=true`) as the smallest editable raw span.
4. Continue with `plan_safe_edit` / `/edit` using the same `session_id`.

## Token Usage Guidance

Best savings:

- large repositories
- repeated lookups in the same sessions
- multi-step edit workflows with dependency reasoning

Can cost more:

- tiny one-file tasks where orchestration overhead dominates
- over-broad `limit`/`radius`/`max_tokens` settings
- unnecessary repeated semantic calls for trivial edits

## Large Documents and Rulebooks (How To Use Semantic Retrieval)

Current state:

- The core parser/indexer is code-focused (Python/JavaScript/TypeScript).
- Large policy/rules markdown documents should be used as:
  - always-on short global rules in IDE skill/rule sections
  - conditional/contextual sections retrieved on demand via semantic-first routing

Recommended pattern:

1. Keep global non-negotiable rules short in IDE skills/rules.
2. Split large rulebooks by topic into smaller files/sections.
3. Route first with `ide_autoroute` using the task description.
4. Fetch only minimal spans (`reference_only=true` + `minimal_raw_seed`).
5. For edit execution, fetch raw code only for the selected target span.

Why this works:

- avoids attaching full rulebooks to every prompt
- preserves deterministic code-context retrieval for edits
- reduces repeated context transfer in multi-step workflows

Mitigations implemented:

- planned-context cache with TTL for repeated query reuse
- default `single_file_fast_path=true` in `ide_autoroute`
- default `reference_only=true` in `/retrieve` and `ide_autoroute`
- adaptive retrieval breadth (`effective_breadth`) based on fanout, logic density, and token budget
- bounded response shapes (no forced expansion of extra raw files)

Additional A/B hardening updates (2026-03-13):

- planner now supports target symbol hints and normalized symbol matching
  - e.g., spaced query terms can resolve camelCase symbols more reliably
- autoroute minimal raw seed is constrained to target-aligned spans
- A/B semantic arm uses per-task refs (not cross-task ref suppression)
- escalation uses replacement-style selection for token accounting
  - if heavy context wins quality, heavy path becomes the counted path
- `/ab_test_dev` now emits richer task diagnostics:
  - `planned_target_symbol`, `target_match`, `seed_target_aligned`
  - `semantic_route`, `semantic_prompt_chars`, `control_prompt_chars`
  - suite-level `gating_metrics` in response JSON

Recent A/B results (todo dev suite):

- prior baseline: `-18.62%` token savings
- post-improvements runs: `+8.07%` and `+5.70%` token savings
- task success remained `11/11` for both arms

## Discoverability

Use these endpoints at runtime:

- `GET /llm_tools` for semantic tool metadata and policies
- `GET /mcp/tools` for MCP-exposed tool list
