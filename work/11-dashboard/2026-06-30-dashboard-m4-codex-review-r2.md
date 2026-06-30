## Findings

WARN `apps/dashboard/web/src/composables/usePolling.ts:141` - `refresh()` can still run after `stop()` has disposed the handle, which leaves `loading` stuck `true` and can issue fetches from a supposedly torn-down poller. `stop()` sets `disposed = true`, clears `loading`, and even sets `inFlight = false` at `apps/dashboard/web/src/composables/usePolling.ts:178`, but neither `refresh()` nor `start()` checks `disposed` before entering `drive()`. Because `drive()` sets `loading.value = true` at `apps/dashboard/web/src/composables/usePolling.ts:100` and then returns early on `disposed` at `apps/dashboard/web/src/composables/usePolling.ts:127`, the final `loading.value = false` at `apps/dashboard/web/src/composables/usePolling.ts:138` is skipped. I confirmed the edge with a one-off reproduction from `apps/dashboard/`: `const h = usePolling(async () => ({ ok: true, data: 1 }), 0); h.stop(); await h.refresh();` leaves `{ data:null, error:null, loading:true, lastUpdated:null }`. The new tests prove late resolution after `stop()`/scope disposal does not commit, but they do not call `refresh()` after disposal; `apps/dashboard/web/test/usePolling.test.ts:117` snapshots only the original in-flight late resolution. Fix by making disposal an entrypoint guard (`if (disposed) return;` in `refresh()` and `start()`, and ideally before any fetch is launched), or split “pause polling” from “dispose forever” if restart-after-stop is intended. Add regression coverage for `stop(); await refresh()` and, if supported by the public handle contract, `stop(); start()`.

## Verified

The stale-param race fix covers the important active-handle paths: a mid-flight param change bumps `generation`, suppresses the old result, coalesces exactly one follow-up, and keeps fetches non-overlapping. The focused tests exercise coalescing, `useLogs` param supersession, late resolution after `stop()`, and `onScopeDispose(stop)`.

The R/A/G rerouting closes the prior NIT. Domain/page components route log priority and run severity through `priorityRag`, `presentationOf`, or `RAG_PRESENTATION`; direct `text-rag-*` class names are confined to the centralized presentation/UI-token layer.

The logs and health schema coverage now exercises valid bodies, malformed `ok:true` data to `{ ok:false, code: "schema" }`, and health degraded passthrough through the shared `fetchJson` path. I also re-ran the focused web checks: `bun test web/test/usePolling.test.ts web/test/client.test.ts web/test/schema.test.ts web/test/severity.test.ts` passed with 43 tests / 0 failures.

I did not find a design-pass correctness regression in the reviewed paths. The `DASHBOARD_NAME.split("—")` fallback would render an empty subtitle rather than `/ undefined` if the separator drifted, fonts are locally imported through Vite, the SPA asset discipline still keeps missing asset-shaped paths at 404, and the read-only/GET-only router behavior remains intact.

VERDICT: FIXES_REQUIRED
