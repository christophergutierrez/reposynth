#!/usr/bin/env python3
"""
booster.py — Pattern-based targeted training data generator for reposynth.

Unlike generate.py (which derives examples from rule files), this script uses
real production code as few-shot examples, then asks the model to generate
variations that apply the same pattern to different scenarios.

Receives config as JSON on stdin. Patterns are loaded from config["patterns_file"].
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

log = logging.getLogger("reposynth.booster")

# ---------------------------------------------------------------------------
# Generation prompt
# ---------------------------------------------------------------------------

BOOSTER_PROMPT = """\
Here is a real function (or pattern snippet) from the codebase:

<reference>
{reference}
</reference>

This demonstrates: {description}

Codebase context:
{context}

Generate {n} diverse training examples that use the EXACT SAME pattern applied to
different scenarios. Each example is a (user_request, code_response) pair.

Rules for the user request:
- 1–3 sentences, sounds like a real developer ask
- Mentions the key behaviour (what it fetches/deletes/updates, error cases, return type)
- Does NOT mention implementation details like template names or variable names

Rules for the assistant response:
- ONLY the code, wrapped in a single ``` ... ``` fence with the appropriate language tag
- Start directly with the function/method definition — NO package declarations or imports
- NO preamble ("Here's the implementation...", "Sure!", etc.)
- NO postamble ("Key points:", "Note that...", "This approach...", etc.)
- NO explanatory inline comments — only comments that would appear in production code
- Must apply the same pattern shown in the reference (same library calls, same error handling)

Return ONLY a valid JSON array — no preamble, no markdown fences:
[
  {{"user": "<developer request>", "assistant": "```lang\\n<code>\\n```"}},
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


def load_patterns(patterns_file: Path) -> list[dict]:
    """Load pattern catalog from YAML file."""
    if not patterns_file.exists():
        log.error("Patterns file not found: %s", patterns_file)
        sys.exit(1)
    with patterns_file.open(encoding="utf-8") as f:
        patterns = yaml.safe_load(f)
    if not patterns:
        log.error("No patterns found in %s", patterns_file)
        sys.exit(1)
    log.info("Loaded %d patterns from %s", len(patterns), patterns_file)
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
    prompt = BOOSTER_PROMPT.format(
        reference=pattern["reference"],
        description=pattern["description"],
        context=context,
        n=pattern.get("n", 8),
    )

    async with semaphore:
        log.info("Generating %d examples for pattern: %s", pattern.get("n", 8), pattern["name"])
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
    patterns_file = Path(config.get("patterns_file", ".reposynth/patterns/go.yaml"))
    if not patterns_file.is_absolute():
        patterns_file = repo_root / patterns_file

    output_file = Path(config["output_file"])
    concurrency = config.get("generate", {}).get("concurrency", 3)
    context = config.get("codebase_context", "")
    system_prompt = config.get(
        "system_prompt",
        "You are a coding assistant that follows the project's established conventions.",
    )

    patterns = load_patterns(patterns_file)

    # Allow overriding n from config
    booster_n = config.get("generate", {}).get("booster_n")
    if booster_n:
        for p in patterns:
            if "n" not in p:
                p["n"] = booster_n

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
                    "_source": f"booster:{pattern['name']}",
                }
                f.write(json.dumps(record, ensure_ascii=False) + "\n")
                total += 1

    log.info("Done. Wrote %d booster examples to %s", total, output_file)


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
