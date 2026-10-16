"""Run one question through pi in RPC mode (the same harness the boxes run)."""
from __future__ import annotations

import json
import os
import shutil
import subprocess
import tempfile
import time
from pathlib import Path
from typing import Any

from common import ANSWER_MODEL, PRIVATE, SPEND

HARNESS_DIR = Path(__file__).resolve().parent
PI_BIN = shutil.which("pi") or str(Path.home() / "membench" / "pi" / "node_modules" / ".bin" / "pi")
PYTHON = shutil.which("python")


def run_pi(system_prompt: str, question: str, *, tools: bool, scope: dict[str, Any] | None = None, model: str = ANSWER_MODEL, timeout_s: int = 180, env_extra: dict[str, str] | None = None) -> dict[str, Any]:
    home = Path(tempfile.mkdtemp(prefix="pihome-"))
    sp = home / "system.md"
    sp.write_text(system_prompt)
    tool_log = home / "tools.jsonl"
    env = dict(os.environ, HOME=str(home), MEMBENCH_TOOL_LOG=str(tool_log), MEMBENCH_PRIVATE=str(PRIVATE), PI_CODING_AGENT_DIR=str(home / ".pi" / "agent"))
    if env_extra:
        env.update(env_extra)
    args = [PI_BIN, "--mode", "rpc", "--no-session", "--provider", "openrouter", "--model", model, "--thinking", "off", "--append-system-prompt", str(sp)]
    if tools and scope:
        scope_path = home / "scope.json"
        scope_path.write_text(json.dumps(scope))
        env["MEMBENCH_SCOPE"] = str(scope_path)
        cfg = home / ".pi" / "agent"
        cfg.mkdir(parents=True)
        (cfg / "mcp.json").write_text(
            json.dumps(
                {
                    "mcpServers": {
                        "ctx": {
                            "command": PYTHON,
                            "args": [str(HARNESS_DIR / "ctx_mcp.py")],
                            "env": {k: env[k] for k in ("MEMBENCH_SCOPE", "MEMBENCH_TOOL_LOG", "MEMBENCH_PRIVATE", "MEMBENCH_TREE", "MEMBENCH_LEAVES", "MEMBENCH_THREADS") if k in env},
                            "cwd": str(HARNESS_DIR),
                            "exposure": "direct",
                            "description": "Search and read this agent's own full conversation history.",
                        }
                    }
                }
            )
        )
        args += ["--tools", "mcp__ctx__*"]
    else:
        args += ["--no-tools", "--no-mcp"]
    t0 = time.time()
    p = subprocess.Popen(args, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=env, cwd=str(home))
    assert p.stdin and p.stdout
    p.stdin.write((json.dumps({"id": "q", "type": "prompt", "message": question}) + "\n").encode())
    p.stdin.flush()
    text_parts: list[str] = []
    usage_total = {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "cost": 0.0}
    tool_calls = 0
    first_token_ms = None
    deadline = t0 + timeout_s
    error = None
    while time.time() < deadline:
        line = p.stdout.readline()
        if not line:
            break
        try:
            r = json.loads(line)
        except json.JSONDecodeError:
            continue
        t = r.get("type")
        if t == "message_update":
            ev = r.get("assistantMessageEvent", {})
            if ev.get("type") == "text_delta":
                if first_token_ms is None:
                    first_token_ms = int((time.time() - t0) * 1000)
        elif t == "message_end":
            m = r.get("message", {})
            if m.get("role") == "assistant":
                for c in m.get("content", []):
                    if c.get("type") == "text":
                        text_parts.append(c["text"])
                    elif c.get("type") == "toolCall":
                        tool_calls += 1
                u = m.get("usage") or {}
                for k in ("input", "output", "cacheRead", "cacheWrite"):
                    usage_total[k] += int(u.get(k, 0) or 0)
                usage_total["cost"] += float((u.get("cost") or {}).get("total", 0.0) or 0.0)
                if m.get("errorMessage"):
                    error = m["errorMessage"]
        elif t == "agent_settled":
            break
    try:
        p.stdin.close()
        p.wait(timeout=15)
    except Exception:
        p.kill()
    stderr = p.stderr.read().decode(errors="replace")[-800:] if p.stderr else ""
    wall = int((time.time() - t0) * 1000)
    SPEND.add("answer", usage_total["cost"], usage_total["input"] + usage_total["cacheRead"] + usage_total["cacheWrite"], usage_total["output"])
    tools_used = []
    if tool_log.exists():
        tools_used = [json.loads(x) for x in tool_log.read_text().splitlines() if x.strip()]
    answer = "\n".join(x for x in text_parts if x.strip()).strip()
    shutil.rmtree(home, ignore_errors=True)
    return {"answer": answer, "usage": usage_total, "tool_calls": tool_calls, "tools_used": tools_used, "wall_ms": wall, "ttft_ms": first_token_ms, "error": error, "stderr": stderr if error or not answer else ""}
