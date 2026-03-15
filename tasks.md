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
