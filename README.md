# reposynth

Generate synthetic LLM fine-tuning training data from any repo's conventions.

`reposynth` scans a repository, reads its coding conventions (markdown rule files), and uses an LLM to produce realistic `(user_request, code_response)` training pairs in ShareGPT JSONL format — ready for QLoRA fine-tuning on models like Qwen2.5-Coder.

---

## Requirements

- Rust (stable) — to build
- Python 3.10+ — for generation scripts
- `pip install anthropic pyyaml` (or `openai pyyaml` for OpenAI-compatible providers)

## Build

```bash
git clone <repo>
cd reposynth
cargo build --release
# binary at: target/release/reposynth
```

---

## Quickstart

```bash
# 1. Initialize — detects languages, writes synth.yaml, copies pattern templates
cd /your/repo
reposynth init

# 2. Edit synth.yaml — set codebase_context with your import paths, libraries, etc.
# 3. Edit .reposynth/patterns/*.yaml — add/remove patterns for your codebase

# 4. Generate training data
export ANTHROPIC_API_KEY=sk-ant-...
reposynth generate

# 5. Check data quality
reposynth check

# 6. If anything fails the health check, add patterns and re-run
reposynth generate --only booster
```

---

## Commands

### `reposynth init`

Detects languages, writes `synth.yaml`, and scaffolds `.reposynth/`:

```
.reposynth/
  patterns/go.yaml          # pattern catalog (edit to add/remove)
  holdout_candidates.yaml   # holdout eval candidates (fill in before running holdout)
```

| Flag | Description |
|------|-------------|
| `--force` | Overwrite existing `synth.yaml` |

### `reposynth generate`

Runs the full generation pipeline:

1. **Rules pass** — reads `*.md` files from `conventions_dir`, generates N examples per file
2. **Booster pass** — reads `patterns_file`, generates targeted examples for specific patterns (runs `--passes` times)
3. Processes output: strip metadata → convert to ShareGPT → normalize code fences
4. If `--passes > 1`: deduplicates across passes by assistant content
5. Combines into a versioned `combined_<date>_clean.jsonl`
6. Runs a health check (unless `--skip-check`)

| Flag | Description |
|------|-------------|
| `--only rules\|booster` | Run only one pass |
| `--passes N` | Run the booster N times and deduplicate across passes (default: 1) |
| `--resume` | Resume an interrupted rules generation run |
| `--skip-check` | Skip health check after generation |
| `--verbose` | Enable verbose LLM logging |

### `reposynth check [FILE]`

Runs a data quality health check. If no file is given, uses the latest `combined_*.jsonl` in `output_dir`.

Checks per language (configurable in `synth.yaml`):
- Percentage of code fences that start with a function/method definition
- Per-pattern occurrence counts against a minimum threshold

### `reposynth clean INPUT`

Normalizes assistant responses in an existing JSONL file without re-generating:

- Strips preamble text before code fences
- Removes Go package declarations and imports before the first `func`

| Flag | Description |
|------|-------------|
| `-o OUTPUT` | Output path (default: `<input>_clean.jsonl`) |

### `reposynth holdout`

Builds a holdout eval set from real repo functions listed in `holdout_candidates.yaml`. Uses the LLM to generate the corresponding user request for each function (reverse-prompt).

| Flag | Description |
|------|-------------|
| `--candidates FILE` | Candidates YAML (default: `.reposynth/holdout_candidates.yaml`) |
| `-o OUTPUT` | Output JSONL path |

### `reposynth scripts-dir`

Prints the path where embedded Python scripts are extracted (`~/.reposynth/python/`).

---

## Configuration (`synth.yaml`)

```yaml
languages:
  - go

provider:
  type: anthropic          # anthropic | openai
  model: claude-sonnet-4-6
  base_url: ~              # null = default endpoint; set for proxies
  api_key_env: ANTHROPIC_API_KEY

conventions_dir: .claude/rules   # *.md convention files (recursive)
patterns_file: .reposynth/patterns/go.yaml
output_dir: .reposynth/data

generate:
  rules_per_file: 5        # examples generated per rule file
  booster_n: 8             # examples generated per booster pattern
  concurrency: 3           # max concurrent LLM calls

health:
  go:
    func_start_pct: 85     # % of Go fences that must start with 'func'
    min_pattern_coverage: 20  # minimum occurrences per tracked pattern

codebase_context: |
  Repository: your-org/your-repo
  - Import paths, mandatory libraries, error handling conventions, etc.
```

### Using an OpenAI-compatible proxy

```yaml
provider:
  type: openai
  model: gpt-4o
  base_url: https://llm-proxy.internal
  api_key_env: OPENAI_API_KEY
```

---

## Pattern catalog (`.reposynth/patterns/go.yaml`)

The booster pass uses a pattern catalog — real code snippets paired with descriptions. Each entry drives targeted generation for patterns that need more coverage.

```yaml
- name: NamedExecContext
  description: Named-parameter INSERT/UPDATE via sqlx.NamedExecContext
  n: 12          # override default booster_n for this pattern
  reference: |
    func (r *UserRepository) Create(ctx context.Context, u *User) error {
        _, err := r.db.NamedExecContext(ctx,
            `INSERT INTO users (id, name) VALUES (:id, :name)`, u)
        return err
    }
```

Run `reposynth check` after generation to see per-pattern coverage counts.

---

## Output format

All output is ShareGPT JSONL:

```json
{
  "conversations": [
    {"from": "system", "value": "You are a coding assistant..."},
    {"from": "human",  "value": "Write a service method that fetches a user by ID"},
    {"from": "gpt",    "value": "```go\nfunc (r *Repo) GetByID(...) {...}\n```"}
  ]
}
```

---

## Workflow for improving a fine-tuned model

```bash
# Check current data quality
reposynth check tools/llm/synth/combined_<date>_clean.jsonl

# Add patterns for anything with low coverage to .reposynth/patterns/go.yaml

# Generate more targeted data — run 3 passes to increase volume, dedup automatically
reposynth generate --only booster --passes 3

# Re-check
reposynth check

# When READY TO TRAIN ✓, scp to training machine and kick off fine-tuning
```
