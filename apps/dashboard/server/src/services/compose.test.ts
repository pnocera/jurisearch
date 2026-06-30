/**
 * Composition tests — prove the `raw providers → Cached → services` wiring actually consumes EACH
 * `cache.*Ms` TTL knob (Codex M3 WARN), using COUNTING providers + an injected clock:
 *   - status/runs/timers are cached at their own TTLs (the overview join reuses the cached handles,
 *     so `statusMs`/`timersMs`/`runsMs` bound the I/O, not just `overviewMs`),
 *   - `/api/overview` shares the all-groups runs slot with `/api/runs` (no duplicate provider I/O),
 *   - packages/logs are cached at `packagesMs`/`logsMs`.
 */

import { describe, expect, test } from "bun:test";
import { resolve } from "node:path";
import {
  arrayOf,
  type LogLineDTO,
  type PackageManifestDTO,
  parseTimer,
  type RunRecordDTO,
  type StatusDTO,
  safeParse,
  safeParseLogLine,
  safeParsePackage,
  safeParseRunRecord,
  safeParseStatus,
  type TimerDTO,
} from "@jurisearch-dashboard/shared";
import type { CacheTtls } from "../config.ts";
import type { LogsQuery } from "../providers/logs.ts";
import type { RunsQuery } from "../providers/runs.ts";
import { composeServices, type RawProviders } from "./compose.ts";

const FIXTURES = resolve(import.meta.dir, "../../../fixtures");
const fixtureJson = (name: string): Promise<unknown> => Bun.file(resolve(FIXTURES, name)).json();

function unwrap<T>(result: { ok: true; value: T } | { ok: false; error: string }): T {
  if (!result.ok) {
    throw new Error(result.error);
  }
  return result.value;
}

/** Counts every `get()` and returns a fixed value. */
class Counter<T> {
  calls = 0;
  constructor(private readonly value: T) {}
  get(): Promise<T> {
    this.calls += 1;
    return Promise.resolve(this.value);
  }
}

/** Counts every `get(query?)`, recording the last query. */
class QueryCounter<T, Q> {
  calls = 0;
  constructor(private readonly value: T) {}
  get(_query?: Q): Promise<T> {
    this.calls += 1;
    return Promise.resolve(this.value);
  }
}

interface Harness {
  services: ReturnType<typeof composeServices>;
  status: Counter<StatusDTO>;
  runs: QueryCounter<RunRecordDTO[], RunsQuery>;
  packages: Counter<PackageManifestDTO>;
  logs: QueryCounter<LogLineDTO[], LogsQuery>;
  timers: Counter<TimerDTO[]>;
  setClock: (ms: number) => void;
}

// Distinct TTLs so a provider-cache hit is distinguishable from an overview-cache hit:
// overviewMs(10) < statusMs/runsMs/timersMs(100) so the overview cache can expire while the
// underlying provider caches are still warm.
const CACHE: CacheTtls = {
  statusMs: 100,
  overviewMs: 10,
  runsMs: 100,
  packagesMs: 100,
  logsMs: 100,
  timersMs: 100,
};

async function harness(): Promise<Harness> {
  const status = new Counter(unwrap(safeParseStatus(await fixtureJson("status.json"))));
  const runs = new QueryCounter<RunRecordDTO[], RunsQuery>([
    unwrap(safeParseRunRecord(await fixtureJson("runrecord-legislation-running-real.json"))),
  ]);
  const packages = new Counter(unwrap(safeParsePackage(await fixtureJson("manifest.json"))));
  const logs = new QueryCounter<LogLineDTO[], LogsQuery>([
    unwrap(safeParseLogLine(await fixtureJson("journal-legislation.json"))),
  ]);
  const timers = new Counter(
    unwrap(safeParse(arrayOf(parseTimer), await fixtureJson("timers.json"))),
  );

  let now = 0;
  const raw: RawProviders = { status, runs, packages, logs, timers };
  const services = composeServices(raw, CACHE, () => now);
  return { services, status, runs, packages, logs, timers, setClock: (ms) => (now = ms) };
}

describe("composeServices — every cache TTL is consumed", () => {
  test("status/runs/timers are cached at their own TTL (overview reuses the cached handles)", async () => {
    const h = await harness();

    await h.services.overview();
    expect(h.status.calls).toBe(1);
    expect(h.timers.calls).toBe(1);
    expect(h.runs.calls).toBe(1);

    // Past overviewMs(10) but within statusMs/timersMs/runsMs(100): the overview join recomputes,
    // yet the underlying providers stay cached ⇒ those TTL knobs are doing the work.
    h.setClock(20);
    await h.services.overview();
    expect(h.status.calls).toBe(1);
    expect(h.timers.calls).toBe(1);
    expect(h.runs.calls).toBe(1);

    // Past the provider TTLs: each is re-invoked exactly once.
    h.setClock(150);
    await h.services.overview();
    expect(h.status.calls).toBe(2);
    expect(h.timers.calls).toBe(2);
    expect(h.runs.calls).toBe(2);
  });

  test("/api/overview shares the all-groups runs slot with /api/runs (no duplicate I/O)", async () => {
    const h = await harness();

    await h.services.overview(); // overview join reads runs (no filter) → all-groups slot
    await h.services.runs(); // same all-groups slot ⇒ served from cache
    expect(h.runs.calls).toBe(1);

    await h.services.runs({ group: "legislation" }); // distinct key ⇒ one more call
    expect(h.runs.calls).toBe(2);
  });

  test("packages and logs are cached at packagesMs/logsMs", async () => {
    const h = await harness();

    await h.services.packages();
    await h.services.packages();
    expect(h.packages.calls).toBe(1);

    await h.services.logs();
    await h.services.logs();
    expect(h.logs.calls).toBe(1);

    h.setClock(150); // past packagesMs/logsMs(100)
    await h.services.packages();
    await h.services.logs();
    expect(h.packages.calls).toBe(2);
    expect(h.logs.calls).toBe(2);
  });
});
