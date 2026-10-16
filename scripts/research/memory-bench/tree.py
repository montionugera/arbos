"""Background builder: leaves -> episodes -> threads -> projects -> user (a DAG; a node may have
several parents). Each internal node gets a one-line heading, a 2-3 sentence summary, entities,
a time range and a token size. Summaries are produced by a cheap LLM and cached by content hash.

Usage:
  python tree.py build            # from $MEMBENCH_PRIVATE/leaves.jsonl -> tree.json
  python tree.py build --through <ms>   # only summarize leaves with at < ms (later leaves stay
                                        # "unsummarized" and are scanned directly at query time)
"""
from __future__ import annotations

import argparse
import asyncio
import json
import re
from pathlib import Path
from typing import Any

from common import PRIVATE, SUMMARY_MODEL, Cache, OpenRouter, fmt_ts, ntok, read_jsonl, sha

EPISODE_GAP_MS = 30 * 60 * 1000
EPISODE_MAX_TOKENS = 2500
EPISODE_MAX_LEAVES = 24
TOOL_CLIP = 700  # chars of a tool output shown to the summarizer

SUMMARY_SYSTEM = (
    "You write index cards for an agent's memory tree. Given raw events (or child cards), return JSON "
    'with keys: "heading" (ONE line, <= 110 chars: who did what, and the outcome, e.g. '
    '"User tells the agent their name; agent saves it to memory"), "summary" (2-3 plain sentences with '
    "the concrete facts, names, numbers, decisions, file paths and results that someone searching later "
    'would need), "entities" (list of <= 10 short strings: people, projects, tools, repos, topics). '
    "Never invent facts. Prefer specifics over generalities. Output JSON only."
)


def parse_card(text: str) -> dict[str, Any]:
    """Parse the summarizer's JSON; fall back to regex when the model wrapped or truncated it."""
    t = text.strip()
    if t.startswith("```"):
        t = t.strip("`")
        t = t[t.find("{") :] if "{" in t else t
    try:
        start, end = t.find("{"), t.rfind("}")
        card = json.loads(t[start : end + 1]) if start >= 0 and end > start else {}
    except json.JSONDecodeError:
        card = {}
    if not card.get("heading"):
        m = re.search(r'"heading"\s*:\s*"((?:[^"\\]|\\.)*)"', t)
        s = re.search(r'"summary"\s*:\s*"((?:[^"\\]|\\.)*)', t)
        card = {
            "heading": (m.group(1) if m else t.split("\n")[0]).replace('\\"', '"'),
            "summary": (s.group(1) if s else "").replace('\\"', '"'),
            "entities": [],
        }
    return card


def leaf_line(leaf: dict[str, Any], clip: int | None = None) -> str:
    who = leaf["role"] or leaf["kind"]
    text = leaf["text"]
    if clip and len(text) > clip:
        text = text[:clip] + f" …[{len(leaf['text']) - clip} more chars]"
    return f"[{fmt_ts(leaf['at'])}] {who} ({leaf['kind']}): {text}"


def split_episodes(leaves: list[dict[str, Any]]) -> list[list[dict[str, Any]]]:
    """Split one thread's time-ordered leaves into episodes at time gaps, size caps, or at a
    user message once the running episode is already substantial."""
    eps: list[list[dict[str, Any]]] = []
    cur: list[dict[str, Any]] = []
    cur_tok = 0
    for leaf in leaves:
        gap = leaf["at"] - cur[-1]["at"] if cur else 0
        new_turn = leaf["kind"] == "user" and cur_tok > EPISODE_MAX_TOKENS // 2 and len(cur) >= 6
        if cur and (
            gap > EPISODE_GAP_MS
            or cur_tok + leaf["tokens"] > EPISODE_MAX_TOKENS
            or len(cur) >= EPISODE_MAX_LEAVES
            or new_turn
        ):
            eps.append(cur)
            cur, cur_tok = [], 0
        cur.append(leaf)
        cur_tok += leaf["tokens"]
    if cur:
        eps.append(cur)
    return eps


