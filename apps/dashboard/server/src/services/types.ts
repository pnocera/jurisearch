/**
 * The service seam the HTTP router depends on (DIP): the router knows ONLY these four async
 * accessors, never the concrete providers/cache behind them. The composition root (`main.ts`) builds
 * the bundle — raw providers → `Cached` → here — so the router is unit-tested against a trivial fake
 * bundle and the wiring lives in exactly one place.
 *
 * `overview` and `packages` are zero-arg (page-shaped / single resource); `runs` and `logs` carry
 * their query so `/api/runs` and `/api/logs` honour `group`/`limit`/`since`.
 */

import type {
  LogLineDTO,
  OverviewDTO,
  PackageManifestDTO,
  RunRecordDTO,
} from "@jurisearch-dashboard/shared";
import type { LogsQuery } from "../providers/logs.ts";
import type { RunsQuery } from "../providers/runs.ts";

export interface DashboardServices {
  overview(): Promise<OverviewDTO>;
  runs(query?: RunsQuery): Promise<RunRecordDTO[]>;
  packages(): Promise<PackageManifestDTO>;
  logs(query?: LogsQuery): Promise<LogLineDTO[]>;
}
