/**
 * The cross-stream API contract (M3 produces, M4 consumes) is importable from the package index and
 * its shapes compose from the M1 DTOs + `severityOf`. This is mostly a TYPE-level lock (a drift here
 * breaks the parallel M3/M4 build); the runtime asserts exercise the `ApiResult` envelope + a built
 * `OverviewDTO` so the fields can't silently change.
 */

import { describe, expect, test } from "bun:test";
import type {
  ApiResult,
  HealthDTO,
  LogsResponse,
  OverviewDTO,
  OverviewGroupDTO,
  PackagesResponse,
  RunsResponse,
} from "./index.ts";
import { severityOf } from "./index.ts";

describe("ApiResult envelope", () => {
  test("ok carries data; err carries a message (+ optional code)", () => {
    const ok: ApiResult<number> = { ok: true, data: 42 };
    const err: ApiResult<number> = { ok: false, error: { code: "status", message: "boom" } };
    expect(ok.ok && ok.data).toBe(42);
    expect(!err.ok && err.error.message).toBe("boom");
  });
});

describe("OverviewDTO composes from the M1 DTOs", () => {
  test("a group entry holds the GroupCard fields with outcome-first severity", () => {
    const group: OverviewGroupDTO = {
      group: "legislation",
      sources: ["legi"],
      severity: severityOf("running", "running"), // neutral — outcome-first
      lastRun: {
        runId: "legislation-1",
        kind: "incremental",
        outcome: "running",
        exitClass: "running",
        startedAt: "2026-06-30T11:30:24Z",
        endedAt: null,
        durationMs: null,
      },
      freshness: {
        staleByAge: false,
        rebaselinePending: false,
        baselines: [],
        fetchCursors: [],
      },
      publishedHeadSequence: 1,
      updateLockHeld: true,
      nextTimer: null,
    };
    const overview: OverviewDTO = {
      generatedAt: "2026-06-30T13:26:08Z",
      corpus: "core",
      overall: "stale",
      publishedHeadSequence: 1,
      publishedManifestGeneratedAt: "2026-06-30T08:32:03Z",
      activeBaselineId: "core-bootstrap-v1",
      updateLockHeld: true,
      groups: [group],
    };
    expect(group.severity).toBe("neutral");
    expect(overview.groups[0]?.lastRun?.durationMs).toBeNull();

    // Per-endpoint aliases are the `T` inside ApiResult<T>.
    const runs: RunsResponse = [];
    const packages: PackagesResponse | null = null;
    const logs: LogsResponse = [];
    const health: HealthDTO = {
      status: "ok",
      name: "Juridia — Update Server",
      version: "0.1.0",
      uptimeMs: 0,
      now: "2026-06-30T13:26:08Z",
    };
    expect(runs).toEqual([]);
    expect(packages).toBeNull();
    expect(logs).toEqual([]);
    expect(health.status).toBe("ok");
  });
});
