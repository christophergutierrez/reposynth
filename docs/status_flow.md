# Status Flow Design

## Overview

The fine-tuning pipeline spans two systems:

- **Eval system** — trains the model, scores holdout examples, produces per-example
  diffs. It has cycle history, base model comparisons, and all expected vs generated
  outputs. This system should produce the status document after each eval run.

- **reposynth** — generates synthetic training data. It knows what patterns exist,
  how many examples are in each, and what the health thresholds are. It should
  consume the status document to prioritize generation.

Neither system should duplicate the other's work. The eval system emits; reposynth
ingests.

---

## What the Eval System Should Produce

After each eval run, write a file (suggested name: `synth_status.yaml`) alongside
the eval report. This file is the handoff contract between the two systems.

The mechanically-derivable fields (scores, boost/skip/uncovered lists, format health)
are produced automatically at the end of each training cycle — no manual steps required.
The `failure_note` fields require a separate LLM analysis pass and are deliberately
omitted until that pass is implemented. reposynth must tolerate their absence.

The emitter is also runnable standalone:
```bash
python handoff/emit_synth_status.py                  # latest adapter eval
python handoff/emit_synth_status.py <eval_json_path> # explicit eval
```

### Schema

```yaml
# Written by the eval system after each scoring run.
# Read by reposynth to adjust generation priorities.

meta:
  generated_at: "2026-04-19T02:46:00Z"
  eval_set: "holdout_v2"       # stem of holdout file used
  cycle: 6                     # from data/manifest.yaml; null if manifest absent
  source_eval: "2026-04-19_0246_my-adapter.json"  # filename of the source eval JSON
  # action decision tree:
  #   boost_conventions non-empty → GENERATE_DATA
  #   avg < 0.6                  → TRAIN
  #   avg >= 0.8                 → DONE
  #   otherwise                  → EVAL
  action: GENERATE_DATA        # GENERATE_DATA | TRAIN | EVAL | DONE

scores:
  avg_similarity: 0.37
  previous_avg: 0.31           # second-newest adapter eval; null if first run
  base_model_avg: 0.08         # base model (no LoRA); null if not measured
  excellent: 3                 # count with avg >= 0.8
  good: 3                      # count with avg >= 0.6
  partial: 0                   # count with avg >= 0.4
  poor: 14                     # count with avg < 0.4
  total: 20

# Conventions where the model is failing — sorted by priority then ascending avg_score.
# priority rules (applied in order):
#   n >= 10 AND avg < 0.5  → critical  (well-covered but still failing = capacity problem)
#   avg < 0.3              → high
#   avg < 0.5              → medium
#   otherwise              → low
boost_conventions:
  - name: error_wrapping
    avg_score: 0.37
    n_holdout_examples: 20
    priority: critical
    # failure_note is optional — present only when the LLM analysis pass has run.
    # reposynth must tolerate its absence; boost/skip/uncovered lists are sufficient
    # to drive prioritized generation without it.
    failure_note: >
      Bracket label uses the callee function name ([sqlx.In], [NamedQueryContext])
      instead of the calling method ([TypeName.MethodName]). Affects 9/14 poor
      examples. Add examples where every fmt.Errorf call uses [Type.Method] notation.

  - name: http_client
    avg_score: 0.33
    n_holdout_examples: 4
    priority: high
    failure_note: >
      Authorization header missing underscore prefix. Correct: Bearer _%s.
      Generated: Bearer %s (no underscore). Also: one case generated NewHTTPClient
      constructor instead of the requested per-request helper function.

  - name: aws_sdk
    avg_score: 0.17
    n_holdout_examples: 1
    priority: high
    failure_note: >
      Uses aws.NewStaticCredentialsFromKey (SDK v2 API — does not exist in this
      codebase). Correct: credentials.NewStaticCredentials(accessKey, secretKey,
      token) from SDK v1. Also omits MaxRetries: aws.Int(5) from session config.

  - name: event_routing
    avg_score: 0.35
    n_holdout_examples: 1
    priority: high
    failure_note: >
      eventdealer.WorkflowError struct has only one field: Error error. Model adds
      FailedAtStep string (does not exist). Default switch case must return
      eventdealer.WorkflowError{} (silent skip) — model returns an error instead.

  - name: empty_result_handling
    avg_score: 0.52
    n_holdout_examples: 3
    priority: medium
    failure_note: >
      Repository methods must use ErrRead.Join(err) for both query failures and
      empty-result paths. Model uses fmt.Errorf(...) which loses the sentinel type.

  - name: retry_logic
    avg_score: 0.16
    n_holdout_examples: 1
    priority: medium
    failure_note: >
      For-loop retry must use plain time.Sleep(backoff) — not
      select { case <-time.After: case <-ctx.Done(): }. Context cancellation is
      checked at the top of the loop body, not inside the sleep.

  - name: pagination_defaults
    avg_score: 0.19
    n_holdout_examples: 2
    priority: medium
    failure_note: >
      Proto-to-params conversion uses page*pageSize offset calculation instead of
      accepting offset as a direct parameter. Also drops the valog.Logger argument
      from the function signature.

  - name: context_deadline
    avg_score: 0.16
    n_holdout_examples: 1
    priority: low
    # failure_note absent — LLM pass not yet run

  - name: grpc_status
    avg_score: 0.19
    n_holdout_examples: 2
    priority: low

  - name: proto_conversion
    avg_score: 0.25
    n_holdout_examples: 2
    priority: low

  - name: json_unmarshal
    avg_score: 0.26
    n_holdout_examples: 4
    priority: low

  - name: sqlx
    avg_score: 0.52
    n_holdout_examples: 9
    priority: low

  - name: valog
    avg_score: 0.46
    n_holdout_examples: 12
    priority: low

# Conventions at or above the GOOD threshold (0.6) — do not add more training data.
# reason: EXCELLENT if avg >= 0.8, else GOOD
skip_conventions:
  - name: sql_template
    avg_score: 0.95
    reason: EXCELLENT
  - name: pagination
    avg_score: 0.89
    reason: EXCELLENT
  - name: custom_error_type
    avg_score: 0.61
    reason: GOOD

# Convention tags present in data/manifest.yaml patterns but absent from all
# holdout records' conventions_tested fields. These cannot be scored until
# holdout examples are added. Empty if manifest is absent.
uncovered_conventions:
  - name: valog_debug_conditional
  - name: named_exec_context_insert
  - name: custom_error_not_found_sentinel

# Format health computed from eval output — not from reposynth's pre-generation check.
# func_start_pct: after stripping ```lang fences, % of non-error responses whose
#   first non-empty line begins with 'func '.
# boilerplate_detected: responses with package/import lines before the first func.
format_health:
  func_start_pct: 100.0
  boilerplate_detected: 1
  boilerplate_cases:
    - func: GetReachCurves
      score: 0.09
      length_ratio: 2.71
