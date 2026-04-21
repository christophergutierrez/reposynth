# Status Flow — trainLLM side of the handoff

This document describes the handoff contract from the perspective of **this
repository** (`trainLLM`): what comes in, what goes out, what files are
involved, and how they are produced. The companion system, `reposynth`, is
referred to here only as an opaque peer: this repo does not care how synthetic
training data is produced, only that it arrives in a usable format.

---

## 1. The two-system flow

```
┌──────────────┐   training.jsonl + holdout.jsonl    ┌──────────────┐
│  reposynth   │ ──────────────────────────────────▶ │   trainLLM   │
│              │                                     │   (here)     │
│              │ ◀────────────────────────────────── │              │
└──────────────┘       synth_status.yaml             └──────────────┘
```

- **reposynth** produces training and holdout data. This repo does not
  participate in that process. All we need is for the files to land at the
  paths configured in `config.yaml` and to match the formats below.
- **trainLLM** (this repo) trains the LoRA adapter, evaluates it on the
  holdout, and emits `synth_status.yaml` to tell reposynth which conventions
  to prioritize next cycle.

Neither side duplicates the other's work. We emit; reposynth ingests.

---

## 2. Inbound: what this repo expects

### 2.1 Training data — `data/training.jsonl`

ShareGPT JSONL, one record per line:

```json
{"conversations": [
  {"from": "human", "value": "..."},
  {"from": "gpt",   "value": "..."}
]}
```

Enforced by `cycle.py::_validate_training_data()` before GPU time is spent.
Validation checks the first three records and fails fast with a descriptive
error. Additional fields per record are ignored.

### 2.2 Holdout — `data/holdout.jsonl`

OpenAI messages JSONL, one record per line:

```json
{
  "id": "example-001",
  "label": "optional human-readable name",
  "messages": [
    {"role": "user",      "content": "..."},
    {"role": "assistant", "content": "..."}
  ],
  "conventions_tested": ["tag-a", "tag-b"],
  "source_file": "optional path"
}
```

`eval.py` strips the assistant turn and compares the generated output against
it. Only `messages` is required. `conventions_tested` is what drives the
per-convention breakdown in the eval report and the boost/skip lists in
`synth_status.yaml`; without it, the file only carries the overall score.

### 2.3 Manifest — `data/manifest.yaml`

Optional. Used only as a reference to enumerate the full set of convention
tags that exist in the training data. The emitter reads two fields:

- `meta.cycle` — surfaced as `meta.cycle` in the output
- `patterns[].tags` — the union of these across all patterns is the domain of
  known conventions; tags present in training but absent from the holdout's
  `conventions_tested` are reported as `uncovered_conventions`

Everything else in `manifest.yaml` is ignored by this repo. If the file is
absent, `cycle` is emitted as `null` and `uncovered_conventions` is empty.

---

## 3. Outbound: `synth_status.yaml`

### 3.1 Where it lives

Written alongside the eval report:

```
evals/<timestamp>_<adapter_name>.json
evals/<timestamp>_<adapter_name>.md
evals/<timestamp>_<adapter_name>_synth_status.yaml   ← this file
```

Timestamped so every cycle's handoff is archived. reposynth should be pointed
at the latest by mtime, or follow a symlink if one is set up.

### 3.2 When it is produced

As **Step 5b** of `cycle.py`, immediately after the fine-tuned eval returns
and before the summary report. The step is a no-op if the fine-tuned eval
produced no JSON (skipped, errored, or `--skip-eval`). Failures during emit
are logged as warnings but never abort the cycle — the `.md`/`.json` eval
artifacts are always the primary output.

Also runnable standalone:

```bash
python handoff/emit_synth_status.py                      # latest adapter eval
python handoff/emit_synth_status.py <eval_json_path>     # explicit eval
```

### 3.3 Schema

```yaml
meta:
  generated_at: "2026-04-20T14:51:01Z"     # UTC, ISO-8601
  eval_set: holdout                         # Path(holdout_file).stem
  cycle: 7                                  # from data/manifest.yaml; null if absent
  action: GENERATE_DATA                     # GENERATE_DATA | TRAIN | EVAL | DONE
  source_eval: 2026-04-19_2239_my-adapter.json

scores:
  avg_similarity: 0.243      # from the fine-tuned eval JSON summary
  previous_avg:   0.26       # prior adapter eval by mtime; null if none
  base_model_avg: 0.068      # latest base-model eval; null if none
  excellent: 0               # count of results with band EXCELLENT
  good:      1
  partial:   0
  poor:      9
  total:     10

boost_conventions:           # conventions below the GOOD threshold (0.6)
  - name: error_wrapping
    avg_score: 0.243
    n_holdout_examples: 10
    priority: critical       # critical | high | medium | low (see §3.4)

skip_conventions:            # conventions at or above GOOD — do not boost
  - name: retry_logic
    avg_score: 0.636
    reason: GOOD             # EXCELLENT | GOOD

uncovered_conventions:       # tags in manifest.yaml but not in holdout
  - name: batch_operation
  - name: grpc_status

format_health:
  func_start_pct: 100.0        # % of non-error responses starting with `func ` after fence strip
  boilerplate_detected: 0      # count of responses with package/import lines before the first func
  boilerplate_cases: []        # [{func, score, length_ratio}] for each such case
```

