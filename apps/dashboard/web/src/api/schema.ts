/**
 * web/ — runtime validators for the `/api/*` RESPONSE bodies, composed from the SHARED validator
 * toolkit (`asRecord`/`str`/`num`/`arrayOf`/`enumOf`/…) so the combinators stay single-sourced in
 * `shared/` (design §9 DRY).
 *
 * Why a web-local schema rather than reusing `safeParseStatus`/`safeParseRunRecord` etc.: those
 * shared validators read the producer's RAW snake_case JSON (they map `run_id`→`runId` via
 * `camelToSnake`), whereas the server (M3) has ALREADY validated+mapped each provider's output, so
 * every `/api/*` body is the camelCase DTO. The `/api/overview` and `/api/health` shapes are
 * server-COMPOSED (`OverviewDTO`/`HealthDTO`) and have no producer-facing validator at all. These
 * readers therefore validate the camelCase DTO directly; each is typed as `Reader<TheSharedDTO>`, so
 * `vue-tsc` fails the build if a field drifts from the shared contract (the type stays the source of
 * truth — see CONTRACT NOTES in `api/client.ts`).
 */

import {
  arrayOf,
  asRecord,
  type BaselineEntryDTO,
  bool,
  enumOf,
  type FetchCursorDTO,
  type HealthDTO,
  type IngestJournalDTO,
  type LogLineDTO,
  mapOf,
  num,
  type OverviewDTO,
  type OverviewFreshnessDTO,
  type OverviewGroupDTO,
  type OverviewLastRunDTO,
  optional,
  optNum,
  optStr,
  type PackageEntryDTO,
  type PackageHighWaterMarkDTO,
  type PackageManifestDTO,
  type Reader,
  type RunRecordDTO,
  type SourceBaselineDTO,
  str,
  type TimerDTO,
  unknownValue,
  withDefault,
} from "@jurisearch-dashboard/shared";

/** A camelCase object reader: reads each field by its OWN key (the body is already camelCase). */
type ReaderMap = Record<string, Reader<unknown>>;
type InferMap<S extends ReaderMap> = { [K in keyof S]: S[K] extends Reader<infer T> ? T : never };

function objectOf<S extends ReaderMap>(schema: S): Reader<InferMap<S>> {
  const entries = Object.entries(schema);
  return (value, path) => {
    const record = asRecord(value, path);
    const out: Record<string, unknown> = {};
    for (const [key, read] of entries) {
      out[key] = read(record[key], `${path}.${key}`);
    }
    return out as InferMap<S>;
  };
}

// ── Closed vocabularies (mirror the shared DTO enums) ────────────────────────────────────────────
const outcome = enumOf("running", "success", "failure");
const runKind = enumOf("incremental", "rebaseline", "dry_run");
const overall = enumOf("current", "stale", "broken");
const severity = enumOf(
  "ok",
  "neutral",
  "transient",
  "data",
  "unprovisioned",
  "config",
  "permanent",
);

// ── Leaf shapes reused across endpoints ──────────────────────────────────────────────────────────
const fetchCursor: Reader<FetchCursorDTO> = objectOf({
  source: str,
  latestFileName: optStr,
  latestCompactTimestamp: optStr,
});

const sourceBaseline: Reader<SourceBaselineDTO> = objectOf({
  source: str,
  state: str,
  fetchedBaseline: optStr,
  adoptedBaseline: optStr,
});

const ingestJournal: Reader<IngestJournalDTO> = objectOf({
  source: str,
  runId: optStr,
  journalCompactTimestamp: optStr,
  archivesIngested: num,
});

const packageHighWaterMark: Reader<PackageHighWaterMarkDTO> = objectOf({
  corpus: str,
  headSequence: optNum,
  includedChangeSeqHigh: optNum,
});

const timer: Reader<TimerDTO> = objectOf({
  group: str,
  timerUnit: str,
  serviceUnit: str,
  nextRun: optNum,
  lastRun: optNum,
});

// ── /api/overview ────────────────────────────────────────────────────────────────────────────────
const overviewLastRun: Reader<OverviewLastRunDTO> = objectOf({
  runId: optStr,
  kind: optional(runKind),
  outcome: optional(outcome),
  exitClass: optStr,
  startedAt: optStr,
  endedAt: optStr,
  durationMs: optNum,
});

const overviewFreshness: Reader<OverviewFreshnessDTO> = objectOf({
  staleByAge: bool,
  rebaselinePending: bool,
  baselines: arrayOf(sourceBaseline),
  fetchCursors: arrayOf(fetchCursor),
});

const overviewGroup: Reader<OverviewGroupDTO> = objectOf({
  group: str,
  sources: arrayOf(str),
  severity,
  lastRun: optional(overviewLastRun),
  freshness: overviewFreshness,
  publishedHeadSequence: optNum,
  updateLockHeld: bool,
  nextTimer: optional(timer),
});

export const overviewReader: Reader<OverviewDTO> = objectOf({
  generatedAt: str,
  corpus: str,
  overall,
  publishedHeadSequence: optNum,
  publishedManifestGeneratedAt: optStr,
  activeBaselineId: optStr,
  updateLockHeld: bool,
  groups: arrayOf(overviewGroup),
});

// ── /api/runs ────────────────────────────────────────────────────────────────────────────────────
const runRecord: Reader<RunRecordDTO> = objectOf({
  runId: str,
  group: str,
  sources: arrayOf(str),
  kind: runKind,
  startedAt: str,
  endedAt: optStr,
  outcome,
  exitClass: str,
  error: optStr,
  fetchCursors: arrayOf(fetchCursor),
  ingestJournals: arrayOf(ingestJournal),
  packageHighWaterMark: optional(packageHighWaterMark),
  publishedPackage: optStr,
  adoptedBaselines: arrayOf(str),
});

export const runsReader: Reader<RunRecordDTO[]> = arrayOf(runRecord);

// ── /api/packages ────────────────────────────────────────────────────────────────────────────────
const baselineEntry: Reader<BaselineEntryDTO> = objectOf({
  baselineId: str,
  generation: str,
  packageKind: str,
  sequence: num,
  schemaVersion: num,
  sha256: str,
  compressedSizeBytes: num,
  uncompressedSizeBytes: num,
});

const packageEntry: Reader<PackageEntryDTO> = objectOf({
  packageId: str,
  fromSequence: num,
  toSequence: num,
  compressedSizeBytes: num,
  uncompressedSizeBytes: num,
  rowCounts: withDefault(mapOf(num), {} as Record<string, number>),
  schemaVersion: num,
  embeddingFingerprint: str,
  sha256: str,
  signature: optional(unknownValue),
});

export const packagesReader: Reader<PackageManifestDTO> = objectOf({
  manifestGeneratedAt: str,
  headSequence: num,
  corpus: str,
  activeBaseline: baselineEntry,
  packages: arrayOf(packageEntry),
});

// ── /api/logs ────────────────────────────────────────────────────────────────────────────────────
const logLine: Reader<LogLineDTO> = objectOf({
  timestamp: optNum,
  priority: optNum,
  unit: optStr,
  message: optStr,
});

export const logsReader: Reader<LogLineDTO[]> = arrayOf(logLine);

// ── /api/health ──────────────────────────────────────────────────────────────────────────────────
export const healthReader: Reader<HealthDTO> = objectOf({
  status: enumOf("ok", "degraded"),
  name: str,
  version: str,
  uptimeMs: num,
  now: str,
});
