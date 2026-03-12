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
  "single_file_fast_path": true
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

## Token Usage Guidance

Best savings:

- large repositories
- repeated lookups in the same sessions
- multi-step edit workflows with dependency reasoning

Can cost more:

- tiny one-file tasks where orchestration overhead dominates
- over-broad `limit`/`radius`/`max_tokens` settings
- unnecessary repeated semantic calls for trivial edits

## Discoverability

Use these endpoints at runtime:

- `GET /llm_tools` for semantic tool metadata and policies
- `GET /mcp/tools` for MCP-exposed tool list
