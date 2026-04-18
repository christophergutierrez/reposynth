#!/usr/bin/env python3
"""
holdout.py — Build a holdout eval set from real repository functions.

Reads candidate functions from a YAML file, extracts their source code,
reverse-engineers a natural developer prompt for each, and writes
(prompt, actual_code) pairs as JSONL.

These records are NEVER used for training — only for evaluating fine-tuned models.

Receives config as JSON on stdin. Writes JSONL to config["output_file"].

Candidates YAML format (config["candidates_file"]):
  - file: path/to/file.go       # repo-relative path
    func: FunctionName           # function to extract
    tags: [sqlx, error_wrapping] # convention tags (optional, stored as metadata)
    language: go                 # optional, defaults to detected from file extension
"""

import asyncio
import json
import logging
import re
import sys
from pathlib import Path
from typing import Optional

import yaml

from llm_client import make_client, complete

log = logging.getLogger("reposynth.holdout")

# ---------------------------------------------------------------------------
# Reverse-engineering prompt
# ---------------------------------------------------------------------------

REVERSE_PROMPT = """\
Here is a real function from a software repository:

```{language}
{code}
```

Write a concise, realistic developer question that would naturally lead to this
exact implementation. The question should:
- Be 1–3 sentences max
- Sound like a real request ("Write a repository method that...", "Implement a
  function that...", "Add a service layer function that...")
- Mention the key behaviour: what it fetches/creates/deletes, what error cases
  it handles (e.g. not-found, timeout), what it returns
- NOT mention implementation details like template names, variable names, or
  library specifics — describe the intent, not the approach

Output ONLY the question. No preamble, no explanation.
"""

# ---------------------------------------------------------------------------
# Language-specific function extractors
# ---------------------------------------------------------------------------


def extract_go_function(source: str, func_name: str) -> Optional[str]:
    """Extract a complete Go function by name using brace-matching."""
    pattern = re.compile(
        rf"^func\s+(?:\([^)]*\)\s+)?{re.escape(func_name)}\s*\(",
        re.MULTILINE,
    )
    m = pattern.search(source)
    if not m:
        return None

    start = m.start()
    brace_pos = source.find("{", start)
    if brace_pos == -1:
        return None

    depth = 0
    for i in range(brace_pos, len(source)):
        if source[i] == "{":
            depth += 1
        elif source[i] == "}":
            depth -= 1
            if depth == 0:
                return source[start:i + 1]

    return None


def extract_python_function(source: str, func_name: str) -> Optional[str]:
    """Extract a Python function/method by name using indent tracking."""
    pattern = re.compile(
        rf"^([ \t]*)(?:async\s+)?def\s+{re.escape(func_name)}\s*\(",
        re.MULTILINE,
    )
    m = pattern.search(source)
    if not m:
        return None

    indent = m.group(1)
    start = m.start()
    lines = source[start:].splitlines(keepends=True)

    # Collect lines that are part of this function (same or deeper indent)
    func_lines = [lines[0]]
    for line in lines[1:]:
        stripped = line.rstrip("\n\r")
        if not stripped:
            func_lines.append(line)
            continue
        line_indent = len(stripped) - len(stripped.lstrip())
        if line_indent <= len(indent) and stripped.strip():
            break
        func_lines.append(line)

    return "".join(func_lines).rstrip()


EXTRACTORS = {
    "go": extract_go_function,
    "py": extract_python_function,
    "python": extract_python_function,
}

LANGUAGE_TAGS = {
    ".go": "go",
    ".py": "python",
    ".rs": "rust",
    ".ts": "typescript",
    ".tsx": "typescript",
    ".js": "javascript",
    ".java": "java",
    ".kt": "kotlin",
}


def detect_language(filepath: str) -> str:
    ext = Path(filepath).suffix.lower()
    return LANGUAGE_TAGS.get(ext, "")


def extract_function(source: str, func_name: str, language: str) -> Optional[str]:
    extractor = EXTRACTORS.get(language)
    if not extractor:
        log.warning("No function extractor for language %r, skipping", language)
        return None
    return extractor(source, func_name)


# ---------------------------------------------------------------------------
# Async reverse-prompt generation
# ---------------------------------------------------------------------------


