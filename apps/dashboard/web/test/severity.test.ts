import { describe, expect, test } from "bun:test";
import { type OverviewFreshnessDTO, type Severity, severityOf } from "@jurisearch-dashboard/shared";
import { groupRag, overallRag, presentationOf, ragOf } from "@/lib/severity.ts";

const FRESH: OverviewFreshnessDTO = {
  staleByAge: false,
  rebaselinePending: false,
  baselines: [],
  fetchCursors: [],
};

describe("ragOf (the ONE Severity→R/A/G mapping)", () => {
  const cases: Array<[Severity, string]> = [
    ["ok", "green"],
    ["neutral", "neutral"],
    ["transient", "amber"],
    ["data", "red"],
    ["unprovisioned", "red"],
    ["config", "red"],
    ["permanent", "red"],
  ];
  for (const [severity, rag] of cases) {
    test(`${severity} → ${rag}`, () => {
      expect(ragOf(severity)).toBe(rag);
    });
  }
});

describe("severityOf → ragOf (outcome-first, from the shared contract)", () => {
  test("success/published → green", () => {
    expect(ragOf(severityOf("success", "published"))).toBe("green");
  });
  test("running → neutral", () => {
    expect(ragOf(severityOf("running", "running"))).toBe("neutral");
  });
  test("failure/fetch-failed (transient) → amber", () => {
    expect(ragOf(severityOf("failure", "fetch-failed"))).toBe("amber");
  });
  test("failure/ingest-failed (permanent) → red", () => {
    expect(ragOf(severityOf("failure", "ingest-failed"))).toBe("red");
  });
  test("never ran (null/null) → neutral", () => {
    expect(ragOf(severityOf(null, null))).toBe("neutral");
  });
});

describe("groupRag (severity overlaid with freshness)", () => {
  test("healthy + fresh stays green", () => {
    expect(groupRag("ok", FRESH)).toBe("green");
  });
  test("healthy but stale-by-age demotes to amber", () => {
    expect(groupRag("ok", { ...FRESH, staleByAge: true })).toBe("amber");
  });
  test("healthy but rebaseline pending demotes to amber", () => {
    expect(groupRag("ok", { ...FRESH, rebaselinePending: true })).toBe("amber");
  });
  test("a failure keeps its own light regardless of freshness", () => {
    expect(groupRag("permanent", { ...FRESH, staleByAge: true })).toBe("red");
  });
});

describe("overallRag (corpus header)", () => {
  test("current → green, stale → amber, broken → red", () => {
    expect(overallRag("current")).toBe("green");
    expect(overallRag("stale")).toBe("amber");
    expect(overallRag("broken")).toBe("red");
  });
});

describe("presentationOf", () => {
  test("returns themed token classes (never a hard-coded hex)", () => {
    expect(presentationOf("ok").badgeClass).toContain("rag-green");
    expect(presentationOf("permanent").dotClass).toContain("rag-red");
  });
});
