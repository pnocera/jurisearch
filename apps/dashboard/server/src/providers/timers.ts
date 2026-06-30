/**
 * TimersProvider — `systemctl list-timers 'jurisearch-producer-*' -o json` → `TimerDTO[]`. The
 * machine schema is a JSON ARRAY of `{next,left,last,passed,unit,activates}` with epoch-µs times and
 * NO `active`/`group` (Spike B §7). The shared `parseTimer` derives the group from the unit name and
 * converts µs→ms (0/absent → null, not 1970). The result is narrowed to the CONFIGURED groups so the
 * dashboard never surfaces a timer for a group it isn't watching.
 */

import { arrayOf, parseTimer, safeParse, type TimerDTO } from "@jurisearch-dashboard/shared";
import type { ProcessRunner } from "../adapters/types.ts";
import type { DashboardConfig } from "../config.ts";
import { type DataProvider, ProviderError, parseJson, unwrap } from "./types.ts";

const SOURCE = "timers";
const TIMER_GLOB = "jurisearch-producer-*";

/** The fixed read command (no shell — the glob is a literal systemctl pattern arg). */
export function timersCommand(): string[] {
  return ["systemctl", "list-timers", TIMER_GLOB, "-o", "json"];
}

/** PURE: raw `list-timers` stdout → `TimerDTO[]`. Typed `ProviderError` on bad input. */
export function parseTimersStdout(stdout: string): TimerDTO[] {
  return unwrap(SOURCE, safeParse(arrayOf(parseTimer), parseJson(SOURCE, stdout)));
}

export class TimersProvider implements DataProvider<TimerDTO[]> {
  constructor(
    private readonly proc: ProcessRunner,
    private readonly config: DashboardConfig,
  ) {}

  async get(): Promise<TimerDTO[]> {
    const result = await this.proc.run(timersCommand());
    if (result.code !== 0) {
      throw new ProviderError(
        SOURCE,
        `${SOURCE}: systemctl exited ${result.code}: ${result.stderr.trim() || "(no stderr)"}`,
      );
    }
    const groups = new Set(this.config.groups);
    return parseTimersStdout(result.stdout).filter((timer) => groups.has(timer.group));
  }
}
