/**
 * OverviewService — the ONE join that builds the page-shaped `OverviewDTO` (design §5.3 + §6.2).
 * It composes three providers and re-uses the shared derivations rather than re-deriving anything:
 *   - StatusProvider — the producer's canonical on-disk summary (corpus header + per-group last-run
 *     summary + freshness). This is the REQUIRED input; if it fails the whole `/api/overview`
 *     endpoint degrades (per-endpoint isolation, design §5.4).
 *   - RunsProvider — supplies the two fields the status summary lacks for a group's last run:
 *     `kind` and `startedAt` (so the card can show the run type + a derived duration). Matched to the
 *     status summary by `lastRunId`, falling back to the most-recent record for the group.
 *   - TimersProvider — the group's next/last scheduled fire.
 *
 * Severity is `severityOf(lastOutcome, lastExitClass)` (outcome-FIRST — a `running`/never-ran group
 * is neutral, never a permanent failure) and duration is `runDurationMs(startedAt, endedAt)` — both
 * imported from `shared/`, NEVER re-implemented here.
 */

import {
  type OverviewDTO,
  type OverviewGroupDTO,
  type OverviewLastRunDTO,
  type RunRecordDTO,
  runDurationMs,
  type StatusDTO,
  type StatusGroupDTO,
  severityOf,
  type TimerDTO,
} from "@jurisearch-dashboard/shared";
import type { DataProvider } from "../providers/types.ts";

/** Index run records by `runId` so the status last-run summary can be matched to its full record. */
function byRunId(records: RunRecordDTO[]): Map<string, RunRecordDTO> {
  const out = new Map<string, RunRecordDTO>();
  for (const record of records) {
    // First write wins is irrelevant: runIds are unique; guard against an accidental dup anyway.
    if (!out.has(record.runId)) {
      out.set(record.runId, record);
    }
  }
  return out;
}

/** The most-recent record per group (by `startedAt`) — the fallback when no `lastRunId` matches. */
function lastByGroup(records: RunRecordDTO[]): Map<string, RunRecordDTO> {
  const startedAtMillis = (record: RunRecordDTO): number => {
    const t = Date.parse(record.startedAt);
    return Number.isNaN(t) ? Number.NEGATIVE_INFINITY : t;
  };
  const out = new Map<string, RunRecordDTO>();
  for (const record of records) {
    const existing = out.get(record.group);
    if (existing === undefined || startedAtMillis(record) > startedAtMillis(existing)) {
      out.set(record.group, record);
    }
  }
  return out;
}

export class OverviewService {
  constructor(
    private readonly status: DataProvider<StatusDTO>,
    private readonly runs: DataProvider<RunRecordDTO[]>,
    private readonly timers: DataProvider<TimerDTO[]>,
  ) {}

  async get(): Promise<OverviewDTO> {
    // Status is REQUIRED — any failure here propagates so the endpoint degrades as one unit.
    const status = await this.status.get();
    const records = await this.runs.get();
    const timers = await this.timers.get();

    const recordById = byRunId(records);
    const recordByGroup = lastByGroup(records);
    const timerByGroup = new Map<string, TimerDTO>();
    for (const timer of timers) {
      if (!timerByGroup.has(timer.group)) {
        timerByGroup.set(timer.group, timer);
      }
    }

    const groups: OverviewGroupDTO[] = status.groups.map((group) =>
      this.buildGroup(group, status, recordById, recordByGroup, timerByGroup),
    );

    return {
      generatedAt: status.generatedAt,
      corpus: status.corpus,
      overall: status.overall,
      publishedHeadSequence: status.publishedHeadSequence,
      publishedManifestGeneratedAt: status.publishedManifestGeneratedAt,
      activeBaselineId: status.activeBaselineId,
      updateLockHeld: status.updateLockHeld,
      groups,
    };
  }

  private buildGroup(
    group: StatusGroupDTO,
    status: StatusDTO,
    recordById: Map<string, RunRecordDTO>,
    recordByGroup: Map<string, RunRecordDTO>,
    timerByGroup: Map<string, TimerDTO>,
  ): OverviewGroupDTO {
    const lastRun = this.resolveLastRun(group, recordById, recordByGroup);

    return {
      group: group.group,
      sources: group.sources,
      // Outcome-FIRST severity (shared), derived from the SAME run shown in `lastRun` so the card
      // colour never disagrees with the run it labels; no run ⇒ neutral.
      severity: lastRun === null ? "neutral" : severityOf(lastRun.outcome, lastRun.exitClass),
      lastRun,
      freshness: {
        staleByAge: group.staleByAge,
        rebaselinePending: group.rebaselinePending,
        baselines: group.baselines,
        fetchCursors: group.fetchCursors,
      },
      // Corpus-level values denormalised onto each card (design §6.2 GroupCard).
      publishedHeadSequence: status.publishedHeadSequence,
      updateLockHeld: status.updateLockHeld,
      nextTimer: timerByGroup.get(group.group) ?? null,
    };
  }

  /**
   * Resolve a group's last run WITHOUT merging fields from two different runs (Codex M3 WARN):
   *   - status has a `lastRunId` → that run is canonical. Join ONLY the record with that EXACT id for
   *     the two status-less fields (`kind`,`startedAt`); if no such record exists keep the status
   *     summary and leave `kind`/`startedAt`/`durationMs` null (never borrow them from another run).
   *   - status has NO `lastRunId` → derive the ENTIRE last run consistently from the single most-recent
   *     record for the group (or null when the group has never run).
   */
  private resolveLastRun(
    group: StatusGroupDTO,
    recordById: Map<string, RunRecordDTO>,
    recordByGroup: Map<string, RunRecordDTO>,
  ): OverviewLastRunDTO | null {
    if (group.lastRunId !== null) {
      const exact = recordById.get(group.lastRunId) ?? null; // same run only
      const startedAt = exact?.startedAt ?? null;
      const endedAt = group.lastEndedAt ?? exact?.endedAt ?? null;
      return {
        runId: group.lastRunId,
        kind: exact?.kind ?? null,
        outcome: group.lastOutcome,
        exitClass: group.lastExitClass,
        startedAt,
        // `durationMs` stays null when the exact record (and thus `startedAt`) is missing.
        endedAt,
        durationMs: runDurationMs(startedAt, endedAt),
      };
    }

    const record = recordByGroup.get(group.group) ?? null;
    if (record === null) {
      return null;
    }
    return {
      runId: record.runId,
      kind: record.kind,
      outcome: record.outcome,
      exitClass: record.exitClass,
      startedAt: record.startedAt,
      endedAt: record.endedAt,
      durationMs: runDurationMs(record.startedAt, record.endedAt),
    };
  }
}
