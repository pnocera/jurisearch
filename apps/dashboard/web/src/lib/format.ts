/**
 * web/ — the ONE place shared display formatters live (design §6.1). Pages AND components import
 * from here; none re-implements a formatter. The numeric DERIVATIONS (e.g. a run's duration) come
 * from `shared/` (`runDurationMs`) — this module only turns already-derived values into strings, so
 * the wire-level math stays single-sourced in the contract.
 */

import { runDurationMs } from "@jurisearch-dashboard/shared";

/** A placeholder for an absent value, used uniformly so empty cells read consistently. */
export const EMPTY = "—";

const RELATIVE_UNITS: Array<{
  limitMs: number;
  divisorMs: number;
  unit: Intl.RelativeTimeFormatUnit;
}> = [
  { limitMs: 60_000, divisorMs: 1_000, unit: "second" },
  { limitMs: 3_600_000, divisorMs: 60_000, unit: "minute" },
  { limitMs: 86_400_000, divisorMs: 3_600_000, unit: "hour" },
  { limitMs: 2_592_000_000, divisorMs: 86_400_000, unit: "day" },
  { limitMs: 31_536_000_000, divisorMs: 2_592_000_000, unit: "month" },
  { limitMs: Number.POSITIVE_INFINITY, divisorMs: 31_536_000_000, unit: "year" },
];

const relativeFormatter = new Intl.RelativeTimeFormat("en", { numeric: "auto" });

/** Parse an ISO timestamp (or epoch ms) to epoch ms, or `null` when absent/unparseable. */
function toEpochMs(value: string | number | null | undefined): number | null {
  if (value === null || value === undefined) {
    return null;
  }
  const ms = typeof value === "number" ? value : Date.parse(value);
  return Number.isNaN(ms) ? null : ms;
}

/**
 * A human relative time ("3 minutes ago", "in 2 hours") for an ISO string or epoch-ms value.
 * `null`/unparseable → {@link EMPTY}. `now` is injectable so the formatter is deterministic in tests.
 */
export function relativeTime(
  value: string | number | null | undefined,
  now: number = Date.now(),
): string {
  const ms = toEpochMs(value);
  if (ms === null) {
    return EMPTY;
  }
  const deltaMs = ms - now;
  const absMs = Math.abs(deltaMs);
  for (const { limitMs, divisorMs, unit } of RELATIVE_UNITS) {
    if (absMs < limitMs) {
      return relativeFormatter.format(Math.round(deltaMs / divisorMs), unit);
    }
  }
  return EMPTY;
}

/** An absolute, locale-stable UTC timestamp for tooltips/detail rows. `null` → {@link EMPTY}. */
export function absoluteTime(value: string | number | null | undefined): string {
  const ms = toEpochMs(value);
  if (ms === null) {
    return EMPTY;
  }
  return new Date(ms).toISOString().replace("T", " ").replace(".000Z", "Z");
}

/** Format a millisecond duration as a compact `h m s` string. `null` → {@link EMPTY}. */
export function formatDuration(durationMs: number | null | undefined): string {
  if (durationMs === null || durationMs === undefined || !Number.isFinite(durationMs)) {
    return EMPTY;
  }
  if (durationMs < 1_000) {
    return `${Math.max(0, Math.round(durationMs))}ms`;
  }
  const totalSeconds = Math.round(durationMs / 1_000);
  const hours = Math.floor(totalSeconds / 3_600);
  const minutes = Math.floor((totalSeconds % 3_600) / 60);
  const seconds = totalSeconds % 60;
  const parts: string[] = [];
  if (hours > 0) {
    parts.push(`${hours}h`);
  }
  if (hours > 0 || minutes > 0) {
    parts.push(`${minutes}m`);
  }
  parts.push(`${seconds}s`);
  return parts.join(" ");
}

/**
 * The duration string for a run, deriving the millisecond value via the shared `runDurationMs`
 * (the ONE place that math lives) and formatting it here. `null` while in flight / unknown.
 */
export function runDuration(
  startedAt: string | null | undefined,
  endedAt: string | null | undefined,
): string {
  return formatDuration(runDurationMs(startedAt, endedAt));
}

const BYTE_UNITS = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"] as const;

/** Human-readable binary byte size. `null`/non-finite → {@link EMPTY}. */
export function formatBytes(bytes: number | null | undefined): string {
  if (bytes === null || bytes === undefined || !Number.isFinite(bytes) || bytes < 0) {
    return EMPTY;
  }
  if (bytes < 1024) {
    return `${bytes} B`;
  }
  let value = bytes;
  let unitIndex = 0;
  while (value >= 1024 && unitIndex < BYTE_UNITS.length - 1) {
    value /= 1024;
    unitIndex += 1;
  }
  const decimals = value >= 100 ? 0 : value >= 10 ? 1 : 2;
  return `${value.toFixed(decimals)} ${BYTE_UNITS[unitIndex]}`;
}

/** A `from → to` sequence range (e.g. an increment package's coverage). */
export function sequenceRange(
  from: number | null | undefined,
  to: number | null | undefined,
): string {
  const left = from ?? null;
  const right = to ?? null;
  if (left === null && right === null) {
    return EMPTY;
  }
  if (left === null) {
    return `→ ${right}`;
  }
  if (right === null) {
    return `${left} →`;
  }
  return left === right ? `${left}` : `${left} → ${right}`;
}

/** A grouped integer (thousands separators). `null` → {@link EMPTY}. */
export function formatCount(value: number | null | undefined): string {
  if (value === null || value === undefined || !Number.isFinite(value)) {
    return EMPTY;
  }
  return value.toLocaleString("en-US");
}

/** A bare display string, substituting {@link EMPTY} for null/empty. */
export function orEmpty(value: string | null | undefined): string {
  return value === null || value === undefined || value === "" ? EMPTY : value;
}
