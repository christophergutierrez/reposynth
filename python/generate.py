#!/usr/bin/env python3
"""
generate.py — Rule-based training data generator for reposynth.

Reads convention rule markdown files from conventions_dir, calls the LLM to
generate realistic (user_request, code_response) pairs, and writes JSONL.

Receives config as JSON on stdin. Writes JSONL to config["output_file"].
"""

import asyncio
import json
import logging
import re
import sys
from pathlib import Path

from llm_client import make_client, complete

log = logging.getLogger("reposynth.generate")

# ---------------------------------------------------------------------------
# Prompts
# ---------------------------------------------------------------------------

RULE_GENERATION_PROMPT = """\
You are creating fine-tuning training data for a coding assistant embedded in
a software repository.

The assistant must correctly apply these conventions when answering coding questions:

<rules>
{rule_content}
</rules>

<codebase_context>
{context}
</codebase_context>

Generate {n} diverse, realistic training examples.

Requirements:
- Tasks must be specific, concrete requests a developer would actually type.
  BAD: "Write a Go function"
  GOOD: "Write a service method that fetches a user by ID from Postgres and
returns a NotFound gRPC status if the row is missing"
- Responses must fully apply every relevant convention — no omissions, no violations.
- Cover a mix of task types: writing new code, reviewing/fixing flawed code,
  creating BUILD/config files, adding tests, debugging.
- Include at least one example where the user pastes code that violates the rules
  and asks the assistant to review or fix it.
- Responses must include complete, working code snippets where relevant.

CRITICAL — Response format:
- The assistant value must contain ONLY the code, wrapped in a single code fence.
- NO preamble before the code (no "Here's the implementation...", "Sure!", etc.).
- NO postamble after the code (no "Key points:", "This approach...", etc.).
- NO inline comments explaining WHY you chose an approach — only production comments.
- Just the bare code fence: ```go\\n<code>\\n```

Return ONLY a valid JSON array — no preamble, no markdown fences:
[
  {{"user": "<concrete developer request>", "assistant": "```go\\n<code>\\n```"}},
  ...
]
"""

CROSS_DOMAIN_PROMPT = """\
You are creating fine-tuning training data for a coding assistant in a software
repository. The assistant must apply ALL of these conventions simultaneously:

<rules>
{rule_content}
</rules>

<codebase_context>
{context}
</codebase_context>

Generate {n} examples where MULTIPLE rules above apply at once.

Same format requirements as above — return ONLY a valid JSON array:
[
  {{"user": "<concrete developer request>", "assistant": "```go\\n<code>\\n```"}},
  ...
]
"""

# ---------------------------------------------------------------------------
# Utilities
# ---------------------------------------------------------------------------


def _strip_frontmatter(text: str) -> str:
    if not text.startswith("---"):
        return text
    end = text.find("---", 3)
    if end == -1:
        return text
    return text[end + 3:].strip()


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


def load_rule_files(repo_root: Path, conventions_dir: str) -> list[dict]:
    rules = []
    rules_path = repo_root / conventions_dir
    if not rules_path.exists():
        log.warning("Conventions directory not found: %s", rules_path)
        return rules

    for md_file in sorted(rules_path.rglob("*.md")):
        raw = md_file.read_text(encoding="utf-8")
        content = _strip_frontmatter(raw)
        if content.strip():
            rules.append({
                "path": str(md_file.relative_to(repo_root)),
                "name": md_file.stem,
                "content": content,
            })
    return rules


def already_generated(output_path: Path) -> set[str]:
    done: set[str] = set()
    if not output_path.exists():
        return done
    with output_path.open(encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                obj = json.loads(line)
                src = obj.get("_source")
                if src:
                    done.add(src)
            except json.JSONDecodeError:
                pass
    return done


# ---------------------------------------------------------------------------
# Async generation
# ---------------------------------------------------------------------------


async def _generate_for_source(
    client,
    config: dict,
    source: dict,
    n: int,
    context: str,
    semaphore: asyncio.Semaphore,
) -> list[dict]:
    prompt = RULE_GENERATION_PROMPT.format(
        rule_content=source["content"],
        context=context,
        n=n,
    )

    async with semaphore:
        log.info("Generating %d examples for %s", n, source["path"])
        for attempt in range(3):
            try:
                text = await complete(client, config, [{"role": "user", "content": prompt}])
                examples = _extract_json_array(text)
                log.info("  ✓ %d examples  ← %s", len(examples), source["path"])
                return examples
            except (ValueError, json.JSONDecodeError) as e:
                log.warning("  Parse error (attempt %d/3) for %s: %s", attempt + 1, source["path"], e)
            except Exception as e:
                log.warning("  API error (attempt %d/3) for %s: %s", attempt + 1, source["path"], e)
            if attempt < 2:
                await asyncio.sleep(2 ** attempt)

    log.error("  ✗ Skipping %s after 3 failed attempts", source["path"])
    return []


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------


async def run(config: dict) -> None:
    repo_root = Path(config.get("repo_root", ".")).resolve()
    conventions_dir = config.get("conventions_dir", ".claude/rules")
    output_file = Path(config["output_file"])
    n = config.get("generate", {}).get("rules_per_file", 5)
    concurrency = config.get("generate", {}).get("concurrency", 3)
    context = config.get("codebase_context", "")
    resume = config.get("resume", False)
    system_prompt = config.get(
        "system_prompt",
        "You are a coding assistant that follows the project's established conventions.",
    )

    sources = load_rule_files(repo_root, conventions_dir)
    log.info("Loaded %d rule files from %s", len(sources), conventions_dir)

    if not sources:
        log.error("No convention files found. Check conventions_dir in synth.yaml.")
        sys.exit(1)

    if resume:
        done = already_generated(output_file)
        before = len(sources)
        sources = [s for s in sources if s["path"] not in done]
        log.info("Resuming: %d remaining (%d already done)", len(sources), before - len(sources))

    if not sources:
        log.info("All sources already generated. Nothing to do.")
        return

    client = make_client(config)
    semaphore = asyncio.Semaphore(concurrency)

    tasks = [_generate_for_source(client, config, src, n, context, semaphore) for src in sources]
    results = await asyncio.gather(*tasks, return_exceptions=True)

    total = 0
    mode = "a" if resume else "w"
    with output_file.open(mode, encoding="utf-8") as f:
        for src, result in zip(sources, results):
            if isinstance(result, Exception):
                log.error("Unexpected error for %s: %s", src["path"], result)
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
                    "_source": src["path"],
                }
                f.write(json.dumps(record, ensure_ascii=False) + "\n")
                total += 1

    log.info("Done. Wrote %d training examples to %s", total, output_file)


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
