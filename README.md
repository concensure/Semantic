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

## CLI-First Usage

`semantic_cli` is now the primary local entrypoint. It bootstraps the same shared application layer that powers the compatibility API and MCP adapters.

## Install On Another Machine Or Project

The fastest local install path today is still Rust-native:

```bash
cargo install --path semantic_cli --locked --force --bin semantic
```

There is also a helper script in this repo:

```powershell
powershell -ExecutionPolicy Bypass -File scripts/install.ps1 -Force
```

That installs a `semantic` binary into your Cargo bin directory, so you can point it at any other project without moving the Semantic repo:

```bash
semantic --repo C:\path\to\other-project status
semantic --repo C:\path\to\other-project route --task "explain auth flow"
semantic --repo C:\path\to\other-project serve api
```

Normal CLI use now reuses an existing local index by default. If `.semantic/semantic.db` already has indexed files, `semantic status`, `semantic retrieve`, `semantic route`, and `semantic serve ...` reuse that index instead of forcing a full repo refresh on every invocation. Use `semantic index` when you want an explicit refresh.

`semantic status` now also avoids first-run indexing entirely. On an unindexed repo it reports:

- `index_available: false`
- `indexed_file_count: 0`
- `bootstrap_index_action: skip_bootstrap_refresh`

That makes status usable as a shallow preflight even on large repos before a full index exists.

Recommended broader-reliability workflow:

- keep this repository as the Semantic source/install repo
- run `semantic --repo <other-project> ...` against external repos
- let each target repo build its own local `.semantic/` runtime state
- rerun `semantic --repo <other-project> status` first when you want a quick preflight

Notes:

- `telemetry` and `test_planner` remain in-tree because they are active workspace members today
- `test_coverage`, `test_fixtures`, and `evolution_graph` can be ignored locally, but tracked files already in git will still remain tracked until deliberately removed from the repository history/workspace
- `.semantic/semantic.db` and `.semantic/tantivy/` are generated runtime state; deleting them is safe when you want a clean re-index
- initial indexing currently uses `full_with_default_excludes` / `source_focused` mode by default for broad external repos, which means obvious heavyweight paths such as `node_modules/`, `target/`, build outputs, caches, and common binary artifacts are skipped on the first pass

```bash
cargo run -p semantic_cli -- --repo ./test_repo/todo_app status
cargo run -p semantic_cli -- --repo ./test_repo/todo_app retrieve --op SearchSymbol --name addTask
cargo run -p semantic_cli -- --repo ./test_repo/todo_app route --task "add due date to tasks"
```

`status` output now also surfaces large-repo onboarding state directly:

- `indexing_mode`
- `indexing_completeness`
- `bootstrap_index_action`
- `indexed_path_hints`

Current values are:

- `indexing_mode: full_with_default_excludes`
- `indexing_completeness: source_focused`
- `bootstrap_index_action: reuse_existing|bootstrap_full|skip_bootstrap_refresh`
- `indexed_path_hints: ...` for the currently indexed directories/files

When no index exists yet, shallow navigation is still available through:

- `get_directory_brief`
- `get_file_brief`

Those operations now fall back to direct filesystem inspection for supported source/doc files, so Semantic can provide lightweight navigation context before a full index is built.

You can also build only the first region you care about:

```bash
semantic --repo C:\path\to\repo index --path src/auth
semantic --repo C:\path\to\repo index --path src/auth --path packages/api
```

Targeted indexing updates only the requested files/directories and keeps the rest of the repo unindexed until you explicitly expand coverage.

After targeted indexing, `semantic status` will surface those ready regions through `indexed_path_hints`, for example:

```text
indexed_path_hints: src/auth | packages/api/src
```

Retrieve and route flows now also surface coverage boundaries directly:

- `index_readiness`
- `index_coverage`
- `index_coverage_target`
- `suggested_index_command`

For partially indexed repos, route verification can now surface:

- `target_path_not_indexed`

That makes partial indexing explicit in normal CLI use instead of only degrading implicitly when a request points outside the indexed region. When coverage is missing, Semantic now also suggests the exact next command to run, for example:

```text
index_follow_up: semantic index --path src/worker
```

`index_readiness` is the compact machine-readable summary:

- `unindexed_repo`
- `target_ready`
- `partial_index_missing_target`
- `indexed_repo`

If you want Semantic to repair that gap automatically once, route and retrieve now support an explicit opt-in:

```bash
semantic --repo C:\path\to\repo route --task "understand src/worker job.ts" --auto-index-target
semantic --repo C:\path\to\repo retrieve --op get_directory_brief --path src/worker --auto-index-target
```

When that retry genuinely improves coverage, text output includes:

```text
auto_index: applied @ src/worker
```

If the target still does not exist or remains uncovered, Semantic keeps the original unindexed warning instead of falsely claiming the retry succeeded.
When the request names an exact file path, the retry now stays file-scoped instead of widening to the containing directory.

