# Development Tasks (Dependency Order)

## Phase 0: Foundation

1. Define shared contracts in `engine`.
2. Keep operation enum backward compatible.
3. Add extensible DTOs for future semantic layers.

## Phase 1: Symbol/Dependency Baseline

1. Build schema for `files`, `symbols`, `dependencies`, `rules`, `skills`.
2. Implement parser symbol/dependency extraction.
3. Implement storage/query APIs for baseline retrieval operations.
4. Implement indexer and watcher for incremental updates.
5. Implement API endpoint and deterministic retrieval contract.

## Phase 2: Logic-Node Layer (Implemented)

1. Add `LogicNodeType`, `LogicNodeRecord`, `LogicEdgeRecord`.
2. Extend operation contract with:
   - `get_logic_nodes`
   - `get_logic_neighborhood`
   - `get_logic_span`
3. Extend SQLite schema with `logic_nodes` and `logic_edges` + indexes.
4. Extend parser to emit logic nodes inside functions/methods.
5. Implement atomic file replacement path that persists:
   - symbols
   - dependencies
   - logic nodes
   - logic edges
6. Build sequential logic edges per symbol from source order.
7. Implement storage read APIs for logic nodes and BFS neighborhoods.
8. Implement retrieval handlers for new logic operations.
9. Extend fixture repo with async control-flow example.
10. Add tests for extraction, persistence, and retrieval determinism.

## Phase 3: Performance and Hardening (Implemented)

1. Add query/runtime instrumentation with per-operation avg/p95/max timings.
2. Persist indexing/update timings in `.semantic/index_performance.json`.
3. Expose combined retrieval + indexing stats via `GetPerformanceStats`.
4. Optimize full-repo indexing by batching file replacements and deferring index refresh/module rebuild until the repo pass completes.
5. Track benchmark targets in the surfaced performance stats:
   - indexing throughput (`10k files under 20s` target)
   - symbol lookup latency (`<10ms` target)
   - planned-context p95 (`<60ms` target)

## Phase 4: Graph Semantics Layer (Implemented)

1. Persist control-flow graph edges per symbol.
2. Persist data-flow edges per symbol with variable names.
3. Add semantic node labels to logic nodes.
4. Persist clustered logic regions per symbol.
5. Add retrieval operations for:
   - `get_control_flow_slice`
   - `get_data_flow_slice`
   - `get_logic_clusters`
6. Upgrade hybrid ranking to use persisted graph signals, not only symbol/dependency heuristics.

## Phase 5: Product Maturity Roadmap (Implemented)

Current closeout status:
- complete for the current production-readiness target
- local quality gate is green and stable
- mutation-ready routing is hard-gated and exact-verified on the current fixture suite
- remaining token hotspots are mostly real code/context, not duplicated envelope metadata

1. Persistent retrieval cache:
   - Move planned-context cache from JSON/in-memory state to SQLite-backed `retrieval_cache`.
   - Add file-change invalidation hooks so stale cache entries are dropped automatically.
2. Higher-fidelity CFG/Data-Flow extraction:
   - Keep persisted graph slices as the primary source for control-flow and data-flow retrieval.
   - Surface richer graph-backed slices and clustering from the primary retrieval API.
3. Adaptive retrieval policy:
   - Auto-select `single_file_fast_path` vs multi-hop retrieval based on dependency fanout and edit risk.
   - Add policy thresholds in `.semantic/retrieval_policy.toml` (template committed as example).
4. Quality-gated A/B evaluation:
   - Score tasks using validated patch readiness and available test execution signals, not only prompt-shape checks.
   - Add regression thresholds for token and step metrics in surfaced quality gates.
5. IDE integration pack:
   - Route legacy MCP tools through the two primary tools: `retrieve` and `ide_autoroute`.
   - Keep semantic-first fail-safe defaults in the documented IDE flow.
6. Context compaction controls:
   - Add per-step token caps (plan/lookup/edit) and enforce structured refs before raw code expansion.
   - Add anti-bloat controls for small single-file tasks.
7. Observability and SLOs:
   - Track p95/p99 latency and cache hit rate per operation over time.
   - Add alert thresholds for latency regressions and cache miss spikes in `GetPerformanceStats`.

