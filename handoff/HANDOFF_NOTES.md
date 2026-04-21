# Handoff Notes

## Format compatibility

The holdout format produced by reposynth (`data/holdout/*.jsonl`) is compatible
with what trainLLM expects. Records use OpenAI messages format with `role`/`content`
and include `conventions_tested` tags — the field that drives per-convention
breakdown in `synth_status.yaml`.

The training data format (`data/training/*.jsonl`) is ShareGPT with `conversations`
using `from`/`value` — confirm this matches `cycle.py::_validate_training_data()`.

## File naming on the training machine

When placing files in the training machine's `data/` directory, use these names:

| reposynth file | trainLLM expects |
|---|---|
| `data/training/combined_YYYYMMDD_vN.jsonl` | `data/training.jsonl` |
| `data/holdout/holdout_vN.jsonl` | `data/holdout.jsonl` |
| `evals/cycleN_manifest.yaml` | `data/manifest.yaml` |

### Cycle 8 files

| reposynth file | trainLLM name |
|---|---|
| `data/training/combined_20260420_v4.jsonl` | `data/training.jsonl` (3902 records; 75 contaminated records filtered from history) |
| `data/holdout/holdout_v2.jsonl` | `data/holdout.jsonl` (primary — use for run-over-run) |
| `data/holdout/holdout_v4.jsonl` | run separately if desired (harder set, not comparable to v2) |
| `evals/cycle8_manifest.yaml` | `data/manifest.yaml` |

The manifest **must** be named `manifest.yaml` in `data/` — `emit_synth_status.py`
reads `cfg.data_dir / "manifest.yaml"` explicitly. If it is absent or misnamed,
`cycle` is emitted as `null` and `uncovered_conventions` is empty.

The training machine's own `config.yaml` is a separate system configuration file
(paths, hyperparameters). Do not overwrite it with the reposynth manifest.

## What trainLLM emits

After each cycle, `synth_status.yaml` is written alongside the eval report:

```
evals/<timestamp>_<adapter>_synth_status.yaml
```

Copy this file back to `reposynth/evals/` so the next generation run can ingest it.
Once reposynth is updated to read `status_file` from `synth.yaml`, it will use this
to adjust pattern weights and inject failure notes into the booster prompt.

## What is not yet emitted

`failure_note` fields and `train_params_suggestions.yaml` require the LLM analysis
pass over expected/generated diffs. That pass is not yet implemented. reposynth
will tolerate absent `failure_note` fields — `boost_conventions` without notes is
still enough to drive prioritized generation.
