/**
 * The `DashboardConfig` the providers depend on (constructor injection — DIP). This is the TYPE
 * ONLY: the flags > env > toml resolution that PRODUCES a `DashboardConfig` is M3 (design §5.5).
 * Providers read these fields to locate the producer binary, its config, the on-disk state/corpora
 * dirs, and the fetch groups; the cache TTLs + log-window defaults are consumed by M3's services.
 */

/** Cache TTLs (ms) for the `Cached<T>` decorator M3 wraps around each provider (design §5.3). */
export interface CacheTtls {
  statusMs: number;
  overviewMs: number;
  runsMs: number;
  packagesMs: number;
  logsMs: number;
  timersMs: number;
}

/** Defaults for the bounded log window (`journalctl -n`/`--since`) — design §5.5. */
export interface LogWindowDefaults {
  /** `-n <limit>` when the request omits one. */
  defaultLimit: number;
  /** Optional `--since <since>` when the request omits one (`null` ⇒ no `--since`). */
  defaultSince: string | null;
}

/** The resolved dashboard configuration (design §5.5). Produced by M3; consumed read-only here. */
export interface DashboardConfig {
  /** Tailnet bind address; M3's fail-closed guard refuses `0.0.0.0`/`::`. */
  bind: string;
  port: number;
  /** Path to the `jurisearch-producer` binary (the `StatusProvider` shells out to it). */
  producerBin: string;
  /** Path to `producer.toml` passed as `status --config <producerConfig>`. */
  producerConfig: string;
  /** Producer state dir (CT 111: `/var/lib/jurisearch-producer`); runs live under `runs/<group>/`. */
  stateDir: string;
  /** Served packages dir (CT 111: `/srv/jurisearch/storebox/packages`); manifest at `core/manifest.json`. */
  corporaDir: string;
  /** The fetch groups (e.g. `legislation`, `jurisprudence`) — drives runs/logs/timers per group. */
  groups: string[];
  cache: CacheTtls;
  logs: LogWindowDefaults;
}
