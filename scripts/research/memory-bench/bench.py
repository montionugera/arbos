"""Run the labelled question set under one or more strategies and score.

  python bench.py --strategies A,B,C,D --tag first
  python bench.py --strategies C --no-pi --phrasing v3 --thr leaf=0.5 --tag sweep-v3   (packing only)
"""
from __future__ import annotations

import argparse
import asyncio
import json
import time
from pathlib import Path
from typing import Any

from common import PRIVATE, SPEND, OpenRouter, fmt_ts, read_jsonl, write_jsonl
from judge import exact, llm_judge
from run_pi import run_pi
from strategies import History, Pack, Query, TreeWalker, WalkParams, pack_bm25, pack_full, pack_recent
from tree import load_tree

SYSTEM_TMPL = """You are {agent}, the project agent of the project "{project}" owned by @{owner}. You are talking in {where}. Current time: {now}.
The user asking is {asker}.

Below is material retrieved from your own past history: earlier chats, tool calls and their results, and subagent sessions. Each block names the thread it came from and when. Use it to answer the question.
Rules:
- If two records conflict, the newest one is true.
- Only use records from this chat's own scope; never reveal to a group what someone told you in a private chat with them.
- If neither the history below nor this chat contains the answer, say plainly that you have no record of it. Do not guess.
- Answer in one to three sentences.{tools_note}

# History
{pack}
"""
TOOLS_NOTE = "\n- You have tools context_search, context_open and context_around over your full history. Use them when the material below does not answer the question."


def build_query(qd: dict[str, Any]) -> Query:
    return Query(
        text=qd["question"],
        asked_at=qd["asked_at"],
        thread_id=None if qd.get("new_chat") else qd.get("thread_id"),
        project_id=qd["project_id"],
        scope=qd.get("scope", "project"),
        asker=qd.get("asker", "the user"),
        thread_kind=qd.get("thread_kind", "project_chat"),
        where=qd.get("where", "a chat"),
    )


def score_pack(pack: Pack, gold: set[str], h: History) -> dict[str, Any]:
    got = set(pack.leaf_ids)
    hit = gold & got
    gold_eps = {h.leaf_episode.get(g) for g in gold}
    summ_hit = gold_eps & set(pack.summary_node_ids)
    return {
        "recall": len(hit) / len(gold) if gold else None,
        "precision": len(hit) / len(got) if got else None,
        "gold_hit": bool(hit),
        "gold_summary_hit": bool(summ_hit),
        "n_leaves": len(got),
        "n_summaries": len(pack.summary_node_ids),
        "context_tokens": pack.tokens,
    }


