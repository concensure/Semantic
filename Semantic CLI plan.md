# Semantic CLI-First Refactor Plan

## Summary

- `semantic_cli` is now the primary entrypoint.
- Shared orchestration moved into the new `semantic_app` crate.
- Existing HTTP and MCP transports are preserved as thin compatibility adapters over the same runtime.

## Existing Features Reused

- Tree-sitter parsing in `parser`
- SQLite + Tantivy indexing in `storage`
- Retrieval and ranking in `retrieval`
- Incremental indexing in `watcher`
- Non-LLM project summarisation in `project_summariser`
- Token caps, anti-bloat rules, cache invalidation, and telemetry already in the repo

## Delta From The Previous Layout

- `api` no longer owns the main orchestration path.
- `mcp_bridge` no longer proxies through the API server.
- Shared runtime state now lives in `semantic_app::runtime`.
- Shared retrieve, route, edit, and adapter behavior now live in `semantic_app`.
- CLI output rendering is isolated in `semantic_app::render` and does not alter engine DTOs.

## Shared Modules Added

- `semantic_app::runtime`
  - bootstraps storage, indexer, retrieval, watcher ownership, config stubs, LLM router, and knowledge graph
- `semantic_app::session`
  - session reuse, ref dedupe, delta tracking, summary suppression state
- `semantic_app::retrieve`
  - transport-neutral retrieve execution
- `semantic_app::route`
  - transport-neutral autoroute execution
- `semantic_app::actions`
  - edit flow, legacy action dispatch, status/config helpers
- `semantic_app::api_server`
  - thin Axum adapter over the shared runtime
- `semantic_app::mcp_server`
  - thin MCP adapter over the shared runtime

## Compatibility Notes

- Primary MCP tools remain `retrieve` and `ide_autoroute`.
- Legacy MCP aliases still route through those two surfaces.
- `POST /retrieve`, `POST /ide_autoroute`, and `PATCH /edit` are preserved.
- `api` and `mcp_bridge` binaries now delegate to the shared runtime rather than carrying separate orchestration code.

## Commands

- `semantic-cli retrieve --op <operation> ...`
- `semantic-cli route --task "<task>"`
- `semantic-cli index [--watch] [--workspace]`
- `semantic-cli status`
- `semantic-cli edit ...`
- `semantic-cli serve api`
- `semantic-cli serve mcp`
- `semantic-cli config init`

## Validation Completed

- `cargo check -p semantic_app -p semantic_cli -p api -p mcp_bridge`
- `cargo test -p semantic_app`
- CLI smoke checks on `test_repo/todo_app` for:
  - `status`
  - `retrieve --op SearchSymbol`
  - `route --task "add due date to tasks"`

## Known Limits

- CLI-first architecture is implemented; it does not yet add new semantic-compression features beyond what already exists in the repo.
- The repo-wide `status` command can still take noticeable time on the full project because bootstrap indexing is local-first and deterministic.