### 3.4 How each field is computed

All logic lives in `handoff/emit_synth_status.py`. Summary:

| Field | Source |
|---|---|
| `scores.*` | Read directly from the eval JSON's `summary` block. |
| `previous_avg` | Second-newest `*_<adapter>.json` in `evals/` by mtime. |
| `base_model_avg` | Newest `*_<base_model_safe>.json` whose timestamp is ≥ the current eval's (fallback: latest overall). |
| `boost_conventions` | Items from `summary.convention_breakdown` with `avg < 0.6`. |
| `skip_conventions` | Items with `avg >= 0.6`. Reason is `EXCELLENT` if `avg >= 0.8`, else `GOOD`. |
| `uncovered_conventions` | Sorted `set(all manifest tags) − set(conventions tested in this holdout)`. |
| `priority` | `critical` if `n ≥ 10 and avg < 0.5`; else `high` if `avg < 0.3`; else `medium` if `avg < 0.5`; else `low`. Intent: well-covered conventions that are still failing get the loudest signal (they aren't a data-volume problem). |
| `action` | `GENERATE_DATA` if `boost_conventions` is non-empty; else `TRAIN` if `avg < 0.6`; else `DONE` if `avg ≥ 0.8`; else `EVAL`. |
| `format_health.func_start_pct` | After stripping ` ```lang ` fences, fraction of responses whose first non-empty line begins with `func `. |
| `format_health.boilerplate_*` | A response is flagged if any line before its first `func ` starts with `package `, `import `, or `import(`. Those responses are listed with their score and length ratio. |

Boost list is sorted by priority rank then ascending `avg_score`, so
reposynth can read top-down.

### 3.5 What is deliberately NOT emitted

- `failure_note` — per-convention root-cause explanations. Requires an LLM
  pass over the expected/generated diffs; not yet implemented. Reposynth
  should tolerate the field being absent.
- `train_params_suggestions.yaml` — hyperparameter advice to the training
  pipeline. Same LLM pass would produce this; see §5.

The mechanically-derivable fields above are sufficient to tell reposynth
*which* conventions to boost. The missing fields would tell it *how* to bias
each boost.

---

## 4. Pipeline wiring (how it runs in practice)

```
cycle.py::main()
  │
  ├─ STEP 1–4  (backup → stop vllm → train → start vllm)
  │
  ├─ STEP 5    step_eval(adapter)      → evals/<ts>_<adapter>.{md,json}
  │            step_eval(base)         → evals/<ts>_<base>.{md,json}
  │
  ├─ STEP 5b   step_emit_synth_status(ft_path)
  │              └─ handoff/emit_synth_status.emit(ft_path)
  │                  → evals/<ts>_<adapter>_synth_status.yaml
  │
  └─ STEP 6    step_report(ft, base, status_path)
                 └─ prints "Synth status (for reposynth): <path>" in summary
```

Step 5b is best-effort: any exception is logged as a `WARN` and the cycle
continues to the report. The `.md` + `.json` eval artifacts are produced
independently and always succeed or fail on their own merits.

---

## 5. Future work: the LLM analysis pass

`failure_note` fields and the companion `train_params_suggestions.yaml`
require semantic analysis of expected-vs-generated diffs on each poor or
partial example. The design for that pass is:

- Input: every `band ∈ {POOR, PARTIAL}` result from the fine-tuned eval
  JSON, plus the convention tags on each.
- Output:
  - Per-convention `failure_note` strings merged into
    `synth_status.yaml::boost_conventions[*].failure_note`.
  - A separate `evals/<ts>_<adapter>_train_params_suggestions.yaml`
    consumed by `train.py` / `cycle.py` on the next cycle. It contains
    `suggestions` (proposed param changes with rationale) and a mandatory
    `holds` section recording parameters that were deliberately left
    unchanged, so the training system does not relitigate them each cycle.

Until that pass exists, reposynth can still operate on the mechanical output:
`boost_conventions` tells it what to generate more of, `skip_conventions`
tells it what to leave alone, and `uncovered_conventions` tells it where the
holdout itself has blind spots.

---

## 6. File reference

| Path | Role |
|---|---|
| `config.yaml` | Paths and hyperparameters. Drives everything. |
| `data/training.jsonl` | Inbound — ShareGPT training data. |
| `data/holdout.jsonl` | Inbound — messages-format holdout with `conventions_tested`. |
| `data/manifest.yaml` | Inbound — optional; supplies `cycle` and the tag universe for `uncovered_conventions`. |
| `cycle.py` | Orchestrator; calls Step 5b after the fine-tuned eval. |
| `eval.py` | Produces `<ts>_<model>.{md,json}`. |
| `handoff/emit_synth_status.py` | Step 5b implementation. Exposes `emit(eval_path) -> Path`. |
| `handoff/status_flow.md` | This document. |
| `evals/<ts>_<adapter>_synth_status.yaml` | Outbound — the handoff artifact. |