## Phase 6: Post-Phase-5 Design Tasks (In Progress)

Current implementation status:
- implemented primitives:
  - `GetDirectoryBrief`
  - `GetFileBrief`
  - `GetSymbolBrief`
  - `GetSectionBrief`
  - session-scoped raw span back-references (`already_in_context`)
  - session-scoped raw expansion budget with:
    - `normal`
    - `strict`
    - `investigate`
    - visible `raw_budget_exhausted`
  - document-aware summary fallback for route flows without a resolved symbol
  - default first-pass indexing exclusions plus surfaced:
    - `indexing_mode`
    - `indexing_completeness`
  - bootstrap reuse of an existing local index by default, with explicit refresh reserved for `semantic index`
  - shallow unindexed onboarding for:
    - `status`
    - `GetDirectoryBrief`
    - `GetFileBrief`
  - targeted first-use indexing via:
    - `semantic index --path <dir-or-file>`
  - surfaced indexed coverage hints via:
    - `indexed_path_hints`
  - explicit partial-index coverage signaling in retrieve/route outputs via:
    - `index_readiness`
    - `index_recovery_mode`
    - `index_recovery_target_kind`
    - `index_coverage`
    - `index_coverage_target`
    - route issue: `target_path_not_indexed`
    - `suggested_index_command`
  - opt-in targeted auto-recovery for uncovered regions via:
    - `--auto-index-target`
    - retry once after targeted indexing
    - only reports `auto_index_applied` when coverage actually improves
    - prefers exact file targets over directory targets when the request names a file path
- still pending for full closeout:
  - make progressive disclosure the default policy across all flows
  - add richer file/symbol/doc brief routing in more paths
  - add true staged/lazy large-repo onboarding instead of only default excludes
  - complete external monorepo validation
  - true one-line install and release packaging

1. Progressive disclosure retrieval model:
   - Keep retrieval escalation ordered as:
     - brief
     - outline
     - refs
     - raw span
   - Use this as the default token-control model for code and docs.
   - Goal: front-load navigation and intent disambiguation before raw expansion.
   - Status:
     - brief primitives exist
     - shallow unindexed brief fallback exists for file/directory navigation
     - not yet the universal default policy

2. Per-directory synthetic summary nodes:
   - Add a `DirectoryBrief` / `GetDirectoryBrief` operation as the navigation layer above files.
   - Return a compact structure with:
     - `dir`
     - `files`
     - `top_symbols`
     - `objective`
   - Keep it code-free by default.
   - Route unfamiliar `understand` flows through this brief before escalating to file outlines or raw spans.
   - Fit it into `project_summariser` as a tier below current micro/nano summaries.
   - Status:
     - `GetDirectoryBrief` implemented
     - route fallback wiring partially implemented

3. File summary enhancement:
   - Add a `FileBrief` shape for low-token navigation before `GetFileOutline` or raw span expansion.
   - Include:
     - purpose
     - top symbols
     - key imports/dependencies
     - no code
   - Use it for code and markdown-like docs where full file expansion would be wasteful.
   - Status:
     - `GetFileBrief` implemented
     - shallow filesystem fallback implemented for unindexed repos
     - broader policy rollout still pending

4. Symbol summary enhancement:
   - Add a `SymbolBrief` shape as a one-line role summary before raw span expansion.
   - Use it to answer:
     - what this symbol is for
     - what it touches
     - whether it is likely the right edit target
   - Status:
     - `GetSymbolBrief` implemented
     - not yet the default pre-expansion symbol layer everywhere

5. Session-scoped span registry:
   - Track spans/sections already expanded as raw content in the current session.
   - Suppress repeated raw resend and replace it with a structured back-reference:
     - `file`
     - `start`
     - `end`
     - `already_in_context: true`
   - Apply to both code spans and document sections.
   - Goal: reduce token churn from repeated escalation and repeated raw expansion.
   - Status:
     - implemented in session state for raw span responses

