"""MCP stdio server exposing the three pull tools of strategy C, scoped per question.

Scope comes from the JSON file named by $MEMBENCH_SCOPE:
  {"allowed_threads": [...], "asked_at": <ms>, "project_id": "...", "thread_id": "..."}
Only leaves in allowed threads with at < asked_at are reachable; nodes outside are invisible.
"""
from __future__ import annotations

import json
import os
from pathlib import Path

from mcp.server.fastmcp import FastMCP

from common import PRIVATE, fmt_ts, read_jsonl
from strategies import History
from tree import leaf_line, load_tree

SCOPE = json.loads(Path(os.environ["MEMBENCH_SCOPE"]).read_text())
TREE_PATH = Path(os.environ.get("MEMBENCH_TREE", PRIVATE / "tree.json"))
LEAVES_PATH = Path(os.environ.get("MEMBENCH_LEAVES", PRIVATE / "leaves.jsonl"))
THREADS_PATH = Path(os.environ.get("MEMBENCH_THREADS", PRIVATE / "threads.json"))

H = History(read_jsonl(LEAVES_PATH), load_tree(TREE_PATH), json.loads(THREADS_PATH.read_text()))
ALLOWED = set(SCOPE["allowed_threads"])
ASKED_AT = int(SCOPE["asked_at"])
CALLS_LOG = os.environ.get("MEMBENCH_TOOL_LOG")

mcp = FastMCP("ctx", instructions="Search and read this agent's own conversation history (every past message, tool call and result).")


def _log(tool: str, args: dict, n: int) -> None:
    if CALLS_LOG:
        with open(CALLS_LOG, "a") as f:
            f.write(json.dumps({"tool": tool, "args": args, "results": n}) + "\n")


def _vis_leaf(lid: str) -> bool:
    leaf = H.leaf_by_id.get(lid)
    return bool(leaf) and leaf["thread_id"] in ALLOWED and leaf["at"] < ASKED_AT


def _vis_node(nid: str) -> bool:
    n = H.nodes.get(nid)
    if not n:
        return False
    if n["level"] == "leaf":
        return _vis_leaf(nid)
    if n["level"] in ("thread", "episode"):
        return n["thread_id"] in ALLOWED and n["t0"] < ASKED_AT
    if n["level"] == "project":
        return any(H.threads.get(t, {}).get("project_id") == n["project_id"] for t in ALLOWED)
    return True


def _card(nid: str) -> dict:
    n = H.nodes[nid]
    if n["level"] == "leaf":
        leaf = H.leaf_by_id[nid]
        return {"id": nid, "kind": f"leaf/{leaf['kind']}", "when": fmt_ts(leaf["at"]), "text": leaf["text"][:300]}
    return {"id": nid, "kind": n["level"] if n["level"] != "thread" else n.get("thread_kind"), "when": f"{fmt_ts(n['t0'])} – {fmt_ts(n['t1'])}", "heading": n["heading"], "summary": n["summary"], "children": len([c for c in n["children"] if _vis_node(c)])}


@mcp.tool()
def context_search(query: str, limit: int = 10) -> str:
    """Keyword search over the whole visible history: raw events and index cards (episodes, threads). Returns ids you can pass to context_open or context_around."""
    limit = max(1, min(limit, 25))
    hits = []
    bm, ids = H.bm25()
    scores = bm.get_scores(H.tokenize(query))
    for i in sorted(range(len(ids)), key=lambda i: -scores[i]):
        if scores[i] <= 0:
            break
        if _vis_leaf(ids[i]):
            hits.append({"score": round(float(scores[i]), 2), **_card(ids[i])})
        if len(hits) >= limit:
            break
    qt = set(H.tokenize(query))
    cards = []
    for nid, n in H.nodes.items():
        if n["level"] in ("episode", "thread") and _vis_node(nid):
            words = set(H.tokenize(n["heading"] + " " + n["summary"] + " " + " ".join(n.get("entities", []))))
            ov = len(qt & words)
            if ov:
                cards.append((ov, nid))
    cards.sort(reverse=True)
    out = {"events": hits, "index_cards": [_card(nid) for _, nid in cards[: max(3, limit // 2)]]}
    _log("context_search", {"query": query, "limit": limit}, len(hits) + len(out["index_cards"]))
    return json.dumps(out, ensure_ascii=False)


@mcp.tool()
def context_open(node_id: str) -> str:
    """Open a node. For a thread or project: its child cards. For an episode: its raw events verbatim, in time order. For a leaf: the full event text."""
    if not _vis_node(node_id):
        _log("context_open", {"node_id": node_id}, 0)
        return json.dumps({"error": "not found or not visible from this chat"})
    n = H.nodes[node_id]
    if n["level"] == "leaf":
        leaf = H.leaf_by_id[node_id]
        full = leaf.get("full_text") or leaf["text"]
        out = {"id": node_id, "text": leaf_line({**leaf, "text": full[:12000] + (" …[truncated; page continues]" if len(full) > 12000 else "")})}
    elif n["level"] == "episode":
        kids = [c for c in n["children"] if _vis_leaf(c)]
        out = {"id": node_id, "heading": n["heading"], "thread": H.thread_label(n["thread_id"]), "events": [leaf_line(H.leaf_by_id[c]) for c in kids]}
    else:
        out = {"id": node_id, "heading": n["heading"], "summary": n["summary"], "children": [_card(c) for c in n["children"] if _vis_node(c)]}
    _log("context_open", {"node_id": node_id}, 1)
    return json.dumps(out, ensure_ascii=False)


@mcp.tool()
def context_around(leaf_id: str, before: int = 4, after: int = 4) -> str:
    """Show the raw events just before and after one event in the same thread."""
    if not _vis_leaf(leaf_id):
        _log("context_around", {"leaf_id": leaf_id}, 0)
        return json.dumps({"error": "not found or not visible from this chat"})
    leaf = H.leaf_by_id[leaf_id]
    same = [x for x in H.leaves if x["thread_id"] == leaf["thread_id"] and x["at"] < ASKED_AT]
    idx = next(i for i, x in enumerate(same) if x["id"] == leaf_id)
    window = same[max(0, idx - before) : idx + after + 1]
    _log("context_around", {"leaf_id": leaf_id}, len(window))
    return json.dumps({"thread": H.thread_label(leaf["thread_id"]), "events": [leaf_line(x) for x in window]}, ensure_ascii=False)


if __name__ == "__main__":
    mcp.run()
