# Status

## Now

fx speaks engy. The built-in connection is `engy` (`https://api.engy.ai/v1`, OpenAI chat completions). The default model is `glm-5.3`. The key comes from `ENGY_API_KEY`. `main` holds `app/` (zeron), `fx/` (fx), `ios/` (a README; the iPhone app arrives in phase 6), and these docs.

## Done today

- Tagged and branched the previous `main` as `legacy-2026-10` (`ff0f7a20259d5c68e42967b5ab0fafe9921a732e`).
- Imported zeron `main` (`40091d469aa8a3f1ab1ec7cb404957b742940278`) into `app/` with `git subtree add --squash`.
- Imported fx `main` (`ea115f8d1ca1aef397edeba832b7a0c4cdf7867c`) into `fx/` with `git subtree add --squash`.
- Added the design, this file, and `docs/decisions.md`.
- CI on Linux checks the layout, licenses, and notices, and compiles `app` (`cargo build --locked -p zeron`) and `fx` (`zig build`).
- fx now injects the engy connection, defaults the model to `glm-5.3`, reads `ENGY_API_KEY`, passes `reasoning_effort`, honors `ARBOS_FX_HOME`, and defaults permission to full access.
- A live `fx acp` turn against engy answered `pong`. See `docs/qa/2026-10-11-fx-engy.md`.
- Root CI on Linux runs fx's 2 ms startup budget and binary-size watch. The GitHub description is "Arbos is a coding-agent app being rebuilt on fx and zeron. The harness speaks engy.ai."

## Next

The zeron `AcpAgentSpec` that spawns this fx.

## Balance

unread — no balance route on the public API

## Known gaps

- The product is not branded Arbos yet.
- The product is not installable yet.

## Blocked

- engy balance stays unread until a real route exists. The public OpenAPI at `https://api.engy.ai/openapi.json` has no credits or balance route (checked 2026-10-11).
