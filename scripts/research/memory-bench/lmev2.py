"""LongMemEval-V2-Small adapter: the two shared 100-trajectory haystacks (web, enterprise) become
two leaf sets + trees (site → trajectory → episode → step); every step is a leaf whose text is the
step header (url, thought, action) plus the accessibility tree (clipped for packing; the full tree
is reachable through context_open). Text-only: screenshots are not used, so no GPU reader.

  python lmev2.py build                         # leaves + trees for both domains (cards cached)
  python lmev2.py run --strategies A,B,C --tag v2a [--n 100] [--qids ...]
Scoring uses the benchmark's own evaluation/qa_eval_metrics.py (deterministic matchers; the two
LLM checkers run on the judge model through OpenRouter instead of the official evaluator).
"""
from __future__ import annotations

import argparse
import asyncio
import json
import os
import random
import re
import sys
import time
from pathlib import Path
from typing import Any
from urllib.parse import urlparse

import bench
from common import JUDGE_MODEL, PRIVATE, SPEND, OpenRouter, ntok, read_jsonl, write_jsonl
from strategies import History, Query, TreeWalker, WalkParams, pack_bm25, pack_recent
from tree import TreeBuilder

PUBLIC = Path(os.environ.get("MEMBENCH_PUBLIC", Path.home() / "membench" / "public"))
V2 = PUBLIC / "lmev2"
WORK = PUBLIC / "lmev2-work"
sys.path.insert(0, str(PUBLIC / "lmev2-repo" / "evaluation"))
import qa_eval_metrics as qem  # noqa: E402

RENDER_CLIP = 4000  # chars of accessibility tree kept in a packed leaf
BASE_T = 1_700_000_000_000
DOMAIN_SYSTEM_PROMPTS = {
    "web": "You are an experienced colleague in a web browsing environment that has a customized magento-based shopping website, a customized magento-based shopping admin cms website, as well as a customized forum website based on reddit/postmill. Answer based on your memory of the environment. If you do not know the answer, output exactly \\boxed{UNKNOWN}. Do not guess. Never attempt to guess an answer if you are not sure. If you believe the question's construction/premise is wrong, provide an explanation in \\boxed{} explaining why the question is flawed.",
    "enterprise": "You are an experienced colleague working in a customized ServiceNow environment. Answer based on your memory of the environment. If you do not know the answer, output exactly \\boxed{UNKNOWN}. Do not guess. Never attempt to guess an answer if you are not sure. If you believe the question's construction/premise is wrong, provide an explanation in \\boxed{} explaining why the question is flawed.",
}
SYSTEM_TMPL = """{domain_prompt}

Your memory is the material below: steps from your own earlier browsing sessions in this environment (each step shows the page URL, what you were thinking, the action you took, and the page's accessibility tree, clipped). Sessions are grouped under their task goal.{tools_note}
Put the final answer in \\boxed{{}} exactly as the question asks.

# Memory
{pack}
"""
TOOLS_NOTE = " You also have tools context_search, context_open (full page tree of a step, or the steps of a session) and context_around over all your sessions; use them when the material below is not enough."


def site_of(url: str) -> str:
    u = urlparse(url or "")
    host = u.netloc or "unknown"
    if "admin" in (u.path or ""):
        host += "/admin"
    return host


def build_domain(domain: str, trajectories: dict[str, Any], order: list[str]) -> tuple[list[dict], dict[str, dict]]:
    leaves: list[dict] = []
    threads: dict[str, dict] = {}
    for ti, tid in enumerate(order):
        t = trajectories[tid]
        site = site_of(t["start_url"])
        t0 = BASE_T + ti * 3_600_000
        threads[tid] = {"id": tid, "kind": "trajectory", "project_id": site, "project_name": f"site {site}", "owner": "agent", "title": f"[{t['outcome']}] {t['goal'][:200]}", "created_at": t0}
        for s in t["states"]:
            i = s["state_index"]
            head = f"step {i} | url: {s.get('url') or ''}"
            if s.get("thought"):
                head += f"\nthought: {s['thought']}"
            if s.get("action"):
                head += f"\naction: {s['action']}"
            tree_txt = s.get("accessibility_tree") or ""
            clipped = tree_txt[:RENDER_CLIP] + (f" …[{len(tree_txt) - RENDER_CLIP} more chars; context_open for the full page]" if len(tree_txt) > RENDER_CLIP else "")
            text = head + "\npage:\n" + clipped
            leaves.append({"id": f"{tid}:{i}", "user": "agent", "project_id": site, "project_name": f"site {site}", "thread_id": tid, "thread_kind": "trajectory", "subagent_id": None, "session_id": tid, "kind": "step", "role": "agent", "at": t0 + i * 10_000, "text": text, "full_text": head + "\npage:\n" + tree_txt, "source": {"url": s.get("url"), "step": i}, "tokens": ntok(text)})
    return leaves, threads