```

---

## What reposynth Needs to Ingest

### Config

Add a field to `synth.yaml` pointing to the status file:

```yaml
# Path to synth_status.yaml copied back from the training machine after each cycle.
# When present, reposynth generate uses boost/skip lists to adjust priorities.
# Relative to repo root. Recommended: keep a stable name (e.g. evals/synth_status.yaml)
# and overwrite it each cycle, rather than tracking the timestamped filename.
status_file: evals/synth_status.yaml
```

The training machine writes this file alongside its eval report as
`evals/<timestamp>_<adapter>_synth_status.yaml`. Copy the latest one back to
reposynth and place it at the path configured above. The training machine also
supports symlinking to the latest-by-mtime file if you prefer that workflow.

### Behavior when status file is present

`reposynth generate`:
1. Load `status_file` if configured and present.
2. For each pattern in the pattern catalog:
   - If the pattern's `tags` overlap with `skip_conventions` → skip (print why).
   - If the pattern's `tags` overlap with `boost_conventions` with priority `critical`
     or `high` → multiply the pattern's `n` by a boost factor (default: 1.5×).
3. If `failure_note` is present for a boosted convention, inject it into the booster
   prompt so the model knows *which specific mistake* to avoid. Tolerate absent
   `failure_note` gracefully — the boost still runs, just without the extra hint.
   (`failure_note` is not emitted until the LLM analysis pass is implemented.)
4. Print a pre-run summary:
   ```
   Status: cycle 6 — avg 0.37 (+0.06 from cycle 5)
   Boosting:  error_wrapping [critical], http_client [high], aws_sdk [high], ...
   Skipping:  sql_template, pagination, custom_error_type
   Uncovered: valog_debug_conditional, named_exec_context_insert, custom_error_not_found_sentinel
   ```

`reposynth check`:
- If `status_file` is present, append a "Last Eval" section to the health report
  showing the score snapshot and the top 3 failure modes. This surfaces the eval
  state without requiring the user to find the eval report separately.

### Behavior when status file is absent

No change to current behavior. `reposynth generate` runs all patterns at their
configured `n` values. A warning is printed if `status_file` is configured but
the file does not exist.

---

## The Root-Cause Analysis Gap

The `failure_note` fields require semantic understanding of why the model failed —
not just that it scored poorly. That analysis cannot be produced mechanically from
scores alone; it requires reading the expected vs generated diffs and categorizing
the failure mode.

The eval pipeline (trainLLM) should implement an LLM analysis pass after scoring:
for each poor/partial example, prompt the LLM with the expected and generated outputs
and ask it to identify the primary failure mode and write a one-sentence explanation.
Aggregate by convention to produce the `failure_note` values.

This same LLM pass produces `train_params_suggestions.yaml` (see below).

**This is trainLLM's work to implement, not reposynth's.** reposynth consumes
`failure_note` values if present and ignores the field if absent — no reposynth
changes are needed when the LLM pass is added.

Without the LLM pass, the boost/skip/uncovered lists and format health are still
sufficient to drive prioritized generation.

---

## Companion Output: train_params_suggestions.yaml

The same LLM analysis pass that produces `failure_note` fields should also write
`train_params_suggestions.yaml` for consumption by the training pipeline. These
are separate files with separate consumers — do not merge them.

`synth_status.yaml` goes to reposynth. `train_params_suggestions.yaml` stays on
the training machine. reposynth does not read or need this file.

### Schema

```yaml
# Written by the eval pipeline's LLM analysis step after each scoring run.
# Read by the training system before launching the next training run.
# The training system reconciles these with canary findings before applying.

