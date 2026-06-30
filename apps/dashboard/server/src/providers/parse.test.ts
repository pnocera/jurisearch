/**
 * Pure-parser tests (DoD: zero real I/O). Each provider's raw→DTO function is exercised over the
 * captured Spike-B fixtures: the producer `status` stdout, the running/finished/no-op run records,
 * the empty-packages manifest, the journald NDJSON, and the timers array. Fixtures are read as bytes
 * only to FEED the pure functions — no subprocess, no provider, no network.
 */

import { describe, expect, test } from "bun:test";
import { resolve } from "node:path";
import { severityOf } from "@jurisearch-dashboard/shared";
import { parseJournalNdjson } from "./logs.ts";
import { parseManifestText } from "./packages.ts";
import { lastRunByGroup, parseRunRecordText, sortByStartedAtDesc } from "./runs.ts";
import { parseStatusStdout } from "./status.ts";
import { parseTimersStdout } from "./timers.ts";
import { ProviderError } from "./types.ts";

const FIXTURES = resolve(import.meta.dir, "../../../fixtures");
const text = (name: string): Promise<string> => Bun.file(resolve(FIXTURES, name)).text();
const json = (name: string): Promise<unknown> => Bun.file(resolve(FIXTURES, name)).json();

describe("parseStatusStdout", () => {
  test("parses real producer status stdout", async () => {
    const status = parseStatusStdout(await text("status.json"));
    expect(status.overall).toBe("stale");
    expect(status.publishedHeadSequence).toBe(1);
    expect(status.groups.map((g) => g.group)).toEqual(["legislation", "jurisprudence"]);
  });

  test("malformed stdout throws a typed ProviderError (not raw SyntaxError)", () => {
    expect(() => parseStatusStdout("not json")).toThrow(ProviderError);
    expect(() => parseStatusStdout('{"overall":"nope"}')).toThrow(ProviderError);
  });
});

describe("parseRunRecordText", () => {
  test.each([
    "runrecord-legislation-running-real.json",
    "runrecord-running-synthetic.json",
  ])("running record %s ⇒ neutral", async (name) => {
    const record = parseRunRecordText(await text(name));
    expect(record.outcome).toBe("running");
    expect(record.endedAt).toBeNull();
    expect(severityOf(record.outcome, record.exitClass)).toBe("neutral");
  });

  test("finished success record", async () => {
    const record = parseRunRecordText(await text("runrecord-legislation-finished-synthetic.json"));
    expect(record.outcome).toBe("success");
    expect(record.exitClass).toBe("published");
    expect(record.packageHighWaterMark?.headSequence).toBe(2);
  });

  test("no-op record keeps NULL coordinates", async () => {
    const record = parseRunRecordText(await text("runrecord-legislation-noop-synthetic.json"));
    expect(record.exitClass).toBe("no-op");
    expect(record.packageHighWaterMark?.headSequence).toBeNull();
    expect(record.ingestJournals[0]?.archivesIngested).toBe(0);
  });

  test("malformed record throws a typed ProviderError", () => {
    expect(() => parseRunRecordText("}{")).toThrow(ProviderError);
  });
});

describe("runs helpers (pure)", () => {
  test("sortByStartedAtDesc + lastRunByGroup pick the most recent per group", async () => {
    const finished = parseRunRecordText(
      await text("runrecord-legislation-finished-synthetic.json"),
    );
    const noop = parseRunRecordText(await text("runrecord-legislation-noop-synthetic.json"));
    const jurisRunning = parseRunRecordText(await text("runrecord-running-synthetic.json"));
    // finished startedAt 06-29T11:30; noop 06-29T12:30 (later); juris 06-30T13:40.
    const sorted = sortByStartedAtDesc([finished, noop, jurisRunning]);
    expect(sorted.map((r) => r.runId)).toEqual([jurisRunning.runId, noop.runId, finished.runId]);

    const last = lastRunByGroup([finished, noop, jurisRunning]);
    expect(last.get("legislation")?.runId).toBe(noop.runId); // 12:30 > 11:30
    expect(last.get("jurisprudence")?.runId).toBe(jurisRunning.runId);
  });
});

