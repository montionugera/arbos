# Arbos rebuild: fx + zeron, from scratch

Ideation only. No code was changed. Research done 2026-10-11 against `zeronsh/zeron` at `40091d4` (2026-10-10) and `vercel-labs/fx` at `ea115f8` (2026-10-10), plus the live engy.ai docs and API. Jacob's rule for this doc: start from forks of fx and zeron, not from the old Arbos code.

## Summary (10 lines)

1. **zeron is Rust + gpui, not Zig.** MIT. v0.2.111, several releases a day. It is a desktop *control plane* for coding agents (Claude Code, Codex, Cursor, Devin, Grok, Hermes, Pi), not an editor. It has a file tree with an editor pane, terminals, a diff pane, self-update, an iOS app core, and local dictation.
2. **fx is Zig 0.16, Apache-2.0, by Vercel Labs.** Canonical repo: `vercel-labs/fx`. A tiny coding agent, "embeddable as a harness". It speaks ACP (Agent Client Protocol, see terms below), has subagents, skills, MCP, compaction, and talks to any OpenAI-compatible endpoint.
3. **They already fit together with zero code.** zeron has a generic ACP harness; `fx acp` is an ACP server. Phase 1 is one new agent spec in zeron and one new provider in fx.
4. **engy.ai is OpenAI-compatible** at `https://api.engy.ai/v1` (also Anthropic-style `/v1/messages`). Bearer key. Public `/v1/models` lists context sizes. Open models only (GLM-5.3, Kimi-K3, DeepSeek V4.1, Qwen, MiniMax). Zero data retention by default.
5. **Architecture:** one zeron-fork binary (`arbos`: UI + engine, headed or headless) that spawns one `fx acp` process per agent chat. The engine, not fx, orchestrates sub-agents, because fx's own subagents are invisible on the ACP wire and cannot be opened as chats.
6. **State:** `<project>/.arbos/` holds project identity, icon, the shared Context folder, the checklist, subscriptions, and one folder per agent; fx's session records are redirected there. zeron's CRDT doc is a render cache, not the truth.
7. **UI:** left rail = projects with settable icons. Top tabs = ● Main, then agent chats, `+` for a new one, and a Context tab. This is the current Arbos "quiet conversation bar" turned into tabs, with the project strip turned into the rail.
8. **Provider:** engy built into the fx fork as the default; the key comes from the OS keychain and is passed as an environment variable at spawn. BYOK keeps fx's existing doors: AI Gateway, OpenRouter/Ollama, Codex and Grok OAuth.
9. **Repos:** two new GitHub forks under `unarbos` (`arbos-app` from zeron, `arbos-fx` from fx), upstream remotes kept, our changes in few files so rebases stay cheap. MIT and Apache-2.0 are compatible.
10. **Cursor's Project model, local-first:** every project has a *home machine* that runs its engine. By default that is this laptop; later it can be ArbosLife, and the UI attaches over the same RPC. Workers are `fx acp` processes started by whichever engine owns the machine they run on. The "cloud" is a home you choose, not a service we run.

## Decisions (Jacob, 2026-10-11)

1. **Local first, designed to run remote.** No sync service on the main path. A project has a *home machine*; the UI attaches to it the same way whether it is this laptop or a server. See §10.
2. **Truth on disk is fx's `events.jsonl` under `<project>/.arbos/`.** zeron's Loro doc is a cache.
3. **One `fx acp` process per sub-agent**, orchestrated by the engine, each a tab.
4. **Permission default is `full-access`.** No model review per action.
5. **Default model is GLM-5.3** for the main chat and for sub-agents, on engy.
6. **fx is the one harness shown.** The ACP door stays open in code for Claude Code, Codex and others, hidden behind a setting until wanted.
7. **Voice and iOS are off the main path.** Added later; nothing in phases 1–5 depends on them.
8. **Adopt Cursor's Project model:** one coordinator you talk to, workers you can open, a shared Context folder every agent reads, a checklist under the chat, and wakes from timers, pull requests and Slack. Built local-first; see §10 for how.

Added later the same day:

