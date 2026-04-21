# Architecture

## Overview

`reposynth` is a Rust CLI that shells out to embedded Python scripts for LLM calls. The Rust layer owns all file I/O, config parsing, JSONL processing, and health checks. Python handles only LLM interaction and prompt logic.

```
reposynth (Rust binary)
├── CLI parsing (clap)
├── Config loading (synth.yaml → config.rs)
├── Language detection (detect.rs)
├── JSONL processing pipeline (process.rs)
├── Health check (health.rs)
└── Python script runner (runner.rs)
      ├── generate.py   — rule-based generation
      ├── booster.py    — pattern-targeted generation
      ├── holdout.py    — reverse-prompt holdout builder
      └── llm_client.py — Anthropic / OpenAI wrapper
```

Python scripts are compiled into the binary via `include_str!` and extracted to `~/.reposynth/python/` at runtime. The binary is self-contained; no separate Python package install is required beyond `anthropic`/`openai` + `pyyaml`.

---

## Data flow: `reposynth generate`

```
conventions_dir/*.md          patterns_file (go.yaml)
        │                              │
        ▼                              ▼
  generate.py                    booster.py  ←── repeated --passes times
  (rules pass)                (booster pass)     (booster_raw_pass1.jsonl …)
        │                              │
        │                    (concat pass files)
        │                              │
        ▼                              ▼
  rules_raw.jsonl            booster_raw.jsonl
        │                              │
        └──────────┬────────────────────┘
                   ▼
         process_pipeline (×2, Rust)
           1. strip_meta        → removes _source field
           2. convert_sharegpt  → messages[] → conversations[]
           3. normalize         → strips preambles, cleans Go fences
                   │
                   ▼  (booster only, when --passes > 1)
                 dedup → removes exact-duplicate assistant responses
                   │
                   ▼
         rules_clean.jsonl + booster_clean.jsonl
                   │
                   ▼
           combine_files (Rust)
                   │
                   ▼
     combined_<date>_clean.jsonl
                   │
                   ▼
           health check (Rust)
```

Intermediate files (`*_raw.jsonl`, `*_pass*.jsonl`, `*_nometa.jsonl`, `*_sharegpt.jsonl`) are deleted after each step.

---

## Two generation strategies

### Rules pass (`generate.py`)

Reads every `*.md` file under `conventions_dir` (default: `.claude/rules`). For each file, it sends the full convention text plus `codebase_context` to the LLM and requests `rules_per_file` `(user, assistant)` pairs.

The prompt enforces:
- Concrete, specific developer requests (not generic "write a Go function")
- At least one example where the user pastes violating code and asks for a review
- Assistant responses: bare code fence only, no preamble/postamble

Results are tagged with `_source: <rule_file_path>` for resume support (`--resume` skips already-generated sources).

### Booster pass (`booster.py`)

Reads a pattern catalog YAML — each entry has a `name`, `description`, `reference` (a real code snippet), and optional `n` override. For each pattern, it sends the reference code as a few-shot example and asks the LLM for `n` variations.

The booster is the primary tool for fixing health check failures: add a pattern with a real reference and run `reposynth generate --only booster`.

**Multi-pass generation (`--passes N`):** Running the booster with `--passes N` invokes `booster.py` N times, each producing an independent set of examples. The LLM generates different variations each run because temperature is non-zero. After normalization, an exact-dedup step removes any records where the assistant response is identical across passes. This is the recommended approach when a pattern needs more volume than a single batch yields — the model saturates at roughly 15–20 usable examples per batch regardless of the `n` value set, so `--passes 3` reliably produces 2–3× the unique examples rather than setting `n` to an impractically large value.

---

## JSONL processing pipeline (`process.rs`)

Three sequential transformations applied by `process_pipeline`, plus an optional dedup step:

