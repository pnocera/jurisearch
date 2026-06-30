/**
 * shared/ — the CROSS-STREAM API contract: the exact shapes the server (M3) returns and the web
 * client (M4) consumes, so both streams build in parallel without drift. This module DECLARES the
 * page-shaped `OverviewDTO` (the status × runs × timers join the Overview page renders) and the
 * per-endpoint response aliases; it does NOT implement the join (that is M3's `OverviewService`).
 *
 * All `/api/*` bodies are wrapped in `ApiResult<T>` so one failing source degrades a single panel
 * (design §5.4 resilience) instead of blanking the dashboard — never throw across the wire.
 */

import type {
  FetchCursorDTO,
  LogLineDTO,
  PackageManifestDTO,
  RunRecordDTO,
  SourceBaselineDTO,
  StatusDTO,
  TimerDTO,
} from "./dto.ts";
import type { RunOutcome, Severity } from "./exit-class.ts";

// ── Transport envelope ───────────────────────────────────────────────────────────────────────────

/**
 * The degraded-panel envelope every `/api/*` endpoint returns. A provider failure becomes
 * `{ ok:false, error }` for that endpoint (design §5.4) — the process stays up and the other panels
 * keep rendering. `code` is an optional machine tag (e.g. a provider/source name) for the UI.
 */
export type ApiResult<T> =
  | { ok: true; data: T }
  | { ok: false; error: { code?: string; message: string } };

// ── OverviewDTO — the page-shaped join (design §5.3 + §6.2) ───────────────────────────────────────

/**
 * The last run a group's GroupCard shows. Composed from the STATUS last-run summary
 * (`lastRunId`/`lastOutcome`/`lastExitClass`/`lastEndedAt`) joined with the matching RunRecord
 * (which alone carries `kind`/`startedAt`). Every field is nullable: a group that never ran has no
 * last run, and `status` has no `startedAt`, so `durationMs` is null until the RunRecord is found.
 */
export interface OverviewLastRunDTO {
  runId: string | null;
  kind: RunRecordDTO["kind"] | null;
  outcome: RunOutcome | null;
  /** The EXACT persisted exit-class string (e.g. `published`, `running`, `no-op`). */
  exitClass: string | null;
  startedAt: string | null;
  endedAt: string | null;
  /** `endedAt − startedAt` in ms (via `runDurationMs`); null while running or when unknown. */
  durationMs: number | null;
}

/**
 * The freshness inputs the `FreshnessMeter` renders: adopted-vs-fetched baseline per source, the
 * latest fetched archive cursor per source (the pending-delta lag), and the two staleness flags —
 * all carried straight from `StatusDTO` (no re-derivation here).
 */
export interface OverviewFreshnessDTO {
  staleByAge: boolean;
  rebaselinePending: boolean;
  baselines: SourceBaselineDTO[];
  fetchCursors: FetchCursorDTO[];
}

/** One GroupCard's worth of joined state — one entry per fetch group. */
export interface OverviewGroupDTO {
  group: string;
  sources: string[];
  /** R/A/G colour, derived via `severityOf(lastOutcome, lastExitClass)` — outcome-first. */
  severity: Severity;
  lastRun: OverviewLastRunDTO | null;
  freshness: OverviewFreshnessDTO;
  /** Corpus-level value denormalised onto the card (design §6.2 GroupCard). */
  publishedHeadSequence: number | null;
  /** Corpus-level lock denormalised onto the card; in-flight lock is normal, not an error. */
  updateLockHeld: boolean;
  /** The group's next/last scheduled fire, from `TimersProvider`; null if no timer is known. */
  nextTimer: TimerDTO | null;
}

/** The Overview page payload: corpus header + one entry per fetch group. */
export interface OverviewDTO {
  generatedAt: string;
  corpus: string;
  overall: StatusDTO["overall"];
  publishedHeadSequence: number | null;
  publishedManifestGeneratedAt: string | null;
  activeBaselineId: string | null;
  updateLockHeld: boolean;
  groups: OverviewGroupDTO[];
}

// ── /api/health ───────────────────────────────────────────────────────────────────────────────────

/** The dashboard's own liveness (design §10 self-observability); built by M3. */
export interface HealthDTO {
  status: "ok" | "degraded";
  /** The branding string (`DASHBOARD_NAME`). */
  name: string;
  /** The stamped build version (`--version` parity). */
  version: string;
  /** Process uptime in ms. */
  uptimeMs: number;
  /** ISO-8601 server time. */
  now: string;
}

// ── Per-endpoint response aliases (the `T` inside `ApiResult<T>`) ──────────────────────────────────
//   GET /api/overview · /api/runs · /api/packages · /api/logs · /api/health

export type OverviewResponse = OverviewDTO;
export type RunsResponse = RunRecordDTO[];
export type PackagesResponse = PackageManifestDTO;
export type LogsResponse = LogLineDTO[];
export type HealthResponse = HealthDTO;
