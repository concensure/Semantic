# Rust Support Current State

## Date

- 2026-04-18

## Scope

This note records the implemented state of optional Rust support in the Semantic project after the
first compiler-backed integration pass.

## Public Surface

Rust support still preserves the current narrow retrieval contract:

- `search_rust_symbol`
- `get_rust_context`

No extra top-level Rust-specific MCP tools were added.

## Build and Runtime Model

- Rust support remains optional at build time behind the existing `rust-support` feature wiring.
- Rust support remains optional at runtime through `.semantic/rust.toml`.
- When disabled or not compiled in, the rest of the project continues to operate without Rust
  retrieval.

## Current Retrieval Architecture

### Indexed Semantic Layer

The project now stores Rust-specific indexed metadata including:

- symbol metadata
- crate name and crate root hints
- lexical import metadata
- module declaration metadata
- scope-aware per-symbol module paths

This supports:

- grouped definitions
- impl block recovery
- associated item recovery
- crate-aware duplicate disambiguation
- qualified-path ranking

### Compiler-Backed Anchor Layer

An optional internal integration now uses a locally installed `rust-analyzer` binary through a
minimal stdio LSP client.

Current internal use:

- `textDocument/didOpen`
- `textDocument/documentSymbol`
- `workspace/symbol` probing remains present but is not the primary effective path for current
  validation

Current effect:

- `search_rust_symbol` can return `rust_analyzer_document_symbol` strategy for fresh queries
- `get_rust_context` can return `rust_analyzer_anchored_context` strategy for fresh queries
- Semantic still performs the final grouped context packaging and token-bounded span selection

## Verified State On A Local Cargo Workspace

The integration was validated against a real local Cargo workspace during development.

Validated outcomes:

- Rust indexing succeeds with optional Rust support enabled
- `search_rust_symbol` resolves Rust symbols from an indexed Rust corpus
- `get_rust_context` returns grouped Rust definitions and related spans
- compiler-backed anchoring was observed on fresh queries using the existing two-operation surface

## Known Remaining Limitation

- Rust search deduplication required an extra pass because nested `documentSymbol` records can map
  back onto the same indexed symbol
- a final cleanup pass was added, but this remains a place to watch in future regression tests

## What This Still Does Not Claim

The current state does not claim full `rust-analyzer` parity.

Still missing relative to a full compiler-backed IDE stack:

- broad reference search integration
- rename integration
- full type/trait resolution surfaced through Semantic
- macro-expanded semantic retrieval
- richer cross-crate semantic operations beyond current anchor usage

## Current Practical Position

The optional Rust support is now stronger than the earlier structural-only pass because it can use
compiler-backed symbol anchors internally while preserving the project’s existing narrow retrieval
surface and compact context packaging model.
