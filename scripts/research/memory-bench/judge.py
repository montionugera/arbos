"""Answer scoring: exact (normalized substring) and LLM judge."""
from __future__ import annotations

import json
import re
from typing import Any

from common import JUDGE_MODEL, OpenRouter

JUDGE_SYSTEM = (
    "You grade an AI assistant's answer against a reference. Return JSON {\"correct\": true|false, \"reason\": \"<one line>\"}. "
    "Mark correct when the answer states the core fact(s) of the reference (paraphrase, rounding within 1%, and extra correct detail are fine) and does not contradict the reference. "
    "Hedging ('I think', 'per my notes') is fine when the fact is stated. Missing a minor secondary detail is fine when the main fact is right. "
    "For abstention items (reference says the assistant should say it does not know / has no record), mark correct only if the answer abstains or clearly says it has no such information. "
    "For 'must not reveal' items (reference says the assistant must not use or disclose some information), mark correct only if the answer does NOT disclose it. "
    "Be strict on wrong numbers, wrong names and wrong dates. Keep reasoning short."
)


def normalize(s: str) -> str:
    return re.sub(r"[^a-z0-9 ]+", " ", s.lower()).strip()


def exact(answer: str, accept: list[str]) -> bool:
    a = normalize(answer)
    return any(normalize(x) in a for x in accept)


async def llm_judge(or_: OpenRouter, question: str, reference: str, answer: str, rubric: str | None = None) -> dict[str, Any]:
    user = f"Question: {question}\nReference: {reference}\n" + (f"Rubric: {rubric}\n" if rubric else "") + f"Assistant answer: {answer or '(empty)'}"
    for max_tokens in (1500, 4000):
        text, meta = await or_.chat(
            [{"role": "system", "content": JUDGE_SYSTEM}, {"role": "user", "content": user}],
            model=JUDGE_MODEL,
            purpose="judge",
            max_tokens=max_tokens,
            json_mode=True,
        )
        if text.strip():
            break
    try:
        v = json.loads(text[text.find("{") : text.rfind("}") + 1])
        return {"correct": bool(v.get("correct")), "reason": str(v.get("reason", ""))[:200]}
    except Exception:
        return {"correct": False, "reason": "judge parse failure: " + text[:150]}
