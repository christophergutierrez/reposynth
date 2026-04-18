#!/usr/bin/env python3
"""
llm_client.py — Provider-agnostic LLM wrapper for reposynth.

Supports:
  - Anthropic SDK (type: anthropic)
  - OpenAI-compatible API (type: openai) — works with proxies, local models, etc.

Config (from synth.yaml passed as JSON via stdin):
  provider.type:         "anthropic" | "openai"
  provider.model:        model name
  provider.base_url:     null = use SDK default, otherwise use as base URL
  provider.api_key_env:  name of the env var holding the API key

Environment variables:
  The API key is read from the env var named in provider.api_key_env.
"""

import asyncio
import logging
import os
from typing import Any

log = logging.getLogger("reposynth.llm")


def _get_api_key(config: dict) -> str:
    env_var = config.get("provider", {}).get("api_key_env", "ANTHROPIC_API_KEY")
    key = os.environ.get(env_var, "")
    if not key:
        raise RuntimeError(
            f"API key not set. Set the environment variable: {env_var}"
        )
    return key


def make_client(config: dict) -> Any:
    """Create and return an async LLM client from config."""
    provider = config.get("provider", {})
    ptype = provider.get("type", "anthropic")
    base_url = provider.get("base_url") or None
    api_key = _get_api_key(config)

    if ptype == "anthropic":
        import anthropic
        kwargs: dict = {"api_key": api_key}
        if base_url:
            kwargs["base_url"] = base_url
        return anthropic.AsyncAnthropic(**kwargs)
    elif ptype == "openai":
        import openai
        kwargs = {"api_key": api_key}
        if base_url:
            kwargs["base_url"] = base_url
        return openai.AsyncOpenAI(**kwargs)
    else:
        raise ValueError(f"Unknown provider type: {ptype!r}. Use 'anthropic' or 'openai'.")


def get_model(config: dict) -> str:
    return config.get("provider", {}).get("model", "claude-sonnet-4-6")


async def complete(
    client: Any,
    config: dict,
    messages: list[dict],
    max_tokens: int = 8192,
) -> str:
    """
    Call the LLM and return the assistant's text content.
    Works with both Anthropic and OpenAI clients.
    """
    model = get_model(config)
    ptype = config.get("provider", {}).get("type", "anthropic")

    if ptype == "anthropic":
        import anthropic
        resp = await client.messages.create(
            model=model,
            max_tokens=max_tokens,
            messages=messages,
        )
        return resp.content[0].text

    elif ptype == "openai":
        resp = await client.chat.completions.create(
            model=model,
            max_tokens=max_tokens,
            messages=messages,
        )
        return resp.choices[0].message.content

    else:
        raise ValueError(f"Unknown provider type: {ptype!r}")
