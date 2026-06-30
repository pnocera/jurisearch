/**
 * `Cached<T>` — the ONE generic TTL+dedupe decorator (design §5.3, DRY: not duplicated per
 * provider). It wraps any async source — a zero-arg `DataProvider<T>.get` or a service `get()`, and
 * (via the optional key function) a parameterised provider like runs/logs — and:
 *   - returns the last value while it is younger than `ttlMs` (no re-invoke under a fast UI poll),
 *   - dedupes concurrent in-flight calls (one source call serves all waiters),
 *   - re-invokes once the value has expired,
 *   - NEVER caches a rejection (a degraded source is retried on the next call, bounded by the poll).
 *
 * Time is injected as a `now()` clock so tests advance it deterministically instead of sleeping;
 * production passes `Date.now`.
 *
 * When `A = []` (the default), `Cached<T>` satisfies `DataProvider<T>` (LSP) — services and the
 * router treat a cached source exactly like a raw one. A parameterised source (e.g. runs/logs) sets
 * `A` to its query tuple and supplies `keyOf` so each distinct query gets its own TTL slot.
 */

/** A monotonic-enough millisecond clock; injected so tests don't depend on wall-clock. */
export type Clock = () => number;

interface CacheEntry<T> {
  value: T;
  expiresAt: number;
}

export class Cached<T, A extends unknown[] = []> {
  private readonly entries = new Map<string, CacheEntry<T>>();
  private readonly inflight = new Map<string, Promise<T>>();

  constructor(
    private readonly source: (...args: A) => Promise<T>,
    private readonly ttlMs: number,
    private readonly now: Clock = Date.now,
    /** Cache key per argument tuple; defaults to a single slot (zero-arg providers/services). */
    private readonly keyOf: (...args: A) => string = () => "",
  ) {}

  get(...args: A): Promise<T> {
    const key = this.keyOf(...args);

    const entry = this.entries.get(key);
    if (entry !== undefined && this.now() < entry.expiresAt) {
      return Promise.resolve(entry.value);
    }

    const pending = this.inflight.get(key);
    if (pending !== undefined) {
      return pending;
    }

    const promise = this.source(...args)
      .then((value) => {
        // Stamp expiry from completion time so a slow source doesn't expire instantly.
        this.entries.set(key, { value, expiresAt: this.now() + this.ttlMs });
        return value;
      })
      .finally(() => {
        this.inflight.delete(key);
      });

    this.inflight.set(key, promise);
    return promise;
  }
}