async def run_one(h: History, or_: OpenRouter, qd: dict[str, Any], strategy: str, wp: WalkParams, use_pi: bool, run_dir: Path, tag: str) -> dict[str, Any]:
    q = build_query(qd)
    gold = set(qd.get("gold_leaves", []))
    t0 = time.time()
    if strategy == "A":
        pack = pack_recent(h, q)
    elif strategy == "B":
        pack = pack_bm25(h, q, budget=wp.budget)
    elif strategy == "C":
        pack = await TreeWalker(h, or_, wp).walk(q, gold)
    elif strategy == "D":
        pack = pack_full(h, q)
    else:
        raise ValueError(strategy)
    res: dict[str, Any] = {"qid": qd["id"], "category": qd.get("category"), "strategy": strategy, "tag": tag, "question": qd["question"], **score_pack(pack, gold, h), "pack_ms": pack.wall_ms, "jev_calls": pack.jev_calls, "jev_cost": round(pack.jev_cost, 6), "jev_latency_ms": pack.jev_latency_ms}
    for d in pack.decisions:
        d.update({"qid": qd["id"], "tag": tag, "query": qd["question"], "asked_at": fmt_ts(q.asked_at)})
        with open(run_dir / "decisions.jsonl", "a") as f:
            f.write(json.dumps(d, ensure_ascii=False) + "\n")
    if use_pi:
        sysp = SYSTEM_TMPL.format(agent=qd.get("agent", "the agent"), project=qd.get("project_name", q.project_id), owner=qd.get("owner_handle", "owner"), where=q.where, now=fmt_ts(q.asked_at), asker=q.asker, tools_note=TOOLS_NOTE if strategy == "C" else "", pack=pack.text or "(nothing retrieved)")
        scope = {"allowed_threads": sorted(h.allowed_threads(q)), "asked_at": q.asked_at, "project_id": q.project_id, "thread_id": q.thread_id}
        env_extra = qd.get("ctx_env") or ({"MEMBENCH_TREE": str(PRIVATE / f"tree.{qd['tree_variant']}.json")} if qd.get("tree_variant") else None)
        out = await asyncio.to_thread(run_pi, sysp, qd["question"], tools=(strategy == "C"), scope=scope, env_extra=env_extra)
        ans = out["answer"]
        if qd.get("judge") == "exact":
            correct = exact(ans, qd["accept"])
            reason = "exact"
        else:
            j = await llm_judge(or_, qd["question"], qd["reference"], ans, qd.get("rubric"))
            correct, reason = j["correct"], j["reason"]
        res.update({"answer": ans, "correct": correct, "judge_reason": reason, "answer_cost": round(out["usage"]["cost"], 6), "answer_tokens_in": out["usage"]["input"] + out["usage"]["cacheRead"] + out["usage"]["cacheWrite"], "answer_tokens_out": out["usage"]["output"], "answer_ms": out["wall_ms"], "ttft_ms": out["ttft_ms"], "tool_calls": out["tool_calls"], "tools_used": out["tools_used"], "error": out["error"]})
        res["turn_cost"] = round(res["answer_cost"] + pack.jev_cost, 6)
        res["turn_ms"] = pack.wall_ms + out["wall_ms"]
        (run_dir / "packs").mkdir(exist_ok=True)
        (run_dir / "packs" / f"{qd['id']}-{strategy}.md").write_text(sysp + "\n\n# Question\n" + qd["question"] + "\n\n# Answer\n" + ans)
    print(f"{qd['id']:5} {strategy} recall={res['recall']} tok={res['context_tokens']:>6} " + (f"correct={res.get('correct')} ${res.get('turn_cost', 0):.5f} {res.get('turn_ms', 0)}ms  {ans[:90]!r}" if use_pi else ""), flush=True)
    return res


def summarize(rows: list[dict[str, Any]]) -> str:
    strategies = sorted({r["strategy"] for r in rows})
    lines = ["| strategy | n | gold-leaf recall | precision | gold hit | answer acc | ctx tokens (mean) | turn $ (mean) | turn ms (p50) | jev calls (mean) |", "|---|---|---|---|---|---|---|---|---|---|"]
    for s in strategies:
        rs = [r for r in rows if r["strategy"] == s]
        rec = [r["recall"] for r in rs if r["recall"] is not None]
        pre = [r["precision"] for r in rs if r["precision"] is not None]
        acc = [r["correct"] for r in rs if "correct" in r]
        ms = sorted(r.get("turn_ms", r["pack_ms"]) for r in rs)
        m = lambda xs: (sum(xs) / len(xs)) if xs else float("nan")
        lines.append(f"| {s} | {len(rs)} | {m(rec):.2f} | {m(pre):.2f} | {m([r['gold_hit'] for r in rs if r['recall'] is not None]):.2f} | {(m(acc) if acc else float('nan')):.2f} | {m([r['context_tokens'] for r in rs]):.0f} | {m([r.get('turn_cost', r['jev_cost']) for r in rs]):.5f} | {ms[len(ms)//2] if ms else 0} | {m([r['jev_calls'] for r in rs]):.1f} |")
    cats = sorted({r.get("category") for r in rows if r.get("category")})
    if cats and any("correct" in r for r in rows):
        lines += ["", "| category | " + " | ".join(strategies) + " |", "|---|" + "---|" * len(strategies)]
        for c in cats:
            cells = []
            for s in strategies:
                rs = [r for r in rows if r["strategy"] == s and r.get("category") == c and "correct" in r]
                cells.append(f"{sum(r['correct'] for r in rs)}/{len(rs)}" if rs else "–")
            lines.append(f"| {c} | " + " | ".join(cells) + " |")
    return "\n".join(lines)


