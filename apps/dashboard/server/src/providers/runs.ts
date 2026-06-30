/**
 * RunsProvider — `<stateDir>/runs/<group>/*.record.json` → `RunRecordDTO[]`. Reads the run records
 * the producer persists per group (CT 111 stateDir `/var/lib/jurisearch-producer`). Only
 * `*.record.json` files are records — the sibling bare `*.json` (a `RunCheckpoint`) and `last.json`
 * are NOT part of this contract (Spike B §3), so they are filtered out.
 *
 * A group that has never run has NO `runs/<group>` dir (Spike B: `runs/jurisprudence` is absent) —
 * that is ABSENCE, not corruption, so it yields no records rather than a typed error. A record that
 * is present but malformed DOES surface a `ProviderError` (so M3 degrades the Runs panel).
 */

import { type RunRecordDTO, safeParseRunRecord } from "@jurisearch-dashboard/shared";
import type { FileSource } from "../adapters/types.ts";
import type { DashboardConfig } from "../config.ts";
import { type DataProvider, ProviderError, parseJson, unwrap } from "./types.ts";

const SOURCE = "runs";
const RECORD_SUFFIX = ".record.json";

export interface RunsQuery {
  /** Restrict to one group (else all configured groups). */
  group?: string;
  /** Cap the number of records returned (most-recent first). */
  limit?: number;
}

/** PURE: one record file's text → validated `RunRecordDTO`. Typed `ProviderError` on bad input. */
export function parseRunRecordText(text: string): RunRecordDTO {
  return unwrap(SOURCE, safeParseRunRecord(parseJson(SOURCE, text)));
}

/** A record's sort key: newest `startedAt` first; unparseable timestamps sort last. */
function startedAtMillis(record: RunRecordDTO): number {
  const t = Date.parse(record.startedAt);
  return Number.isNaN(t) ? Number.NEGATIVE_INFINITY : t;
}

/** Most-recent-first by `startedAt`. */
export function sortByStartedAtDesc(records: RunRecordDTO[]): RunRecordDTO[] {
  return [...records].sort((a, b) => startedAtMillis(b) - startedAtMillis(a));
}

/**
 * The last (most-recent) run per group — the helper the Overview join (M3) uses. Pure over a flat
 * record list, so it is trivially unit-tested.
 */
export function lastRunByGroup(records: RunRecordDTO[]): Map<string, RunRecordDTO> {
  const out = new Map<string, RunRecordDTO>();
  for (const record of records) {
    const existing = out.get(record.group);
    if (existing === undefined || startedAtMillis(record) > startedAtMillis(existing)) {
      out.set(record.group, record);
    }
  }
  return out;
}

export class RunsProvider implements DataProvider<RunRecordDTO[]> {
  constructor(
    private readonly files: FileSource,
    private readonly config: DashboardConfig,
  ) {}

  /** Read every record for one group; an absent `runs/<group>` dir ⇒ `[]` (never ran). */
  private async readGroup(group: string): Promise<RunRecordDTO[]> {
    const dir = `${this.config.stateDir}/runs/${group}`;
    let names: string[];
    try {
      names = await this.files.list(dir);
    } catch {
      // Absent dir = the group has never run. Not a degradation.
      return [];
    }
    const records: RunRecordDTO[] = [];
    for (const name of names) {
      if (!name.endsWith(RECORD_SUFFIX)) {
        continue;
      }
      let text: string;
      try {
        text = await this.files.read(`${dir}/${name}`);
      } catch (error) {
        throw new ProviderError(SOURCE, `${SOURCE}: cannot read ${group}/${name}`, {
          cause: error,
        });
      }
      records.push(parseRunRecordText(text));
    }
    return records;
  }

  async get(query?: RunsQuery): Promise<RunRecordDTO[]> {
    const groups = query?.group ? [query.group] : this.config.groups;
    const all: RunRecordDTO[] = [];
    for (const group of groups) {
      all.push(...(await this.readGroup(group)));
    }
    const sorted = sortByStartedAtDesc(all);
    return query?.limit !== undefined ? sorted.slice(0, query.limit) : sorted;
  }
}