9. **One monorepo: `unarbos/arbos`.** The current tree is tagged and branched `legacy-2026-10`, then removed from `main`. `main` starts clean: `app/` (zeron, via `git subtree`), `fx/` (fx, via `git subtree`), `ios/`, `docs/`. The product is still called Arbos.
10. **Remote identity is Cloudflare Access** in front of a headless engine, with a service token for headless clients; ssh is the fallback for people without Cloudflare.
11. **Slack wakes use fx's own Slack connector** (signs in through fx.sh). Use whatever fx.sh already gives.
12. **Sub-agent depth is one.**
13. **Ships as a Mac download** (signed, notarised DMG) and an **iPhone app on TestFlight**. Every push to `main` that passes CI publishes a build, and the installed app offers it as an update. GitHub Releases hosts the artifacts and `manifest.json`.
14. **iPhone scope now:** list projects, follow a project's Main chat live, send prompts. No voice, no calls.
15. **Any machine running Arbos can be offered as a worker.** Headed or headless, an install can register itself in `machines.toml` and accept `spawn { machine }`.
16. **Build loop:** the engy key from the vault with a budget of 100k credits; Jacob's MacBook is reachable as Cursor self-hosted worker `2a05bc55-df5b-4687-8b45-ac94702d71d0` for Mac and iOS builds and for installing each passing build.

## Terms

- **ACP** (Agent Client Protocol): JSON-RPC 2.0 over stdio, newline-delimited. An editor (the *client*) starts an agent program (the *server*), sends `session/prompt`, and gets `session/update` notifications back. Spec at agentclientprotocol.com.
- **Harness**: the piece of code that starts an agent program, speaks its wire protocol, and turns its output into one event stream the UI can draw.
- **gpui**: Zed's GPU-drawn Rust UI toolkit. zeron pins a fork (`zeronsh/zui`).
- **Loro / CRDT**: a data structure that two devices can edit at the same time and later merge without conflicts. zeron stores each chat transcript as a Loro document.
- **Durable Object**: a Cloudflare Worker that keeps state. zeron's optional sync relays Loro documents through them.
- **MCP** (Model Context Protocol): a standard way to give a model extra tools. fx can load MCP servers that the ACP client hands it on the same connection.
- **BYOK**: bring your own key. The person supplies a key for a provider other than the default.
- **ZDR**: zero data retention. The provider does not store prompts or outputs.

## 1. The goal, restated

Build Arbos again from two open-source bases: zeron becomes the Arbos desktop, fx becomes the only agent that runs inside it. The result looks and works like Cursor Projects: a left rail of projects, each with one main chat that talks to a coordinating agent, and that agent sends work to sub-agents that appear as their own chats. Inference defaults to engy.ai with the key in a vault; other providers are bring-your-own-key. The old Arbos code is a reference for behaviour, not a source of code.

## 2. What each base gives, and what neither gives

### What zeron gives (Rust, MIT, ~457k lines)

| Area | What is there | Fit |
| --- | --- | --- |
| Shell | gpui window, collapsible sidebar, header, right pane (Changes, Files, terminals), command palette, settings pages, keymap | Base for rail and tabs |
| Engine | `zeron-engine`: sessions, run journal with crash recovery, steering mailbox, terminals (portable-pty), repos and worktrees, diff capture, uploads | Keep whole |
| Harness | `Harness` trait; generic `AcpHarness` with per-agent `AcpAgentSpec` (binary, args, env override, model discovery, steering mode, effort ladder) | Add one spec for fx; drop the others from the picker |
| Transcript | virtualised list, streaming markdown, tool folding, question panel, MessageRail minimap, attachments | Keep |
| Headless | one binary, `zeron headless` runs the engine as a daemon; UI attaches over typed RPC (WebSocket or in-process) | Matches "agent is a service, clients are views" |
| Update | `zeron-update`: manifest with sha256 per artifact, managed/MacApp/Windows install kinds, background download, restart to update | Re-point to our bucket and signing |
| Mobile | `crates/mobile` (UniFFI Rust core) plus `apps/ios` Swift shell (60 Swift files), live activities | Head start on iOS |
| Voice | `zeron-voice` local Parakeet dictation; `zeron-voice-session` call lifecycle built on Codex's realtime protocol; `zeron-orb` | Dictation reusable; calls are Codex-bound |
| Sync | Loro docs in SQLite locally; optional relay through Cloudflare Durable Objects (`edge/`, TypeScript) with WorkOS login | Compiled out (decision 1); remote goes through engine RPC instead, §10 |
| Files | file tree, `gpui-base` editor, markdown and image preview, search, git status | The "editor" part |

