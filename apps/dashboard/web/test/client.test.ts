import { afterEach, beforeEach, expect, test } from "bun:test";
import { parseLogLine, safeParseRunRecord } from "@jurisearch-dashboard/shared";
import { fetchHealth, fetchLogs, fetchRuns } from "@/api/endpoints.ts";
import journalLine from "../../fixtures/journal-legislation.json";
import finishedRun from "../../fixtures/runrecord-legislation-finished-synthetic.json";

// A fixture-derived camelCase RunRecordDTO — exactly the shape the M3 server returns in
// `{ ok:true, data:[…] }`.
const record = (() => {
  const parsed = safeParseRunRecord(finishedRun);
  if (!parsed.ok) {
    throw new Error(parsed.error);
  }
  return parsed.value;
})();

const logLine = parseLogLine(journalLine, "$");

const realFetch = globalThis.fetch;
function mockFetch(impl: () => Response | Promise<Response>): void {
  globalThis.fetch = (async () => impl()) as typeof fetch;
}

beforeEach(() => {
  globalThis.fetch = realFetch;
});
afterEach(() => {
  globalThis.fetch = realFetch;
});

test("ok:true envelope validates through the shared-toolkit reader", async () => {
  mockFetch(() => Response.json({ ok: true, data: [record] }));
  const result = await fetchRuns();
  expect(result.ok).toBe(true);
  if (result.ok) {
    expect(result.data).toHaveLength(1);
    expect(result.data[0]?.runId).toBe(record.runId);
    expect(result.data[0]?.exitClass).toBe("published");
  }
});

test("server degraded envelope passes through verbatim (no throw)", async () => {
  mockFetch(() => Response.json({ ok: false, error: { code: "runs", message: "cannot read" } }));
  const result = await fetchRuns();
  expect(result.ok).toBe(false);
  if (!result.ok) {
    expect(result.error.code).toBe("runs");
    expect(result.error.message).toBe("cannot read");
  }
});

test("non-2xx → transport degraded", async () => {
  mockFetch(() => new Response("boom", { status: 500 }));
  const result = await fetchRuns();
  expect(result.ok).toBe(false);
  if (!result.ok) {
    expect(result.error.code).toBe("transport");
  }
});

test("malformed envelope → schema degraded", async () => {
  mockFetch(() => Response.json({ unexpected: true }));
  const result = await fetchRuns();
  expect(result.ok).toBe(false);
  if (!result.ok) {
    expect(result.error.code).toBe("schema");
  }
});

test("ok:true with invalid data → schema degraded", async () => {
  mockFetch(() => Response.json({ ok: true, data: [{ runId: 123 }] }));
  const result = await fetchRuns();
  expect(result.ok).toBe(false);
  if (!result.ok) {
    expect(result.error.code).toBe("schema");
  }
});

test("fetch rejection → transport degraded (never throws)", async () => {
  globalThis.fetch = (async () => {
    throw new Error("network down");
  }) as typeof fetch;
  const result = await fetchRuns();
  expect(result.ok).toBe(false);
  if (!result.ok) {
    expect(result.error.code).toBe("transport");
    expect(result.error.message).toContain("network down");
  }
});

// ── /api/logs (logsReader) and /api/health (healthReader) through the same fetchJson path ──────────

test("fetchLogs: ok:true validates a camelCase log line through logsReader", async () => {
  mockFetch(() => Response.json({ ok: true, data: [logLine] }));
  const result = await fetchLogs({ group: "legislation" });
  expect(result.ok).toBe(true);
  if (result.ok) {
    expect(result.data[0]?.unit).toBe("jurisearch-producer-legislation.service");
  }
});

test("fetchLogs: ok:true with an invalid field → schema degraded", async () => {
  mockFetch(() => Response.json({ ok: true, data: [{ ...logLine, priority: "high" }] }));
  const result = await fetchLogs();
  expect(result.ok).toBe(false);
  if (!result.ok) {
    expect(result.error.code).toBe("schema");
  }
});

test("fetchHealth: ok:true validates through healthReader", async () => {
  mockFetch(() =>
    Response.json({
      ok: true,
      data: {
        status: "ok",
        name: "Juridia — Update Server",
        version: "jurisearch-dashboard 0.1.0 (abc, x86_64-unknown-linux-gnu)",
        uptimeMs: 10,
        now: "2026-06-30T16:14:31.282Z",
      },
    }),
  );
  const result = await fetchHealth();
  expect(result.ok).toBe(true);
  if (result.ok) {
    expect(result.data.status).toBe("ok");
  }
});

test("fetchHealth: ok:true with an invalid field → schema degraded", async () => {
  mockFetch(() => Response.json({ ok: true, data: { status: "exploded" } }));
  const result = await fetchHealth();
  expect(result.ok).toBe(false);
  if (!result.ok) {
    expect(result.error.code).toBe("schema");
  }
});

test("fetchHealth: server degraded envelope passes through verbatim (endpoint other than runs)", async () => {
  mockFetch(() => Response.json({ ok: false, error: { code: "router", message: "health off" } }));
  const result = await fetchHealth();
  expect(result.ok).toBe(false);
  if (!result.ok) {
    expect(result.error.code).toBe("router");
    expect(result.error.message).toBe("health off");
  }
});
