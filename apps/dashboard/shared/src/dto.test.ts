/**
 * Contract tests: every captured Spike-B fixture in `apps/dashboard/fixtures/` parses through its
 * validator, and the load-bearing invariants hold (running ⇒ neutral + null duration; empty-packages
 * manifest; sparse coalesced journal lines; timer group derivation + µs→ms).
 */
import { describe, expect, test } from "bun:test";
import { resolve } from "node:path";
import {
  parseLogLine,
  parsePackage,
  parseRunRecord,
  parseStatus,
  parseTimer,
  safeParseStatus,
} from "./dto.ts";
import { severityOf } from "./exit-class.ts";
import { runDurationMs } from "./mapping.ts";

const FIXTURES = resolve(import.meta.dir, "../../fixtures");
const fixture = (name: string): Promise<unknown> => Bun.file(resolve(FIXTURES, name)).json();

describe("StatusDTO ← status.json", () => {
  test("parses the real status fixture", async () => {
    const status = parseStatus(await fixture("status.json"), "$");
    expect(status.overall).toBe("stale");
    expect(status.corpus).toBe("core");
    expect(status.activeBaselineId).toBe("core-bootstrap-v1");
    expect(status.publishedHeadSequence).toBe(1);
    expect(status.updateLockHeld).toBe(true); // in-flight lock is normal, not an error
    expect(status.groups.length).toBe(2);

    const legislation = status.groups[0];
    expect(legislation?.group).toBe("legislation");
    expect(legislation?.lastOutcome).toBe("running");
    expect(legislation?.lastExitClass).toBe("running");
    expect(legislation?.lastEndedAt).toBeNull();
    expect(legislation?.fetchCursors[0]?.latestFileName).toBe("LEGI_20260629-223234.tar.gz");
    // outcome-first: a running group is neutral, never a failure.
    expect(severityOf(legislation?.lastOutcome, legislation?.lastExitClass)).toBe("neutral");

    const jurisprudence = status.groups[1];
    expect(jurisprudence?.lastRunId).toBeNull(); // never ran
    expect(jurisprudence?.lastOutcome).toBeNull();
    expect(severityOf(jurisprudence?.lastOutcome, jurisprudence?.lastExitClass)).toBe("neutral");
    expect(jurisprudence?.fetchCursors[0]?.latestFileName).toBeNull();
  });

  test("safeParse rejects malformed input without throwing", () => {
    const result = safeParseStatus({ overall: "nope" });
    expect(result.ok).toBe(false);
  });
});

describe("RunRecordDTO ← run records", () => {
  test.each([
    "runrecord-legislation-running-real.json",
    "runrecord-running-synthetic.json",
  ])("running record %s ⇒ neutral severity + null duration", async (name) => {
    const record = parseRunRecord(await fixture(name), "$");
    expect(record.outcome).toBe("running");
    expect(record.exitClass).toBe("running");
    expect(record.endedAt).toBeNull();
    expect(record.fetchCursors).toEqual([]);
    expect(record.packageHighWaterMark).toBeNull();
    expect(severityOf(record.outcome, record.exitClass)).toBe("neutral");
    expect(runDurationMs(record.startedAt, record.endedAt)).toBeNull();
  });

  test("finished success record ⇒ ok severity + positive duration", async () => {
    const record = parseRunRecord(
      await fixture("runrecord-legislation-finished-synthetic.json"),
      "$",
    );
    expect(record.outcome).toBe("success");
    expect(record.exitClass).toBe("published");
    expect(record.publishedPackage).toBe("core-1-2");
    expect(record.packageHighWaterMark?.headSequence).toBe(2);
    expect(record.ingestJournals[0]?.archivesIngested).toBe(1);
    expect(severityOf(record.outcome, record.exitClass)).toBe("ok");
    expect(runDurationMs(record.startedAt, record.endedAt)).toBe(737_000); // 12m17s
  });

  test("no-op record keeps producer-valid NULL coordinates (Option<T>) — not dropped/defaulted", async () => {
    const record = parseRunRecord(await fixture("runrecord-legislation-noop-synthetic.json"), "$");
    expect(record.outcome).toBe("success");
    expect(record.exitClass).toBe("no-op");
    // IngestJournalCoordinate.run_id / .journal_compact_timestamp are Option<String> → null survives.
    const journal = record.ingestJournals[0];
    expect(journal?.source).toBe("legi");
    expect(journal?.runId).toBeNull();
    expect(journal?.journalCompactTimestamp).toBeNull();
    expect(journal?.archivesIngested).toBe(0);
    // PackageHighWaterMark.head_sequence / .included_change_seq_high are Option<u64> → null survives.
    expect(record.packageHighWaterMark).not.toBeNull();
    expect(record.packageHighWaterMark?.corpus).toBe("core");
    expect(record.packageHighWaterMark?.headSequence).toBeNull();
    expect(record.packageHighWaterMark?.includedChangeSeqHigh).toBeNull();
    expect(severityOf(record.outcome, record.exitClass)).toBe("ok");
  });
});