That means Semantic indexed the source-focused subset of the repo with heavyweight/generated paths excluded by default. This is an onboarding safeguard, not a claim of full staged/lazy indexing yet.

`route` text output now includes live verification state. When Semantic thinks the returned context needs manual inspection, the CLI prints a compact `verification: needs_review` line plus the recommended action. It also prints a compact `verification_scope` line showing the selected symbol and top file, a `mutation_safety` line for edit-capable routes, and, when mutation neighborhood verification fails, a compact `verification_graph_issue` line showing which files or symbols were missing or extra. Verbose mode adds exact verification checks like `target_in_file=true`, `target_span=false`, and `scope_graph=false`, plus a `verification_graph_diff` line with the full expected-vs-actual neighborhood summary. Use `--output json` for the full verification block and machine-readable issues.

For automation-heavy local flows, `route` also supports verification gates:

- `--require-high-confidence` exits non-zero unless the live verification status is `high_confidence`
- `--min-verification needs_review` allows `needs_review` and `high_confidence`, but still fails on weaker states like `low_confidence`, `no_context_refs`, or `fallback_search`
- `--require-mutation-ready` exits non-zero unless an edit-capable route is explicitly marked `mutation_safety: ready`

When a gate is used, route text output prints compact summary lines like `verification_gate: min=needs_review actual=needs_review` and `mutation_gate: min=ready actual=blocked` before any non-zero exit, so CI or local logs still show why the run was accepted or rejected.

For blocked `implement` or `refactor` routes, Semantic now also attempts a deterministic exact retry before giving up. If that retry can confirm the target through file outline or exact symbol lookup, the route is promoted to `mutation_safety: ready` and the retry evidence is attached in the verification metadata.

Quality status is also available directly from the CLI without booting the full runtime path:

```bash
cargo run -p semantic_cli -- --repo . status --quality
cargo run -p semantic_cli -- --repo . status --quality --output json
```

This reads the locally generated quality snapshot and reports the current `stable|watch|drifting` health plus recent retrieval/route latency deltas. The status output now splits that top-level health into `latency_health` and `graph_drift_health`, includes a compact machine-readable `diagnosis` such as `clean`, `latency_only_drift`, `graph_only_drift`, or `mixed_drift`, and surfaces an `action_recommendation` such as `no_action`, `watch_latency`, `inspect_graph_drift`, `inspect_incomplete_mutation_scope`, `investigate_mixed_regression`, or `investigate_mixed_incomplete_mutation_scope`, plus an `action_priority`, `triage_path`, `action_target`, `action_primary_command`, command categories, source artifacts, concrete `latency_hotspot` / `graph_drift_hotspot` hints with companion bucket ids, and a `summary_lookup_hint` plus `summary_lookup_scope` when there is something specific to inspect first. In incomplete-mutation cases, the lookup scope now narrows to `mutation_scope_bucket`, the source-artifact list expands to include the full quality report JSON, and the lookup hint points at a stable `mutation-scope-bucket: <fixture>__mutation_scope` label in the markdown summary so local triage can inspect mutation trust coverage directly instead of only the broader graph-drift summary surface. The snapshot also carries an `action_checklist` and `action_commands` list for non-clean runs, so local runs and scripts can move directly from classification to next step. When mutation-neighborhood drift exists, it also surfaces the current `leading_graph_drift` mode directly in the status output, a compact `graph_drift_trend` line showing whether that failure shape is worsening, improving, flat, or newly appearing versus the recent trailing average, fixture-aware drift lines so you can see which repo shape is moving most and, when applicable, which fixture is currently worsening fastest, and a separate `mutation_scope_incomplete_rate` so local automation can distinguish incomplete mutation neighborhoods from the older missing/extra graph-drift buckets. The JSON form also includes `latency_score`, `latency_score_delta_vs_trailing`, `latency_score_direction`, `latency_severity`, `latency_severity_reason`, `graph_drift_score`, `graph_drift_score_delta_vs_trailing`, `graph_drift_score_direction`, `graph_drift_severity`, and `graph_drift_severity_reason` fields so local automation can threshold, sort, and trend both performance and graph-drift pressure without parsing the human-readable trend text. The local quality exporter now does one unmeasured warmup pass per route/retrieval case before recording best-of-N latency, so the snapshot reflects warmed Semantic behavior instead of repeatedly overreacting to per-case cold-start cost. Local artifacts are written under `docs/doc_ignore/`:

The current `status --quality --output json` contract is:

- identity and state: `kind`, `snapshot_path`, `status`, `health`, `latency_health`, `graph_drift_health`, `diagnosis`
- actioning: `action_recommendation`, `action_priority`, `triage_path`, `action_target`
- executable triage: `action_checklist`, `action_commands`, `action_primary_command`
- command metadata: `action_command_categories`, `action_primary_command_category`
- artifact metadata: `action_source_artifacts`, `summary_lookup_hint`, `summary_lookup_scope`
- latency triage: `latency_hotspot`, `latency_hotspot_bucket_id`, `latency_severity`, `latency_severity_reason`, `latency_score`, `latency_score_delta_vs_trailing`, `latency_score_direction`
- graph-drift triage: `graph_drift_hotspot`, `graph_drift_hotspot_bucket_id`, `leading_graph_drift`, `leading_graph_drift_fixture`, `graph_drift_trend`, `graph_drift_fixture_trend`, `top_worsening_graph_drift_fixture`, `graph_drift_severity`, `graph_drift_severity_reason`, `graph_drift_score`, `graph_drift_score_delta_vs_trailing`, `graph_drift_score_direction`, `leading_graph_drift_delta_vs_trailing_pp`, `mutation_scope_incomplete_rate`
- counts: `regression_count`, `threshold_failure_count`, `fixture_count`
- aggregate metrics: `retrieval.avg_latency_ms`, `retrieval.p95_latency_ms`, `retrieval.avg_latency_delta_vs_trailing`, `retrieval.p95_latency_delta_vs_trailing`, `route.avg_latency_ms`, `route.p95_latency_ms`, `route.avg_latency_delta_vs_trailing`, `route.p95_latency_delta_vs_trailing`

- `quality_report.json`
- `quality_report_summary.md`
- `quality_report_history.json`
- `quality_report_trend_snapshot.json`

## Retrieval Confidence And Fallback

Semantic should not be described as `>99% accurate` today, either globally or for edit routing on arbitrary external repos.

What the project can claim honestly today:

- the current fixture-backed quality gate is green and stable for the local production-readiness target
- edit-capable routes are designed to fail closed rather than silently proceed on weak retrieval
- read-only routes surface explicit verification state instead of pretending every retrieval is equally trustworthy

Built-in fallback and alerting behavior:

- read-only routes surface verification states such as:
  - `high_confidence`
  - `needs_review`
  - `low_confidence`
  - `no_context_refs`
  - `fallback_search`
- edit-capable routes remain blocked until they are explicitly `mutation_safety: ready`
- blocked `implement` / `refactor` routes attempt a deterministic exact retry before failing
- CLI output surfaces:
  - `verification`
  - `verification_scope`
  - `verification_graph_issue`
  - `mutation_safety`
- CLI gates can force non-zero exit when trust is too low:
  - `--require-high-confidence`
  - `--min-verification ...`
  - `--require-mutation-ready`

Current confidence boundary:

- Semantic is designed to reduce retrieval lottery for code by using deterministic structure, symbol/span indexing, and runtime verification
- it is not yet validated enough to claim `>99%` retrieval accuracy across arbitrary external repos
- for large unfamiliar repos, treat `status`, verification output, and mutation gates as the authoritative trust signals

Serve the legacy transports through the CLI-first runtime:

```bash
cargo run -p semantic_cli -- --repo ./test_repo serve api
cargo run -p semantic_cli -- --repo ./test_repo serve mcp --token my-local-token
```

Legacy binaries are still available for compatibility:

```bash
cargo run -p api -- ./test_repo
cargo run -p mcp_bridge -- ./test_repo
```

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
- `get_directory_brief`
- `get_file_brief`
- `get_symbol_brief`
- `get_section_brief`

Phase-6 token-control primitives now partially implemented:

- session-scoped raw span back-references via `already_in_context: true`
- session-scoped raw expansion modes:
  - `normal`
  - `strict`
  - `investigate`
- visible raw-budget exhaustion via:
  - `raw_budget_exhausted: true`
- document-aware brief fallback for route flows without a resolved symbol

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
- compact hybrid ranked context payloads (`GetHybridRankedContext`) that keep ranked spans and graph-rank signals cheap while leaving full graph detail to the dedicated hint/cluster endpoints

## Phase 5 Maturity

Implemented maturity features now include:

- SQLite-backed planned-context cache with automatic invalidation on index changes
- policy-driven token caps and adaptive retrieval thresholds
- anti-bloat controls for small single-file tasks
- quality-gated A/B evaluation with validated patch/test signals
- p95/p99 latency alerts and cache hit-rate alerts in `GetPerformanceStats`
- full MCP compatibility through the two primary tools: `retrieve` and `ide_autoroute`

Current production-readiness closeout for this phase:

- the local quality gate is expected to stay green before new routing/retrieval changes are treated as acceptable
- route verification is live at runtime, not just in offline tests
- edit-capable routes are fail-closed until exact local checks make them mutation-ready
- mutation routes are verified against exact target, exact span, workspace boundary, and graph-scoped impact-neighborhood checks
- the quality status path exposes both latency health and graph-drift health so performance pressure and context-correctness pressure are separated
- the largest remaining payload buckets are now mostly real code/context rather than duplicate routing scaffolding

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