async def build(a: argparse.Namespace) -> None:
    traj = json.load(open(V2 / "small_trajectories.json"))
    hs = json.load(open(V2 / "haystacks" / "lme_v2_small.json"))
    qs = [json.loads(l) for l in open(V2 / "questions.jsonl")]
    for domain in ("web", "enterprise"):
        qid = next(q["id"] for q in qs if q["domain"] == domain)
        order = hs[qid]
        leaves, threads = build_domain(domain, traj, order)
        d = WORK / domain
        d.mkdir(parents=True, exist_ok=True)
        write_jsonl(d / "leaves.jsonl", leaves)
        (d / "threads.json").write_text(json.dumps(threads))
        t0 = time.time()
        tree = await TreeBuilder(leaves, threads, None, episode_max_tokens=8000, episode_max_leaves=12, tool_clip=1500).build()
        (d / "tree.json").write_text(json.dumps(tree))
        lv: dict[str, int] = {}
        for n in tree["nodes"].values():
            lv[n["level"]] = lv.get(n["level"], 0) + 1
        print(domain, len(leaves), "leaves", sum(x["tokens"] for x in leaves), "packed tokens", lv, f"{time.time() - t0:.0f}s", json.dumps(SPEND.as_dict()), flush=True)


def answer_phrases(q: dict[str, Any]) -> list[str]:
    spec = q["eval_function"]
    if not spec.startswith("norm_phrase_set_match"):
        return []
    seps = ",;"
    m = re.search(r"separators=([^|]+)", spec)
    if m:
        seps = m.group(1)
    phrases = [qem.normalize_phrase(p, lower=True, normalize_hyphen=True, strip_punct=True) for p in re.split("[" + re.escape(seps) + "]", str(q["answer"]))]
    phrases = [p for p in phrases if p]
    if any(len(p) < 4 or p.isdigit() for p in phrases):
        return []
    return phrases


_NORM_CACHE: dict[int, list[tuple[str, str]]] = {}
_GOLD_CACHE: dict[tuple[int, tuple[str, ...]], set[str]] = {}


def proxy_gold(h: History, phrases: list[str]) -> set[str]:
    """Leaves whose full page text contains every answer phrase (evidence proxy; the public release
    has no evidence labels). Normalized page texts are cached per history."""
    if not phrases:
        return set()
    key = (id(h), tuple(phrases))
    if key in _GOLD_CACHE:
        return _GOLD_CACHE[key]
    if id(h) not in _NORM_CACHE:
        _NORM_CACHE[id(h)] = [(leaf["id"], qem.normalize_phrase(leaf.get("full_text") or leaf["text"], lower=True, normalize_hyphen=True, strip_punct=True)) for leaf in h.leaves]
    out = {lid for lid, t in _NORM_CACHE[id(h)] if all(p in t for p in phrases)}
    _GOLD_CACHE[key] = out
    return out


def score(q: dict[str, Any], prediction: str) -> tuple[bool, str]:
    spec = q["eval_function"]
    name = qem.eval_name(spec)
    parsed = qem.extract_boxed_answer(prediction) or prediction
    kwargs: dict[str, Any] = {}
    if name.startswith("llm_"):
        kwargs.update(question_item=q, parsed_prediction=parsed, model_response=prediction, evaluator_model=JUDGE_MODEL, evaluator_base_url="https://openrouter.ai/api/v1", evaluator_api_key=os.environ["OPENROUTER_API_KEY"], evaluator_max_completion_tokens=3000)
        SPEND.add("judge", 0.004)  # approximate; the official module does not return usage
    ok = bool(qem.eval_from_spec(spec, parsed, q["answer"], **kwargs))
    return ok, name