class TreeBuilder:
    def __init__(self, leaves: list[dict[str, Any]], threads: dict[str, dict[str, Any]], through: int | None):
        self.leaves = leaves
        self.threads = threads
        self.through = through
        self.nodes: dict[str, dict[str, Any]] = {}
        self.cache = Cache(PRIVATE / "summary-cache.json")
        self.or_ = OpenRouter(concurrency=10)
        self.unsummarized: list[str] = []

    def add(self, node: dict[str, Any]) -> dict[str, Any]:
        node.setdefault("parents", [])
        node.setdefault("children", [])
        self.nodes[node["id"]] = node
        return node

    def link(self, parent: str, child: str) -> None:
        if child not in self.nodes[parent]["children"]:
            self.nodes[parent]["children"].append(child)
        if parent not in self.nodes[child]["parents"]:
            self.nodes[child]["parents"].append(parent)

    async def summarize(self, kind: str, body: str, hint: str) -> dict[str, Any]:
        key = sha(SUMMARY_MODEL + kind + hint + body)
        hit = self.cache.get(key)
        if hit:
            return hit
        user = f"Node kind: {kind}. Context: {hint}\n\nContent:\n{body}"
        text, meta = await self.or_.chat(
            [{"role": "system", "content": SUMMARY_SYSTEM}, {"role": "user", "content": user}],
            model=SUMMARY_MODEL,
            purpose="tree_summaries",
            max_tokens=900,
            json_mode=True,
        )
        card = parse_card(text)
        card = {
            "heading": str(card.get("heading", ""))[:160],
            "summary": str(card.get("summary", ""))[:900],
            "entities": [str(e)[:40] for e in (card.get("entities") or [])][:10],
        }
        self.cache.put(key, card)
        return card

    async def build(self) -> dict[str, Any]:
        by_thread: dict[str, list[dict[str, Any]]] = {}
        for leaf in self.leaves:
            by_thread.setdefault(leaf["thread_id"], []).append(leaf)
            self.add(
                {
                    "id": leaf["id"],
                    "level": "leaf",
                    "thread_id": leaf["thread_id"],
                    "project_id": leaf["project_id"],
                    "at": leaf["at"],
                    "t0": leaf["at"],
                    "t1": leaf["at"],
                    "tokens": leaf["tokens"],
                    "kind": leaf["kind"],
                    "role": leaf["role"],
                    "heading": leaf_line(leaf, clip=140),
                }
            )
        users = sorted({leaf["user"] for leaf in self.leaves})
        root = self.add({"id": "user", "level": "user", "heading": "Everything the user has done with their agents", "summary": "", "entities": [], "t0": 0, "t1": 0, "tokens": 0})
        projects: dict[str, dict[str, Any]] = {}
        for tid, tleaves in by_thread.items():
            th = self.threads.get(tid, {"kind": "unknown", "project_id": tleaves[0]["project_id"], "project_name": tleaves[0]["project_name"], "title": tid})
            pid = th["project_id"]
            if pid not in projects:
                projects[pid] = self.add({"id": f"project:{pid}", "level": "project", "project_id": pid, "heading": f"Project {th['project_name']}", "summary": "", "entities": [], "t0": 0, "t1": 0, "tokens": 0, "name": th["project_name"]})
                self.link("user", f"project:{pid}")
            tnode = self.add(
                {
                    "id": f"thread:{tid}",
                    "level": "thread",
                    "thread_id": tid,
                    "project_id": pid,
                    "thread_kind": th["kind"],
                    "title": th.get("title", tid),
                    "members": th.get("human_handles", []),
                    "heading": "",
                    "summary": "",
                    "entities": [],
                    "t0": tleaves[0]["at"],
                    "t1": tleaves[-1]["at"],
                    "tokens": sum(x["tokens"] for x in tleaves),
                }
            )
            # Subagent threads hang under the chat that spawned them; everything else under the project.
            parent_chat = th.get("parent_chat")
            if th["kind"] == "subagent" and parent_chat and parent_chat in by_thread:
                self.link(f"thread:{parent_chat}", tnode["id"])
            else:
                self.link(f"project:{pid}", tnode["id"])
            for i, ep in enumerate(split_episodes(tleaves)):
                enode = self.add(
                    {
                        "id": f"ep:{tid}:{i}",
                        "level": "episode",
                        "thread_id": tid,
                        "project_id": pid,
                        "heading": "",
                        "summary": "",
                        "entities": [],
                        "t0": ep[0]["at"],
                        "t1": ep[-1]["at"],
                        "tokens": sum(x["tokens"] for x in ep),
                    }
                )
                self.link(tnode["id"], enode["id"])
                for leaf in ep:
                    self.link(enode["id"], leaf["id"])
        # Episodes: summarize from raw leaves. Those past the build cut-off stay unsummarized.
        leaf_by_id = {leaf["id"]: leaf for leaf in self.leaves}
        tasks = []
        for n in self.nodes.values():
            if n["level"] != "episode":
                continue
            if self.through is not None and n["t1"] >= self.through:
                n["unsummarized"] = True
                self.unsummarized.append(n["id"])
                continue
            th = self.threads.get(n["thread_id"], {})
            hint = f"thread '{th.get('title', '')}' ({th.get('kind', '')}) in project {th.get('project_name', '')}"
            body = "\n".join(leaf_line(leaf_by_id[c], clip=TOOL_CLIP if leaf_by_id[c]["kind"] != "user" else None) for c in n["children"])
            tasks.append(self._fill(n, "episode", body, hint))
        await asyncio.gather(*tasks)
        # Threads from their episode cards; projects from thread cards; root from project cards.
        for level, child_level in (("thread", "episode"), ("project", "thread"), ("user", "project")):
            tasks = []
            for n in self.nodes.values():
                if n["level"] != level:
                    continue
                kids = [self.nodes[c] for c in n["children"] if self.nodes[c]["level"] in (child_level, "thread") and self.nodes[c].get("heading")]
                if level == "project":
                    kids = [k for k in kids if k["level"] == "thread" or k["level"] == child_level]
                if not kids:
                    continue
                n["t0"] = min(k["t0"] for k in kids if k["t0"]) if any(k["t0"] for k in kids) else 0
                n["t1"] = max(k["t1"] for k in kids)
                n["tokens"] = sum(k["tokens"] for k in kids)
                body = "\n".join(f"- [{fmt_ts(k['t0'])} – {fmt_ts(k['t1'])}] {k['heading']} — {k['summary']}" for k in kids)
                hint = n.get("title") or n.get("name") or n["id"]
                if level == "thread":
                    hint = f"{n['thread_kind']} '{n['title']}'" + (f" with {', '.join('@' + m for m in n['members'])}" if n.get("members") else "")
                tasks.append(self._fill(n, level, body, hint))
            await asyncio.gather(*tasks)
        return {
            "nodes": self.nodes,
            "root": "user",
            "built_through": self.through,
            "unsummarized_episodes": self.unsummarized,
            "summary_model": SUMMARY_MODEL,
        }

    async def _fill(self, n: dict[str, Any], kind: str, body: str, hint: str) -> None:
        card = await self.summarize(kind, body, hint)
        n.update(card)


