---
cursor:
  subagentId: "bc-fa3ecba8-af54-52c6-bfbd-2531863b9c4f"
---

# fx engy check, 2026-10-11

Independent check of `unarbos/arbos` commit `bf9dbe5b9c8cd67716cc415af5d738609d8278e2` ("Make engy the built-in fx provider."). Checked out in `/tmp/arbos-fx-check`. The GitHub repo was not edited, committed, or pushed. `/workspace` was not used.

Judged against section 5 of the rebuild design and the Phase 1 fx sentence: built-in engy, default model `glm-5.3`, `ENGY_API_KEY` from the environment, `reasoning_effort` passthrough, model metadata from `/v1/models`, `ARBOS_FX_HOME`, and `full-access` when the client does not set a mode. The binary stays named `fx`.

`fx` is the Zig coding agent. ACP (Agent Client Protocol) is JSON-RPC over the process's standard input and output. engy is the inference API at `https://api.engy.ai/v1`. `ARBOS_FX_HOME` is the environment variable that should replace the home directory fx uses for its profile. `reasoning_effort` is the request field that tells the model how hard to think. `full-access` is the permission mode that runs actions without asking. `context_window` is the token capacity fx uses to time compaction.

I did not read a credit balance.

## What passed

- The diff is 15 files under `fx/` (528 insertions, 51 deletions) plus 4 docs. The new behavior is `fx/src/builtins/engy.zig` (330 lines). The other 14 files are short wiring: default provider, default mode, home override, and the chat-completions field. `fx/build.zig.zon` only changes the fork fingerprint.
- The built binary is `fx/zig-out/bin/fx`. ACP `agentInfo.name` is `fx`, version `0.0.13`.
- Fresh `ARBOS_FX_HOME`, no saved settings, and no mode from the client. `fx acp` returned provider `engy`, model `glm-5.3`, and mode `full-access`. The client never sent `modeId` or `session/set_mode`.
- The assistant text was the single word `pong`. Stop reason `end_turn`. Standard error was empty. The prompt result reported 7066 input tokens and 21 output tokens. `usage_update` reported `size` 327680, the same number as the model's `context_window`.
- The first run wrote `$ARBOS_FX_HOME/.fx/engy-models.json` (8 models, no key). `glm-5.3` has `context_window` 327680 and `max_output_tokens` 32768. One smaller model has `max_output_tokens` 26024, which is its context divided by 8, so the live `/v1/models` body went through the mapper.
- The session directory, system prompt, and `events.jsonl` are under that `ARBOS_FX_HOME`. The key string is absent from those files and from the ACP transcript.
- The commit diff contains no API key. The only token-shaped strings added are the placeholder names already described in `docs/qa/2026-10-11-bootstrap.md`.
- The new unit tests are in the tree: engy default model, metadata mapping, env name with no secret, effort ladder `none` through `max`, `selectHome`, `selectProviderModel` defaults to engy `glm-5.3`, and chat-completions writes `reasoning_effort` for `none`, `low`, and `max` while omitting `auto`. None of those names are in the failure list below.
- Provider options on `session/new` still list Vercel AI Gateway, Codex, and Grok beside engy.
- CI run [38106778233](https://github.com/unarbos/arbos/actions/runs/38106778233) is green on this exact commit. Layout, compile app, and compile fx all succeeded.

## What broke

`zig build test -Doptimize=ReleaseSafe` failed: 10014 passed, 24 skipped, 18 failed, in 917 seconds.

Thirteen failures are in `fx ask` tests. The new default permission is full access (`yolo` inside fx). `fx ask` then prints `Full access enabled: fx permission checks disabled` and tries to save that acknowledgment. The fixtures have no `HOME`, so the save fails with `HomeNotSet` and the extra lines land on stderr. Examples: `fx ask renders one-off resume denial in text and JSON modes`, `json run with missing API key prints diagnostic then final object`, `fx ask raw output still propagates prompt failure`.

Five more failures are in files this commit does not change: one git-worktree status test (`expected .dirty, found .unknown`), three MCP startup tests (timeout versus early exit), and one skill-refresh test (`expected .adopted, found .unchanged`). I did not re-run those five alone.

Root CI never runs `zig build test`. The green run only compiles `fx` and runs `scripts/smoke-binary.sh` (`--version`, `help`, `status --json`).

## What is weaker than the design

- Section 5 says the engine fetches `/v1/models` and writes `model_metadata`. This commit does that fetch inside fx and stores `$ARBOS_FX_HOME/.fx/engy-models.json`. The numbers are right. The engine is not the writer.
- Section 5's key lives in the OS keychain and is placed in the child environment by the engine. fx reads `ENGY_API_KEY` and does not touch a keychain. That matches the Phase 1 fx sentence. The keychain part is still unbuilt.
- `ARBOS_FX_HOME` covers settings, sessions, and the model cache. Profile usage still calls `getenv("HOME")` in `fx/src/acp/sessions.zig` (`initializeProfileUsage`). This run appended one `incomplete` incident to `~/.fx/usage.jsonl`. I removed that line after recording it. The session's own `usage-v2.json` stayed at `request_count` 0 and `billing` `incomplete` while the ACP result carried the token counts.
- There is no `--home` flag. The design's profile sentence allows `ARBOS_FX_HOME` or `--home`.
- The profile directory is still `.fx` inside `ARBOS_FX_HOME`. Section 6 also renames that directory and the binary. The commit's decision file keeps the binary name `fx` on purpose. The directory rename is in the same unfinished branding bucket.
- Default effort is `auto`. The live turn therefore omitted `reasoning_effort`. The passthrough is the unit test, plus the effort picker listing `none`, `low`, `medium`, `high`, `xhigh`, and `max`. I did not send a non-auto effort to engy.
- GLM-5.3 still thought. Five `agent_thought_chunk` updates formed one sentence, and the session recorded `thinking_duration_ms` 1455. The visible reply stayed `pong`. The design says this model cannot turn thinking off. The author's note for the same prompt reported no thought chunks. This run did see them.
- The mapper sets `supports_tool_use` true for every model it keeps. It does not read a tool field from `/v1/models`.
- Wiring edits `app_lifecycle.zig`, `cli_surface.zig`, `acp/server.zig`, `chat_completions_protocol.zig`, `picker_presentation.zig`, and `config_runtime.zig`. Those are upstream hot files. The edits are small. Section 6 says to avoid that class of file.
- `reasoning_effort` is added on every custom OpenAI chat-completions connection, which is the adapter the design names. A non-engy connection that rejects the field will now receive it whenever effort is not `auto`.

## How the live turn was run

ReleaseSafe build, Zig 0.16.0, same flags as CI (`zig build -Doptimize=ReleaseSafe -Dcpu=baseline`). Key loaded from the vault into `ENGY_API_KEY` and not printed. `FX_PROVIDER`, `FX_MODEL`, and `FX_PERMISSION_MODE` were unset. Home was a new `/tmp` directory.

1. `initialize` with protocol version 1 and `_meta.fx.terminal` false.
2. `session/new` with an absolute cwd, `mcpServers` `[]`, and a system prompt: "Reply with the single word pong. Do not use tools."
3. `session/prompt` with that same sentence.

No mode was sent. The client was not asked for permission.
