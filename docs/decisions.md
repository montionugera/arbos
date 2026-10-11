# Decisions

- 2026-10-11: The import commit does not publish a GitHub Release. Releases start when the binary is branded Arbos and actually launches. A signed DMG of unmodified zeron would ship the wrong product.
- 2026-10-11: This product key is vault item ENGY_2, because it is the key Jacob named.
- 2026-10-11: Balance stays unread until a real route exists.
- 2026-10-11: The fx binary is still named `fx` this increment so the upstream smoke test and subtree pull stay intact.
- 2026-10-11: The GitHub description is "Arbos is a coding-agent app being rebuilt on fx and zeron. The harness speaks engy.ai." Root CI runs fx's 2 ms startup budget and the Linux binary-size watch at the upstream thresholds (2 ms raw mean, 52,429 byte warning).
- 2026-10-11: A missing profile home does not fail `fx ask`. Full access stays the default when the client sets no mode. The one-time full-access warning and its acknowledgment save are skipped when no home is available.
- 2026-10-11: The profile usage ledger uses the session home override, then `ARBOS_FX_HOME`, then `HOME`.
- 2026-10-11: Root CI runs a filtered Zig check for engy defaults, `reasoning_effort`, home selection, and the ask tests. The full `zig build test` suite is not a required check. Six failures in that suite already fail on `0f998ae8`, before the engy commit.