Not in zeron: projects as a first-class thing with icons (it has *spaces*: device+folder pairs, and a repo-favicon fetcher), a coordinator role, a horizontal tab strip (removed 2026-08-10; the sidebar is the session list), timers or subscriptions, a self-hosted sync contract.

### What fx gives (Zig 0.16, Apache-2.0, ~766k lines)

| Area | What is there | Fit |
| --- | --- | --- |
| Agent loop | tools (filesystem, shell in the login shell, web, skills, MCP), permissions (`ask`, `auto`, `full-access`), compaction with saved records, hooks | The whole agent |
| ACP server | `fx acp`: `initialize`, `session/new|load|resume|close|list|remove`, `session/prompt`, `session/cancel`, `set_config_option`, `set_mode`. Extensions under `_meta.fx`: mid-turn steering, client system prompt (`systemPrompt` on `session/new`), per-session `cwd`, client MCP servers kept loaded, `terminal: false` | Everything the engine needs |
| Providers | Vercel AI Gateway, Codex OAuth, Grok OAuth, and named custom connections: `{protocol: "openai-chat-completions", base_url, auth: {type: "bearer", env: "VAR"}, model_metadata}` | engy is one connection away |
| Subagents | one-off and named children inside one fx session; same workspace; cannot spawn their own; conversations are not listed separately | Not the orchestration we need (see §3) |
| Sessions | `~/.fx/sessions/v2/<id>/` with `session.json`, `events.jsonl`, checkpoints; global, portable across workspaces | Redirect under `.arbos/` |
| Embedding | libfx: WASM and Node-API builds of the same ACP core | Not needed for desktop; useful for a web client later |

Not in fx: a UI, multi-project awareness, timers, cross-machine anything, a reasoning-effort passthrough on custom connections (docs: "does not support provider-specific reasoning controls"), a way to move its home directory (`~/.fx` is a constant in `profile_paths.zig`).

### What neither gives (the rebuild list)

These exist in the current Arbos and must be built again. Nothing is ported.

| Capability | Current Arbos | zeron | fx | Rebuild cost |
| --- | --- | --- | --- | --- |
| Filesystem-first kernel: `.arbos/` per project, inbox files, `GOALS.md`, git-saveable state, rewind | yes | no (SQLite + Loro) | partly (session dir) | medium: a project layout the engine owns, fx sessions redirected into it |
| Subscriptions: timers, PR, CI, inbox wakes | yes | no | no | medium: an engine scheduler that writes a prompt into a chat |
| Hub / mesh: attach to a kernel on another machine by name through a tunnel | yes | different model: CRDT relay via Cloudflare DO | no | low if we self-host zeron's edge; high if we want Arbos's hub model |
| Full-duplex voice | in progress | dictation yes; calls bound to Codex realtime | no; engy has no speech model | high: separate speech path |
| iOS app | yes (Swift) | Rust core + Swift shell exist | no | medium: rebrand, point at our engine |
| Signed update channel | yes (Ed25519 feed) | yes (sha256 manifest, R2) | yes (`fx upgrade`) | low: our bucket, our Developer ID |
| Project-chat vs classic-chat rendering | yes | one style | one style | medium: a second transcript style in the UI |

## 3. Proposed architecture

### Processes

```
arbos (zeron fork, one binary)
├── UI (gpui)  ── RPC (in-process or WebSocket) ──┐
└── engine (tokio daemon; also `arbos headless`) ◄┘
      ├── spawns  fx acp   ← main chat of project A   (cwd = project A)
      ├── spawns  fx acp   ← sub-agent "fix-tests"    (cwd = project A or a worktree)
      ├── spawns  fx acp   ← user agent chat #2       (cwd = project A)
      └── serves  MCP "arbos" over each ACP connection (spawn/send/list/result tools)
```

- **One fx process per agent chat.** Each chat is one ACP session in its own `fx acp` process. A crashed sub-agent takes nothing else down. The engine already has per-session child lifecycle hardening (SIGTERM→SIGKILL, stderr tail, handshake timeout).
- **The engine is the orchestrator.** Not fx. Reasons: fx's subagents never appear on the ACP wire as sessions, cannot be opened as chats, cannot nest, and share one process. Jacob's requirement 2 needs each sub-agent to be a tab.
- **fx is the one harness shown.** The zeron fork keeps the `AcpHarness` code and registers `arbos-fx` (`fx acp`, `_meta.fx.terminal=false`) as the only visible spec. The other ACP specs (Claude Code, Codex, Devin, Grok, Hermes) stay compiled in behind a hidden setting, `harnesses.show_all`, so a BYOK person can switch one on later. The native non-ACP drivers (Pi, opencode) are deleted.

