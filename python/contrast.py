#!/usr/bin/env python3
"""
contrast.py — Contrast-based training data generator for reposynth.

Produces (wrong_code_input → correct_code_output) training pairs. The user
message contains plausible-but-wrong code (standard Go idioms, not VA
conventions) and asks for a review. The assistant response is the corrected
VA-idiomatic version.

This directly trains the model to prefer VA conventions when it sees familiar
standard patterns — addressing the root cause of fine-tuning plateau where the
model defaults to pretraining idioms.

Receives config as JSON on stdin. Contrast patterns are loaded from
config["contrast_file"] (default: .reposynth/patterns/contrast.yaml).
Writes JSONL to config["output_file"].
"""

import asyncio
import json
import logging
import re
import sys
from pathlib import Path

import yaml

from llm_client import make_client, complete

log = logging.getLogger("reposynth.contrast")

# ---------------------------------------------------------------------------
# Generation prompt
# ---------------------------------------------------------------------------

CONTRAST_PROMPT = """\
Here is the CORRECT implementation of a pattern from our codebase:

<correct>
{reference}
</correct>

This demonstrates: {description}

A developer wrote this WRONG version instead — it uses standard Go idioms but \
NOT our codebase conventions:

<wrong>
{wrong_example}
</wrong>

The key mistake(s): {wrong_description}

Codebase context:
{context}

Generate {n} training examples where a developer submits code with the SAME \
TYPE OF MISTAKE and asks for a review, and the assistant corrects it.

Rules for the user request:
- Paste their wrong code inline: "Here's what I have:" then a ```go fence
- Append a brief review ask on the same line after the fence OR as a trailing \
sentence ("Does this look right?", "Can you refactor this to match our \
conventions?", "My PR got flagged — what needs changing here?")
- Vary the scenario: different function names, domains, field types — but the \
SAME type of mistake
- 1 sentence of context before the code, 0–1 sentences after

Rules for the assistant response:
- ONLY the corrected code in a single ```go ... ``` fence
- Start directly with the function/method definition — NO package declarations \
or imports
- NO preamble ("Here's the fix:", "Sure!", etc.)
- NO postamble ("Key changes:", "Note that...", etc.)
- NO explanatory inline comments — only comments that would appear in \
production code
- Must fix EXACTLY the mistake described — apply the same VA pattern shown in \
the correct reference

Return ONLY a valid JSON array — no preamble, no markdown fences:
[
  {{"user": "<review request with wrong code>", \
"assistant": "```go\\n<corrected code>\\n```"}},
  ...
]
"""

# ---------------------------------------------------------------------------
# Utilities
# ---------------------------------------------------------------------------


def _extract_json_array(text: str) -> list[dict]:
    text = text.strip()
    text = re.sub(r"^```(?:json)?\s*", "", text)
    text = re.sub(r"\s*```\s*$", "", text)

    start = text.find("[")
    if start == -1:
        raise ValueError("No JSON array found in model output")

    array_text = text[start:]
    end = array_text.rfind("]")

    if end != -1:
        try:
            return json.loads(array_text[:end + 1])
        except json.JSONDecodeError:
            pass

    last_close = array_text.rfind("}")
    while last_close > 0:
        candidate = array_text[:last_close + 1]
        candidate = re.sub(r",\s*$", "", candidate) + "]"
        try:
            result = json.loads(candidate)
            if result:
                log.debug("Partial recovery: extracted %d objects", len(result))
                return result
        except json.JSONDecodeError:
            pass
        last_close = array_text.rfind("}", 0, last_close)

    raise ValueError("Could not extract any valid JSON objects from model output")


def load_contrast_patterns(contrast_file: Path) -> list[dict]:
    """Load contrast pattern catalog from YAML file."""
    if not contrast_file.exists():
        log.error("Contrast file not found: %s", contrast_file)
        sys.exit(1)
    with contrast_file.open(encoding="utf-8") as f:
        patterns = yaml.safe_load(f)
    if not patterns:
        log.error("No patterns found in %s", contrast_file)
        sys.exit(1)
    log.info("Loaded %d contrast patterns from %s", len(patterns), contrast_file)
    return patterns


# ---------------------------------------------------------------------------
# Async generation
# ---------------------------------------------------------------------------


async def _generate_pattern(
    client,
    config: dict,
    pattern: dict,
    context: str,
    semaphore: asyncio.Semaphore,
) -> list[dict]:
    n = pattern.get("n", 8)
    prompt = CONTRAST_PROMPT.format(
        reference=pattern["reference"].strip(),
        description=pattern["description"],
        wrong_example=pattern["wrong_example"].strip(),
        wrong_description=pattern["wrong_description"],
        context=context,
        n=n,
    )

    async with semaphore:
        log.info("Generating %d contrast examples for pattern: %s", n, pattern["name"])
        for attempt in range(3):
            try:
                text = await complete(client, config, [{"role": "user", "content": prompt}])
                examples = _extract_json_array(text)
                log.info("  ✓ %d examples  ← %s", len(examples), pattern["name"])
                return examples
            except (ValueError, json.JSONDecodeError) as e:
                log.warning("  Parse error (attempt %d/3) for %s: %s", attempt + 1, pattern["name"], e)
            except Exception as e:
                log.warning("  API error (attempt %d/3) for %s: %s", attempt + 1, pattern["name"], e)
            if attempt < 2:
                await asyncio.sleep(2 ** attempt)

    log.error("  ✗ Skipping %s after 3 failed attempts", pattern["name"])
    return []


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------


async def run(config: dict) -> None:
    repo_root = Path(config.get("repo_root", ".")).resolve()

    contrast_file = config.get("contrast_file", ".reposynth/patterns/contrast.yaml")
    contrast_path = Path(contrast_file)
    if not contrast_path.is_absolute():
        contrast_path = repo_root / contrast_path

    output_file = Path(config["output_file"])
    concurrency = config.get("generate", {}).get("concurrency", 3)
    context = config.get("codebase_context", "")
    system_prompt = config.get(
        "system_prompt",
        "You are a coding assistant that follows the project's established conventions.",
    )

    patterns = load_contrast_patterns(contrast_path)

    # Allow global n override from config
    contrast_n = config.get("generate", {}).get("contrast_n")
    if contrast_n:
        for p in patterns:
            if "n" not in p:
                p["n"] = contrast_n

    client = make_client(config)
    semaphore = asyncio.Semaphore(concurrency)

    tasks = [_generate_pattern(client, config, p, context, semaphore) for p in patterns]
    results = await asyncio.gather(*tasks, return_exceptions=True)

    total = 0
    with output_file.open("w", encoding="utf-8") as f:
        for pattern, result in zip(patterns, results):
            if isinstance(result, Exception):
                log.error("Unexpected error for %s: %s", pattern["name"], result)
                continue
            for ex in result:
                if not isinstance(ex, dict):
                    continue
                user_msg = ex.get("user", "").strip()
                asst_msg = ex.get("assistant", "").strip()
                if not user_msg or not asst_msg:
                    continue
                record = {
                    "messages": [
                        {"role": "system", "content": system_prompt},
                        {"role": "user", "content": user_msg},
                        {"role": "assistant", "content": asst_msg},
                    ],
                    "_source": f"contrast:{pattern['name']}",
                }
                f.write(json.dumps(record, ensure_ascii=False) + "\n")
                total += 1

    log.info("Done. Wrote %d contrast examples to %s", total, output_file)


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