describe("parseManifestText", () => {
  test("unwraps payload, tolerates empty packages", async () => {
    const manifest = parseManifestText(await text("manifest.json"));
    expect(manifest.headSequence).toBe(1);
    expect(manifest.packages).toEqual([]);
    expect(manifest.activeBaseline.baselineId).toBe("core-bootstrap-v1");
  });

  test("parses a non-empty packages[] increment entry field-for-field (RemotePackageEntry)", async () => {
    const manifest = parseManifestText(await text("manifest-with-increment-synthetic.json"));
    expect(manifest.headSequence).toBe(2);
    expect(manifest.packages.length).toBe(1);
    const entry = manifest.packages[0];
    expect(entry?.packageId).toBe("core-1-2");
    expect(entry?.fromSequence).toBe(1);
    expect(entry?.toSequence).toBe(2);
    expect(entry?.compressedSizeBytes).toBe(4_823_344);
    expect(entry?.uncompressedSizeBytes).toBe(18_211_902);
    expect(entry?.rowCounts).toEqual({ documents: 1284, chunks: 9173, embeddings: 9173 });
    expect(entry?.schemaVersion).toBe(24);
    expect(entry?.embeddingFingerprint).toBe("bge-m3:1024:v1");
    expect(entry?.sha256).toBe(
      "sha256:1f4d2c9b7a6e5d3c0b8a17263544f9e8d7c6b5a4938271605f4e3d2c1b0a9988",
    );
    // signature carried verbatim (forward-compat; not re-verified).
    expect(entry?.signature).toMatchObject({ algorithm: "ed25519", key_id: "producer-k1" });
  });

  test("malformed manifest throws a typed ProviderError", () => {
    expect(() => parseManifestText('{"payload":42}')).toThrow(ProviderError);
  });

  test("a package entry missing a required field ⇒ typed ProviderError (not false-green)", () => {
    // Drop `sha256` from an otherwise-valid entry: the schema must reject, not silently pass.
    const bad = {
      payload: {
        generated_at: "2026-06-30T13:40:00Z",
        head_sequence: 2,
        corpus: "core",
        active_baseline: {
          baseline_id: "core-bootstrap-v1",
          generation: "core_g0001",
          package_kind: "baseline",
          sequence: 1,
          schema_version: 24,
          sha256: "sha256:deadbeef",
          compressed_size_bytes: 1,
          uncompressed_size_bytes: 1,
        },
        packages: [
          {
            package_id: "core-1-2",
            from_sequence: 1,
            to_sequence: 2,
            compressed_size_bytes: 1,
            uncompressed_size_bytes: 1,
            schema_version: 24,
            embedding_fingerprint: "bge-m3:1024:v1",
          },
        ],
      },
    };
    expect(() => parseManifestText(JSON.stringify(bad))).toThrow(ProviderError);
  });
});

describe("parseJournalNdjson", () => {
  test("parses realistic compact NDJSON built from the fixture line", async () => {
    const obj = await json("journal-legislation.json");
    // journalctl emits ONE compact object per line; the fixture is the single captured line.
    const ndjson = `${JSON.stringify(obj)}\n${JSON.stringify(obj)}\n`;
    const lines = parseJournalNdjson(ndjson);
    expect(lines.length).toBe(2);
    // Producer-first: the lifecycle line's UNIT is the producer service, not init.scope.
    expect(lines[0]?.unit).toBe("jurisearch-producer-legislation.service");
    expect(lines[0]?.priority).toBe(6);
    expect(lines[0]?.timestamp).toBe(1_782_819_024_079);
  });

  test("sparse/empty output yields [] (not an error)", () => {
    expect(parseJournalNdjson("")).toEqual([]);
    expect(parseJournalNdjson("\n  \n")).toEqual([]);
  });

  test("a non-blank malformed line throws a typed ProviderError", () => {
    expect(() => parseJournalNdjson("{not json}")).toThrow(ProviderError);
  });
});

describe("parseTimersStdout", () => {
  test("parses the timers array, derives group + µs→ms", async () => {
    const timers = parseTimersStdout(await text("timers.json"));
    expect(timers.map((t) => t.group)).toEqual(["legislation", "jurisprudence"]);
    expect(timers[0]?.nextRun).toBe(1_782_859_358_634);
    expect(timers[0]?.serviceUnit).toBe("jurisearch-producer-legislation.service");
  });

  test("malformed timers stdout throws a typed ProviderError", () => {
    expect(() => parseTimersStdout('{"not":"an array"}')).toThrow(ProviderError);
  });
});