### How zeron UI talks to fx

| Need | ACP call the engine makes |
| --- | --- |
| New chat | `session/new { cwd: <project>, systemPrompt: [role + project context], mcpServers: [arbos] }` |
| Resume after restart | `session/load` or `session/resume` with the saved session id |
| Send | `session/prompt` |
| Steer mid-turn | `session/prompt` with `_meta.fx.steer: true` (fx extension; zeron's harness gets a small adapter from its `_session/steering` shape) |
| Stop | `session/cancel` |
| Model and effort | `session/set_config_option` against the advertised options |
| Permissions | `session/set_mode` → `full-access` (decision 4) |
| Tool cards, thinking, text | `session/update` notifications, already mapped by zeron's `normalize::map_update` |

### Where state lives (filesystem-first)

```
<project>/.arbos/
├── project.toml          id, name, icon, home machine, default model (glm-5.3), created
├── context/              Cursor's "Context": files every agent in the project reads and writes
│   ├── GOALS.md          standing goals and decisions; the main chat owns it
│   ├── status.md         the checklist drawn under the main chat (checkbox lines)
│   ├── preferences.md    how Jacob likes work done
│   └── research/…        what workers learned, for the next worker
├── subscriptions/        one TOML per wake: timer, github_pr, github_ci, slack
├── agents/
│   ├── main/             the coordinator
│   │   ├── agent.toml    role = "coordinator", fx session id, model
│   │   └── fx/           fx's own session dir (session.json, events.jsonl, checkpoints)
│   ├── fix-tests/        a sub-agent
│   │   ├── agent.toml    parent = "main", brief, status, result pointer
│   │   └── fx/
│   └── chat-2/           a chat the person opened with +
└── runtime/              not committed: pids, sockets, caches
```

- **fx fork change:** `profile_paths.zig` reads `ARBOS_FX_HOME` (or a `--home` flag) so the engine points each process at `agents/<id>/fx/`. Settings and credentials still come from the engine, so nothing else in `~/.fx` is needed.
- **zeron's Loro doc** for each chat is a projection: the engine fills it from the ACP stream as it does today, and it drives the UI. Truth for rewind and git is `.arbos/` (decision 2). The projection is one-way, file to doc, never back.
- **Cross-project list** (the rail) is zeron's workspace registry: one row per project with path, icon ref, main chat id, last activity.

### Sub-agent orchestration on fx

The engine serves one MCP server named `arbos` into every main-chat session (fx keeps client MCP tools loaded on every turn). Its tools:

| Tool | What the engine does |
| --- | --- |
| `agents.spawn { brief, name?, model?, readonly?, worktree?, machine? }` | makes `agents/<name>/`, starts `fx acp` with `cwd` on `machine` (phase 1: only `local`), sends the brief as the first prompt, opens a tab |
| `context.write { path, text }` / `context.read { path }` | reads and writes files under `.arbos/context/`; `status.md` edits redraw the checklist |
| `agents.send { name, text }` | steers if running, else prompts |
| `agents.list` | reads `agents/*/agent.toml` |
| `agents.result { name }` | returns the child's last reply and result pointer |

When a child's turn ends, the engine writes a short "done" message into the main chat: a steer if the main chat is mid-turn, otherwise a new prompt with `_meta.fx` marking it as a system wake so the UI draws it as a `Worked` segment, not a user bubble. This is the Cursor coordinator loop. The main chat's system prompt says: plan, delegate, do not edit code; one quick read or command is fine.

Sub-agents are plain fx sessions that get `context.*` but not `agents.*`, so depth is one unless Jacob wants more. fx's internal subagents stay available to a sub-agent as a cheap in-process helper. Every session's system prompt ends with: "Read `.arbos/context/GOALS.md` first. Write what you learn to `.arbos/context/`. Keep `status.md` current."

## 4. UI model

```
┌──┬───────────────────────────────────────────────────────────┐
│🧪│ ● Main │ ⚙ fix-tests │ ⚙ docs │ chat 2 │ +3 │ +  ┊ Context │
│🏠│───────────────────────────────────────────────────────────│
│🚀│                                                           │
│  │   transcript (Project style for Main, classic for others) │
│  │                                                           │
│+ │   composer                                                │
└──┴───────────────────────────────────────────────────────────┘
```

- **Projects rail (left).** One icon per project, settable: pick an emoji, a file, or the repo's favicon (zeron's `project_icon.rs` already probes 17 icon paths). `+` at the bottom adds a project through zeron's folder picker. Cmd+1..9 jump. This replaces zeron's sidebar and the current Arbos dark project strip.
- **Main chat.** `agents/main`, Project style: prompt bubble, folded `Worked Xs ›`, worker lines, chips, short prose, no tool cards. Rule set as in the current `project-chat-vs-agent-chat.md`, re-implemented in zeron's transcript as a second style.
- **Top tabs.** This is the current Arbos quiet conversation bar, one for one: ● Main first, a divider, then ⚙ sub-agents the main chat spawned, then chats the person opened. Overflow folds to `+N`. The trailing `+` starts a new agent chat in this project (Cmd+N). zeron removed its own tab strip, so the module is new but the pattern (session navigation, `cycle_target`) exists.
- **Sub-agent tabs** show a working pulse while running and settle to a check or an error mark. Clicking opens the child's classic chat.
- **Context tab** (right end of the strip): zeron's Files surface rooted at `.arbos/context/`, with its editor. This is Cursor's Context tab: research, plans, preferences, readable and editable by Jacob and by every agent.
- **Checklist under the chat:** `.arbos/context/status.md` rendered as checkbox rows between the transcript and the composer (zeron's `todo_panel.rs` is the start). The coordinator keeps it; a file watcher redraws it.
- **Right pane** keeps zeron's Changes, Files (with editor), and terminals, closed by default.
- **Status strip** at the bottom keeps zeron's update strip ("Restart to update").

## 5. Provider

- **Default: engy.** The fx fork adds a built-in connection named `engy`: `base_url = https://api.engy.ai/v1`, `protocol = openai-chat-completions`, bearer from `ENGY_API_KEY`. On first run the engine fetches the public `/v1/models` and writes `model_metadata` (fx needs `context_window` to time compaction; engy reports `context_length` and `input_modalities`). Default model: **GLM-5.3** (327K context) for the main chat and sub-agents (decision 5). Note: GLM-5.3 cannot turn thinking off; engy runs it at `low` instead, so leave room in `max_tokens`.
- **Key handling.** The engine stores the key in the OS keychain (zeron has `security-framework` on macOS, files elsewhere) and sets `ENGY_API_KEY` only in the `fx acp` child's environment. The key never lands in `.arbos/`, settings files, logs, or this doc.
- **Reasoning effort.** engy accepts `reasoning_effort` from `none` to `max` and translates it per model. fx's custom-connection adapter does not send it today; the fork adds a passthrough so the effort picker works.
- **Images.** engy takes inline base64 only, no URLs. fx already inlines, so nothing to do.
- **BYOK.** fx's existing providers stay: AI Gateway key, Codex OAuth, Grok OAuth, and any OpenAI-compatible URL (OpenRouter, Ollama, vLLM). The Arbos settings page writes the fx connection and keychain entry; fx is never configured by hand.
- **Not possible through fx today:** Anthropic-native or Responses-API endpoints on custom connections. engy's `/v1/messages` and `/v1/responses` are unused.

## 6. Branch and repo strategy

- **One monorepo, `unarbos/arbos`** (decision 9). Tag and branch the current tree as `legacy-2026-10`, then clear `main`. Bring zeron in as `app/` and fx in as `fx/` with `git subtree add --prefix … --squash`, so `git subtree pull` can still merge upstream fixes; both upstreams move daily and a plain copy would freeze us on October 2026. `ios/` holds the phone app; `docs/` holds this design and what follows it.
- **Keep our diff small and bounded.** zeron: new modules (`rail.rs` for projects, `tabs.rs` reborn, a `project` layer in the engine, the `arbos` MCP server, one `AcpAgentSpec`), plus deletions of other harnesses. fx: `engy` provider, effort passthrough, home-dir override, branding. Avoid touching upstream hot files.
- **Branding.** Rename binaries (`arbos`, `arbos-fx`), the profile dir (`.arbos`, not `.fx` or `.zeron`), and regenerate fx's `build.zig.zon` fingerprint as its comment asks of forks.
- **Licenses.** zeron MIT; fx Apache-2.0; gpui Apache-2.0 (zeron avoids Zed's GPL crates: keep that rule); Parakeet weights CC BY 4.0 attribution; Codex voice protocol Apache-2.0. MIT and Apache-2.0 combine freely in one repo: the root stays MIT, `fx/LICENSE` and `fx/THIRD_PARTY_NOTICES.md` stay Apache-2.0 inside their subtree, and the shipped bundle carries both notices.
- **Zig toolchain.** fx needs Zig 0.16.0+. CI builds Linux x86_64 as the gate, macOS arm64 on need, with a startup budget of 2 ms and a binary-size watch. Keep those workflows; they are cheap insurance.

## 7. Risks and unknowns

1. **Two languages, two build systems.** Rust + gpui and Zig 0.16 in one product. The boundary is a stdio protocol, which keeps it honest, but every contributor needs both toolchains.
2. **fx is marked Experimental** by Vercel. Its wire extensions (`_meta.fx.*`) can change. Pin a version and re-verify the three extensions we rely on (steer, systemPrompt, cwd) on each upstream merge.
3. **Upstream velocity.** zeron shipped four releases on 2026-10-10. A fork that touches hot files will bleed on every merge. The small-diff rule above is the mitigation.
4. **Sub-agent visibility.** The engine-level design means a sub-agent's transcript is a full ACP session we own, which is good, but it costs one process and one compaction context per child. Cap fan-out in `project.toml`.
5. **`full-access` means no safety net against prompt injection.** fx's `auto` mode exists to catch a web page or file that tells the agent to do something destructive. With it off, the only guards are fx's hard refusals and the project's git history. Accepted by decision 4; worth a note in `preferences.md` for projects that touch production.
6. **engy model set.** Open models only. Coordinator quality on GLM-5.3 is untested for this role; BYOK covers Claude or GPT when needed.
7. **zeron's sync is tied to zeron.sh.** WorkOS login and a hosted edge. Decision 1 keeps it off the main path: compile it out, keep the files for a later look.
8. **Loro versus files.** Two copies of each transcript. The projection is one-way, file to doc, enforced in one place in the engine.
9. **Remote later means auth later.** §10 keeps the door by making every path go through engine RPC, but a remote home needs a tunnel and an identity check before it is safe. Not designed here; the shape is a `machines.toml` entry with a URL and a token.
10. **Unverified:** that `fx acp` runs cleanly under zeron's generic spec without the `npm` adapter path; that fx's `session/load` restores the `systemPrompt` and MCP list (docs say yes); that `gpui-base`'s editor is good enough as "the editor".

## 8. Open questions

None. Every question raised on 2026-10-11 was answered the same day; the answers are the Decisions list above. The build prompt is in `docs/arbos-rebuild-build-prompt.md`.

## 9. Phased plan

**Phase 1: fx speaks engy, Arbos speaks fx.**
Fork both. fx: `engy` connection with GLM-5.3 default, `ENGY_API_KEY` from env, effort passthrough, `ARBOS_FX_HOME`. zeron: one visible `AcpAgentSpec` for fx (others behind `harnesses.show_all`), steer adapter for `_meta.fx.steer`, `full-access` on `session/new`, sync compiled out.
Demonstrable: open the app, pick a folder, chat with fx on engy, see tool cards stream, steer mid-turn, stop, resume after relaunch.

**Phase 2: Projects and Context.**
`.arbos/project.toml`, `context/` with `GOALS.md`, `status.md`, `preferences.md`; projects rail with settable icons; one main chat per project with the coordinator system prompt; top tabs with ● Main, agent chats, `+`, Context; checklist under the chat from `status.md`.
Demonstrable: three projects in the rail with different icons; each has a Main tab, extra chats and a Context tab; Main edits `status.md` and the checklist redraws; state survives restart.

**Phase 3: Delegation.**
The `arbos` MCP server over ACP; `agents.spawn/send/list/result` and `context.read/write`; one fx process per sub-agent; "done" wakes the main chat; sub-agent tabs with working status; Project-style rendering for Main, classic for the rest. `machine` on `spawn` accepts only `local`.
Demonstrable: ask Main to do two things at once; two ⚙ tabs appear, work in parallel, write to `context/research/`, report; Main summarises in Project style and ticks the checklist.

**Phase 4: Wakes and ship.**
`subscriptions/*.toml` (timer, github_pr, github_ci) fired by an engine scheduler into the main chat; keychain for keys; BYOK settings page writing fx connections; self-update pointed at our bucket with Developer ID signing and notarisation; Linux tarball.
Demonstrable: a timer wakes Main every morning with no message from Jacob; a PR subscription reports CI going red; install from a DMG and get the update strip on the next release.

**Phase 5: Remote home.**
`arbos headless` as a daemon on ArbosLife; `machines.toml`; `arbos project move <name> --to arboslife` pushes the `.arbos/` repo and starts the engine there; the rail entry points at the remote home; `spawn { machine }` starts a worker on another machine's engine; remote identity per open question 1.
Demonstrable: close the laptop; the project keeps working on ArbosLife; reopen and the Main chat has moved on; a worker runs on the laptop for a task that needs it.

**Phase 6: Phone.**
`ios/` on zeron's mobile Rust core, pointed at the home engine over RPC: list projects, follow Main live, send a prompt. TestFlight from CI on every green push to `main`.
Demonstrable: the phone lists the same projects as the Mac, shows Main updating while a worker runs, and a prompt typed on the phone appears on the Mac.

**Later, off the main path:** full-duplex voice and the call screen; optional Loro sync for offline reading on the phone.

## 10. Cursor's Project model, built local-first

Cursor's text, line by line, and what each line becomes here. The rule: **every path goes through the engine's RPC, whether the engine is in this process or on another machine.** zeron already enforces that boundary (in-process RPC is the same protocol as the WebSocket one, no shortcuts). So "local" and "remote" are one code path with a different address.

| Cursor says | Arbos does | Local today | Remote later |
| --- | --- | --- | --- |
| "A Project is a long-running workspace for one body of work" | one folder with `.arbos/`; one entry in the rail | the folder is on this disk | the folder is on the home machine; the rail entry carries `home = arboslife` |
| "You talk to one coordinator… it stays free so you can steer" | `agents/main`, an fx session with the coordinator system prompt; `agents.spawn` returns at once; results arrive as wakes | same | same |
| "The agents that do the work are workers. You can open any worker" | one `fx acp` process per sub-agent, a tab each | started by this engine | started by the engine on `machine`, over engine-to-engine RPC |
| "Workers run on their own computers" | `spawn { machine }`; `machines.toml` in `~/.arbos/` lists engines by name | one entry: `local` | one entry per machine with URL and credential |
| "A Project lives in the cloud, so closing your laptop does not stop it" | the project's *home machine* runs its engine | home = this laptop, so sleep pauses it; timers coalesce on wake | `arbos project move --to arboslife`; the laptop becomes a viewer |
| "If a task has to run on your machine, the coordinator starts a worker there" | `spawn { machine: "laptop" }` from a project whose home is the server | n/a | the laptop's engine registers with the home engine while the app is open |
| "Context is shared… a set of files every agent can read" | `.arbos/context/`; every session's system prompt points at it; `context.read/write` MCP tools | files on disk, read directly by fx in its `cwd` | zeron already has relay-forwarded workspace file RPCs with path containment; a remote worker reads and writes `context/` through them, or the `.arbos/` git repo is fetched to the worker's machine |
| "The checklist under this chat is that shared status" | `context/status.md` rendered under the Main chat | file watcher | the home engine streams the file change over RPC |
| "It can wake up on its own… Slack, pull requests, a schedule" | `subscriptions/*.toml`, one scheduler in the engine that writes a system-wake prompt into Main | fires while the laptop is awake | fires on the always-on home |
| "What they learned stays in Context for the next turn" | workers write `context/research/<topic>.md` and report a path, not a dump | same | same |

Three choices make "remote later" cheap instead of a rewrite:

1. **The engine owns every file write under `.arbos/`.** The UI never writes files; it sends an intent over RPC. Locally that is a function call; remotely it is the same message on a socket. This is also what keeps the Loro projection one-way.
2. **`machine` is in the spawn contract from day one,** even though phase 1 only accepts `local`. Adding a remote engine is adding an address, not changing the tool.
3. **`.arbos/` is a git repository** (`runtime/` ignored). Moving a project's home is `git push` plus starting an engine; a worker on another machine can clone `context/` for offline reading. No new sync format.
4. **Every install is a possible worker** (decision 15). The same `arbos` binary, headed on a laptop or headless on a server, can register in another engine's `machines.toml` and accept `spawn { machine }`. There is no separate worker program.

What this does not give, on purpose: offline editing from two devices at once (that is what Loro was for; it can come back later for the phone), and a hosted service anyone else runs. The "cloud" is a machine Jacob names.
