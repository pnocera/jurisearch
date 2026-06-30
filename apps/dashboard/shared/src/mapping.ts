/**
 * shared/ — the ONE place producer snake_case ⇄ dashboard camelCase mapping lives, plus the
 * numeric/string derivations the DTOs share. Keeping every mapping primitive here (and nowhere
 * else) is what keeps the contract DRY: validators name a field once and the conversion is
 * centralised in `camelToSnake`.
 */

/** `activeBaselineId` → `active_baseline_id`. The single camelCase→snake_case mapping mechanism. */
export function camelToSnake(key: string): string {
  return key.replace(/[A-Z]/g, (c) => `_${c.toLowerCase()}`);
}

/** `active_baseline_id` → `activeBaselineId`. The inverse, for completeness/reuse. */
export function snakeToCamel(key: string): string {
  return key.replace(/_([a-z0-9])/g, (_, c: string) => c.toUpperCase());
}

/**
 * Derive a fetch group from a systemd unit name, e.g.
 * `jurisearch-producer-legislation.timer` → `legislation` (also handles `.service`). `systemctl
 * list-timers -o json` carries no `group`, so the dashboard never re-infers the name elsewhere.
 */
export function groupFromUnit(unit: string): string {
  return unit.replace(/\.(timer|service)$/, "").replace(/^jurisearch-producer-/, "");
}

/**
 * Convert an epoch-MICROSECONDS value (systemd `list-timers`, journald `__REALTIME_TIMESTAMP`) to
 * epoch milliseconds for JS `Date`. systemd uses `0` / absent for "none"; both map to `null` so a
 * timer that has never fired reads as null rather than 1970. Accepts the µs as a number OR a
 * string (journald serialises it as a string).
 */
export function microsToMillis(value: unknown): number | null {
  let micros: number;
  if (typeof value === "number") {
    micros = value;
  } else if (typeof value === "string") {
    micros = Number.parseInt(value, 10);
  } else {
    return null;
  }
  if (!Number.isFinite(micros) || micros <= 0) {
    return null;
  }
  return Math.floor(micros / 1000);
}

/** Parse an integer that the producer may serialise as a string (e.g. journald `PRIORITY`). */
export function parseIntOrNull(value: unknown): number | null {
  if (typeof value === "number") {
    return Number.isFinite(value) ? value : null;
  }
  if (typeof value === "string") {
    const n = Number.parseInt(value, 10);
    return Number.isNaN(n) ? null : n;
  }
  return null;
}

/**
 * The run duration the producer does NOT store: `endedAt − startedAt` in ms, or `null` while a run
 * is in flight (`endedAt` absent). Derivation lives here (one place); the DTO only exposes the
 * `startedAt`/`endedAt` pieces and never fabricates a number.
 */
export function runDurationMs(
  startedAt: string | null | undefined,
  endedAt: string | null | undefined,
): number | null {
  if (!startedAt || !endedAt) {
    return null;
  }
  const start = Date.parse(startedAt);
  const end = Date.parse(endedAt);
  if (Number.isNaN(start) || Number.isNaN(end)) {
    return null;
  }
  return end - start;
}