async def run(a: argparse.Namespace) -> None:
    qs = [json.loads(l) for l in open(V2 / "questions.jsonl")]
    if a.qids:
        want = set(a.qids.split(","))
        qs = [q for q in qs if q["id"] in want]
    elif a.n:
        rng = random.Random(a.seed)
        by: dict[str, list] = {}
        for q in qs:
            by.setdefault((q["domain"], q["question_type"]), []).append(q)
        per = max(1, a.n // len(by))
        picked = []
        for k, xs in sorted(by.items()):
            rng.shuffle(xs)
            picked.extend(xs[:per])
        rng.shuffle(picked)
        qs = picked[: a.n]
    hist: dict[str, History] = {}
    for domain in ("web", "enterprise"):
        d = WORK / domain
        hist[domain] = History(read_jsonl(d / "leaves.jsonl"), json.loads((d / "tree.json").read_text()), json.loads((d / "threads.json").read_text()))
    for h in hist.values():
        proxy_gold(h, ["warm cache"])
    or_ = OpenRouter(concurrency=10)
    wp = WalkParams(phrasing="v1", budget=a.budget, leaf_clip=1200)
    wp.thresholds.update({"project": 0.15, "thread": 0.5, "episode": 0.5, "leaf": 0.5})
    wp.beam.update({"thread": 12, "episode": 16})
    for kv in a.thr or []:
        k, v = kv.split("=")
        wp.thresholds[k] = float(v)
    for kv in a.beam or []:
        k, v = kv.split("=")
        wp.beam[k] = int(v)
    run_dir = PRIVATE / "results" / a.tag
    run_dir.mkdir(parents=True, exist_ok=True)
    (run_dir / "params.json").write_text(json.dumps({"benchmark": "LongMemEval-V2-Small", "n": len(qs), "strategies": a.strategies, "walk": wp.__dict__, "budget": a.budget, "dmax_budget": a.dmax_budget}, indent=1))
    rows: list[dict[str, Any]] = []
    sem = asyncio.Semaphore(a.parallel)

    async def one(q: dict[str, Any], s: str) -> None:
        h = hist[q["domain"]]
        phrases = answer_phrases(q)
        query = Query(text=q["question"], asked_at=BASE_T + 10**9, thread_id=None, project_id="all", scope="user", asker="a colleague", where="a new session", thread_kind="question")
        async with sem:
            t0 = time.time()
            try:
                gold = await asyncio.to_thread(proxy_gold, h, phrases)
                if s == "A":
                    pack = pack_recent(h, query)
                elif s == "B":
                    pack = pack_bm25(h, query, k=40, budget=a.budget)
                elif s == "C":
                    pack = await TreeWalker(h, or_, wp).walk(query, gold)
                elif s == "D":
                    pack = pack_bm25(h, query, k=400, budget=a.dmax_budget)
                else:
                    raise ValueError(s)
                res: dict[str, Any] = {"qid": q["id"], "domain": q["domain"], "category": q["question_type"], "eval": qem.eval_name(q["eval_function"]), "strategy": s, "tag": a.tag, "question": q["question"][:300], "has_image": bool(q.get("image")), **bench.score_pack(pack, gold, h), "proxy_gold_n": len(gold), "pack_ms": pack.wall_ms, "jev_calls": pack.jev_calls, "jev_cost": round(pack.jev_cost, 6), "jev_latency_ms": pack.jev_latency_ms}
                norm_pack = qem.normalize_phrase(pack.text, lower=True, normalize_hyphen=True, strip_punct=True) if phrases else ""
                res["answer_in_context"] = (all(p in norm_pack for p in phrases) if phrases else None)
                for d in pack.decisions:
                    d.update({"qid": q["id"], "tag": a.tag, "query": q["question"][:300], "benchmark": "lme-v2", "needed_is_proxy": True})
                    with open(run_dir / "decisions.jsonl", "a") as f:
                        f.write(json.dumps(d, ensure_ascii=False) + "\n")
                if a.no_pi:
                    print(f"{q['id']} {s} {q['question_type'][:14]:14} aic={res['answer_in_context']} hit={res['gold_hit']} tok={res['context_tokens']:>6} jev={pack.jev_calls} ${pack.jev_cost:.4f}", flush=True)
                    rows.append(res)
                    return
                sysp = SYSTEM_TMPL.format(domain_prompt=DOMAIN_SYSTEM_PROMPTS[q["domain"]], tools_note=TOOLS_NOTE if s == "C" else "", pack=pack.text or "(no memory retrieved)")
                scope = {"allowed_threads": sorted(h.allowed_threads(query)), "asked_at": query.asked_at, "project_id": "all", "thread_id": None}
                env_extra = {"MEMBENCH_TREE": str(WORK / q["domain"] / "tree.json"), "MEMBENCH_LEAVES": str(WORK / q["domain"] / "leaves.jsonl"), "MEMBENCH_THREADS": str(WORK / q["domain"] / "threads.json")}
                out = await asyncio.to_thread(bench.run_pi, sysp, q["question"], tools=(s == "C"), scope=scope, env_extra=env_extra, timeout_s=240)
                ans = out["answer"]
                correct, ev = await asyncio.to_thread(score, q, ans)
                res.update({"answer": ans[-600:], "boxed": qem.extract_boxed_answer(ans), "correct": correct, "answer_cost": round(out["usage"]["cost"], 6), "answer_tokens_in": out["usage"]["input"] + out["usage"]["cacheRead"] + out["usage"]["cacheWrite"], "answer_tokens_out": out["usage"]["output"], "answer_ms": out["wall_ms"], "ttft_ms": out["ttft_ms"], "tool_calls": out["tool_calls"], "tools_used": out["tools_used"], "error": out["error"]})
                res["turn_cost"] = round(res["answer_cost"] + pack.jev_cost, 6)
                res["turn_ms"] = pack.wall_ms + out["wall_ms"]
                (run_dir / "packs").mkdir(exist_ok=True)
                (run_dir / "packs" / f"{q['id']}-{s}.md").write_text(sysp + "\n\n# Question\n" + q["question"] + "\n\n# Answer\n" + ans)
                print(f"{q['id']} {s} {q['question_type'][:14]:14} ok={correct} aic={res['answer_in_context']} tok={res['context_tokens']:>6} ${res['turn_cost']:.4f} {res['turn_ms']}ms {res['boxed'][:50]!r}", flush=True)
            except Exception as e:
                print("ERROR", q["id"], s, repr(e)[:200], flush=True)
                res = {"qid": q["id"], "domain": q["domain"], "category": q["question_type"], "strategy": s, "tag": a.tag, "error": repr(e)[:300], "recall": None, "precision": None, "gold_hit": False, "context_tokens": 0, "pack_ms": 0, "jev_calls": 0, "jev_cost": 0.0}
            rows.append(res)

    strategies = a.strategies.split(",")
    tasks = []
    for q in qs:
        for s in strategies:
            if s == "D" and a.d_subset and q["id"] not in a.d_subset:
                continue
            tasks.append(one(q, s))
    await asyncio.gather(*tasks)
    write_jsonl(run_dir / "results.jsonl", rows)
    table = summarize_v2(rows)
    (run_dir / "summary.md").write_text(table + "\n\nspend this run: " + json.dumps(SPEND.as_dict()) + "\n")
    print(table)
    print("spend this run:", json.dumps(SPEND.as_dict()))


def summarize_v2(rows: list[dict[str, Any]]) -> str:
    out = [bench.summarize(rows), ""]
    strategies = sorted({r["strategy"] for r in rows})
    if not any("correct" in r for r in rows):
        for s in strategies:
            rs = [r for r in rows if r["strategy"] == s]
            aic = [r["answer_in_context"] for r in rs if r.get("answer_in_context") is not None]
            out.append(f"{s}: n={len(rs)} answer-in-context={sum(aic) / len(aic):.2f} ({len(aic)}) gold-hit={sum(r['gold_hit'] for r in rs if r['recall'] is not None) / max(1, len(aic)):.2f} ctx={sum(r['context_tokens'] for r in rs) / len(rs):.0f}")
        return "\n".join(out)
    out.append("| strategy | n | answer-in-context (phrase Qs) | web acc | enterprise acc | abstention acc | non-abs acc | image Qs acc |")
    out.append("|---|---|---|---|---|---|---|---|")
    for s in strategies:
        rs = [r for r in rows if r["strategy"] == s and "correct" in r]
        aic = [r["answer_in_context"] for r in rs if r.get("answer_in_context") is not None]
        m = lambda xs: (sum(xs) / len(xs)) if xs else float("nan")
        out.append(f"| {s} | {len(rs)} | {m(aic):.2f} ({len(aic)}) | {m([r['correct'] for r in rs if r['domain']=='web']):.2f} | {m([r['correct'] for r in rs if r['domain']=='enterprise']):.2f} | {m([r['correct'] for r in rs if r['category'].endswith('-abs')]):.2f} | {m([r['correct'] for r in rs if not r['category'].endswith('-abs')]):.2f} | {m([r['correct'] for r in rs if r.get('has_image')]):.2f} |")
    return "\n".join(out)


if __name__ == "__main__":
    ap = argparse.ArgumentParser()
    ap.add_argument("cmd", choices=["build", "run"])
    ap.add_argument("--strategies", default="A,B,C")
    ap.add_argument("--tag", default="lmev2")
    ap.add_argument("--n", type=int, default=0)
    ap.add_argument("--seed", type=int, default=0)
    ap.add_argument("--qids")
    ap.add_argument("--budget", type=int, default=16000)
    ap.add_argument("--dmax-budget", type=int, default=120000)
    ap.add_argument("--d-subset", help="file with question ids (one per line) that also get strategy D")
    ap.add_argument("--parallel", type=int, default=6)
    ap.add_argument("--no-pi", action="store_true")
    ap.add_argument("--thr", action="append")
    ap.add_argument("--beam", action="append")
    a = ap.parse_args()
    if a.d_subset:
        a.d_subset = set(Path(a.d_subset).read_text().split())
    asyncio.run(build(a) if a.cmd == "build" else run(a))
