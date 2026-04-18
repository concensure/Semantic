# Rust Support Benchmark Bar

## Date

- 2026-04-18

## Why This Note Exists

The phrase "better than LSP quality" is ambiguous unless the benchmark axis is explicit.

For this project, Rust support can realistically outperform:

- grep on symbol grouping and related-context recovery
- raw LSP context dumping on token efficiency and LLM-ready packaging

It cannot honestly outperform a mature Rust language server on semantic correctness unless the
project starts depending on compiler-grade resolution.

## Benchmark Axes

### Axis 1: Semantic Exactness

Definition:

- exact type/trait/module/name resolution
- robust handling of aliases, generics, macro-heavy code, and compiler-informed paths

Current ceiling without compiler integration:

- below rust-analyzer

Reason:

- current Rust support is lexical and structural, not type-resolved
- no macro expansion
- no borrow/type inference
- no compiler-backed trait resolution

### Axis 2: Retrieval Quality For LLM Use

Definition:

- returns the right definition, impl blocks, and associated items
- avoids unrelated sibling symbols
- keeps context compact and grouped
- handles qualified symbol queries well

This is the axis the project can reasonably beat raw LSP output on.

Reason:

- language servers usually expose lower-level navigation primitives
- this project can package task-ready grouped context directly
- the retrieval surface is intentionally constrained to two Rust-aware operations

### Axis 3: Token Efficiency

Definition:

- fewer irrelevant lines
- bounded grouped spans instead of full-file dumps
- deterministic compact payloads

This is another axis the project can realistically beat raw LSP-style retrieval on.

## Current State

Implemented improvements now include:

- indexed Rust symbol retrieval rather than repo rescans
- crate-aware duplicate disambiguation
- persisted Rust import metadata
- persisted Rust module declaration metadata
- qualified query handling such as `api::Serialize`
- scope-aware module paths per symbol, including inline modules
- grouped retrieval for definitions, impl blocks, and associated items
- bounded span packing with retrieval caching

## What "Competitive" Should Mean

For this project, Rust support should be considered competitive when it meets all of the following:

- beats grep on missed impl blocks and irrelevant context
- beats raw LSP dump workflows on token cost for task-oriented retrieval
- remains inside the existing retrieval contract and two-tool surface
- stays deterministic enough for benchmark fixtures and regression tests

## What Still Blocks True Semantic Parity

- alias-aware import resolution beyond simple lexical cues
- cross-crate trait/type resolution in ambiguous cases
- macro-expanded symbol surfaces
- generic specialization awareness
- compiler-backed path canonicalization

## Practical Recommendation

Use the following benchmark claim going forward:

"Rust support is intended to outperform grep and raw LSP dumping for LLM-oriented retrieval quality and token efficiency, while remaining lighter than compiler-backed semantic tooling."

That claim is defensible with the current architecture. A stronger semantic-correctness claim is not.
