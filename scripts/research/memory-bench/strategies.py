"""Context strategies.

A  recent   : last N leaves of the current thread only (today's behaviour for a fresh session: nothing).
B  bm25     : BM25 over raw leaves (scoped, as-of), top-k packed under their headings.
C  tree     : Jev top-down beam walk over the tree; relevant leaves verbatim under headings,
              summary fallback over budget; recent window always; unsummarized leaves scanned directly.
D  full     : every scoped leaf verbatim (upper bound; only feasible for small histories).

Every strategy returns a Pack: the text to put in front of the model, the leaf ids it contains
(verbatim), the node ids whose summaries it contains, and the decision log of the walk (C only).
"""
from __future__ import annotations

import asyncio
import pickle
import re
import time
from pathlib import Path
from dataclasses import dataclass, field
from typing import Any

from rank_bm25 import BM25Okapi

from common import OpenRouter, fmt_ts, ntok
from tree import leaf_line

RECENT_N = 12
DEFAULT_BUDGET = 6000
LEAF_CLIP_FOR_JEV = 420


@dataclass
class Query:
    text: str
    asked_at: int  # ms epoch; only leaves strictly before this are visible
    thread_id: str | None  # the chat the question is asked in (None = brand-new chat)
    project_id: str
    scope: str = "project"  # project | group | user
    asker: str = "the user"
    thread_kind: str = "project_chat"
    where: str = ""


@dataclass
class Pack:
    text: str
    leaf_ids: list[str] = field(default_factory=list)
    summary_node_ids: list[str] = field(default_factory=list)
    recent_leaf_ids: list[str] = field(default_factory=list)
    tokens: int = 0
    decisions: list[dict[str, Any]] = field(default_factory=list)
    jev_calls: int = 0
    jev_cost: float = 0.0
    jev_latency_ms: int = 0
    wall_ms: int = 0
    notes: dict[str, Any] = field(default_factory=dict)


