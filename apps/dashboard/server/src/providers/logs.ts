/**
 * LogsProvider — `journalctl -u <serviceUnit> -o json -n <limit> [--since <since>]` → `LogLineDTO[]`.
 * Output is NDJSON: one JSON object PER LINE. The shared `parseLogLine` coalesces `_SYSTEMD_UNIT ??
 * UNIT` and parses the string µs timestamp / string priority. This unit's journald is SPARSE
 * (Spike B/C: often a single lifecycle line, sometimes none) — that is EXPECTED, so empty/blank
 * output yields `[]`, not a typed error. A non-blank line that is not valid JSON IS surfaced.
 */

import { type LogLineDTO, safeParseLogLine } from "@jurisearch-dashboard/shared";
import type { ProcessRunner } from "../adapters/types.ts";
import type { DashboardConfig } from "../config.ts";
import { type DataProvider, ProviderError, parseJson, unwrap } from "./types.ts";

const SOURCE = "logs";

export interface LogsQuery {
  /** Restrict to one group's service unit (else all configured groups). */
  group?: string;
  /** `-n <limit>` (else `config.logs.defaultLimit`). */
  limit?: number;
  /** `--since <since>` (else `config.logs.defaultSince`; omitted when null). */
  since?: string;
}

/** The producer service unit for a group. */
export function serviceUnit(group: string): string {
  return `jurisearch-producer-${group}.service`;
}

/** Build the fixed `journalctl` argv (no shell). */
export function logsCommand(config: DashboardConfig, query?: LogsQuery): string[] {
  const groups = query?.group ? [query.group] : config.groups;
  const limit = query?.limit ?? config.logs.defaultLimit;
  const since = query?.since ?? config.logs.defaultSince ?? undefined;
  const cmd = ["journalctl", "-o", "json", "-n", String(limit)];
  for (const group of groups) {
    cmd.push("-u", serviceUnit(group));
  }
  if (since !== undefined) {
    cmd.push("--since", since);
  }
  return cmd;
}

/** PURE: raw NDJSON stdout → `LogLineDTO[]`. Blank lines are skipped; a bad line throws typed. */
export function parseJournalNdjson(stdout: string): LogLineDTO[] {
  const lines: LogLineDTO[] = [];
  for (const raw of stdout.split("\n")) {
    const line = raw.trim();
    if (line === "") {
      continue;
    }
    lines.push(unwrap(SOURCE, safeParseLogLine(parseJson(SOURCE, line))));
  }
  return lines;
}

export class LogsProvider implements DataProvider<LogLineDTO[]> {
  constructor(
    private readonly proc: ProcessRunner,
    private readonly config: DashboardConfig,
  ) {}

  async get(query?: LogsQuery): Promise<LogLineDTO[]> {
    const result = await this.proc.run(logsCommand(this.config, query));
    if (result.code !== 0) {
      throw new ProviderError(
        SOURCE,
        `${SOURCE}: journalctl exited ${result.code}: ${result.stderr.trim() || "(no stderr)"}`,
      );
    }
    return parseJournalNdjson(result.stdout);
  }
}
