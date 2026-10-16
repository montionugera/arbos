"""Offline analysis of the Jev decision log: per-level separation, threshold sweep, beam-width
effects, and export of a fine-tuning JSONL (state + question + label).

  python sweep.py analyze --run full1
  python sweep.py export --run full1 --out ../private/finetune/jev-memory-walk.jsonl
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path
from typing import Any

from common import PRIVATE, read_jsonl
from strategies import PHRASINGS


def analyze(rows: list[dict[str, Any]]) -> str:
    out = []
    levels = ["project", "thread", "episode", "leaf"]
    out.append("| level | decisions | needed | P(yes) needed mean | P(yes) not-needed mean | min P needed | max P not-needed | recall@thr | precision@thr | expanded |")
    out.append("|---|---|---|---|---|---|---|---|---|---|")
    for lv in levels:
        rs = [r for r in rows if r["level"] == lv]
        if not rs:
            continue
        need = [r for r in rs if r["needed"]]
        not_need = [r for r in rs if not r["needed"]]
        m = lambda xs: sum(x["p"] for x in xs) / len(xs) if xs else float("nan")
        thr = rs[0]["threshold"]
        tp = sum(1 for r in need if r["expanded"])
        fp = sum(1 for r in not_need if r["expanded"])
        rec = tp / len(need) if need else float("nan")
        prec = tp / (tp + fp) if (tp + fp) else float("nan")
        out.append(f"| {lv} | {len(rs)} | {len(need)} | {m(need):.2f} | {m(not_need):.2f} | {min((r['p'] for r in need), default=float('nan')):.2f} | {max((r['p'] for r in not_need), default=float('nan')):.2f} | {rec:.2f} (thr {thr}) | {prec:.2f} | {tp + fp} |")
    out.append("")
    out.append("Threshold sweep (recall of needed nodes / fraction of all nodes expanded), per level:")
    out.append("")
    out.append("| level | " + " | ".join(f"t={t}" for t in (0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8)) + " |")
    out.append("|---|" + "---|" * 8)
    for lv in levels:
        rs = [r for r in rows if r["level"] == lv]
        if not rs:
            continue
        need = [r for r in rs if r["needed"]]
        cells = []
        for t in (0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8):
            rec = sum(1 for r in need if r["p"] >= t) / len(need) if need else float("nan")
            frac = sum(1 for r in rs if r["p"] >= t) / len(rs)
            cells.append(f"{rec:.2f} / {frac:.2f}")
        out.append(f"| {lv} | " + " | ".join(cells) + " |")
    # misses: needed nodes that were not expanded
    misses = [r for r in rows if r["needed"] and not r["expanded"]]
    out.append("")
    out.append(f"Needed nodes NOT expanded: {len(misses)}")
    for r in misses[:25]:
        out.append(f"- {r['qid']} {r['level']} p={r['p']:.2f} (thr {r['threshold']}) — {r['heading'][:110]}  ⟵ query: {r['query'][:70]}")
    return "\n".join(out)


def export(rows: list[dict[str, Any]], out: Path) -> int:
    """Fine-tuning rows: one per decision. Input = (query, context card); label = needed."""
    ph = PHRASINGS["v1"]
    out.parent.mkdir(parents=True, exist_ok=True)
    n = 0
    with open(out, "w") as f:
        for r in rows:
            if "card" not in r:
                continue
            row = {
                "state": {"query": {"text": r["query"], "asked_at": r.get("asked_at")}, "candidate": r["card"]},
                "question": {"type": "noul", "instructions": ph["instructions"].format(cid="candidate"), "criteria": {"true": ph["true"], "false": ph["false"]}},
                "label": bool(r["needed"]),
                "jev_p": r["p"],
                "level": r["level"],
                "qid": r["qid"],
                "tag": r.get("tag"),
                "node_id": r["node_id"],
            }
            f.write(json.dumps(row, ensure_ascii=False) + "\n")
            n += 1
    return n


if __name__ == "__main__":
    ap = argparse.ArgumentParser()
    ap.add_argument("cmd", choices=["analyze", "export"])
    ap.add_argument("--run", required=True)
    ap.add_argument("--out")
    a = ap.parse_args()
    rows = read_jsonl(PRIVATE / "results" / a.run / "decisions.jsonl")
    if a.cmd == "analyze":
        print(analyze(rows))
    else:
        n = export(rows, Path(a.out) if a.out else PRIVATE / "finetune" / f"{a.run}.jsonl")
        print(n, "rows")
