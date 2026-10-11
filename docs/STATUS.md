# Status

## Now

Bootstrap the monorepo. The previous tree is on the `legacy-2026-10` tag and branch. `main` holds `app/` (zeron), `fx/` (fx), an empty `ios/`, and these docs.

## Done today

- Tagged and branched the previous `main` as `legacy-2026-10` (`ff0f7a20259d5c68e42967b5ab0fafe9921a732e`).
- Imported zeron `main` (`40091d469aa8a3f1ab1ec7cb404957b742940278`) into `app/` with `git subtree add --squash`.
- Imported fx `main` (`ea115f8d1ca1aef397edeba832b7a0c4cdf7867c`) into `fx/` with `git subtree add --squash`.
- Added the design, this file, and `docs/decisions.md`.
- CI on Linux checks the layout, licenses, and notices, and compiles `app` (`cargo build --locked -p zeron`) and `fx` (`zig build`).

## Next

Phase 1: fx speaks engy, and Arbos speaks fx. `fx acp` with engy as the built-in default.

## Balance

engy balance unread this cycle.

## Known gaps

- The product is not branded Arbos yet.
- The product is not installable yet.

## Blocked

- engy balance unread this cycle. `op` is not available here, so the credit number was not read.
