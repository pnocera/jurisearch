/**
 * shared/ — the COMPLETE producer exit-class vocabulary + the outcome-first severity logic,
 * backend-owned (one map, imported by server AND web — design §4/§9). The class strings and exit
 * codes mirror the producer exactly:
 *   - success set:  `crates/jurisearch-producer/src/exit.rs` (`SUCCESS_CLASSES`)
 *   - exit codes:   `crates/jurisearch-producer/src/exit.rs` (`exit_code_for`)
 *   - failure set:  `crates/jurisearch-producer/src/error.rs` (`ProducerError::class`) plus the two
 *                   (`upstream-unreachable`, `integrity-failed`) that live only in `exit_code_for`.
 *   - `running`:    `crates/jurisearch-producer/src/runrecord.rs` (`RunRecord::started`).
 * A build-time drift test (`exit-class.drift.test.ts`) re-derives the Rust set from source so this
 * table cannot silently drift.
 */

/** The terminal state a run record persists (`RunOutcome` in `runrecord.rs`). */
export type RunOutcome = "running" | "success" | "failure";

/** Which bucket a class falls in — derived, never hand-maintained per class. */
export type ExitClassKind = "success" | "running" | "failure";

/**
 * The dashboard severity, derived from the producer's `sysexits.h`-style exit-code buckets plus the
 * neutral in-flight state: `ok`(0) · `transient`(75) · `data`(65) · `unprovisioned`(69) ·
 * `config`(78) · `permanent`(70) · `neutral`(running/unknown).
 */
export type Severity =
  | "ok"
  | "neutral"
  | "transient"
  | "data"
  | "unprovisioned"
  | "config"
  | "permanent";

/** Process-exit `0` classes (mirror of `exit.rs::SUCCESS_CLASSES`). */
export const SUCCESS_CLASSES = [
  "ok",
  "published",
  "published-enrich-degraded",
  "no-op",
  "rebaselined",
  "dry-run",
] as const;

/** The in-flight class an open run record carries (`runrecord.rs`); NOT a failure. */
export const RUNNING_CLASS = "running";

/** Every failure class the producer can persist (`error.rs::class` + `exit_code_for`-only pair). */
export const FAILURE_CLASSES = [
  "fetch-failed",
  "upstream-unreachable",
  "skipped-lock-held",
  "integrity-failed",
  "producer-db-unprovisioned",
  "config-invalid",
  "ingest-failed",
  "enrich-degraded",
  "embed-failed",
  "publish-failed",
  "provision-failed",
  "storage-failed",
  "needs-rebaseline",
  "alert-hook-failed",
  "io-failed",
] as const;

const SUCCESS_SET: ReadonlySet<string> = new Set(SUCCESS_CLASSES);

/** Whether `exitClass` is a successful outcome (process exit 0) — mirror of `exit.rs::is_success`. */
export function isSuccess(exitClass: string): boolean {
  return SUCCESS_SET.has(exitClass);
}

/**
 * The process exit code for a class — a faithful port of `exit.rs::exit_code_for` (success → 0;
 * transient 75; data 65; unprovisioned 69; config 78; any other permanent failure → 70).
 */
export function exitCodeFor(exitClass: string): number {
  if (isSuccess(exitClass)) {
    return 0;
  }
  switch (exitClass) {
    case "skipped-lock-held":
    case "fetch-failed":
    case "upstream-unreachable":
      return 75;
    case "integrity-failed":
      return 65;
    case "producer-db-unprovisioned":
      return 69;
    case "config-invalid":
      return 78;
    default:
      return 70;
  }
}

/** Map an `exit_code_for` bucket to a UI severity. */
function severityFromExitCode(code: number): Severity {
  switch (code) {
    case 0:
      return "ok";
    case 75:
      return "transient";
    case 65:
      return "data";
    case 69:
      return "unprovisioned";
    case 78:
      return "config";
    default:
      return "permanent";
  }
}

/** A row in the typed exit-class table. */
export interface ExitClassInfo {
  kind: ExitClassKind;
  /** The producer process exit code, or `null` for the in-flight `running` class. */
  exitCode: number | null;
  severity: Severity;
}

/**
 * The COMPLETE class → {kind, exitCode, severity} table, DERIVED from the three class lists +
 * `exitCodeFor` (no per-class hand-maintenance). 22 classes: 6 success, 1 running, 15 failure.
 */
export const EXIT_CLASS_TABLE: Readonly<Record<string, ExitClassInfo>> = (() => {
  const table: Record<string, ExitClassInfo> = {};
  for (const cls of SUCCESS_CLASSES) {
    table[cls] = { kind: "success", exitCode: 0, severity: "ok" };
  }
  table[RUNNING_CLASS] = { kind: "running", exitCode: null, severity: "neutral" };
  for (const cls of FAILURE_CLASSES) {
    const code = exitCodeFor(cls);
    table[cls] = { kind: "failure", exitCode: code, severity: severityFromExitCode(code) };
  }
  return table;
})();

/**
 * Severity derived from `outcome` FIRST, then the exit-class bucket — NEVER from the class string
 * alone (design §4). A `running` outcome (or an unknown/absent outcome, e.g. a group that never
 * ran) is neutral/in-progress and must never be bucketed as a permanent failure; a `success`
 * outcome is `ok` regardless of class; only a `failure` outcome consults `exitCodeFor`.
 */
export function severityOf(
  outcome: RunOutcome | null | undefined,
  exitClass: string | null | undefined,
): Severity {
  if (outcome === "running") {
    return "neutral";
  }
  if (outcome === "success") {
    return "ok";
  }
  if (outcome === "failure") {
    if (!exitClass) {
      return "permanent";
    }
    return severityFromExitCode(exitCodeFor(exitClass));
  }
  // outcome null/undefined (never ran / unknown): neutral — never a permanent failure.
  return "neutral";
}
