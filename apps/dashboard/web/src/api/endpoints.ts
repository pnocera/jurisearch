/**
 * web/ — the ONE endpoint map (design §6.1): the single place that knows each `/api/*` URL and the
 * shared-toolkit reader that validates its body. Composables depend on these bound fetchers, never on
 * raw paths or `fetchJson` directly, so a URL or query shape changes in exactly one place.
 */

import type {
  ApiResult,
  HealthResponse,
  LogsResponse,
  OverviewResponse,
  PackagesResponse,
  RunsResponse,
} from "@jurisearch-dashboard/shared";
import { fetchJson } from "./client.ts";
import { healthReader, logsReader, overviewReader, packagesReader, runsReader } from "./schema.ts";

/** A `?group=&limit=` style query for the runs/logs endpoints. */
export interface RunsParams {
  group?: string;
  limit?: number;
}

export interface LogsParams {
  group?: string;
  limit?: number;
  since?: string;
}

/** Build a path + query string, omitting empty params (so caches/keys stay stable). */
function withQuery(path: string, params: Record<string, string | number | undefined>): string {
  const query = new URLSearchParams();
  for (const [key, value] of Object.entries(params)) {
    if (value !== undefined && value !== "") {
      query.set(key, String(value));
    }
  }
  const qs = query.toString();
  return qs === "" ? path : `${path}?${qs}`;
}

export function fetchOverview(): Promise<ApiResult<OverviewResponse>> {
  return fetchJson("/api/overview", overviewReader);
}

export function fetchRuns(params: RunsParams = {}): Promise<ApiResult<RunsResponse>> {
  return fetchJson(
    withQuery("/api/runs", { group: params.group, limit: params.limit }),
    runsReader,
  );
}

export function fetchPackages(): Promise<ApiResult<PackagesResponse>> {
  return fetchJson("/api/packages", packagesReader);
}

export function fetchLogs(params: LogsParams = {}): Promise<ApiResult<LogsResponse>> {
  return fetchJson(
    withQuery("/api/logs", { group: params.group, limit: params.limit, since: params.since }),
    logsReader,
  );
}

export function fetchHealth(): Promise<ApiResult<HealthResponse>> {
  return fetchJson("/api/health", healthReader);
}
