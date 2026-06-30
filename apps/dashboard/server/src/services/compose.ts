/**
 * The composition of raw providers → `Cached` → services (the seam `services/types.ts` documents),
 * factored out of `main.ts` so it is unit-testable with counting providers (Codex M3 WARN: every
 * `cache.*Ms` TTL must actually be consumed, and `/api/overview` must reuse the runs cache rather
 * than duplicating provider I/O).
 *
 * EACH source provider is wrapped in its OWN `Cached` with its own TTL: status/timers/runs feed
 * `OverviewService` through the CACHED handles (so `statusMs`/`timersMs`/`runsMs` bound the I/O and
 * the overview join shares the same all-groups runs slot as `/api/runs`). A thin overview-level
 * `Cached` (`overviewMs`) adds page-level dedupe on top — distinct layer, not a redundant re-cache of
 * the same bytes. The clock is injected so tests advance it deterministically.
 */

import type {
  LogLineDTO,
  PackageManifestDTO,
  RunRecordDTO,
  StatusDTO,
  TimerDTO,
} from "@jurisearch-dashboard/shared";
import { Cached, type Clock } from "../cache/Cached.ts";
import type { CacheTtls } from "../config.ts";
import type { LogsQuery } from "../providers/logs.ts";
import type { RunsQuery } from "../providers/runs.ts";
import type { DataProvider } from "../providers/types.ts";
import { OverviewService } from "./OverviewService.ts";
import type { DashboardServices } from "./types.ts";

/** A provider whose `get` takes an optional query (runs/logs). */
interface QueryProvider<T, Q> {
  get(query?: Q): Promise<T>;
}

/** The raw (un-cached) providers the composition root constructs from the real adapters. */
export interface RawProviders {
  status: DataProvider<StatusDTO>;
  runs: QueryProvider<RunRecordDTO[], RunsQuery>;
  packages: DataProvider<PackageManifestDTO>;
  logs: QueryProvider<LogLineDTO[], LogsQuery>;
  timers: DataProvider<TimerDTO[]>;
}

/** Stable cache keys for the parameterised providers (one TTL slot per distinct query). */
const runsKey = (q?: RunsQuery): string => JSON.stringify([q?.group ?? null, q?.limit ?? null]);
const logsKey = (q?: LogsQuery): string =>
  JSON.stringify([q?.group ?? null, q?.limit ?? null, q?.since ?? null]);

/**
 * Wire the cached providers + services. `clock` defaults to wall-clock; tests pass a manual clock.
 */
export function composeServices(
  raw: RawProviders,
  cache: CacheTtls,
  clock: Clock = Date.now,
): DashboardServices {
  const statusCached = new Cached(() => raw.status.get(), cache.statusMs, clock);
  const timersCached = new Cached(() => raw.timers.get(), cache.timersMs, clock);
  const packagesCached = new Cached<PackageManifestDTO>(
    () => raw.packages.get(),
    cache.packagesMs,
    clock,
  );
  const runsCached = new Cached<RunRecordDTO[], [RunsQuery?]>(
    (q) => raw.runs.get(q),
    cache.runsMs,
    clock,
    runsKey,
  );
  const logsCached = new Cached<LogLineDTO[], [LogsQuery?]>(
    (q) => raw.logs.get(q),
    cache.logsMs,
    clock,
    logsKey,
  );

  // OverviewService consumes the CACHED status/runs/timers. It calls runs with no args → the
  // all-groups slot, the same one `/api/runs` (no filter) hits, so the overview poll shares that I/O.
  const overviewService = new OverviewService(
    statusCached,
    { get: () => runsCached.get() },
    timersCached,
  );
  const overviewCached = new Cached(() => overviewService.get(), cache.overviewMs, clock);

  return {
    overview: () => overviewCached.get(),
    packages: () => packagesCached.get(),
    runs: (q) => runsCached.get(q),
    logs: (q) => logsCached.get(q),
  };
}