class History:
    """Leaves + tree + threads with scope/as-of filtering."""

    def __init__(self, leaves: list[dict[str, Any]], tree: dict[str, Any], threads: dict[str, dict[str, Any]]):
        self.leaves = leaves
        self.leaf_by_id = {x["id"]: x for x in leaves}
        self.tree = tree
        self.nodes = tree["nodes"]
        self.threads = threads
        self.leaf_episode: dict[str, str] = {}
        for n in self.nodes.values():
            if n["level"] == "episode":
                for c in n["children"]:
                    self.leaf_episode[c] = n["id"]
        self._bm25: BM25Okapi | None = None
        self._bm25_ids: list[str] = []

    # ---- scope ---------------------------------------------------------------------------
    def allowed_threads(self, q: Query) -> set[str]:
        if q.scope == "group":
            allowed = {q.thread_id} if q.thread_id else set()
            # subagents spawned from inside the group belong to it
            for tid, th in self.threads.items():
                if th.get("parent_chat") == q.thread_id:
                    allowed.add(tid)
            return allowed
        if q.scope == "user":
            return set(self.threads)
        return {tid for tid, th in self.threads.items() if th.get("project_id") == q.project_id}

    def visible(self, q: Query) -> list[dict[str, Any]]:
        allowed = self.allowed_threads(q)
        return [x for x in self.leaves if x["thread_id"] in allowed and x["at"] < q.asked_at]

    def recent(self, q: Query) -> list[dict[str, Any]]:
        if not q.thread_id:
            return []
        same = [x for x in self.leaves if x["thread_id"] == q.thread_id and x["at"] < q.asked_at]
        return same[-RECENT_N:]

    # ---- BM25 ----------------------------------------------------------------------------
    @staticmethod
    def tokenize(s: str) -> list[str]:
        return re.findall(r"[a-z0-9@_\-\.']+", s.lower())

    bm25_cache: Path | None = None

    def bm25(self) -> tuple[BM25Okapi, list[str]]:
        if self._bm25 is None:
            if self.bm25_cache and self.bm25_cache.exists():
                with open(self.bm25_cache, "rb") as f:
                    self._bm25, self._bm25_ids = pickle.load(f)
                return self._bm25, self._bm25_ids
            self._bm25_ids = [x["id"] for x in self.leaves]
            self._bm25 = BM25Okapi([self.tokenize(x.get("full_text") or x["text"]) for x in self.leaves])
            if self.bm25_cache:
                with open(self.bm25_cache, "wb") as f:
                    pickle.dump((self._bm25, self._bm25_ids), f)
        return self._bm25, self._bm25_ids

    def bm25_search(self, q: Query, query_text: str, k: int) -> list[tuple[str, float]]:
        bm, ids = self.bm25()
        scores = bm.get_scores(self.tokenize(query_text))
        allowed = self.allowed_threads(q)
        ranked = sorted(range(len(ids)), key=lambda i: -scores[i])
        out = []
        for i in ranked:
            leaf = self.leaf_by_id[ids[i]]
            if leaf["thread_id"] in allowed and leaf["at"] < q.asked_at and scores[i] > 0:
                out.append((ids[i], float(scores[i])))
            if len(out) >= k:
                break
        return out

    # ---- rendering -----------------------------------------------------------------------
    def thread_label(self, tid: str) -> str:
        th = self.threads.get(tid, {})
        kind = th.get("kind", "thread")
        label = f"{th.get('project_name', '')} › {th.get('title', tid)} [{kind}"
        if th.get("human_handles"):
            label += " with " + ", ".join("@" + h for h in th["human_handles"])
        return label + "]"

    def render(self, leaf_ids: list[str], summary_nodes: list[str], budget: int, recent_ids: list[str]) -> tuple[str, list[str], list[str]]:
        """Group leaves by thread/episode, time order, headings on top. Drop whole episodes
        to their summary when over budget (lowest-priority first = oldest)."""
        chosen = [self.leaf_by_id[i] for i in leaf_ids if i in self.leaf_by_id]
        chosen.sort(key=lambda x: (x["at"], x["id"]))
        groups: dict[str, list[dict[str, Any]]] = {}
        for leaf in chosen:
            groups.setdefault(self.leaf_episode.get(leaf["id"], f"ep:{leaf['thread_id']}:?"), []).append(leaf)
        blocks: list[tuple[str, str, int, list[str]]] = []  # (episode_id, text, tokens, leaf ids)
        for eid, leaves in groups.items():
            ep = self.nodes.get(eid, {})
            tid = leaves[0]["thread_id"]
            head = f"## {self.thread_label(tid)}\n### {ep.get('heading') or 'recent, not yet summarized'} ({fmt_ts(leaves[0]['at'])} – {fmt_ts(leaves[-1]['at'])})\n"
            body = "\n".join(leaf_line(x) for x in leaves)
            text = head + body + "\n"
            blocks.append((eid, text, ntok(text), [x["id"] for x in leaves]))
        total = sum(b[2] for b in blocks)
        dropped: list[str] = []
        kept_ids: list[str] = []
        out_blocks: list[str] = []
        # oldest first; the recent window is protected
        for eid, text, tok, ids in blocks:
            protected = any(i in recent_ids for i in ids)
            if total > budget and not protected and eid in self.nodes:
                ep = self.nodes[eid]
                stub = f"## {self.thread_label(ep['thread_id'])}\n### {ep['heading']} ({fmt_ts(ep['t0'])} – {fmt_ts(ep['t1'])})\n(summary) {ep['summary']}\n"
                total -= tok - ntok(stub)
                out_blocks.append(stub)
                dropped.append(eid)
            else:
                out_blocks.append(text)
                kept_ids.extend(ids)
        summaries = []
        for nid in summary_nodes:
            n = self.nodes[nid]
            if nid in dropped:
                continue
            tid = n.get("thread_id")
            label = self.thread_label(tid) if tid else n["heading"]
            summaries.append(f"## {label}\n### {n['heading']} ({fmt_ts(n['t0'])} – {fmt_ts(n['t1'])})\n(summary) {n['summary']}\n")
        text = "\n".join(summaries + out_blocks)
        return text, kept_ids, dropped + summary_nodes


# ---- strategies ----------------------------------------------------------------------------
def pack_recent(h: History, q: Query) -> Pack:
    t0 = time.time()
    rec = h.recent(q)
    ids = [x["id"] for x in rec]
    text, kept, _ = h.render(ids, [], 10**9, ids) if ids else ("", [], [])
    return Pack(text=text, leaf_ids=kept, recent_leaf_ids=ids, tokens=ntok(text), wall_ms=int((time.time() - t0) * 1000))


def pack_full(h: History, q: Query, budget: int = 10**9) -> Pack:
    t0 = time.time()
    vis = h.visible(q)
    rec = [x["id"] for x in h.recent(q)]
    ids = [x["id"] for x in vis]
    text, kept, summ = h.render(ids, [], budget, rec)
    return Pack(text=text, leaf_ids=kept, summary_node_ids=summ, recent_leaf_ids=rec, tokens=ntok(text), wall_ms=int((time.time() - t0) * 1000))


def pack_bm25(h: History, q: Query, k: int = 30, budget: int = DEFAULT_BUDGET) -> Pack:
    t0 = time.time()
    hits = h.bm25_search(q, q.text, k)
    rec = [x["id"] for x in h.recent(q)]
    ids = list(dict.fromkeys([i for i, _ in hits] + rec))
    text, kept, summ = h.render(ids, [], budget, rec)
    return Pack(text=text, leaf_ids=kept, summary_node_ids=summ, recent_leaf_ids=rec, tokens=ntok(text), wall_ms=int((time.time() - t0) * 1000), notes={"bm25_hits": hits})


