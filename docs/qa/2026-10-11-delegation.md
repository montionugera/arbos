# Delegation, 2026-10-11

The headed `zeron` binary, built from this tree, asked Main to do two things at once. The key was `ENGY_API_KEY` from vault item ENGY_2. This note has no key. A scan of the app data dir, the project's `.arbos` tree, the ACP log, and `docs/` found no copy of the key.

## What was on screen

Display: Xvfb `:98`, llvmpipe, picom. The window name from `xdotool` was `Arbos`. The window was dark. Data dir: `ZERON_DATA_DIR=/tmp/arbos-qa/phase3-data`. Project folder: `/tmp/arbos-qa/phase3-project`.

Before the prompt, the rail showed one icon. The tabs were `● Main`, `+`, and `Context`. The composer chip read `engy` and `glm-5.3`. The checklist under Main read:

- ☐ Sky note
- ☐ Grass note

The prompt was one user bubble. It asked Main to spawn two local workers, `sky` and `grass`, without waiting, to write `research/sky.md` and `research/grass.md`, then summarise and tick both lines.

While that turn was still going, the tab strip showed `⚙ sky` with a check and `⚙ grass` with a dot. Under the prompt was a folded line, `Thought 8 times · wrote 8 notes · Ran 2 commands · called 14 tools`. Below that was a line starting `Worked · Worker sky done.` The checklist was still both open. The word `Pondering` was under the transcript. No tool cards were on Main.

At that moment `ps` showed three `fx acp` processes. `agents/sky/agent.toml` said `status = "done"`. `agents/grass/agent.toml` said `status = "working"`. `context/research/sky.md` existed. `context/research/grass.md` did not.

Later both files existed. The grass line in `status.md` was ticked first. Then both lines were ticked. The settled Main view showed:

- `⚙ sky` and `⚙ grass`, each with a check
- two lines starting `Worked · Worker sky done.` and `Worked · Worker grass done.`
- a folded line, `Worked for 7s`
- a short reply that names both notes and ends `The task is complete; no project code was modified.`
- the checklist, both lines ticked: `Sky note (research/sky.md)` and `Grass note (research/grass.md)`

Those `Worked ·` lines are not user bubbles. The only user bubble on Main is the original prompt.

The grass tab, selected after that, showed the worker's own prompt bubble, `Thought process`, `Thought process · Called 2 tools`, and `Thought 2 times · Called 3 tools`. The checklist was not under that chat.

## Files and processes

`agents/main/fx`, `agents/sky/fx`, and `agents/grass/fx` each contain one fx session. Main's system prompt starts `You are the coordinator for this project. Plan the work and delegate it. Do not edit project code.` The sky and grass prompts start `You are a worker. Do the brief. Do not spawn agents and do not edit project code.`

The ACP log has four `session/new` lines. The first has `mcp=[]`. That session id is not under this project's `.arbos`. The next three have `mcp=['arbos']`. Their session ids are the main, sky, and grass sessions above. Sky's `session/new` is logged while Main's prompt is still open. Grass's `session/new` is logged after that, still before Main's first wake.

Two prompt lines carry the wake mark: `steer=True wake=True`, then `steer=None wake=True`.

Main's transcript tools include `agents.spawn`, `agents.result`, `context.read`, and `context.write`. Sky's tools are `context.read`, `Selecting MCP tool`, and `context.write`. Grass's tools are `context.read`, `Selecting MCP tool`, and `context.write`. Neither worker transcript contains `agents.spawn`.

`context/research/sky.md` is one sentence about the sky looking blue. `context/research/grass.md` is one sentence about grass looking green.

`git -C` the project's `.arbos` printed `Create the project layout` and four commits titled `Save the project after a turn`.

A direct `ArbosCall` spawn with `machine` set to `arboslife` returned `only local workers are available`. No `agents/remote-try` directory was created.

## Screenshots

Saved under the store `media/phase3/` directory.

- `00-open.png` — Main, two open checklist lines, `engy` `glm-5.3`
- `01-workers.png` — `⚙ sky` with a check, `⚙ grass` with a dot, the prompt, the folded work line, and `Worked · Worker sky done.`
- `02-summary.png` — both worker tabs checked, both `Worked ·` lines, `Worked for 7s`, the short reply, both checklist lines ticked
- `03-grass.png` — the grass chat, with thought lines and `Called N tools`
