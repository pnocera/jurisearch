# Codex Review — Dashboard M4

## Findings

WARN: Reactive query refreshes can be silently dropped while a request is in flight.

`apps/dashboard/web/src/composables/resources.ts:49` and `apps/dashboard/web/src/composables/resources.ts:72` call `handle.refresh()` when `group`/`limit`/`since` changes, but `apps/dashboard/web/src/composables/usePolling.ts:76` returns immediately when `inFlight` is true. If the user changes the Logs tab or window size during a slow `/api/logs` request, the requested refresh for the new params is skipped, and the old response is still committed at `usePolling.ts:83-91`. The UI can therefore show data for the previous query until the next interval; in the hidden-tab case, or after an immediate navigation, the skipped refresh may not be corrected promptly. This violates the “reactive params re-fetch” behavior the wrappers promise while still looking green in the current tests.

Concrete fix: make refresh requests coalesce instead of disappear. One approach is to add a `pendingRefresh` flag inside `usePolling`: when `refresh()` is called during `inFlight`, set the flag and return; in `finally`, if still active/visible and `pendingRefresh` is true, clear it and run one more refresh. For param-sensitive resources, also use a generation token so results from an older query cannot overwrite state after params changed. Add a gated-fetch test where `useLogs` starts with `group=all`, the group changes while the first fetch is blocked, then the old fetch resolves; assert the final data came from a second fetch using the new params and that the two fetches never overlapped.

WARN: `usePolling` still commits state after `stop()`/unmount.

`stop()` at `apps/dashboard/web/src/composables/usePolling.ts:124-132` clears the interval and listener, but an already-running `refresh()` continues and writes `data`, `error`, `lastUpdated`, and `loading` in `usePolling.ts:83-95`. In a component lifecycle path (`usePolling.ts:135-139`), unmounting during a slow fetch can still mutate the disposed handle after cleanup. This is exactly the race the milestone called out to verify; the current test at `apps/dashboard/web/test/usePolling.test.ts:48-58` only proves `start()` calls once and `stop()` is callable, not that a pending request is suppressed after disposal.

Concrete fix: pair the coalescing fix above with an `activeGeneration` or `disposed` guard. Capture the generation before awaiting the fetcher, and before every state write in both the success/error path and `finally`, verify the handle is still active and the generation still matches. Alternatively, wire an `AbortController` through `fetchJson`, abort on `stop()`, and still keep the generation guard for fetchers that ignore abort. Add a test that starts a gated fetch, calls `stop()` before releasing it, then asserts `data`, `error`, `lastUpdated`, and `loading` are not changed by the late resolution.

NIT: Some R/A/G token usage bypasses the single presentation mapping.

The main severity surfaces correctly use `lib/severity.ts`, but there are still direct component-level R/A/G classes at `apps/dashboard/web/src/components/LogViewer.vue:16-20`, `apps/dashboard/web/src/components/RunRow.vue:40`, and `apps/dashboard/web/src/pages/RunsPage.vue:55`. That weakens the “one Severity→R/A/G mapping / no per-component R/A/G color hard-coding” rule, even though the classes are token-based rather than raw colors.

Concrete fix: replace the direct `text-rag-*` strings with `RAG_PRESENTATION.red.textClass` / `.amber.textClass`, or add a small `priorityRag()` helper in `lib/severity.ts` for syslog priority. The alert UI variants can remain centralized in the UI component, but page/domain components should not name R/A/G classes directly.

NIT: Web schema test coverage does not exercise every reader called by the endpoint map.

`apps/dashboard/web/test/schema.test.ts` covers `packagesReader` and `overviewReader`, and `apps/dashboard/web/test/client.test.ts` indirectly covers `runsReader` through `fetchRuns()`. I did not find equivalent web-side malformed/fixture coverage for `logsReader` or `healthReader`, even though `apps/dashboard/web/src/api/endpoints.ts:57-65` exposes both through the same validation path. The readers are simple and appear faithful to the M3 DTOs, but this leaves a false-green hole for endpoint-map or reader drift.

Concrete fix: add schema/client tests for `/api/logs` and `/api/health`: a valid camelCase payload, a malformed field that returns `{ ok:false, code:"schema" }`, and one degraded envelope pass-through for an endpoint other than runs.

Tests not run during this review; I avoided workspace scripts because the dashboard test/build commands run stamp/build steps and the review request allowed modifying only this review file.

VERDICT: FIXES_REQUIRED