meta:
  generated_at: "2026-04-19T03:15:00Z"
  source_cycle: 6                        # logical cycle number, not a path
  source_eval_id: "2026-04-19_0246"      # matches the eval report ID
  priority: advisory                     # advisory | strong | urgent

# Reviewed signals summarized here so the training system has context
# without re-reading the full eval report.

suggestions:
  - param: lora_rank
    current: 16
    proposed: 32
    rationale: >
      Conventions with sufficient training volume (error_wrapping n=20 at 0.37,
      valog n=12 at 0.46, sqlx n=9 at 0.52) still miss specific tokens while
      getting structural shape right. That pattern reads as capacity-limited,
      not data-limited. Doubling rank is the cheapest test of that hypothesis.

  - param: lora_alpha
    current: 32
    proposed: 64
    rationale: >
      Maintain the existing 2:1 alpha:rank ratio when bumping rank to 32.
      Changing the ratio alongside rank would confound the diagnostic.

# Explicit holds — parameters that were considered and deliberately left unchanged.
# The training system should not change these without a new suggestion.
holds:
  - param: learning_rate
    current: 2.0e-4
    reason: >
      No plateau or divergence signal that would justify a change. Data gaps
      dominate the remaining error; adjusting LR now would muddy the signal
      from the next data-enriched run.

  - param: max_steps
    current: 2000
    reason: >
      Adding steps on thin data invites overfitting on sparse conventions.
      Revisit only after synth_status.yaml boosts land.

  - param: lora_dropout
    current: 0
    reason: >
      Consider 0.05 only if excellent/good count stops growing after rank bump
      and the dataset has been enriched. Premature regularization on thin data
      will hurt more than help.

notes:
  - >
    Priority is data, not parameters. The rank bump hedges against a capacity
    ceiling on conventions that already have adequate coverage; it is not a
    substitute for the data boosts in synth_status.yaml.
  - >
    If the canary on rank=32 shows a drop vs. rank=16 at equal data, revert
    rank and report back so the next analysis pass can retract the suggestion.
  - >
    Re-evaluate the full parameter set after excellent/good count rises above
    ~50% of holdout (currently 6/20 = 30%). Below that threshold the scoring
    signal is too noisy for confident parameter tuning.
```

### Key design decisions

- **`holds` section is mandatory** — explicitly recording what was considered and
  declined prevents the training system from relitigating the same parameters each
  cycle. Without it, every run risks undoing deliberate choices.

- **`source_eval_id` not a path** — use a logical identifier that matches the eval
  report name, not a filesystem path. Keeps the file portable across machines.

- **Advisory by default** — the training system should reconcile suggestions with
  canary run results before applying. The LLM analysis pass sees scores and diffs
  but not training dynamics.

- **Separate files, separate consumers** — `synth_status.yaml` goes to reposynth;
  `train_params_suggestions.yaml` goes to the training pipeline. Do not merge them
  into one file even though they are produced in the same pass.

---

## Human-Readable Status Report (optional output)

The eval system can also write a `STATUS.md` for human consumption, derived from
the same data as `synth_status.yaml`. This replaces the manually-written
`context_from_training.md` files. Format:

```
# Fine-Tuning Status

Generated: <timestamp>
Eval set: <eval_set>
Cycle: <n>
Action required: <action>

## Score Snapshot
[table: this cycle / previous / base model]

## Root Cause Analysis
[one section per failure mode, with correct vs wrong examples and training data needed]

## Uncovered Conventions
[table of conventions with training data but no holdout examples]

## Do Not Touch
[table of conventions scoring well]

## Recommended Next Steps
[ordered list by priority]
```

`context_from_training.md` in the reposynth root can be retired once the eval
system produces this automatically.
