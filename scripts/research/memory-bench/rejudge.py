"""Re-score an existing results.jsonl with the current judge (no pi re-run). Writes
results.rejudged.jsonl and summary.rejudged.md next to it.

  python rejudge.py --run full1 [--questions ../private/questions.jsonl]
"""
from __future__ import annotations

import argparse
import asyncio
import json
from pathlib import Path

import bench
from common import PRIVATE, SPEND, OpenRouter, read_jsonl, write_jsonl
from judge import exact, llm_judge


async def amain(a: argparse.Namespace) -> None:
    run_dir = PRIVATE / "results" / a.run
    rows = read_jsonl(run_dir / "results.jsonl")
    qmap = {}
    if a.questions:
        qmap = {q["id"]: q for q in read_jsonl(Path(a.questions))}
    or_ = OpenRouter(concurrency=8)
    sem = asyncio.Semaphore(8)

    async def one(r: dict) -> None:
        if "answer" not in r:
            return
        qd = qmap.get(r["qid"], {})
        ref = qd.get("reference") or r.get("reference")
        if qd.get("judge") == "exact":
            r["correct"] = exact(r["answer"], qd["accept"])
            r["judge_reason"] = "exact"
            return
        if not ref:
            return
        async with sem:
            j = await llm_judge(or_, r["question"], ref, r["answer"], qd.get("rubric") or r.get("rubric"))
        r["correct_before"] = r.get("correct")
        r["correct"] = j["correct"]
        r["judge_reason"] = j["reason"]

    await asyncio.gather(*(one(r) for r in rows))
    write_jsonl(run_dir / "results.rejudged.jsonl", rows)
    table = bench.summarize(rows)
    (run_dir / "summary.rejudged.md").write_text(table + "\n\nrejudge spend: " + json.dumps(SPEND.as_dict()) + "\n")
    print(table)
    changed = [(r["qid"], r["strategy"], r.get("correct_before"), r["correct"]) for r in rows if "correct_before" in r and r["correct_before"] != r["correct"]]
    print("changed:", changed)


if __name__ == "__main__":
    ap = argparse.ArgumentParser()
    ap.add_argument("--run", required=True)
    ap.add_argument("--questions")
    asyncio.run(amain(ap.parse_args()))
