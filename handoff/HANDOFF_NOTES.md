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

### Cycle 9 holdout additions

`holdout_v5.jsonl` — 8 new records targeting weak/uncovered conventions from cycle 8 synth_status:

| id | function | conventions |
|---|---|---|
| holdout_v5_001 | ListExports | **logging** (IsDebugEnabled + valog.Logger enrichment), sqlx, context_timeout |
| holdout_v5_002 | CreateShareAssociation | **named_exec_context_insert**, sql_template |
| holdout_v5_003 | InsertExport | **named_exec_context_insert**, ErrRead.Join variant |
| holdout_v5_004 | UpdateExport | **named_exec_context_insert**, ErrRead.Join, UPDATE variant |
| holdout_v5_005 | GetExport | **grpc_status** (NotFound + InvalidArgument), parameter_conversion |
| holdout_v5_006 | NewListExportsRequestFromServiceRequest | **parameter_conversion**, **pagination_defaults** (sqlfunc.Paging + page token) |
| holdout_v5_007 | ProtoToEntityMethodology | **proto_conversion** (bidirectional enum) |
| holdout_v5_008 | GetIdentity | **http_client** (Bearer _key), **json_unmarshal** |

For cycle 9, use holdout_v2 + holdout_v5 together as `data/holdout.jsonl` (concatenate). Do NOT mix in holdout_v4 — it is not comparable to v2 baseline. holdout_v5 adds coverage for `logging` (previously uncovered) and provides more signal on the four low-scoring conventions.

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
