/**
 * web/ — the per-resource composables (design §6.1). Each is a THIN wrapper over the single
 * `usePolling` primitive bound to one endpoint from the `api/` map; none duplicates fetch/loading/
 * error logic. `useRuns`/`useLogs` accept reactive params and re-fetch when they change.
 *
 * The poll intervals mirror the server's per-resource cache TTLs (`resolve.ts` DEFAULT_CONFIG) so the
 * client refreshes about as often as the data can change, without hammering past the cache.
 */

import { type MaybeRefOrGetter, toValue, watch } from "vue";
import {
  fetchLogs,
  fetchOverview,
  fetchPackages,
  fetchRuns,
  type LogsParams,
  type RunsParams,
} from "../api/endpoints.ts";
import { usePolling } from "./usePolling.ts";

/** Poll cadences (ms), aligned with the server cache TTLs. */
export const POLL_INTERVALS = {
  overview: 4000,
  runs: 5000,
  packages: 20000,
  logs: 3000,
} as const;

export function useOverview() {
  return usePolling(fetchOverview, POLL_INTERVALS.overview);
}

export function usePackages() {
  return usePolling(fetchPackages, POLL_INTERVALS.packages);
}

/** Reactive params for the runs view; re-fetches whenever `group`/`limit` change. */
export interface UseRunsOptions {
  group?: MaybeRefOrGetter<string | undefined>;
  limit?: MaybeRefOrGetter<number | undefined>;
}

export function useRuns(options: UseRunsOptions = {}) {
  const params = (): RunsParams => ({
    group: toValue(options.group),
    limit: toValue(options.limit),
  });
  const handle = usePolling(() => fetchRuns(params()), POLL_INTERVALS.runs);
  watch(
    () => [toValue(options.group), toValue(options.limit)],
    () => {
      void handle.refresh();
    },
  );
  return handle;
}

/** Reactive params for the logs view; re-fetches whenever `group`/`limit`/`since` change. */
export interface UseLogsOptions {
  group?: MaybeRefOrGetter<string | undefined>;
  limit?: MaybeRefOrGetter<number | undefined>;
  since?: MaybeRefOrGetter<string | undefined>;
}

export function useLogs(options: UseLogsOptions = {}) {
  const params = (): LogsParams => ({
    group: toValue(options.group),
    limit: toValue(options.limit),
    since: toValue(options.since),
  });
  const handle = usePolling(() => fetchLogs(params()), POLL_INTERVALS.logs);
  watch(
    () => [toValue(options.group), toValue(options.limit), toValue(options.since)],
    () => {
      void handle.refresh();
    },
  );
  return handle;
}