async def reverse_engineer_prompt(
    client,
    config: dict,
    code: str,
    language: str,
    semaphore: asyncio.Semaphore,
) -> str:
    prompt = REVERSE_PROMPT.format(code=code, language=language)
    async with semaphore:
        for attempt in range(3):
            try:
                text = await complete(
                    client, config,
                    [{"role": "user", "content": prompt}],
                    max_tokens=256,
                )
                return text.strip()
            except Exception as e:
                log.warning("API error attempt %d: %s", attempt + 1, e)
                if attempt < 2:
                    await asyncio.sleep(2 ** attempt)
    return ""


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------


async def run(config: dict) -> None:
    repo_root = Path(config.get("repo_root", ".")).resolve()
    candidates_file = Path(config.get("candidates_file", ".reposynth/holdout_candidates.yaml"))
    if not candidates_file.is_absolute():
        candidates_file = repo_root / candidates_file

    output_file = Path(config["output_file"])
    system_prompt = config.get(
        "system_prompt",
        "You are a coding assistant that follows the project's established conventions.",
    )

    if not candidates_file.exists():
        log.error("Candidates file not found: %s", candidates_file)
        log.error("Create it with entries like:")
        log.error("  - file: path/to/file.go")
        log.error("    func: FunctionName")
        log.error("    tags: [sqlx, error_wrapping]")
        sys.exit(1)

    with candidates_file.open(encoding="utf-8") as f:
        candidates = yaml.safe_load(f) or []

    log.info("Loaded %d candidates from %s", len(candidates), candidates_file)

    client = make_client(config)
    semaphore = asyncio.Semaphore(4)

    records = []
    skipped = []

    for candidate in candidates:
        filepath = repo_root / candidate["file"]
        if not filepath.exists():
            log.warning("File not found, skipping: %s", candidate["file"])
            skipped.append(candidate.get("func", candidate["file"]))
            continue

        language = candidate.get("language") or detect_language(candidate["file"])
        source = filepath.read_text(encoding="utf-8")
        code = extract_function(source, candidate["func"], language)
        if not code:
            log.warning("Function %r not found in %s", candidate["func"], candidate["file"])
            skipped.append(candidate["func"])
            continue

        log.info("Extracted %s (%d lines)", candidate["func"], code.count("\n") + 1)
        records.append((candidate, code, language))

    if not records:
        log.error("No functions extracted. Check candidates file and repo path.")
        sys.exit(1)

    log.info("Generating prompts for %d functions...", len(records))
    tasks = [
        reverse_engineer_prompt(client, config, code, lang, semaphore)
        for _, code, lang in records
    ]
    prompts = await asyncio.gather(*tasks)

    written = 0
    with output_file.open("w", encoding="utf-8") as f:
        for (candidate, code, language), prompt in zip(records, prompts):
            if not prompt:
                log.warning("Empty prompt for %s, skipping", candidate["func"])
                continue
            record = {
                "id": f"holdout_{written + 1:03d}",
                "source_file": candidate["file"],
                "function_name": candidate["func"],
                "conventions_tested": candidate.get("tags", []),
                "messages": [
                    {"role": "system", "content": system_prompt},
                    {"role": "user", "content": prompt},
                    {"role": "assistant", "content": f"```{language}\n{code}\n```"},
                ],
            }
            f.write(json.dumps(record, ensure_ascii=False) + "\n")
            log.info(
                "  [%03d] %s\n        → %s",
                written + 1,
                candidate["func"],
                prompt[:80] + ("..." if len(prompt) > 80 else ""),
            )
            written += 1

    log.info("Done. Wrote %d holdout records to %s", written, output_file)
    if skipped:
        log.warning("Skipped (not found): %s", ", ".join(skipped))


def main() -> None:
    config = json.load(sys.stdin)
    logging.basicConfig(
        level=logging.DEBUG if config.get("verbose") else logging.INFO,
        format="%(asctime)s %(levelname)s %(message)s",
        datefmt="%H:%M:%S",
        stream=sys.stderr,
    )
    asyncio.run(run(config))


if __name__ == "__main__":
    main()
