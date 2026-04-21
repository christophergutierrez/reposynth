The same adapter scores very differently on the two holdouts — your suspicion was right.

  ┌─────────────────────────────┬──────────────────────────────────┬────────────────────────────────┐
  │                             │ holdout_v2 (20 rec, old primary) │ current holdout.jsonl (10 rec) │
  ├─────────────────────────────┼──────────────────────────────────┼────────────────────────────────┤
  │ Avg                         │ 0.41                             │ 0.24                           │
  ├─────────────────────────────┼──────────────────────────────────┼────────────────────────────────┤
  │ Excellent/Good/Partial/Poor │ 3 / 5 / 0 / 12                   │ 0 / 1 / 0 / 9                  │
  └─────────────────────────────┴──────────────────────────────────┴────────────────────────────────┘

  Same adapter, same vLLM process — only the holdout changed. So the 22:39 adapter isn't as bad as 0.24 made it look;
  it's competitive with (actually slightly above) the 0.37 cycle-6 number mentioned in status_flow.md.

  On conventions they share, the current holdout is harder across the board:

  ┌───────────────────┬────────────┬─────────────────┐
  │    Convention     │ holdout_v2 │     current     │
  ├───────────────────┼────────────┼─────────────────┤
  │ pagination        │ 0.89       │ 0.14            │
  ├───────────────────┼────────────┼─────────────────┤
  │ event_routing     │ 0.68       │ 0.23            │
  ├───────────────────┼────────────┼─────────────────┤
  │ aws_sdk           │ 0.80       │ 0.32            │
  ├───────────────────┼────────────┼─────────────────┤
  │ custom_error_type │ 0.65       │ 0.28            │
  ├───────────────────┼────────────┼─────────────────┤
  │ valog             │ 0.53       │ 0.21            │
  ├───────────────────┼────────────┼─────────────────┤
  │ sqlx              │ 0.51       │ 0.19            │
  ├───────────────────┼────────────┼─────────────────┤
  │ error_wrapping    │ 0.41       │ 0.24            │
  ├───────────────────┼────────────┼─────────────────┤
  │ retry_logic       │ 0.16       │ 0.64 (reversed) │
  └───────────────────┴────────────┴─────────────────┘

  That's a consistent downshift, not noise. The current 10-record holdout is either harder examples of the same tags, or
   differently-tagged patterns — and mixing the two in a run-over-run comparison masks what training actually did.

  Takeaway: the earlier diagnosis (rank 16 → 32 hurt the adapter on the current holdout) still holds as a within-holdout
   fact, but the absolute scores of recent cycles aren't comparable to pre-holdout-swap cycles. Next cycle I'd either
  (a) pin a stable eval set to use across cycles, or (b) include both in every run and track them separately.

  Files:
  - One-off output moved to evals/one-off/2026-04-20_1002_my-adapter.{md,json} — won't pollute "previous run" lookups in
   the next cycle.py.
  - Main evals/ untouched.


