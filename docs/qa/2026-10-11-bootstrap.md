---
cursor:
  subagentId: "bc-e1c8b505-299f-5555-969f-700fd0afaf85"
---

# Bootstrap check, 2026-10-11

Checked `unarbos/arbos` read-only against decision 9 and the repo section of the design. `main` has not moved. Everything in the claim passed.

## What passed

- Tag `legacy-2026-10` and branch `legacy-2026-10` both point at `ff0f7a20259d5c68e42967b5ab0fafe9921a732e`, and that commit is the old tree (`crates/`, `desktop/`, `harness/`, `voice-server/`, `www/`), not `app/` and `fx/`.
- `main` is still `0f998ae84aa45c9355c6cce462b69b315d2c1226` ("Lay out the Arbos rebuild bootstrap.", 2026-10-11T01:29:55Z).
- `app/` is a squashed subtree of `zeronsh/zeron` at `40091d469aa8a3f1ab1ec7cb404957b742940278`; its tree SHA matches upstream (`3c7ae17e1a0d80e1f8a1f6245131e7faf169c059`).
- `fx/` is a squashed subtree of `vercel-labs/fx` at `ea115f8d1ca1aef397edeba832b7a0c4cdf7867c`; its tree SHA matches upstream (`9d04e557f723ffaa4ba980b964a2a53f95a858af`).
- `ios/` exists as `ios/README.md` only, and that file says the iPhone app arrives in phase 6.
- `docs/design/arbos-rebuild-ideation.md` is present and matches the design file byte for byte.
- Root `LICENSE` is MIT; `fx/LICENSE` is Apache-2.0; `fx/THIRD_PARTY_NOTICES.md`, `app/LICENSE`, and `app/THIRD_PARTY_NOTICES.md` exist.
- CI run [38102038488](https://github.com/unarbos/arbos/actions/runs/38102038488) is green on that commit: layout, compile app, and compile fx all succeeded.
- No GitHub Release was created or published on or after 2026-10-10; none targets this commit.
- No private keys in the new tree, and no `sk-`, `ghp_`, or `github_pat_` strings in the files this bootstrap added (`README.md`, `LICENSE`, `docs/`, `ios/`, `.github/`).

## What broke

- Nothing.

## What is uglier than the design

- Root CI compiles `app` and `fx` on Linux and smoke-tests the `fx` binary; it does not run fx's 2 ms startup budget or the binary-size watch, and the workflows that do that sit under `fx/.github/workflows/`, where GitHub will not run them.

## Nits

- The GitHub repo description still sells the old product (voice, iPhone app, mesh) while the README says the rebuild is not branded and not installable.
- `docs/STATUS.md` and the README call `ios/` empty; the directory holds one README.
- Token-shaped strings (`sk-abcdefghijklmnop`, `ghp_abcdefghijklmnopqrstuvwxyz…`, `github_pat_1234567890abcdef`, `xoxb-test-…`) appear only inside upstream test fixtures in `app/` and `fx/`, which were copied whole with the matching trees.
