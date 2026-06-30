/**
 * StatusProvider — `jurisearch-producer status --config <producerConfig>` → `StatusDTO`.
 * Spike B: `status` prints JSON on stdout with NO `--json` flag, touches no DB/network. The pure
 * `parseStatus` (raw stdout → DTO) is unit-tested separately; `get()` only adds the subprocess.
 */

import { type StatusDTO, safeParseStatus } from "@jurisearch-dashboard/shared";
import type { ProcessRunner } from "../adapters/types.ts";
import type { DashboardConfig } from "../config.ts";
import { type DataProvider, ProviderError, parseJson, unwrap } from "./types.ts";

const SOURCE = "status";

/** The fixed read command (no shell). */
export function statusCommand(config: DashboardConfig): string[] {
  return [config.producerBin, "status", "--config", config.producerConfig];
}

/** PURE: raw `status` stdout → validated `StatusDTO`. Throws a typed `ProviderError` on bad input. */
export function parseStatusStdout(stdout: string): StatusDTO {
  return unwrap(SOURCE, safeParseStatus(parseJson(SOURCE, stdout)));
}

export class StatusProvider implements DataProvider<StatusDTO> {
  constructor(
    private readonly proc: ProcessRunner,
    private readonly config: DashboardConfig,
  ) {}

  async get(): Promise<StatusDTO> {
    const result = await this.proc.run(statusCommand(this.config));
    if (result.code !== 0) {
      throw new ProviderError(
        SOURCE,
        `${SOURCE}: producer exited ${result.code}: ${result.stderr.trim() || "(no stderr)"}`,
      );
    }
    return parseStatusStdout(result.stdout);
  }
}
