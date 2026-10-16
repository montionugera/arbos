"""Shared clients: OpenRouter chat, Jev decisions, token counting, cost tracking.

No data lives here. Paths to private data come from MEMBENCH_PRIVATE (default ~/membench/private).
"""
from __future__ import annotations

import asyncio
import hashlib
import json
import os
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

import httpx
import tiktoken

PRIVATE = Path(os.environ.get("MEMBENCH_PRIVATE", Path.home() / "membench" / "private"))
OPENROUTER = "https://openrouter.ai/api"
JEV_MODEL = os.environ.get("MEMBENCH_JEV_MODEL", "typesafe/jev-1.14")
SUMMARY_MODEL = os.environ.get("MEMBENCH_SUMMARY_MODEL", "anthropic/claude-haiku-5.5")
ANSWER_MODEL = os.environ.get("MEMBENCH_ANSWER_MODEL", "anthropic/claude-haiku-5.5")
JUDGE_MODEL = os.environ.get("MEMBENCH_JUDGE_MODEL", "anthropic/claude-sonnet-5.5")

_enc = tiktoken.get_encoding("cl100k_base")


def ntok(text: str) -> int:
    return len(_enc.encode(text, disallowed_special=()))


def sha(s: str) -> str:
    return hashlib.sha256(s.encode()).hexdigest()[:16]


def fmt_ts(ms: int) -> str:
    return time.strftime("%Y-%m-%d %H:%M UTC", time.gmtime(ms / 1000))


@dataclass
class Spend:
    """Running cost ledger, by purpose."""

    usd: dict[str, float] = field(default_factory=dict)
    calls: dict[str, int] = field(default_factory=dict)
    tokens_in: dict[str, int] = field(default_factory=dict)
    tokens_out: dict[str, int] = field(default_factory=dict)

    def add(self, purpose: str, usd: float, tin: int = 0, tout: int = 0) -> None:
        self.usd[purpose] = self.usd.get(purpose, 0.0) + usd
        self.calls[purpose] = self.calls.get(purpose, 0) + 1
        self.tokens_in[purpose] = self.tokens_in.get(purpose, 0) + tin
        self.tokens_out[purpose] = self.tokens_out.get(purpose, 0) + tout

    def total(self) -> float:
        return sum(self.usd.values())

    def as_dict(self) -> dict[str, Any]:
        return {
            "total_usd": round(self.total(), 6),
            "by_purpose": {
                k: {
                    "usd": round(v, 6),
                    "calls": self.calls[k],
                    "tokens_in": self.tokens_in[k],
                    "tokens_out": self.tokens_out[k],
                }
                for k, v in self.usd.items()
            },
        }


SPEND = Spend()


class OpenRouter:
    def __init__(self, key: str | None = None, concurrency: int = 8):
        self.key = key or os.environ["OPENROUTER_API_KEY"]
        self.http = httpx.AsyncClient(timeout=120)
        self.sem = asyncio.Semaphore(concurrency)

    async def chat(
        self,
        messages: list[dict[str, Any]],
        model: str = SUMMARY_MODEL,
        purpose: str = "chat",
        max_tokens: int = 800,
        temperature: float = 0.0,
        json_mode: bool = False,
    ) -> tuple[str, dict[str, Any]]:
        body: dict[str, Any] = {
            "model": model,
            "messages": messages,
            "max_tokens": max_tokens,
            "temperature": temperature,
            "usage": {"include": True},
        }
        if json_mode:
            body["response_format"] = {"type": "json_object"}
        async with self.sem:
            for attempt in range(4):
                t0 = time.time()
                r = await self.http.post(
                    f"{OPENROUTER}/v1/chat/completions",
                    headers={"Authorization": f"Bearer {self.key}"},
                    json=body,
                )
                if r.status_code >= 500 or r.status_code == 429:
                    await asyncio.sleep(1.5 * (attempt + 1))
                    continue
                r.raise_for_status()
                v = r.json()
                usage = v.get("usage", {})
                SPEND.add(
                    purpose,
                    float(usage.get("cost", 0.0)),
                    int(usage.get("prompt_tokens", 0)),
                    int(usage.get("completion_tokens", 0)),
                )
                text = v["choices"][0]["message"]["content"] or ""
                return text, {
                    "usage": usage,
                    "latency_ms": int((time.time() - t0) * 1000),
                    "model": v.get("model", model),
                }
        raise RuntimeError("openrouter chat failed after retries")

    async def decide(
        self,
        state: dict[str, Any],
        questions: dict[str, Any],
        model: str = JEV_MODEL,
        purpose: str = "jev",
    ) -> tuple[dict[str, float], dict[str, Any]]:
        """Decisions API. Returns {question_id: P(yes)} and meta (usage, latency, model build)."""
        body = {"model": model, "state": state, "questions": questions}
        async with self.sem:
            for attempt in range(4):
                t0 = time.time()
                r = await self.http.post(
                    f"{OPENROUTER}/alpha/decisions",
                    headers={"Authorization": f"Bearer {self.key}"},
                    json=body,
                    timeout=30,
                )
                if r.status_code >= 500 or r.status_code == 429:
                    await asyncio.sleep(1.5 * (attempt + 1))
                    continue
                if r.status_code >= 400:
                    raise RuntimeError(f"decisions HTTP {r.status_code}: {r.text[:500]}")
                v = r.json()
                usage = v.get("usage", {})
                SPEND.add(
                    purpose,
                    float(usage.get("cost", 0.0)),
                    int(usage.get("input_tokens", 0)),
                    int(usage.get("output_tokens", 0)),
                )
                probs = {}
                for qid, a in v.get("answers", {}).items():
                    if a.get("type") == "noul":
                        probs[qid] = float(a["noul"])
                    else:
                        probs[qid] = float(a.get("confidence", 0.0))
                return probs, {
                    "usage": usage,
                    "latency_ms": int((time.time() - t0) * 1000),
                    "model": v.get("model", model),
                }
        raise RuntimeError("decisions failed after retries")


def read_jsonl(p: Path) -> list[dict[str, Any]]:
    out = []
    with open(p) as f:
        for line in f:
            line = line.strip()
            if line:
                out.append(json.loads(line))
    return out


def write_jsonl(p: Path, rows: list[dict[str, Any]]) -> None:
    p.parent.mkdir(parents=True, exist_ok=True)
    with open(p, "w") as f:
        for r in rows:
            f.write(json.dumps(r, ensure_ascii=False) + "\n")


def append_jsonl(p: Path, row: dict[str, Any]) -> None:
    p.parent.mkdir(parents=True, exist_ok=True)
    with open(p, "a") as f:
        f.write(json.dumps(row, ensure_ascii=False) + "\n")


class Cache:
    """Tiny JSON cache keyed by content hash, so re-runs do not re-pay for summaries."""

    def __init__(self, path: Path):
        self.path = path
        self.data: dict[str, Any] = {}
        if path.exists():
            self.data = json.loads(path.read_text())

    def get(self, key: str) -> Any:
        return self.data.get(key)

    def put(self, key: str, value: Any) -> None:
        self.data[key] = value
        self.path.parent.mkdir(parents=True, exist_ok=True)
        tmp = self.path.with_suffix(".tmp")
        tmp.write_text(json.dumps(self.data, ensure_ascii=False))
        tmp.replace(self.path)
