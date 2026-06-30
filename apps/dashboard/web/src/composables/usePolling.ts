/**
 * web/ — the ONE polling/refresh primitive (design §6.1). Every resource view is a thin wrapper over
 * `usePolling`; none re-implements fetch/loading/error/interval logic (DRY).
 *
 * Properties:
 *   - visibility-aware: pauses the interval while the tab is hidden (`visibilitychange`) and refreshes
 *     immediately on becoming visible again — no wasted polls against a backgrounded dashboard.
 *   - non-overlapping + COALESCING: at most one fetch runs at a time; a `refresh()` requested while one
 *     is in flight (e.g. a Logs tab/window change via `resources.ts`) is NOT dropped — it is coalesced
 *     into exactly one follow-up fetch that runs (with the LATEST params) once the current one settles.
 *   - generation-guarded: each fetch captures a generation token; a `refresh()` mid-flight and `stop()`
 *     both bump it, so a superseded (stale-param) result never commits and a late resolution after
 *     `stop()`/unmount is a no-op on the disposed handle.
 *   - degraded-aware: the fetcher returns the shared `ApiResult<T>`; an `{ ok:false }` result sets
 *     `error` (the degraded panel) WITHOUT throwing and WITHOUT discarding the last good `data`
 *     (stale-while-degraded), so a transient blip doesn't blank a panel.
 *   - lifecycle-bound when used inside a component (auto start on mount / stop on unmount); fully
 *     manual (`start`/`stop`/`refresh`) when constructed outside a component (unit tests).
 */

import type { ApiResult } from "@jurisearch-dashboard/shared";
import {
  getCurrentInstance,
  getCurrentScope,
  onMounted,
  onScopeDispose,
  type Ref,
  ref,
  shallowRef,
} from "vue";

/** The degraded-panel error shape carried by `ApiResult`'s failure branch. */
export interface PollingError {
  code?: string;
  message: string;
}

export interface PollingHandle<T> {
  /** The last successfully validated payload, or `null` before the first success. */
  data: Ref<T | null>;
  /** The current degraded error, or `null` when the last refresh succeeded. */
  error: Ref<PollingError | null>;
  /** True while a refresh is in flight. */
  loading: Ref<boolean>;
  /** Epoch ms of the last SUCCESSFUL refresh (for a "last updated" line); `null` until then. */
  lastUpdated: Ref<number | null>;
  /** Run one refresh now (used by the manual refresh button and param-change watchers). */
  refresh: () => Promise<void>;
  /** Begin polling (immediate refresh + interval); idempotent. */
  start: () => void;
  /** Stop polling and detach the visibility listener; idempotent. */
  stop: () => void;
}

/** True when the document is visible (or there is no document, e.g. in tests). */
function documentVisible(): boolean {
  return typeof document === "undefined" || document.visibilityState !== "hidden";
}

export function usePolling<T>(
  fetcher: () => Promise<ApiResult<T>>,
  intervalMs: number,
): PollingHandle<T> {
  const data = shallowRef<T | null>(null);
  const error = ref<PollingError | null>(null);
  const loading = ref(false);
  const lastUpdated = ref<number | null>(null);

  /** Polling has been started (interval + visibility wiring live). */
  let active = false;
  /** The handle has been torn down (`stop()`/unmount); no further state writes are allowed. */
  let disposed = false;
  let inFlight = false;
  /** A refresh requested while one was in flight; serviced as exactly one follow-up. */
  let pendingRefresh = false;
  /** Bumped on every new request and on `stop()`; a fetch whose captured token is stale never writes. */
  let generation = 0;
  let timer: ReturnType<typeof setInterval> | null = null;

  function clearTimer(): void {
    if (timer !== null) {
      clearInterval(timer);
      timer = null;
    }
  }

  function arm(): void {
    clearTimer();
    if (intervalMs > 0) {
      timer = setInterval(() => {
        void refresh();
      }, intervalMs);
    }
  }

  /** Run one fetch end-to-end, committing only if not superseded, then service any coalesced request. */
  async function drive(): Promise<void> {
    const myGen = generation;
    inFlight = true;
    loading.value = true;
    try {
      const result = await fetcher();
      // Commit only when not disposed AND this is the newest request (params unchanged, not stopped).
      if (!disposed && myGen === generation) {
        if (result.ok) {
          data.value = result.data;
          error.value = null;
          lastUpdated.value = Date.now();
        } else {
          // Stale-while-degraded: keep the last good `data`, surface the degraded error.
          error.value = result.error;
        }
      }
    } catch (caught) {
      // The fetcher contractually never rejects; treat any escape defensively, still gen-guarded.
      if (!disposed && myGen === generation) {
        error.value = {
          code: "transport",
          message: caught instanceof Error ? caught.message : String(caught),
        };
      }
    } finally {
      inFlight = false;
    }

    // Disposed: `stop()` already settled `loading`; never touch the handle further.
    if (disposed) {
      return;
    }
    // Coalesce: a refresh requested during this fetch runs exactly once more (with the LATEST params).
    const again = pendingRefresh && documentVisible();
    pendingRefresh = false;
    if (again) {
      generation += 1;
      await drive();
      return;
    }
    loading.value = false;
  }

  async function refresh(): Promise<void> {
    // Disposed handles are fully inert: never launch a fetch or flip `loading` after teardown.
    if (disposed) {
      return;
    }
    if (inFlight) {
      // Coalesce + supersede: the in-flight (stale-param) result will be skipped; one follow-up runs.
      pendingRefresh = true;
      generation += 1;
      return;
    }
    generation += 1;
    await drive();
  }

  function onVisibilityChange(): void {
    if (!active) {
      return;
    }
    if (documentVisible()) {
      void refresh();
      arm();
    } else {
      clearTimer();
    }
  }

  function start(): void {
    // Dispose-once contract: a disposed handle never restarts (no interval armed, no fetch issued).
    if (disposed || active) {
      return;
    }
    active = true;
    if (typeof document !== "undefined") {
      document.addEventListener("visibilitychange", onVisibilityChange);
    }
    if (documentVisible()) {
      void refresh();
      arm();
    }
  }

  function stop(): void {
    if (disposed) {
      return;
    }
    disposed = true;
    active = false;
    // Invalidate any in-flight fetch so its late resolution cannot write to the disposed handle.
    generation += 1;
    pendingRefresh = false;
    inFlight = false;
    loading.value = false;
    clearTimer();
    if (typeof document !== "undefined") {
      document.removeEventListener("visibilitychange", onVisibilityChange);
    }
  }

  // Auto lifecycle: start on mount when used inside a COMPONENT; stop when the owning effect SCOPE is
  // disposed (component unmount, or an explicit `effectScope`). Outside any scope (a bare unit test)
  // neither hook registers, and the caller drives start/stop manually.
  if (getCurrentInstance() !== null) {
    onMounted(start);
  }
  if (getCurrentScope() !== undefined) {
    onScopeDispose(stop);
  }

  return { data, error, loading, lastUpdated, refresh, start, stop };
}