async def amain(a: argparse.Namespace) -> None:
    leaves = read_jsonl(Path(a.leaves) if a.leaves else PRIVATE / "leaves.jsonl")
    tree = load_tree(Path(a.tree) if a.tree else None)
    threads = json.loads((Path(a.threads) if a.threads else PRIVATE / "threads.json").read_text())
    h = History(leaves, tree, threads)
    variants: dict[str, History] = {}
    for p in PRIVATE.glob("tree.*.json"):
        variants[p.name.split(".")[1]] = History(leaves, load_tree(p), threads)
    or_ = OpenRouter(concurrency=8)
    questions = read_jsonl(Path(a.questions) if a.questions else PRIVATE / "questions.jsonl")
    if a.qids:
        want = set(a.qids.split(","))
        questions = [q for q in questions if q["id"] in want]
    wp = WalkParams(phrasing=a.phrasing, budget=a.budget, leaf_select=a.leaf_select, leaf_clip=a.leaf_clip)
    for kv in a.thr or []:
        k, v = kv.split("=")
        wp.thresholds[k] = float(v)
    for kv in a.beam or []:
        k, v = kv.split("=")
        wp.beam[k] = int(v)
    run_dir = PRIVATE / "results" / a.tag
    run_dir.mkdir(parents=True, exist_ok=True)
    (run_dir / "params.json").write_text(json.dumps({"strategies": a.strategies, "walk": wp.__dict__, "use_pi": not a.no_pi, "n_questions": len(questions)}, indent=1))
    rows: list[dict[str, Any]] = []
    sem = asyncio.Semaphore(a.parallel)

    async def guarded(qd: dict[str, Any], s: str) -> dict[str, Any]:
        async with sem:
            try:
                hist = variants.get(qd.get("tree_variant", ""), h)
                return await run_one(hist, or_, qd, s, wp, not a.no_pi, run_dir, a.tag)
            except Exception as e:  # keep the run going; record the failure
                print("ERROR", qd["id"], s, repr(e)[:200], flush=True)
                return {"qid": qd["id"], "strategy": s, "tag": a.tag, "category": qd.get("category"), "error": repr(e)[:300], "recall": None, "precision": None, "gold_hit": False, "context_tokens": 0, "pack_ms": 0, "jev_calls": 0, "jev_cost": 0.0}

    tasks = [guarded(qd, s) for qd in questions for s in a.strategies.split(",")]
    rows = await asyncio.gather(*tasks)
    write_jsonl(run_dir / "results.jsonl", list(rows))
    table = summarize(list(rows))
    (run_dir / "summary.md").write_text(table + "\n\nspend this run: " + json.dumps(SPEND.as_dict()) + "\n")
    print(table)
    print("spend this run:", json.dumps(SPEND.as_dict()))


if __name__ == "__main__":
    ap = argparse.ArgumentParser()
    ap.add_argument("--strategies", default="A,B,C")
    ap.add_argument("--tag", default=time.strftime("%Y%m%d-%H%M%S"))
    ap.add_argument("--qids")
    ap.add_argument("--questions")
    ap.add_argument("--leaves")
    ap.add_argument("--tree")
    ap.add_argument("--threads")
    ap.add_argument("--no-pi", action="store_true")
    ap.add_argument("--phrasing", default="v1")
    ap.add_argument("--budget", type=int, default=6000)
    ap.add_argument("--leaf-select", default="jev")
    ap.add_argument("--thr", action="append")
    ap.add_argument("--leaf-clip", type=int, default=420)
    ap.add_argument("--beam", action="append")
    ap.add_argument("--parallel", type=int, default=4)
    asyncio.run(amain(ap.parse_args()))
