/**
 * The provider seam (design §5.2). One provider per data source, each `DataProvider<T>`. A provider
 * does exactly: invoke an adapter → pure parse (a separate, unit-tested raw→DTO fn) → validate with
 * the `shared/` `safeParseX` → DTO. Adding a source (e.g. the Phase-2 PG provider) is a new
 * `DataProvider` with no edits to the others (Open/Closed).
 */

import type { Result } from "@jurisearch-dashboard/shared";

/** The uniform provider contract (LSP): every source yields its DTO the same way. */
export interface DataProvider<T> {
  get(): Promise<T>;
}

/**
 * A TYPED provider failure. Parsing/validation/IO failures are wrapped in this so M3 can catch it
 * and degrade ONE panel (`ApiResult.ok=false`) instead of an uncaught `SyntaxError`/`ValidationError`
 * blanking the dashboard (design §5.4). `source` tags which panel failed; `cause` keeps the original.
 */
export class ProviderError extends Error {
  constructor(
    /** The source/provider tag, e.g. `status`, `runs`, `packages`, `logs`, `timers`. */
    public readonly source: string,
    message: string,
    options?: { cause?: unknown },
  ) {
    super(message, options);
    this.name = "ProviderError";
  }
}

/** Parse JSON text, wrapping a `SyntaxError` as a typed `ProviderError` (never throws raw). */
export function parseJson(source: string, text: string): unknown {
  try {
    return JSON.parse(text);
  } catch (error) {
    throw new ProviderError(source, `${source}: invalid JSON`, { cause: error });
  }
}

/** Unwrap a `shared/` validator `Result`, converting an `{ok:false}` into a typed `ProviderError`. */
export function unwrap<T>(source: string, result: Result<T>): T {
  if (result.ok) {
    return result.value;
  }
  throw new ProviderError(source, `${source}: ${result.error}`);
}
