/**
 * Config resolution (design §5.5) — precedence **flags > env > toml > built-in defaults**.
 *
 * Each layer is parsed into a `Partial<DashboardConfig>`; `resolveConfig` then picks, per field, the
 * first layer that defines it. The built-in defaults are the CT-111 production values (the deploy
 * config/env/flags only need to override the bind address + anything site-specific). `bind` defaults
 * to loopback so a bare `jurisearch-dashboard` is dev-safe; the HTTP bind guard still refuses any
 * wildcard, so production MUST set an explicit tailnet address.
 *
 * TOML is a small `[dashboard]` block parsed with the built-in `Bun.TOML` (no dep). Env vars are
 * `DASHBOARD_*`. Flags are pre-parsed by `main.ts` into the same `Partial` shape.
 */

import type { CacheTtls, DashboardConfig, LogWindowDefaults } from "../config.ts";

/** Built-in defaults — the CT-111 production layout (design §5.5). */
export const DEFAULT_CONFIG: DashboardConfig = {
  // Loopback by default (dev-safe; allowed by the bind guard). Production sets the tailnet address.
  bind: "127.0.0.1",
  port: 8787,
  producerBin: "/usr/local/bin/jurisearch-producer",
  producerConfig: "/etc/jurisearch/producer.toml",
  stateDir: "/var/lib/jurisearch-producer",
  corporaDir: "/srv/jurisearch/storebox/packages",
  groups: ["legislation", "jurisprudence"],
  cache: {
    statusMs: 3000,
    overviewMs: 3000,
    runsMs: 3000,
    packagesMs: 15000,
    logsMs: 2000,
    timersMs: 5000,
  },
  logs: { defaultLimit: 200, defaultSince: null },
};

/** A layer's contribution: any subset of fields (nested `cache`/`logs` are themselves partial). */
export interface PartialConfig {
  bind?: string;
  port?: number;
  producerBin?: string;
  producerConfig?: string;
  stateDir?: string;
  corporaDir?: string;
  groups?: string[];
  cache?: Partial<CacheTtls>;
  logs?: Partial<LogWindowDefaults>;
}

/** Pick the first defined value across the precedence-ordered layers. */
function pick<T>(...candidates: (T | undefined)[]): T | undefined {
  for (const candidate of candidates) {
    if (candidate !== undefined) {
      return candidate;
    }
  }
  return undefined;
}

function resolveCache(layers: (Partial<CacheTtls> | undefined)[], base: CacheTtls): CacheTtls {
  const at = (key: keyof CacheTtls): number =>
    pick(...layers.map((layer) => layer?.[key])) ?? base[key];
  return {
    statusMs: at("statusMs"),
    overviewMs: at("overviewMs"),
    runsMs: at("runsMs"),
    packagesMs: at("packagesMs"),
    logsMs: at("logsMs"),
    timersMs: at("timersMs"),
  };
}

function resolveLogs(
  layers: (Partial<LogWindowDefaults> | undefined)[],
  base: LogWindowDefaults,
): LogWindowDefaults {
  return {
    defaultLimit: pick(...layers.map((l) => l?.defaultLimit)) ?? base.defaultLimit,
    // `defaultSince` is nullable; a layer may legitimately set it to `null` (no `--since`).
    defaultSince:
      layers.reduce<string | null | undefined>(
        (acc, layer) => (acc !== undefined ? acc : layer?.defaultSince),
        undefined,
      ) ?? base.defaultSince,
  };
}

/**
 * Resolve the effective config. Layers are applied highest-precedence first: `flags`, then `env`,
 * then `toml`, then `DEFAULT_CONFIG`.
 */
export function resolveConfig(opts: {
  flags?: PartialConfig;
  env?: PartialConfig;
  toml?: PartialConfig;
  defaults?: DashboardConfig;
}): DashboardConfig {
  const base = opts.defaults ?? DEFAULT_CONFIG;
  const { flags, env, toml } = opts;
  const ordered = [flags, env, toml]; // highest → lowest precedence

  return {
    bind: pick(flags?.bind, env?.bind, toml?.bind) ?? base.bind,
    port: pick(flags?.port, env?.port, toml?.port) ?? base.port,
    producerBin: pick(flags?.producerBin, env?.producerBin, toml?.producerBin) ?? base.producerBin,
    producerConfig:
      pick(flags?.producerConfig, env?.producerConfig, toml?.producerConfig) ?? base.producerConfig,
    stateDir: pick(flags?.stateDir, env?.stateDir, toml?.stateDir) ?? base.stateDir,
    corporaDir: pick(flags?.corporaDir, env?.corporaDir, toml?.corporaDir) ?? base.corporaDir,
    groups: pick(flags?.groups, env?.groups, toml?.groups) ?? base.groups,
    cache: resolveCache(
      ordered.map((layer) => layer?.cache),
      base.cache,
    ),
    logs: resolveLogs(
      ordered.map((layer) => layer?.logs),
      base.logs,
    ),
  };
}

// ── Layer parsers ────────────────────────────────────────────────────────────────────────────────

/**
 * Parse a non-negative decimal integer, ANCHORED so partial/suffixed/fractional/empty input is
 * rejected (Codex M3 NIT: `Number.parseInt("123abc")` would otherwise yield `123`). Returns `null`
 * when the (trimmed) text is not exactly `^\d+$`.
 */