6. Session-scoped raw expansion budget:
   - Promote raw expansion budgeting from per-call policy to session-level policy.
   - Add a visible exhaustion signal:
     - `raw_budget_exhausted: true`
   - Include explicit override modes:
     - normal
     - strict
     - investigate
   - Goal: control token growth across long coding sessions without breaking legitimate deep-debug flows.
   - Status:
     - implemented at session scope for retrieve/route paths
     - more policy tuning still pending

7. Document detection before document retrieval:
   - Detect whether the current repo, file, or request is primarily:
     - code
     - mixed code + docs
     - document-first
   - Only route into the document retrieval layer when the target is actually document-oriented.
   - Goal: avoid sending normal code flows through a document-first path unnecessarily.
   - Status:
     - file/request-level detection helpers implemented
     - repo-level detection policy still pending

8. Document retrieval layer for operational docs:
   - Treat markdown/text docs as structured documents, not pseudo-code.
   - Add a `SectionBrief` shape with:
     - heading
     - objective
     - key references/rules
   - Index:
     - headings
     - section anchors
     - section summaries
     - local metadata
   - Support regular-reference docs such as:
     - `skills.md`
     - `rules.md`
     - policies
     - architecture notes
   - Reuse session back-references for sections already opened.
   - Goal: make Semantic usable as a transient reference database for repeated LLM document access, not just code.
   - Status:
     - `GetSectionBrief` implemented
     - full doc-layer indexing/routing still pending

9. Lazy indexing and staged onboarding for large repos:
   - Add explicit staged indexing modes for large repos and monorepos.
   - Candidate modes:
     - shallow bootstrap: directory tree + filenames + coarse summaries
     - targeted indexing: only requested directories/workspaces first
     - background full indexing: continue asynchronously after first-use readiness
   - Preserve current deterministic guarantees once a file/symbol is promoted into the indexed set.
   - Goal: make first-use on very large repos feel immediate without sacrificing exact retrieval on indexed regions.
   - Status:
     - partially implemented
     - current mitigations are:
       - default first-pass exclusion of heavyweight paths
       - default bootstrap reuse of an existing local index instead of reindexing on every CLI invocation
       - shallow unindexed `status` plus filesystem-backed directory/file briefs
       - explicit targeted indexing of requested files/directories without full-repo refresh
       - surfaced partial-coverage hints for the indexed regions currently ready
       - explicit route/retrieve signaling when a request targets an unindexed region
       - exact follow-up indexing guidance for uncovered regions via `semantic index --path ...`
       - explicit opt-in auto-index-and-retry for uncovered regions

10. Large-repo onboarding policy:
   - Add repo-size-aware defaults for initial setup on repos up to roughly 30GB.
   - Exclude obvious heavyweight directories from first-pass indexing unless explicitly requested:
     - `node_modules`
     - `target`
     - build artifacts
     - caches
   - Use inclusion rules that favor:
     - source code
     - key configuration files
     - primary documentation
     - relevant tests
   - Surface the current indexing mode and completeness level clearly in CLI status.
   - Goal: seamless first-run behavior on external repos without misleading the user about readiness/completeness.
   - Status:
     - default exclusion policy implemented
     - bootstrap reuse policy implemented
     - `indexing_mode` / `indexing_completeness` surfaced in status
     - staged onboarding still pending

11. Monorepo and external-repo validation:
   - Add explicit validation tasks for large external repos, including monorepos with mixed languages and generated artifacts.
   - Track:
     - first-use time-to-first-answer
     - time-to-accurate-symbol-resolution
     - token cost under staged indexing
     - fallback frequency
   - Use this to decide whether staged indexing and document retrieval are genuinely production-ready.
   - Status:
     - still pending

12. True one-line install and release packaging:
   - Publish release binaries for Windows/macOS/Linux.
   - Add a remote install path such as `irm <url> | iex` and `curl -fsSL <url> | sh`.
   - Decide whether to support:
     - `npm` / `npx` via a thin package that downloads the correct binary
     - `uv` via a Python wrapper package or standalone install script
   - Keep `cargo install --path semantic_cli --locked --force --bin semantic` as the source-based fallback.
   - Acceptance target: users can install and run `semantic` on another machine without cloning the repo manually.
   - Status:
     - intentionally deferred until the rest of Phase 6 primitives are validated