describe("PackageDTO ← Signed<RemoteManifest>", () => {
  test("unwraps payload, handles EMPTY packages, ignores forward-compat fields", async () => {
    const manifest = parsePackage(await fixture("manifest.json"), "$");
    expect(manifest.headSequence).toBe(1);
    expect(manifest.corpus).toBe("core");
    expect(manifest.packages).toEqual([]); // zero increments published yet
    expect(manifest.activeBaseline.baselineId).toBe("core-bootstrap-v1");
    expect(manifest.activeBaseline.packageKind).toBe("baseline");
    expect(manifest.activeBaseline.compressedSizeBytes).toBe(155_435_205_911);
    expect(manifest.manifestGeneratedAt).toBe("2026-06-30T08:32:03Z");
  });
});

describe("LogLineDTO ← journalctl -o json", () => {
  test("coalesces _SYSTEMD_UNIT ?? UNIT; parses string priority/timestamp", async () => {
    const line = parseLogLine(await fixture("journal-legislation.json"), "$");
    expect(line.unit).toBe("init.scope"); // _SYSTEMD_UNIT present → wins the coalesce
    expect(line.priority).toBe(6); // from the string "6"
    expect(line.timestamp).toBe(1_782_819_024_079); // µs string → ms
    expect(line.message?.startsWith("Starting jurisearch-producer-legislation")).toBe(true);
  });

  test("tolerates a service-emitted line (unit only in _SYSTEMD_UNIT) and missing fields", () => {
    const line = parseLogLine(
      { MESSAGE: "fetch ok", _SYSTEMD_UNIT: "jurisearch-producer-legislation.service" },
      "$",
    );
    expect(line.unit).toBe("jurisearch-producer-legislation.service");
    expect(line.timestamp).toBeNull();
    expect(line.priority).toBeNull();
  });
});

describe("TimerDTO ← systemctl list-timers -o json", () => {
  test("derives group from unit, maps unit/activates, converts µs→ms", async () => {
    const timers = (await fixture("timers.json")) as unknown[];
    const parsed = timers.map((t) => parseTimer(t, "$"));
    expect(parsed.map((t) => t.group)).toEqual(["legislation", "jurisprudence"]);

    const legi = parsed[0];
    expect(legi?.timerUnit).toBe("jurisearch-producer-legislation.timer");
    expect(legi?.serviceUnit).toBe("jurisearch-producer-legislation.service");
    expect(legi?.nextRun).toBe(1_782_859_358_634); // µs → ms (floored)
    expect(legi?.lastRun).toBe(1_782_818_696_168);
  });

  test("a 0/absent timer time maps to null, not 1970", () => {
    const timer = parseTimer(
      {
        unit: "jurisearch-producer-jurisprudence.timer",
        activates: "jurisearch-producer-jurisprudence.service",
        next: 1_782_863_290_585_276,
        last: 0,
      },
      "$",
    );
    expect(timer.lastRun).toBeNull();
    expect(timer.nextRun).not.toBeNull();
  });
});