function decimalInt(value: string): number | null {
  const trimmed = value.trim();
  if (!/^\d+$/.test(trimmed)) {
    return null;
  }
  const n = Number(trimmed);
  return Number.isSafeInteger(n) ? n : null;
}

function parsePort(value: string | undefined): number | undefined {
  if (value === undefined) {
    return undefined;
  }
  const n = decimalInt(value);
  if (n === null || n > 65535) {
    throw new Error(`invalid port: "${value}"`);
  }
  return n;
}

function parseGroups(value: string | undefined): string[] | undefined {
  if (value === undefined) {
    return undefined;
  }
  const groups = value
    .split(",")
    .map((g) => g.trim())
    .filter((g) => g !== "");
  return groups.length > 0 ? groups : undefined;
}

/** Parse the `DASHBOARD_*` environment layer. */
export function parseEnvConfig(env: Record<string, string | undefined>): PartialConfig {
  const cache: Partial<CacheTtls> = {};
  const cacheEnv: Array<[keyof CacheTtls, string]> = [
    ["statusMs", "DASHBOARD_CACHE_STATUS_MS"],
    ["overviewMs", "DASHBOARD_CACHE_OVERVIEW_MS"],
    ["runsMs", "DASHBOARD_CACHE_RUNS_MS"],
    ["packagesMs", "DASHBOARD_CACHE_PACKAGES_MS"],
    ["logsMs", "DASHBOARD_CACHE_LOGS_MS"],
    ["timersMs", "DASHBOARD_CACHE_TIMERS_MS"],
  ];
  for (const [key, name] of cacheEnv) {
    const raw = env[name];
    if (raw !== undefined) {
      const n = decimalInt(raw);
      if (n === null) {
        throw new Error(`invalid ${name}: "${raw}"`);
      }
      cache[key] = n;
    }
  }

  const logs: Partial<LogWindowDefaults> = {};
  if (env.DASHBOARD_LOG_LIMIT !== undefined) {
    logs.defaultLimit = parsePort(env.DASHBOARD_LOG_LIMIT); // reuse non-negative int parse (bounded)
  }
  if (env.DASHBOARD_LOG_SINCE !== undefined) {
    logs.defaultSince = env.DASHBOARD_LOG_SINCE;
  }

  return prune({
    bind: env.DASHBOARD_BIND,
    port: parsePort(env.DASHBOARD_PORT),
    producerBin: env.DASHBOARD_PRODUCER_BIN,
    producerConfig: env.DASHBOARD_PRODUCER_CONFIG,
    stateDir: env.DASHBOARD_STATE_DIR,
    corporaDir: env.DASHBOARD_CORPORA_DIR,
    groups: parseGroups(env.DASHBOARD_GROUPS),
    cache: Object.keys(cache).length > 0 ? cache : undefined,
    logs: Object.keys(logs).length > 0 ? logs : undefined,
  });
}

/** Parse the `[dashboard]` TOML layer (built-in `Bun.TOML`; tolerant of unknown keys). */
export function parseTomlConfig(text: string): PartialConfig {
  const parsed = Bun.TOML.parse(text) as Record<string, unknown>;
  const block = (parsed.dashboard ?? {}) as Record<string, unknown>;

  const asString = (v: unknown): string | undefined => (typeof v === "string" ? v : undefined);
  const asNumber = (v: unknown): number | undefined =>
    typeof v === "number" && Number.isFinite(v) ? v : undefined;
  const asStringArray = (v: unknown): string[] | undefined =>
    Array.isArray(v) && v.every((x) => typeof x === "string") ? (v as string[]) : undefined;

  const cacheBlock = (block.cache ?? {}) as Record<string, unknown>;
  const cache: Partial<CacheTtls> = {};
  const cacheKeys: Array<[keyof CacheTtls, string]> = [
    ["statusMs", "status_ms"],
    ["overviewMs", "overview_ms"],
    ["runsMs", "runs_ms"],
    ["packagesMs", "packages_ms"],
    ["logsMs", "logs_ms"],
    ["timersMs", "timers_ms"],
  ];
  for (const [key, tomlKey] of cacheKeys) {
    const n = asNumber(cacheBlock[tomlKey]);
    if (n !== undefined) {
      cache[key] = n;
    }
  }

  const logsBlock = (block.logs ?? {}) as Record<string, unknown>;
  const logs: Partial<LogWindowDefaults> = {};
  const limit = asNumber(logsBlock.default_limit);
  if (limit !== undefined) {
    logs.defaultLimit = limit;
  }
  if (typeof logsBlock.default_since === "string") {
    logs.defaultSince = logsBlock.default_since;
  } else if (logsBlock.default_since === null) {
    logs.defaultSince = null;
  }

  return prune({
    bind: asString(block.bind),
    port: asNumber(block.port),
    producerBin: asString(block.producer_bin),
    producerConfig: asString(block.producer_config),
    stateDir: asString(block.state_dir),
    corporaDir: asString(block.corpora_dir),
    groups: asStringArray(block.groups),
    cache: Object.keys(cache).length > 0 ? cache : undefined,
    logs: Object.keys(logs).length > 0 ? logs : undefined,
  });
}

/** Drop `undefined` entries so a layer never shadows a lower one with an absent value. */
function prune(config: PartialConfig): PartialConfig {
  const out: PartialConfig = {};
  for (const [key, value] of Object.entries(config)) {
    if (value !== undefined) {
      (out as Record<string, unknown>)[key] = value;
    }
  }
  return out;
}
