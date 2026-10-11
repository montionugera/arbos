# Status

## Now

The app in `app/` talks to fx and opens a folder as a project. Main can spawn workers on this machine or on a registered install. Each local worker is its own `fx acp` process and its own tab. One visible harness runs `fx acp`. The window title, the about string, and `zeron --version` say Arbos. The Rust package and the on-disk binary stay `zeron`. A project's `home` can be another machine. That machine runs `arbos headless`, and the rail shows the home name. fx speaks engy (`https://api.engy.ai/v1`, model `glm-5.3`). A present `ENGY_API_KEY` is stored outside the project, in the macOS keychain or in a mode 600 file on Linux, and passed into the fx child. Settings → Keys can store a provider key the same way and write the fx connection without the secret. Files in `.arbos/subscriptions/` can wake Main as a Worked line when no one has typed. The home engine keeps doing that after the local window quits. The update checker reads the GitHub Release tag `edge` and looks for `arbos-<version>-linux-<arch>.tar.gz`. This tree does not publish a DMG. The phone in `ios/` lists projects, follows Main, and sends a prompt over the same RPC. It has no voice and no calls. `main` holds `app/` (zeron), `fx/` (fx), `ios/` (the phone), and these docs.

## Done today

- Tagged and branched the previous `main` as `legacy-2026-10` (`ff0f7a20259d5c68e42967b5ab0fafe9921a732e`).
- Imported zeron `main` (`40091d469aa8a3f1ab1ec7cb404957b742940278`) into `app/` with `git subtree add --squash`.
- Imported fx `main` (`ea115f8d1ca1aef397edeba832b7a0c4cdf7867c`) into `fx/` with `git subtree add --squash`.
- Added the design, this file, and `docs/decisions.md`.
- CI on Linux checks the layout, licenses, and notices, and compiles `app` (`cargo build --locked -p zeron`) and `fx` (`zig build`).
- fx now injects the engy connection, defaults the model to `glm-5.3`, reads `ENGY_API_KEY`, passes `reasoning_effort`, honors `ARBOS_FX_HOME`, and defaults permission to full access.
- A live `fx acp` turn against engy answered `pong`. See `docs/qa/2026-10-11-fx-engy.md`.
- Root CI on Linux runs fx's 2 ms startup budget and binary-size watch. The GitHub description is "Arbos is a coding-agent app being rebuilt on fx and zeron. The harness speaks engy.ai."
- `fx ask` no longer fails when no profile home is set. Full access stays the default. The one-time warning is skipped when that home is missing.
- Profile usage follows the session home, then `ARBOS_FX_HOME`, then `HOME`. A run with `ARBOS_FX_HOME` set does not append to the real `~/.fx/usage.jsonl`.
- Root CI runs the filtered Zig behavior check for engy defaults, the effort field, home selection, and the ask tests. The independent check is `docs/qa/2026-10-11-fx-engy-check.md`.
- A live `fx acp` turn with reasoning effort `low` answered `pong`. The gateway trace recorded `effort=low` and `reasoning=selected`. The real `~/.fx/usage.jsonl` did not change.
- The app spawns `fx acp`, sets full-access on a fresh session, passes `ENGY_API_KEY` and `ARBOS_FX_HOME`, and sends a mid-turn steer as `session/prompt` with `_meta.fx.steer`. Hosted sync is not in this build. See `docs/qa/2026-10-11-app-fx.md`.
- The left rail is projects. A folder gets `<folder>/.arbos/` with `project.toml`, `context/`, and a git repo that is committed after a turn. Each project has a Main tab, extra chats, and a Context tab. The checklist under Main is `context/status.md`. See `docs/qa/2026-10-11-projects.md`.
- Main can spawn local workers through an MCP server named `arbos`. Each worker is its own `fx acp` process, its own tab, and its own `agents/<id>/fx` home. A finished worker wakes Main as a Worked line. Main's transcript is the folded project style. Other chats stay classic. See `docs/qa/2026-10-11-delegation.md`.
- A timer, a pull request, a branch's CI, and Slack are subscription files. A firing prompts Main with no typed message. The live proof was a one-minute timer, plus scratch pull request 820 on `cursor/phase4-red-ci-22f6`. The morning file is 08:00 and did not fire. Settings → Keys saved an OpenRouter connection. The key file is mode 600 and is outside the project. A rejected tool call no longer fails the next prompt, and a later successful turn clears an error card that shared a line with earlier text. See `docs/qa/2026-10-11-wakes.md`.
- `phase5-remote` has home `arboslife`. Headless Arbos runs beside the existing processes on that machine, in its own directory, systemd units, and port. Cloudflare Access is in front. Closing the local window left the remote engine running, and reopening showed new `Worked ·` lines from a short timer. A second local install, registered as the offline MacBook `2a05bc55-df5b-4687-8b45-ac94702d71d0`, ran `remote-ship` and wrote `remote-ok`. See `docs/qa/2026-10-11-remote.md`.
- The phone client lists `phone-demo`, follows Main, and queues a prompt on the engine RPC. The proof is `docs/qa/2026-10-11-phone.md`. There is no voice and no call screen.

## Next

The root README, after this phone slice is green. No voice. No `FINAL.md`.

## Balance

unread — no balance route on the public API

## Known gaps

- The window title, the about string, and `zeron --version` say Arbos. The Rust package stays `zeron`. The on-disk binary stays `zeron`. `zeron --version` prints `Arbos 0.2.111`. The published Linux tarball is `arbos-<version>-linux-<arch>.tar.gz` and also contains an `arbos` binary.
- The phone is not on a device yet. This machine has no iOS simulator. The protocol proof is the Rust test in `docs/qa/2026-10-11-phone.md`.
- The product is not installable yet.
- Some Pi and opencode live tests and examples still name the deleted drivers. `cargo build --locked -p zeron` does not compile those files. Root CI does not run them.
- These `zig build test` failures also fail on `0f998ae8`, the fx tree before the engy commit. That commit does not change their files. They are left unfixed so the required CI check can stay green:
  - `git worktree reports dirty for obvious metadata and tracked-file changes` (expected `.dirty`, found `.unknown`)
  - `McpRuntime cancels blocked stdio discovery without exposing partial state`
  - `caller cancellation interrupts blocked candidate connection` (expected `.disconnected`, found `.failed`)
  - `McpRuntime publishes and calls ready servers while another server is connecting`
  - `McpRuntime continues discovery after one server times out` (expected a startup timeout, found an early exit)
  - `skill refresh publishes one generation and coalesces one latest request` (expected `.adopted`, found `.unchanged`; this one also passes on some runs)
- A `slack` subscription does not wake until `~/.fx/slack/installation.json` exists. This fx build has no Slack message stream, so an installed workspace still does not produce a wake. The public install config did not ask for a Vercel login.

## Blocked

- engy balance stays unread until a real route exists. The public OpenAPI at `https://api.engy.ai/openapi.json` has no credits or balance route (checked 2026-10-11).
