/**
 * OverviewService tests — the status × runs × timers join (design §6.2), over fixture-backed fake
 * `DataProvider`s (no I/O). Asserts the corpus header, the per-group R/A/G `severity` (outcome-first,
 * reused from shared), the last-run join (status summary + record `kind`/`startedAt` → derived
 * `durationMs`), freshness pass-through, and the timer attach.
 */

import { describe, expect, test } from "bun:test";
import { resolve } from "node:path";
import {
  arrayOf,
  parseTimer,
  type RunRecordDTO,
  type StatusDTO,
  safeParse,
  safeParseRunRecord,
  safeParseStatus,
  type TimerDTO,
} from "@jurisearch-dashboard/shared";
import type { DataProvider } from "../providers/types.ts";
import { OverviewService } from "./OverviewService.ts";

const FIXTURES = resolve(import.meta.dir, "../../../fixtures");
const fixtureJson = (name: string): Promise<unknown> => Bun.file(resolve(FIXTURES, name)).json();

function unwrap<T>(result: { ok: true; value: T } | { ok: false; error: string }): T {
  if (!result.ok) {
    throw new Error(result.error);
  }
  return result.value;
}

const provider = <T>(value: T): DataProvider<T> => ({ get: () => Promise.resolve(value) });

async function loadStatus(): Promise<StatusDTO> {
  return unwrap(safeParseStatus(await fixtureJson("status.json")));
}

async function loadRecords(): Promise<RunRecordDTO[]> {
  const names = [
    "runrecord-legislation-running-real.json",
    "runrecord-legislation-noop-synthetic.json",
    "runrecord-legislation-finished-synthetic.json",
  ];
  const records: RunRecordDTO[] = [];
  for (const name of names) {
    records.push(unwrap(safeParseRunRecord(await fixtureJson(name))));
  }
  return records;
}

async function loadTimers(): Promise<TimerDTO[]> {
  return unwrap(safeParse(arrayOf(parseTimer), await fixtureJson("timers.json")));
}

describe("OverviewService", () => {
  test("builds the OverviewDTO: corpus header + per-group join", async () => {
    const service = new OverviewService(
      provider(await loadStatus()),
      provider(await loadRecords()),
      provider(await loadTimers()),
    );
    const overview = await service.get();

    // Corpus header from status.
    expect(overview.corpus).toBe("core");
    expect(overview.overall).toBe("stale");
    expect(overview.publishedHeadSequence).toBe(1);
    expect(overview.publishedManifestGeneratedAt).toBe("2026-06-30T08:32:03Z");
    expect(overview.activeBaselineId).toBe("core-bootstrap-v1");
    expect(overview.updateLockHeld).toBe(true);
    expect(overview.groups.map((g) => g.group)).toEqual(["legislation", "jurisprudence"]);
  });

  test("legislation: in-flight run is neutral with kind/startedAt joined and null duration", async () => {
    const service = new OverviewService(
      provider(await loadStatus()),
      provider(await loadRecords()),
      provider(await loadTimers()),
    );
    const overview = await service.get();
    const leg = overview.groups.find((g) => g.group === "legislation");

    expect(leg).toBeDefined();
    // Outcome-first severity: running ⇒ neutral, NEVER a permanent failure.
    expect(leg?.severity).toBe("neutral");
    expect(leg?.lastRun?.runId).toBe("legislation-1782819024-111818663-787");
    expect(leg?.lastRun?.outcome).toBe("running");
    expect(leg?.lastRun?.exitClass).toBe("running");
    // kind + startedAt come from the matched record (status carries neither).
    expect(leg?.lastRun?.kind).toBe("incremental");
    expect(leg?.lastRun?.startedAt).toBe("2026-06-30T11:30:24Z");
    // Derived duration is null while running (endedAt null) — the documented trap.
    expect(leg?.lastRun?.durationMs).toBeNull();
    // Corpus-level denormalisation + freshness pass-through.
    expect(leg?.publishedHeadSequence).toBe(1);
    expect(leg?.updateLockHeld).toBe(true);
    expect(leg?.freshness.staleByAge).toBe(false);
    expect(leg?.nextTimer?.group).toBe("legislation");
  });

  test("jurisprudence: never-ran group ⇒ null lastRun, neutral severity, timer attached", async () => {
    const service = new OverviewService(
      provider(await loadStatus()),
      provider(await loadRecords()),
      provider(await loadTimers()),
    );
    const overview = await service.get();
    const juris = overview.groups.find((g) => g.group === "jurisprudence");

    expect(juris?.lastRun).toBeNull();
    expect(juris?.severity).toBe("neutral");
    expect(juris?.nextTimer?.group).toBe("jurisprudence");
  });

  test("a finished run derives a positive duration (reuses shared runDurationMs)", async () => {
    // Build a status whose legislation last run points at the FINISHED record.
    const status = await loadStatus();
    const legGroup = status.groups[0];
    if (legGroup === undefined) {
      throw new Error("fixture missing legislation group");
    }
    const finishedStatus: StatusDTO = {
      ...status,
      groups: [
        {
          ...legGroup,
          lastRunId: "legislation-1782730000-000000000-042",
          lastOutcome: "success",
          lastExitClass: "published",
          lastEndedAt: "2026-06-29T11:42:17Z",
        },
        ...status.groups.slice(1),
      ],
    };
    const service = new OverviewService(
      provider(finishedStatus),
      provider(await loadRecords()),
      provider(await loadTimers()),
    );
    const leg = (await service.get()).groups.find((g) => g.group === "legislation");

    expect(leg?.severity).toBe("ok"); // success outcome
    expect(leg?.lastRun?.kind).toBe("incremental");
    // 2026-06-29T11:42:17Z − 2026-06-29T11:30:00Z = 12m17s = 737000 ms.
    expect(leg?.lastRun?.durationMs).toBe(737_000);
  });

  test("missing lastRunId + a DIFFERENT run in the records ⇒ exact-id join, no field-mixing", async () => {
    // Status points at a run id that is NOT in the records; the records hold another (published) run.
    const status = await loadStatus();
    const legGroup = status.groups[0];
    if (legGroup === undefined) {
      throw new Error("fixture missing legislation group");
    }
    const mixedStatus: StatusDTO = {
      ...status,
      groups: [
        {
          ...legGroup,
          lastRunId: "legislation-DOES-NOT-EXIST-999",
          lastOutcome: "failure",
          lastExitClass: "ingest-failed",
          lastEndedAt: "2026-06-30T12:00:00Z",
        },
        ...status.groups.slice(1),
      ],
    };
    const otherRun = unwrap(
      safeParseRunRecord(await fixtureJson("runrecord-legislation-finished-synthetic.json")),
    );
    const service = new OverviewService(
      provider(mixedStatus),
      provider([otherRun]),
      provider(await loadTimers()),
    );
    const leg = (await service.get()).groups.find((g) => g.group === "legislation");

    // Status fields are kept verbatim …
    expect(leg?.lastRun?.runId).toBe("legislation-DOES-NOT-EXIST-999");
    expect(leg?.lastRun?.outcome).toBe("failure");
    expect(leg?.lastRun?.exitClass).toBe("ingest-failed");
    // … and the record-only fields are NULL (never borrowed from the other run) → no bogus duration.
    expect(leg?.lastRun?.kind).toBeNull();
    expect(leg?.lastRun?.startedAt).toBeNull();
    expect(leg?.lastRun?.durationMs).toBeNull();
    // Severity tracks the SAME (status) run shown: failure/ingest-failed ⇒ permanent.
    expect(leg?.severity).toBe("permanent");
  });
});
