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

## Phase 2: Logic-Node Layer (Current)

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

## Phase 3: Performance and Hardening

1. Benchmark indexing throughput (10k files under 20s target).
2. Benchmark lookup latency (<10ms symbol lookup target).
3. Add instrumentation for index/update/query timings.
4. Optimize transaction batching and index refresh strategy if needed.

## Phase 4: Reserved Future Work (Not Implemented)

1. Control-flow graph edges.
2. Data-flow edges.
3. Logic clustering.
4. Semantic node labels.
5. Hybrid graph ranking.

## Phase 5: Product Maturity Roadmap

1. Persistent retrieval cache:
   - Move planned-context cache from in-memory to SQLite-backed persistence.
   - Add file-change invalidation hooks so stale cache entries are dropped automatically.
2. Full CFG/Data-Flow graph extraction:
   - Extend parser/indexer/storage to persist actual control-flow and data-flow edges (not just hints).
   - Add retrieval operations for CFG slice and data-flow slice.
3. Adaptive retrieval policy:
   - Auto-select `single_file_fast_path` vs multi-hop retrieval based on dependency fanout and edit risk.
   - Add policy thresholds in `.semantic` config.
4. Quality-gated A/B evaluation:
   - Score tasks by executable patch success + tests passing, not only prompt-shape checks.
   - Add regression thresholds for token and step metrics.
5. IDE integration pack:
   - Provide ready-made templates for RooCode/KiloCode/Codex/Claude to call `ide_autoroute` first.
   - Include semantic-first fail-safe policy defaults.
6. Context compaction controls:
   - Add per-step token caps (plan/lookup/edit) and enforce structured refs before raw code expansion.
   - Add strict anti-bloat mode for small single-file edits.
7. Observability and SLOs:
   - Track p95/p99 latency and cache hit rate per operation over time.
   - Add alert thresholds for latency regressions and cache miss spikes.
