# Safe Editing

## Goal

Provide deterministic, policy-aware, preview-first code editing over the existing semantic retrieval engine.

## Pipeline

```text
Agent / IDE
  -> /edit (PATCH)
  -> Impact Analysis
  -> Safe Edit Planner
  -> LLM Router
  -> Patch Engine (AST transform -> unified diff)
  -> Patch Memory (record + stats update)
  -> Policy + Validation
  -> Apply / Preview
```

## Core Models

- `ImpactReport`
  - changed symbol
  - impacted symbols
  - impacted files
  - impacted tests
  - optional signature impact
- `EditPlan`
  - target symbol
  - edit type
  - impacted symbols
  - required context
- `CodePatch`
  - file path
  - representation: `UnifiedDiff` or `ASTTransform`

## Patch Modes

Configured in `.semantic/edit_config.toml`.

- `confirm`: show preview, require confirmation before apply.
- `auto_apply`: apply automatically after checks.
- `preview_only`: do not apply, return preview only.

Request-level overrides can be provided through `/edit`.

## Policies

Configured in `.semantic/policies.toml`.

Supported controls:

- `protected_paths`
- `require_confirmation_for`

These are enforced before patch apply.

## LLM Routing

Routing is task-based and performance-aware:

- tasks: `Planning`, `CodeExecution`, `InteractiveChat`
- provider endpoints: `.semantic/llm_config.toml`
- task preferences: `.semantic/llm_routing.toml`
- runtime metrics: `.semantic/model_metrics.json`

Router selects among preferred providers using deterministic score ranking.

## Validation

Optional validation toggles in `.semantic/validation.toml`:

- `run_tests`
- `run_lint`
- `run_typecheck`

Current implementation resolves validation config and returns execution intent metadata in edit response.

## Patch Memory

Patch history is persisted in:

- `.semantic/patch_memory/patch_log.jsonl`
- `.semantic/patch_memory/patch_stats.json`
- `.semantic/patch_memory/model_performance.json`

Exposed endpoints:

- `GET /patch_memory`
- `GET /patch_stats`
- `GET /model_performance`

Each supports optional filtering by `repository`, `symbol`, `model`, and `time_range`.
