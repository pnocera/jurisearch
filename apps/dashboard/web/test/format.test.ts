import { describe, expect, test } from "bun:test";
import { safeParsePackage, safeParseRunRecord } from "@jurisearch-dashboard/shared";
import {
  absoluteTime,
  EMPTY,
  formatBytes,
  formatCount,
  formatDuration,
  orEmpty,
  relativeTime,
  runDuration,
  sequenceRange,
} from "@/lib/format.ts";
import manifestFixture from "../../fixtures/manifest-with-increment-synthetic.json";
import finishedRun from "../../fixtures/runrecord-legislation-finished-synthetic.json";

describe("relativeTime", () => {
  const now = Date.parse("2026-06-30T13:00:30Z");
  test("formats past seconds", () => {
    expect(relativeTime("2026-06-30T13:00:00Z", now)).toBe("30 seconds ago");
  });
  test("formats future minutes", () => {
    expect(relativeTime("2026-06-30T13:30:30Z", now)).toBe("in 30 minutes");
  });
  test("null/unparseable → EMPTY", () => {
    expect(relativeTime(null)).toBe(EMPTY);
    expect(relativeTime("not-a-date")).toBe(EMPTY);
  });
});

describe("runDuration (derived via shared runDurationMs)", () => {
  const run = (() => {
    const parsed = safeParseRunRecord(finishedRun);
    if (!parsed.ok) {
      throw new Error(parsed.error);
    }
    return parsed.value;
  })();

  test("formats a finished run's duration", () => {
    // 2026-06-29T11:30:00Z → 11:42:17Z = 12m 17s.
    expect(runDuration(run.startedAt, run.endedAt)).toBe("12m 17s");
  });
  test("null endedAt (in flight) → EMPTY", () => {
    expect(runDuration(run.startedAt, null)).toBe(EMPTY);
  });
  test("sub-second → ms", () => {
    expect(formatDuration(450)).toBe("450ms");
  });
});

describe("formatBytes", () => {
  const baseline = (() => {
    const parsed = safeParsePackage(manifestFixture);
    if (!parsed.ok) {
      throw new Error(parsed.error);
    }
    return parsed.value.activeBaseline;
  })();

  test("formats a multi-GiB baseline", () => {
    expect(formatBytes(baseline.compressedSizeBytes)).toContain("GiB");
  });
  test("small values stay bytes", () => {
    expect(formatBytes(512)).toBe("512 B");
  });
  test("null → EMPTY", () => {
    expect(formatBytes(null)).toBe(EMPTY);
  });
});

describe("misc formatters", () => {
  test("sequenceRange from→to", () => {
    expect(sequenceRange(1, 2)).toBe("1 → 2");
    expect(sequenceRange(5, 5)).toBe("5");
    expect(sequenceRange(null, null)).toBe(EMPTY);
  });
  test("formatCount groups thousands", () => {
    expect(formatCount(1284)).toBe("1,284");
    expect(formatCount(null)).toBe(EMPTY);
  });
  test("orEmpty substitutes", () => {
    expect(orEmpty(null)).toBe(EMPTY);
    expect(orEmpty("")).toBe(EMPTY);
    expect(orEmpty("x")).toBe("x");
  });
  test("absoluteTime renders ISO-ish", () => {
    expect(absoluteTime("2026-06-30T13:00:00Z")).toBe("2026-06-30 13:00:00Z");
    expect(absoluteTime(null)).toBe(EMPTY);
  });
});
