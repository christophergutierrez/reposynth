# Cycle 8 Feedback

**For:** reposynth, planning cycle 9
**From:** trainLLM, 2026-04-21
**Eval:** `2026-04-20_1702_my-adapter` on `holdout_v2.jsonl` (20 records)
**Companion machine-readable file:** `2026-04-20_1702_my-adapter_synth_status.yaml`

## Headline

The cycle-8 inputs (filter 75 contaminated records; +1148 new; revert rank 32→16) produced **no change in average score** against the cycle-7 adapter on the same holdout.

|              | Cycle 7 adapter on v2 (one-off) | Cycle 8 adapter on v2 | Base on v2 |
|---           |---                              |---                    |---         |
| Avg          | 0.41                            | 0.41                  | 0.10       |
| E / G / P / Poor | 3 / 5 / 0 / 12              | 3 / 3 / 3 / 11        | 0 / 0 / 0 / 20 |

Fine-tuned delta vs base is +0.32 — the adapter is doing real work. But the headline has plateaued at 0.41 for two cycles. Reposynth should NOT read the synth_status as "boost the same weak conventions harder" — that was cycle-8's premise and it did not unlock movement.

## Per-convention deltas (cycle 7 adapter on v2 → cycle 8 adapter on v2)

Improved:

| Convention          | Δ     |
|---                  |---    |
| sql_template        | +0.12 |
| parameter_conversion| +0.11 |
| context_timeout     | +0.09 |
| batch_operation     | +0.08 |
| sqlx_in             | +0.08 |

Regressed:

| Convention        | Δ     |
|---                |---    |
| event_routing     | −0.14 |
| proto_conversion  | −0.12 |
| custom_error_type | −0.11 |
| aws_sdk           | −0.08 |
| context           | −0.07 |

Most regressions are on n=1 conventions — individual-record noise dominates. Treat these as low-confidence signals.

## Predictions that held

- `known_gaps.retry_bearer_contamination` in `cycle8_manifest.yaml`: *"the model may not overcome the ratio"*. Confirmed — `retry_logic` is still 0.17 after the 75-record filter.
- The `fixes_applied.contaminated_patterns_boosted` note flagged that dilution via boosted new records might not be sufficient. Confirmed — `http_client` only 0.36, `json_unmarshal` 0.25.

## Hypothesis for cycle 9: the bottleneck is not what we boost

Two plausible ceilings, in rough order of suspected payoff:

**(1) Legacy record dominance.** ~2500 training records are cumulative from cycles 1–6, produced before the convention fixes of cycles 7–8. The filter removed the 75 most obviously wrong patterns; subtler legacy patterns likely remain and pull the adapter in a direction that no amount of boosting corrects.

**Ask:** Can reposynth emit a **clean-only training file** — only records produced or regenerated in cycles 7–8 — as the cycle-9 training input? This is the single highest-leverage experiment we can run: same model config, same holdout, 5× less data but all of it aligned with current conventions. If scores climb, legacy was the bottleneck. If they don't, proceed to (2).

**(2) Metric ceiling.** `eval.py` uses `difflib.SequenceMatcher` — token-level sequence similarity. Semantically correct Go code that varies in identifier names, field order, or whitespace scores well below 1.0. Many current POOR cases may be correct-but-differently-worded code. If so, no data change will push the average much past 0.41 on this metric.

Addressing (2) requires changing the scoring function. That's a trainLLM task, not a reposynth task — flagged here for visibility, not as an ask.

## Artifacts included in this handoff

- `2026-04-20_1702_my-adapter_synth_status.yaml` — machine-readable; action `GENERATE_DATA`, 16 boost_conventions, 3 skip_conventions, 1 uncovered (`logging`).
- `cycle8_feedback.md` — this file.
