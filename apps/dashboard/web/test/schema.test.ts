import { expect, test } from "bun:test";
import {
  type HealthDTO,
  type OverviewDTO,
  parseLogLine,
  safeParse,
  safeParsePackage,
} from "@jurisearch-dashboard/shared";
import { healthReader, logsReader, overviewReader, packagesReader } from "@/api/schema.ts";
import journalLine from "../../fixtures/journal-legislation.json";
import manifestFixture from "../../fixtures/manifest-with-increment-synthetic.json";

// Derive a camelCase PackageManifestDTO from the SIGNED manifest fixture (the shared validator
// unwraps `payload`), then re-validate it through the web reader — the body the API actually serves.
const manifest = (() => {
  const parsed = safeParsePackage(manifestFixture);
  if (!parsed.ok) {
    throw new Error(parsed.error);
  }
  return JSON.parse(JSON.stringify(parsed.value)) as unknown;
})();

test("packagesReader accepts the unwrapped camelCase manifest", () => {
  const result = safeParse(packagesReader, manifest);
  expect(result.ok).toBe(true);
  if (result.ok) {
    expect(result.value.activeBaseline.baselineId).toBe("core-bootstrap-v1");
    expect(result.value.packages).toHaveLength(1);
    expect(result.value.packages[0]?.rowCounts.documents).toBe(1284);
  }
});

test("packagesReader rejects a wrong-typed field (drift degrades the panel)", () => {
  const bad = { ...(manifest as Record<string, unknown>), headSequence: "two" };
  expect(safeParse(packagesReader, bad).ok).toBe(false);
});

// A minimal, fixture-shaped OverviewDTO — the server-composed body has no producer-facing validator,
// so the reader is the only runtime guard for it.
const overview: OverviewDTO = {
  generatedAt: "2026-06-30T13:26:08Z",
  corpus: "core",
  overall: "stale",
  publishedHeadSequence: 1,
  publishedManifestGeneratedAt: "2026-06-30T08:32:03Z",
  activeBaselineId: "core-bootstrap-v1",
  updateLockHeld: true,
  groups: [
    {
      group: "legislation",
      sources: ["legi"],
      severity: "neutral",
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
    },
  ],
};

test("overviewReader accepts a composed OverviewDTO (incl. null lastRun fields)", () => {
  const result = safeParse(overviewReader, JSON.parse(JSON.stringify(overview)));
  expect(result.ok).toBe(true);
  if (result.ok) {
    expect(result.value.groups[0]?.lastRun?.durationMs).toBeNull();
    expect(result.value.groups[0]?.lastRun?.kind).toBe("incremental");
  }
});

test("overviewReader rejects an invalid severity enum", () => {
  const bad = JSON.parse(JSON.stringify(overview)) as { groups: Array<{ severity: string }> };
  const group = bad.groups[0];
  if (group) {
    group.severity = "explosive";
  }
  expect(safeParse(overviewReader, bad).ok).toBe(false);
});

// A fixture-derived camelCase LogLineDTO — the body shape `/api/logs` serves.
const logLine = JSON.parse(JSON.stringify(parseLogLine(journalLine, "$"))) as unknown;

test("logsReader accepts a camelCase log line array", () => {
  const result = safeParse(logsReader, [logLine]);
  expect(result.ok).toBe(true);
  if (result.ok) {
    expect(result.value[0]?.unit).toBe("jurisearch-producer-legislation.service");
    expect(typeof result.value[0]?.timestamp).toBe("number");
  }
});

test("logsReader rejects a wrong-typed field (drift degrades the panel)", () => {
  const bad = { ...(logLine as Record<string, unknown>), timestamp: "not-a-number" };
  expect(safeParse(logsReader, [bad]).ok).toBe(false);
});

const health: HealthDTO = {
  status: "ok",
  name: "Juridia — Update Server",
  version: "jurisearch-dashboard 0.1.0 (abc, x86_64-unknown-linux-gnu)",
  uptimeMs: 1234,
  now: "2026-06-30T16:14:31.282Z",
};

test("healthReader accepts a valid HealthDTO", () => {
  const result = safeParse(healthReader, JSON.parse(JSON.stringify(health)));
  expect(result.ok).toBe(true);
  if (result.ok) {
    expect(result.value.status).toBe("ok");
    expect(result.value.uptimeMs).toBe(1234);
  }
});

test("healthReader rejects an out-of-vocabulary status", () => {
  const bad = { ...health, status: "exploded" };
  expect(safeParse(healthReader, bad).ok).toBe(false);
});

test("healthReader rejects a wrong-typed uptime", () => {
  const bad = { ...health, uptimeMs: "soon" };
  expect(safeParse(healthReader, bad).ok).toBe(false);
});
