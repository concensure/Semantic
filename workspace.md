# Workspace Intelligence

## Graph Layers

```text
Workspace
  -> Repositories
    -> Modules
      -> Files
        -> Symbols
          -> Logic Nodes
```

## Repository Graph

```text
repoA --> repoB
repoA --> repoC
```

## Module Graph (Per Repo)

```text
api --> utils
api --> auth
```

## Symbol Graph

```text
fetchData -> retryRequest -> backoff
```

## Logic Graph

```text
Conditional -> Await -> Return
```

## Phase-5 Additions

- repository registry in SQLite (`repositories`, `repo_dependencies`)
- repo-aware symbols (`repo_id`)
- semantic symbol fallback search
- workspace reasoning retrieval operation
- AST cache module
- invalidation engine module

## Phase-6 Workspace Safe Editing

Safe editing is workspace-aware and can traverse:

- target repository
- dependent repositories
- cross-repo symbol callers/callees

### Cross-Repo Edit Impact

```text
repoA: api.fetchData
  -> repoB: utils.retryRequest
  -> repoB: auth.refreshToken
```

If `retryRequest` changes, impact analysis includes both local and dependent-repo symbols/files.

### Safe Edit Pipeline (Workspace Scope)

```text
Edit Request
  -> Impact Analysis (workspace graph + dependency graph)
  -> Safe Edit Plan
  -> Module/Repo scoped context retrieval
  -> LLM route selection
  -> Patch preview
  -> Policy + validation gates
  -> Apply / preview-only
```

### Budget Priority Across Repositories

1. target repository
2. dependency repositories
3. external repositories

This preserves token budget for highest-value context first.
