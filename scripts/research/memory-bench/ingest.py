"""Turn exported agentd stores (t3 tables) into one leaves.jsonl + threads.json.

Input layout (private, never committed):
  $MEMBENCH_PRIVATE/boxes/<user_id>/<project_id>/.arbos/agentd/agentd.db
  $MEMBENCH_PRIVATE/relay_meta.json   {people_chats: [...], projects: [...]}
  $MEMBENCH_PRIVATE/users.json        {user_id: handle}
Output:
  $MEMBENCH_PRIVATE/leaves.jsonl      one raw event per line, time-ordered
  $MEMBENCH_PRIVATE/threads.json      thread metadata (kind, project, title, members, parent chat)
"""
from __future__ import annotations

import json
import sqlite3
from pathlib import Path

from common import PRIVATE, ntok, write_jsonl

SKIP_KINDS = {"route"}  # internal routing decision; duplicates the assistant text


def main() -> None:
    meta = json.loads((PRIVATE / "relay_meta.json").read_text())
    users = json.loads((PRIVATE / "users.json").read_text())
    projects = {p["id"]: p for p in meta["projects"]}
    people = {c["id"]: c for c in meta["people_chats"]}

    leaves: list[dict] = []
    threads: dict[str, dict] = {}
    for db_path in sorted((PRIVATE / "boxes").glob("*/*/.arbos/agentd/agentd.db")):
        user_id = db_path.parts[-5]
        project_id = db_path.parts[-4]
        pname = projects.get(project_id, {}).get("name", project_id)
        con = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
        con.row_factory = sqlite3.Row
        chats = {r["id"]: dict(r) for r in con.execute("select * from chats")}
        subs = {r["id"]: dict(r) for r in con.execute("select * from subagents")}
        for cid, c in chats.items():
            threads[cid] = {
                "id": cid,
                "kind": "project_chat",
                "project_id": project_id,
                "project_name": pname,
                "owner": user_id,
                "title": c["title"],
                "created_at": c["created_at"],
            }
        for sid, s in subs.items():
            threads[sid] = {
                "id": sid,
                "kind": "subagent",
                "project_id": project_id,
                "project_name": pname,
                "owner": user_id,
                "title": f"subagent {s['name']}: {s['purpose']}",
                "parent_chat": s.get("chat_id"),
                "harness": s.get("harness"),
                "created_at": s["created_at"],
            }
        for r in con.execute("select * from t3 order by at, id"):
            if r["kind"] in SKIP_KINDS:
                continue
            tid = r["thread_id"]
            if tid not in threads:
                pc = people.get(tid)
                if pc:
                    members = sorted(
                        {v for m in pc["members"] for v in m.values() if ":" in str(v)}
                    )
                    humans = [users.get(m.split(":", 1)[1], m) for m in members if m.startswith("user:")]
                    threads[tid] = {
                        "id": tid,
                        "kind": f"people_{pc['kind']}",
                        "project_id": project_id,
                        "project_name": pname,
                        "owner": user_id,
                        "title": pc.get("title") or f"{pc['kind']} with {', '.join('@' + h for h in humans)}",
                        "members": members,
                        "human_handles": humans,
                        "created_at": pc["created_at"],
                    }
                else:
                    threads[tid] = {
                        "id": tid,
                        "kind": "unknown",
                        "project_id": project_id,
                        "project_name": pname,
                        "owner": user_id,
                        "title": tid,
                        "created_at": r["at"],
                    }
            src = json.loads(r["source"]) if r["source"] else {}
            text = r["text"]
            leaves.append(
                {
                    "id": r["id"],
                    "user": user_id,
                    "project_id": project_id,
                    "project_name": pname,
                    "thread_id": tid,
                    "thread_kind": threads[tid]["kind"],
                    "subagent_id": r["subagent_id"],
                    "session_id": r["session_id"],
                    "kind": r["kind"],
                    "role": r["role"],
                    "at": r["at"],
                    "text": text,
                    "source": src,
                    "tokens": ntok(text),
                }
            )
        con.close()
    leaves.sort(key=lambda x: (x["at"], x["id"]))
    write_jsonl(PRIVATE / "leaves.jsonl", leaves)
    (PRIVATE / "threads.json").write_text(json.dumps(threads, indent=1, ensure_ascii=False))
    by_kind: dict[str, int] = {}
    for leaf in leaves:
        by_kind[leaf["thread_kind"]] = by_kind.get(leaf["thread_kind"], 0) + 1
    print(f"{len(leaves)} leaves, {len(threads)} threads, {sum(x['tokens'] for x in leaves)} tokens")
    print("by thread kind:", by_kind)


if __name__ == "__main__":
    main()