@dataclass
class WalkParams:
    thresholds: dict[str, float] = field(default_factory=lambda: {"project": 0.15, "thread": 0.3, "episode": 0.4, "leaf": 0.4})
    beam: dict[str, int] = field(default_factory=lambda: {"project": 4, "thread": 8, "episode": 12, "leaf": 60})
    min_keep: dict[str, int] = field(default_factory=lambda: {"project": 1, "thread": 2, "episode": 2, "leaf": 0})
    budget: int = DEFAULT_BUDGET
    phrasing: str = "v1"
    leaf_select: str = "jev"  # jev | all
    chunk: int = 24
    leaf_clip: int = LEAF_CLIP_FOR_JEV
    bm25_extra: int = 0  # keyword hits added as extra leaf-level candidates for Jev


PHRASINGS = {
    "v1": {
        "instructions": "Could `candidates.{cid}` contain information needed to answer `query`? Judge this candidate on its own, from its heading, summary, time and entities. When in doubt, say yes: missing a needed record is worse than reading an extra one.",
        "true": "The candidate covers the same subject, person, file, project, decision or event as the query, or an episode where the answer was likely said; or the query asks about a time span the candidate falls in.",
        "false": "The candidate is about an unrelated subject and nothing in it would help answer the query.",
    },
    "v2": {
        "instructions": "Is `candidates.{cid}` relevant to `query`?",
        "true": "Relevant.",
        "false": "Not relevant.",
    },
    "v3": {
        "instructions": "An agent must answer `query` for the user. Should it open `candidates.{cid}` (a branch of its own memory) to find the answer? Judge from the heading, summary, time and entities only.",
        "true": "Opening it would likely reveal the fact, event, decision, file or result the query asks for, or context that changes the answer (for example a later correction).",
        "false": "It is about something else; the query would be answered just as well without it.",
    },
}