def load_tree(path: Path | None = None) -> dict[str, Any]:
    return json.loads((path or PRIVATE / "tree.json").read_text())


async def amain(args: argparse.Namespace) -> None:
    leaves = read_jsonl(Path(args.leaves) if args.leaves else PRIVATE / "leaves.jsonl")
    threads = json.loads((Path(args.threads) if args.threads else PRIVATE / "threads.json").read_text())
    b = TreeBuilder(leaves, threads, args.through)
    tree = await b.build()
    out = Path(args.out) if args.out else PRIVATE / "tree.json"
    out.write_text(json.dumps(tree, ensure_ascii=False))
    levels: dict[str, int] = {}
    for n in tree["nodes"].values():
        levels[n["level"]] = levels.get(n["level"], 0) + 1
    from common import SPEND

    print("nodes by level:", levels, "unsummarized episodes:", len(tree["unsummarized_episodes"]))
    print("spend:", json.dumps(SPEND.as_dict()))


if __name__ == "__main__":
    ap = argparse.ArgumentParser()
    ap.add_argument("cmd", choices=["build"])
    ap.add_argument("--through", type=int, default=None, help="ms epoch; episodes ending at/after this stay unsummarized")
    ap.add_argument("--leaves")
    ap.add_argument("--threads")
    ap.add_argument("--out")
    asyncio.run(amain(ap.parse_args()))
