## Findings

None. The r2 disposal WARN is resolved: `refresh()` now returns before touching `generation`, `loading`, or `fetcher` once `stop()` has set `disposed`, and `start()` now returns before setting `active`, attaching listeners, firing the immediate refresh, or arming an interval on a disposed handle. A late in-flight resolution after disposal still cannot write state because `stop()` bumps `generation`, clears `pendingRefresh`, settles `loading` to `false`, clears the timer, and `drive()` re-checks `disposed` before any final loading or coalesced-follow-up work.

The new regression tests exercise the actual failed edges, not just a happy path: `stop(); await refresh()` proves no fetch is issued and the public state remains unchanged with `loading === false`, while `stop(); start()` with a nonzero interval proves neither the immediate refresh nor several would-be interval ticks fire after disposal. The existing late-resolution tests still cover the pending-fetch path, and the normal non-disposed manual `refresh()` and coalescing/generation paths remain covered by the success/degraded/coalescing/param-change tests. I also re-ran `bun test web/test/usePolling.test.ts` from `apps/dashboard/`: 7 pass / 0 fail.

VERDICT: GO
