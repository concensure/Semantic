# A/B Dev Suite Results

This file tracks notable `POST /ab_test_dev` outcomes after major retrieval/planner changes.

---

## Latest Run — 2026-03-27

### Configuration

| Parameter | Value |
|---|---|
| `provider` | openai |
| `model` | gpt-4o-mini |
| `autoroute_first` | true |
| `single_file_fast_path` | false |
| `scenario` | core (11 tasks) |
| Suite | `dev_suite_v2:todo_app_11_tasks` |

### Results

| Metric | Without Semantic | With Semantic |
|---|---:|---:|
| Total tokens | 9,723 | 10,377 |
| Token savings | — | **-6.73%** (slightly more) |
| Estimated steps | 72 | 52 |
| **Step savings** | — | **+27.78%** |
| Task success (hits ≥ 2) | 11/11 (100%) | 11/11 (100%) |
| Validated success | — | 11/11 (100%) |
| Validation passed | — | 11/11 (100%) |

### Retrieval Quality

| Signal | Value |
|---|---|
| `avg_retrieval_ms` | 13 ms |
| `misdirection_risk_pct` | 0% (no tasks with empty refs on code-change tasks) |
| `target_match_pct` | 81.8% (9/11 target symbols correctly identified) |
| `empty_ref_pct` | 0% |
| `semantic_execution_success_pct` | 100% |
| `escalation_attempt_pct` | 0% |
| `heavy_first_pct` | 0% |
| `runtime_trim_applied` | 0 |

### Quality Gates

- `regression_alert`: **false** ✓
- `validated_patch_success_pct`: 100% ✓
- `semantic_prompt_over_control_pct`: 81.8% (semantic prompts are larger but produce fewer steps)

### Interpretation

The primary metric is **step savings, not token savings**. Semantic costs ~6.7% more tokens per
query but reduces estimated developer steps by 27.8% — from 72 steps to 52. The ROI is in
fewer back-and-forth iterations, not fewer tokens per single call.

`avg_context_coverage_pct` reports 0% because `autoroute_first` mode returns structured
context references (file, line ranges, module) rather than raw code. Coverage scoring requires
raw code text and is only meaningful with `autoroute_first=false` or `include_raw_code=true`.

---

## Historical Results

| Date | tokens_without | tokens_with | savings_pct | step_savings_pct | notes |
|---|---:|---:|---:|---:|---|
| 2026-03-13 (baseline) | 9,738 | 11,551 | -18.62% | — | before hardening |
| 2026-03-13 (run A) | 9,365 | 8,609 | +8.07% | — | post-hardening |
| 2026-03-13 (run B) | 9,749 | 9,193 | +5.70% | — | post-hardening |
| **2026-03-27** | **9,723** | **10,377** | **-6.73%** | **+27.78%** | after test enhancements; equalized thresholds; step savings added as primary metric |

---

## Test Scenario Changes (2026-03-27)

The following design flaws were corrected and new metrics added in this run:

### Flaws Fixed

1. **Asymmetric success thresholds** — Control arm required `hits >= 2`, semantic arm only
   required `hits >= 1` (plus `validation_passed`, which always passed). Both arms now use
   `hits >= 2` for a fair apples-to-apples comparison. A separate `validated_success_with`
   field tracks the bonus criterion (plan + 1+ hit).

2. **Inconsistent total_success counters** — The per-task success flags used different thresholds
   than the aggregate counters (`> 0` vs `>= 2`). Both now use `>= 2` throughout.

3. **Local `estimate_tokens` inconsistency** — The A/B test had a private `estimate_tokens`
   using `chars / 4.0` while the budgeter was fixed to `chars / 3.0`. Now consistent.

### New Metrics Added (zero extra token cost)

| Metric | Description |
|---|---|
| `retrieval_quality.avg_retrieval_ms` | Mean latency of semantic context fetch per task |
| `retrieval_quality.misdirection_risk_pct` | % of code-change tasks where semantic returned zero context refs (highest wrong-file risk) |
| `retrieval_quality.avg_context_coverage_pct` | % of expected terms found in fetched context text before LLM call (retrieval quality, LLM-independent) |
| `validated_success_with_pct` | Tasks where plan_safe_edit ran AND 1+ hit (structural bonus) |
| Per-task `retrieval_ms` | Individual retrieval latency |
| Per-task `misdirection_risk` | Boolean flag for tasks at risk of wrong context |
| Per-task `context_coverage` / `context_coverage_pct` | Coverage score for the fetched context |

---

## How to Reproduce

```bash
# 1. Start the API against the benchmark repo
cargo run -p api -- ./test_repo

# 2. Run the A/B test (core suite, 11 tasks)
curl -X POST <SEMANTIC_API_BASE_URL>/ab_test_dev \
  -H "Content-Type: application/json" \
  -d '{"autoroute_first": true, "single_file_fast_path": false, "scenario": "core"}'

# 3. Run the extended suite (adds 4 cross-file and large-footprint tasks)
curl -X POST <SEMANTIC_API_BASE_URL>/ab_test_dev \
  -H "Content-Type: application/json" \
  -d '{"autoroute_first": true, "single_file_fast_path": false, "scenario": "extended"}'
```

Results are written to:
- `.semantic/ab_test_results.csv` — aggregate per-run summary
- `.semantic/ab_test_dev_task_metrics.jsonl` — per-task detail (prompts, tokens, routing, scores)

Environment: set `OPENAI_API_KEY` (or configure provider in `.semantic/llm_config.toml`)
before running. Do not commit the `.env` file or any file containing real API keys.
