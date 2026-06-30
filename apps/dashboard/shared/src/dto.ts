/**
 * shared/ — the producer JSON contract as TypeScript DTOs + runtime validators. Each DTO type is
 * INFERRED from its validator schema (`Parsed<…>`), so every field is declared exactly once and the
 * type can never drift from the validator. snake_case→camelCase mapping is centralised in
 * `validate.ts`/`mapping.ts`. Imported by the backend (M2 providers validate adapter output) and
 * the frontend (M4 types the API responses) — DRY across the wire.
 */

import { groupFromUnit, microsToMillis, parseIntOrNull } from "./mapping.ts";
import {
  arrayOf,
  asRecord,
  bool,
  enumOf,
  from,
  num,
  object,
  optional,
  optNum,
  optStr,
  type Parsed,
  type Reader,
  type Result,
  safeParse,
  str,
  unknownValue,
  withDefault,
} from "./validate.ts";

// ── Shared leaf schemas (reused across Status + RunRecord) ───────────────────────────────────────

const outcome = enumOf("running", "success", "failure");

/** A per-source fetch cursor — identical shape in `status` and a run record. */
export const fetchCursorSchema = object({
  source: str,
  latestFileName: optStr,
  latestCompactTimestamp: optStr,
});
export type FetchCursorDTO = Parsed<typeof fetchCursorSchema>;

// ── StatusDTO ← `jurisearch-producer status` ─────────────────────────────────────────────────────

const sourceBaselineSchema = object({
  source: str,
  // `current` | `no_baseline_fetched` | `rebaseline_pending` — kept open (Rust type is String).
  state: str,
  fetchedBaseline: optStr,
  adoptedBaseline: optStr,
});
export type SourceBaselineDTO = Parsed<typeof sourceBaselineSchema>;

const statusGroupSchema = object({
  group: str,
  sources: arrayOf(str),
  lastRunId: optStr,
  lastOutcome: optional(outcome),
  lastExitClass: optStr,
  lastEndedAt: optStr,
  lastError: optStr,
  fetchCursors: arrayOf(fetchCursorSchema),
  baselines: arrayOf(sourceBaselineSchema),
  rebaselinePending: bool,
  staleByAge: bool,
});
export type StatusGroupDTO = Parsed<typeof statusGroupSchema>;

const statusSchema = object({
  generatedAt: str,
  corpus: str,
  overall: enumOf("current", "stale", "broken"),
  publishedHeadSequence: optNum,
  publishedManifestGeneratedAt: optStr,
  activeBaselineId: optStr,
  updateLockHeld: bool,
  groups: arrayOf(statusGroupSchema),
});
export type StatusDTO = Parsed<typeof statusSchema>;

export const parseStatus: Reader<StatusDTO> = statusSchema;
export const safeParseStatus = (input: unknown): Result<StatusDTO> =>
  safeParse(statusSchema, input);

// ── RunRecordDTO ← `state_dir/runs/<group>/*.record.json` ────────────────────────────────────────

const ingestJournalSchema = object({
  source: str,
  // `run_id`/`journal_compact_timestamp` are Option<String> (cursors.rs:40-42): a run that ingests
  // no new archive (no-op window) persists them null — must NOT reject.
  runId: optStr,
  journalCompactTimestamp: optStr,
  archivesIngested: num,
});
export type IngestJournalDTO = Parsed<typeof ingestJournalSchema>;

const packageHighWaterMarkSchema = object({
  corpus: str,
  // `head_sequence`/`included_change_seq_high` are Option<u64> (cursors.rs:52-54): a no-op/no-package
  // window persists the HWM with both null — must NOT reject.
  headSequence: optNum,
  includedChangeSeqHigh: optNum,
});
export type PackageHighWaterMarkDTO = Parsed<typeof packageHighWaterMarkSchema>;

const runRecordSchema = object({
  runId: str,
  group: str,
  sources: arrayOf(str),
  kind: enumOf("incremental", "rebaseline", "dry_run"),
  startedAt: str,
  endedAt: optStr,
  // EXACT persisted string; an in-flight record persists outcome=running AND exitClass="running".
  outcome,
  exitClass: str,
  error: optStr,
  fetchCursors: arrayOf(fetchCursorSchema),
  ingestJournals: arrayOf(ingestJournalSchema),
  packageHighWaterMark: optional(packageHighWaterMarkSchema),
  publishedPackage: optStr,
  adoptedBaselines: arrayOf(str),
});
export type RunRecordDTO = Parsed<typeof runRecordSchema>;

