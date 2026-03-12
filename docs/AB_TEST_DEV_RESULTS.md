# A/B Dev Suite Results

This file tracks notable `POST /ab_test_dev` outcomes after major retrieval/planner changes.

## Latest Snapshot (2026-03-13)

Configuration:

- `provider=openai`
- `autoroute_first=true`
- `single_file_fast_path=true`
- suite: `dev_suite_v2:todo_app_10_tasks` (11 tasks)

Results:

| Run | tokens_without | tokens_with | savings_pct | task_success_without | task_success_with |
|---|---:|---:|---:|---:|---:|
| Baseline (before hardening) | 9738 | 11551 | -18.62% | 11/11 | 11/11 |
| Post-hardening run A | 9365 | 8609 | +8.07% | 11/11 | 11/11 |
| Post-hardening run B (latest) | 9749 | 9193 | +5.70% | 11/11 | 11/11 |

Latest gating signals (run B):

- `target_match_pct=100`
- `empty_ref_pct=0`
- `semantic_prompt_over_control_pct=18.18`
- `escalation_attempt_pct=0`

## Improvements Applied

- normalized symbol matching + preferred target hint in planner
- target-aligned minimal raw seed in autoroute
- per-task ref handling in A/B semantic arm
- replacement-style escalation token accounting
- richer task-level diagnostics and suite-level gating metrics

## How to Reproduce

1. Start API on the benchmark repo.
2. Call:

```bash
curl -X POST http://127.0.0.1:4317/ab_test_dev \
  -H "content-type: application/json" \
  -d '{"provider":"openai","autoroute_first":true,"single_file_fast_path":true}'
```

3. Inspect:

- `<PROJECT_ROOT>/.semantic/ab_test_results.csv`
- `<PROJECT_ROOT>/.semantic/ab_test_dev_task_metrics.jsonl`
