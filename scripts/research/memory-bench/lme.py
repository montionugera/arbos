"""LongMemEval_S adapter: each question's haystack (≈50 chat sessions, ≈120k tokens) becomes a
leaf set + tree (months → sessions → episodes → turns); the same strategies answer the question.

  python lme.py --n 50 --strategies A,B,C,D --tag lme1
Data: $MEMBENCH_PUBLIC/longmemeval_s (HF xiaowu0162/longmemeval). Work files per question under
$MEMBENCH_PUBLIC/lme-work/<qid>/ (public benchmark data, not private history).
"""
from __future__ import annotations

import argparse
import asyncio
import json
import os
import random
import time
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

import bench
from common import SPEND, OpenRouter, ntok, read_jsonl, write_jsonl
from strategies import History, WalkParams
from tree import TreeBuilder

PUBLIC = Path(os.environ.get("MEMBENCH_PUBLIC", Path.home() / "membench" / "public"))


def parse_date(s: str) -> int:
    # "2023/05/30 (Tue) 23:40"
    d = datetime.strptime(s.split(" (")[0] + " " + s.split(") ")[1], "%Y/%m/%d %H:%M").replace(tzinfo=timezone.utc)
    return int(d.timestamp() * 1000)


def to_leaves(x: dict[str, Any]) -> tuple[list[dict], dict[str, dict], list[str]]:
    leaves, threads, gold = [], {}, []
    for sid, date, sess in zip(x["haystack_session_ids"], x["haystack_dates"], x["haystack_sessions"]):
        t0 = parse_date(date)
        month = date[:7].replace("/", "-")
        threads[sid] = {"id": sid, "kind": "chat_session", "project_id": month, "project_name": f"chats in {month}", "owner": "user", "title": f"session on {date}", "created_at": t0}
        is_ans = sid in x["answer_session_ids"]
        for i, turn in enumerate(sess):
            lid = f"{sid}:{i}"
            text = turn["content"]
            leaves.append({"id": lid, "user": "user", "project_id": month, "project_name": f"chats in {month}", "thread_id": sid, "thread_kind": "chat_session", "subagent_id": None, "session_id": sid, "kind": turn["role"], "role": turn["role"], "at": t0 + i * 1000, "text": text, "source": {}, "tokens": ntok(text)})
            if is_ans and turn.get("has_answer"):
                gold.append(lid)
        if is_ans and not any(t.get("has_answer") for t in sess):
            gold.extend(f"{sid}:{i}" for i, t in enumerate(sess) if t["role"] == "user")
    leaves.sort(key=lambda z: (z["at"], z["id"]))
    return leaves, threads, gold