export const parseRunRecord: Reader<RunRecordDTO> = runRecordSchema;
export const safeParseRunRecord = (input: unknown): Result<RunRecordDTO> =>
  safeParse(runRecordSchema, input);

// ── PackageDTO ← served `core/manifest.json` (a `Signed<RemoteManifest>` wrapper) ────────────────

const baselineEntrySchema = object({
  baselineId: str,
  generation: str,
  packageKind: str,
  sequence: num,
  schemaVersion: num,
  sha256: str,
  compressedSizeBytes: num,
  uncompressedSizeBytes: num,
});
export type BaselineEntryDTO = Parsed<typeof baselineEntrySchema>;

const packageEntrySchema = object({
  packageId: str,
  fromSequence: num,
  toSequence: num,
  compressedSizeBytes: num,
  uncompressedSizeBytes: num,
  rowCounts: withDefault(
    (value, path) => {
      const record = asRecord(value, path);
      const out: Record<string, number> = {};
      for (const key of Object.keys(record)) {
        out[key] = num(record[key], `${path}.${key}`);
      }
      return out;
    },
    {} as Record<string, number>,
  ),
  schemaVersion: num,
  embeddingFingerprint: str,
  sha256: str,
  // Carried verbatim (forward-compat); the dashboard does not re-verify it.
  signature: optional(unknownValue),
});
export type PackageEntryDTO = Parsed<typeof packageEntrySchema>;

/** The manifest payload the dashboard renders (NOT the signed wrapper). */
const manifestSchema = object({
  manifestGeneratedAt: from("generated_at", str),
  headSequence: num,
  corpus: str,
  activeBaseline: baselineEntrySchema,
  packages: arrayOf(packageEntrySchema),
});
export type PackageManifestDTO = Parsed<typeof manifestSchema>;

/** The `Signed<RemoteManifest>` wrapper: `{ payload, signature }` — we expose `payload`. */
const signedManifestSchema = object({ payload: manifestSchema });

export const parsePackage: Reader<PackageManifestDTO> = (input, path) =>
  signedManifestSchema(input, path).payload;
export const safeParsePackage = (input: unknown): Result<PackageManifestDTO> =>
  safeParse(parsePackage, input);

// ── LogLineDTO ← `journalctl -o json` (sparse; idiosyncratic systemd keys) ───────────────────────

export interface LogLineDTO {
  /** Epoch ms from the string `__REALTIME_TIMESTAMP` (µs); `null` if absent. */
  timestamp: number | null;
  /** Numeric syslog priority from the string `PRIORITY`; `null` if absent. */
  priority: number | null;
  /** `_SYSTEMD_UNIT ?? UNIT` — service lines carry the unit in the former, lifecycle in the latter. */
  unit: string | null;
  message: string | null;
}

/** Bespoke (not schema-driven): journald keys are SCREAMING/underscored and the unit is coalesced. */
export const parseLogLine: Reader<LogLineDTO> = (input, path) => {
  const record = asRecord(input, path);
  const systemdUnit = record._SYSTEMD_UNIT;
  const unitFallback = record.UNIT;
  const unit =
    typeof systemdUnit === "string"
      ? systemdUnit
      : typeof unitFallback === "string"
        ? unitFallback
        : null;
  return {
    timestamp: microsToMillis(record.__REALTIME_TIMESTAMP),
    priority: parseIntOrNull(record.PRIORITY),
    unit,
    message: typeof record.MESSAGE === "string" ? record.MESSAGE : null,
  };
};
export const safeParseLogLine = (input: unknown): Result<LogLineDTO> =>
  safeParse(parseLogLine, input);

// ── TimerDTO ← `systemctl list-timers -o json` (machine schema; no `group`, no `active`) ─────────

export interface TimerDTO {
  /** Derived from the unit name (`jurisearch-producer-<group>.timer`). */
  group: string;
  timerUnit: string;
  serviceUnit: string;
  /** Epoch ms for the next/last fire; `null` when systemd reports `0`/absent. */
  nextRun: number | null;
  lastRun: number | null;
}

/** Bespoke: the source keys are semantic renames (`unit`→timerUnit, `activates`→serviceUnit, …). */
export const parseTimer: Reader<TimerDTO> = (input, path) => {
  const record = asRecord(input, path);
  const timerUnit = str(record.unit, `${path}.unit`);
  const serviceUnit = str(record.activates, `${path}.activates`);
  return {
    group: groupFromUnit(timerUnit),
    timerUnit,
    serviceUnit,
    nextRun: microsToMillis(record.next),
    lastRun: microsToMillis(record.last),
  };
};
export const safeParseTimer = (input: unknown): Result<TimerDTO> => safeParse(parseTimer, input);