| Step | Input | Output | What it does |
|------|-------|--------|--------------|
| `strip_meta` | `*_raw.jsonl` | `*_nometa.jsonl` | Removes `_source` field |
| `convert_sharegpt` | `*_nometa.jsonl` | `*_sharegpt.jsonl` | Converts `messages[role/content]` → `conversations[from/value]` (`user`→`human`, `assistant`→`gpt`) |
| `normalize` | `*_sharegpt.jsonl` | `*_clean.jsonl` | Strips preamble/postamble text; for Go, removes package declarations and imports above the first `func`/`type`/`var` |
| `dedup` *(booster, `--passes > 1` only)* | `*_clean.jsonl` | `*_deduped_clean.jsonl` | Removes records with identical assistant/gpt content; dedup key is the full normalized `gpt` turn value |

`normalize` works on both `messages` and `conversations` formats — it can be called standalone via `reposynth clean` on any existing JSONL file.

---

## Health check (`health.rs`)

Parses each assistant turn, extracts code fences via regex, and accumulates per-language metrics:

**func_start_pct** — fraction of code fences whose first non-whitespace line starts with `func `/ `def `/`async def `/`fn `. Configurable per language in `synth.yaml`. Languages without a config entry auto-pass.

**pattern_coverage** — counts occurrences of tracked identifier strings within Go code fences. Currently hardcoded: `NamedQueryContext`, `StructScan`, `NamedExecContext`, `sqlx.In`, `NotFoundError`, `valog.Logger`, `WithTimeout`. Minimum threshold is `min_pattern_coverage` from config (default: 20).

A language passes if both thresholds are met. The overall report is `READY TO TRAIN` only if all configured languages pass.

---

## Config schema (`config.rs`)

```
Config
├── languages: Vec<String>
├── provider: ProviderConfig
│   ├── type: "anthropic" | "openai"
│   ├── model: String
│   ├── base_url: Option<String>   # null = SDK default
│   └── api_key_env: String
├── conventions_dir: Option<String>   # default: .claude/rules
├── patterns_file: Option<String>     # default: .reposynth/patterns/go.yaml
├── output_dir: Option<String>        # default: .reposynth/data
├── generate: Option<GenerateConfig>
│   ├── rules_per_file: usize         # default: 5
│   ├── booster_n: usize              # default: 8
│   └── concurrency: usize            # default: 3
├── health: Option<HashMap<lang, HealthConfig>>
│   ├── func_start_pct: Option<f64>
│   └── min_pattern_coverage: Option<usize>
├── codebase_context: Option<String>  # injected into every generation prompt
└── system_prompt: Option<String>     # overrides default system identity
```

---

## Python ↔ Rust interface

The Rust runner serializes the full `Config` struct plus runtime fields (`repo_root`, `output_file`, `resume`, `verbose`) into a JSON object and writes it to the Python script's stdin. Scripts read it with `json.load(sys.stdin)`.

This is intentionally simple: no sockets, no temp files, no environment variable passing (except the API key, which Python reads from the env var named in `config["provider"]["api_key_env"]`).

---

## LLM client (`llm_client.py`)

Provider-agnostic async wrapper. `make_client(config)` returns either `anthropic.AsyncAnthropic` or `openai.AsyncOpenAI` based on `provider.type`. The `complete()` coroutine normalizes both response shapes to a plain string.

All generation scripts use `asyncio.gather` with a `Semaphore` for bounded concurrency. Error handling: each task retries up to 3 times with exponential backoff; failures are logged and skipped rather than aborting the run.

---

## Extending reposynth

**Add a new language:**
1. Add detection logic in `detect.rs`
2. Add a pattern template in `templates/patterns/<lang>.yaml` and register it in `main.rs` (`cmd_init`)
3. Add `func_start` logic for the language in `health.rs` (`is_func_start` match arm)
4. Add a `health.<lang>` section to `templates/synth.yaml.tpl`

**Add tracked patterns for Go:**
Edit `health.rs` — the `go_patterns` array near the top of `check()`.

**Add a new generation strategy:**
Add a Python script to `python/`, embed it in `runner.rs` via `include_str!`, extract it in `ensure_scripts_extracted`, and call it from `cmd_generate` in `main.rs`.