def sample(data: list[dict], n: int, seed: int = 0) -> list[dict]:
    rng = random.Random(seed)
    by_type: dict[str, list[dict]] = {}
    for x in data:
        by_type.setdefault(x["question_type"], []).append(x)
    per = max(1, n // len(by_type))
    out: list[dict] = []
    for t, xs in sorted(by_type.items()):
        abs_ = [x for x in xs if x["question_id"].endswith("_abs")]
        reg = [x for x in xs if not x["question_id"].endswith("_abs")]
        rng.shuffle(abs_)
        rng.shuffle(reg)
        k_abs = min(len(abs_), max(1, per // 6)) if abs_ else 0
        out.extend(abs_[:k_abs] + reg[: per - k_abs])
    rng.shuffle(out)
    return out[:n]


async def amain(a: argparse.Namespace) -> None:
    data = json.load(open(PUBLIC / "longmemeval_s"))
    qs = sample(data, a.n, a.seed)
    if a.qids:
        want = set(a.qids.split(","))
        qs = [x for x in data if x["question_id"] in want]
    or_ = OpenRouter(concurrency=10)
    wp = WalkParams(phrasing=a.phrasing, budget=a.budget, leaf_clip=a.leaf_clip, leaf_select=a.leaf_select)
    for kv in a.thr or []:
        k, v = kv.split("=")
        wp.thresholds[k] = float(v)
    for kv in a.beam or []:
        k, v = kv.split("=")
        wp.beam[k] = int(v)
    run_dir = Path(os.environ.get("MEMBENCH_PRIVATE", Path.home() / "membench" / "private")) / "results" / a.tag
    run_dir.mkdir(parents=True, exist_ok=True)
    (run_dir / "params.json").write_text(json.dumps({"benchmark": "LongMemEval_S", "n": len(qs), "strategies": a.strategies, "walk": wp.__dict__, "seed": a.seed}, indent=1))
    rows: list[dict[str, Any]] = []
    sem = asyncio.Semaphore(a.parallel)

    async def one(x: dict[str, Any]) -> None:
        qid = x["question_id"]
        work = PUBLIC / "lme-work" / qid
        work.mkdir(parents=True, exist_ok=True)
        leaves, threads, gold = to_leaves(x)
        tree_path = work / "tree.json"
        if tree_path.exists() and not a.rebuild:
            tree = json.loads(tree_path.read_text())
        else:
            t0 = time.time()
            tree = await TreeBuilder(leaves, threads, None).build()
            tree_path.write_text(json.dumps(tree))
            print(f"{qid} tree built in {time.time() - t0:.0f}s", flush=True)
        write_jsonl(work / "leaves.jsonl", leaves)
        (work / "threads.json").write_text(json.dumps(threads))
        h = History(leaves, tree, threads)
        is_abs = qid.endswith("_abs")
        qd = {
            "id": qid,
            "category": x["question_type"] + ("_abs" if is_abs else ""),
            "question": x["question"],
            "asked_at": parse_date(x["question_date"]),
            "thread_id": None,
            "new_chat": True,
            "project_id": "all",
            "scope": "user",
            "asker": "the user",
            "where": "a new chat session",
            "gold_leaves": gold,
            "reference": ("The history does not contain this information; the assistant should say it does not know or has no record." if is_abs else str(x["answer"])),
            "judge": "llm",
            "agent": "the assistant",
            "project_name": "the user's chat history",
            "owner_handle": "user",
            "rubric": ("Correct only if the answer abstains / says it does not know." if is_abs else "The date of the question is " + x["question_date"] + "; for temporal questions compute relative to it."),
            "ctx_env": {"MEMBENCH_TREE": str(tree_path), "MEMBENCH_LEAVES": str(work / "leaves.jsonl"), "MEMBENCH_THREADS": str(work / "threads.json")},
        }
        for s in a.strategies.split(","):
            async with sem:
                try:
                    r = await bench.run_one(h, or_, qd, s, wp, not a.no_pi, run_dir, a.tag)
                except Exception as e:
                    print("ERROR", qid, s, repr(e)[:200], flush=True)
                    r = {"qid": qid, "strategy": s, "tag": a.tag, "category": qd["category"], "error": repr(e)[:300], "recall": None, "precision": None, "gold_hit": False, "context_tokens": 0, "pack_ms": 0, "jev_calls": 0, "jev_cost": 0.0}
                rows.append(r)

    await asyncio.gather(*(one(x) for x in qs))
    write_jsonl(run_dir / "results.jsonl", rows)
    table = bench.summarize(rows)
    (run_dir / "summary.md").write_text(table + "\n\nspend this run: " + json.dumps(SPEND.as_dict()) + "\n")
    print(table)
    print("spend this run:", json.dumps(SPEND.as_dict()))


if __name__ == "__main__":
    ap = argparse.ArgumentParser()
    ap.add_argument("--n", type=int, default=50)
    ap.add_argument("--seed", type=int, default=0)
    ap.add_argument("--qids")
    ap.add_argument("--strategies", default="A,B,C")
    ap.add_argument("--tag", default="lme")
    ap.add_argument("--no-pi", action="store_true")
    ap.add_argument("--rebuild", action="store_true")
    ap.add_argument("--phrasing", default="v1")
    ap.add_argument("--budget", type=int, default=6000)
    ap.add_argument("--thr", action="append")
    ap.add_argument("--leaf-clip", type=int, default=420)
    ap.add_argument("--beam", action="append")
    ap.add_argument("--leaf-select", default="jev")
    ap.add_argument("--parallel", type=int, default=4)
    asyncio.run(amain(ap.parse_args()))