class TreeWalker:
    def __init__(self, h: History, or_: OpenRouter, params: WalkParams):
        self.h = h
        self.or_ = or_
        self.p = params

    def card(self, nid: str) -> dict[str, Any]:
        n = self.h.nodes[nid]
        if n["level"] == "leaf":
            leaf = self.h.leaf_by_id[nid]
            return {"kind": f"raw {leaf['kind']} event", "when": fmt_ts(leaf["at"]), "text": leaf["text"][: self.p.leaf_clip]}
        c: dict[str, Any] = {"kind": n["level"] if n["level"] != "thread" else n.get("thread_kind", "thread"), "heading": n["heading"], "summary": n["summary"], "when": f"{fmt_ts(n['t0'])} – {fmt_ts(n['t1'])}"}
        if n.get("entities"):
            c["entities"] = n["entities"]
        if n["level"] == "thread" and n.get("members"):
            c["people"] = ["@" + m for m in n["members"]]
        return c

    async def score(self, q: Query, level: str, nids: list[str], recent_text: str) -> dict[str, float]:
        """One Decisions request per chunk of candidates; all questions answered in parallel."""
        out: dict[str, float] = {}
        ph = PHRASINGS[self.p.phrasing]
        chunks = [nids[i : i + self.p.chunk] for i in range(0, len(nids), self.p.chunk)]

        async def one(chunk: list[str]) -> None:
            cands = {f"c{i}": self.card(nid) for i, nid in enumerate(chunk)}
            state = {
                "query": {"text": q.text, "asked_at": fmt_ts(q.asked_at), "asked_in": q.where, "asked_by": q.asker, "recent_messages_in_this_chat": recent_text},
                "candidates": cands,
            }
            questions = {
                cid: {
                    "type": "noul",
                    "instructions": ph["instructions"].format(cid=cid),
                    "criteria": {"true": ph["true"], "false": ph["false"]},
                }
                for cid in cands
            }
            probs, meta = await self.or_.decide(state, questions)
            self._calls += 1
            self._cost += float(meta["usage"].get("cost", 0.0))
            self._lat.append(meta["latency_ms"])
            for i, nid in enumerate(chunk):
                out[nid] = probs.get(f"c{i}", 0.0)

        await asyncio.gather(*(one(c) for c in chunks))
        return out

    async def walk(self, q: Query, gold: set[str] | None = None) -> Pack:
        t0 = time.time()
        self._calls, self._cost, self._lat = 0, 0.0, []
        allowed = self.h.allowed_threads(q)
        rec = self.h.recent(q)
        rec_ids = [x["id"] for x in rec]
        recent_text = "\n".join(leaf_line(x, clip=200) for x in rec[-4:]) or "(new chat, no messages yet)"
        decisions: list[dict[str, Any]] = []
        gold = gold or set()

        def subtree_has_gold(nid: str) -> bool:
            if not gold:
                return False
            stack = [nid]
            seen = set()
            while stack:
                x = stack.pop()
                if x in seen:
                    continue
                seen.add(x)
                if x in gold:
                    return True
                stack.extend(self.h.nodes[x]["children"])
            return False

        def node_visible(nid: str) -> bool:
            n = self.h.nodes[nid]
            if n["level"] == "leaf":
                return n["thread_id"] in allowed and n["at"] < q.asked_at
            if n["level"] in ("thread", "episode"):
                return n["thread_id"] in allowed and n["t0"] < q.asked_at
            if n["level"] == "project":
                return any(self.h.threads.get(t, {}).get("project_id") == n["project_id"] for t in allowed)
            return True

        # frontier: start at the children of the root; group scope starts at the thread itself
        if q.scope == "group" and q.thread_id:
            frontier = [f"thread:{q.thread_id}"]
            level_order = ["thread", "episode", "leaf"]
        else:
            frontier = [c for c in self.h.nodes["user"]["children"] if node_visible(c)]
            level_order = ["project", "thread", "episode", "leaf"]
        chosen_leaves: list[tuple[str, float]] = []
        summary_fallback: list[str] = []
        direct_leaves: list[str] = []
        for level in level_order:
            if not frontier:
                break
            # Episodes that straddle the as-of time, or that were never summarized, are scanned
            # at leaf level directly instead of trusting a card.
            if level == "episode":
                stable, direct = [], []
                for nid in frontier:
                    n = self.h.nodes[nid]
                    if n.get("unsummarized") or n["t1"] >= q.asked_at or not n.get("heading"):
                        direct.extend(c for c in n["children"] if node_visible(c))
                    else:
                        stable.append(nid)
                frontier = stable
                direct_leaves = direct
            if level == "leaf" and self.p.leaf_select == "all":
                chosen_leaves.extend((nid, 1.0) for nid in frontier if node_visible(nid))
                break
            cands = [nid for nid in frontier if node_visible(nid)]
            extra: set[str] = set()
            if level == "leaf":
                cands = list(dict.fromkeys(cands + direct_leaves))
                if self.p.bm25_extra:
                    hits = [lid for lid, _ in self.h.bm25_search(q, q.text, self.p.bm25_extra)]
                    extra = {lid for lid in hits if lid not in cands}
                    cands = list(dict.fromkeys(cands + hits))
            if not cands:
                break
            probs = await self.score(q, level, cands, recent_text)
            thr = self.p.thresholds[level]
            ranked = sorted(cands, key=lambda nid: -probs[nid])
            keep = [nid for nid in ranked if probs[nid] >= thr][: self.p.beam[level]]
            for nid in ranked[: self.p.min_keep[level]]:
                if nid not in keep:
                    keep.append(nid)
            # the chat the question is asked in is always expanded
            if level == "thread" and q.thread_id and f"thread:{q.thread_id}" in cands and f"thread:{q.thread_id}" not in keep:
                keep.append(f"thread:{q.thread_id}")
            for nid in cands:
                decisions.append(
                    {
                        "level": level,
                        "node_id": nid,
                        "heading": self.h.nodes[nid]["heading"][:160],
                        "p": round(probs[nid], 4),
                        "threshold": thr,
                        "expanded": nid in keep,
                        "needed": subtree_has_gold(nid),
                        "phrasing": self.p.phrasing,
                        "card": self.card(nid),
                        "n_siblings": len(cands),
                        "source": "bm25" if nid in extra else "tree",
                    }
                )
            if level == "leaf":
                chosen_leaves = [(nid, probs[nid]) for nid in keep]
                # an episode that was judged relevant but yielded no leaf keeps its summary
                for eid in {self.h.leaf_episode.get(nid) for nid in cands} - {self.h.leaf_episode.get(nid) for nid in keep}:
                    if eid and eid in self.h.nodes and self.h.nodes[eid].get("heading"):
                        summary_fallback.append(eid)
            else:
                nxt: list[str] = []
                for nid in keep:
                    nxt.extend(self.h.nodes[nid]["children"])
                frontier = nxt
        leaf_ids = list(dict.fromkeys([nid for nid, _ in chosen_leaves] + rec_ids))
        text, kept, summ = self.h.render(leaf_ids, summary_fallback, self.p.budget, rec_ids)
        return Pack(
            text=text,
            leaf_ids=kept,
            summary_node_ids=summ,
            recent_leaf_ids=rec_ids,
            tokens=ntok(text),
            decisions=decisions,
            jev_calls=self._calls,
            jev_cost=self._cost,
            jev_latency_ms=max(self._lat) if self._lat else 0,
            wall_ms=int((time.time() - t0) * 1000),
            notes={"jev_latencies_ms": self._lat, "direct_leaves": len(direct_leaves)},
        )
