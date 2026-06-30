/**
 * web/ — the ONE typed HTTP client (design §6.1). `fetchJson<T>` fetches an `/api/*` endpoint and
 * returns the SHARED `ApiResult<T>` envelope, NEVER throwing on a degraded result:
 *
 *   - transport failure (network / non-2xx / non-JSON)  → `{ ok:false, error:{ code:"transport" }}`
 *   - the server's own degraded envelope (`{ ok:false }`) → surfaced VERBATIM (the panel degrades,
 *     the rest of the dashboard keeps rendering — design §5.4)
 *   - a 200 `{ ok:true, data }` whose `data` fails the shared-toolkit validator → `{ ok:false,
 *     error:{ code:"schema" }}` (a contract drift degrades that one panel rather than rendering junk)
 *
 * CONTRACT NOTE (reported to the orchestrator, NOT a `shared/` edit): the producer-facing validators
 * (`safeParseStatus`/`safeParseRunRecord`/`safeParsePackage`/…) cannot be reused here — they map the
 * producer's RAW snake_case JSON, while `/api/*` bodies are the server's already-mapped camelCase
 * DTOs (and `/api/overview`+`/api/health` are server-composed shapes with no producer validator, and
 * `/api/packages` returns the UNWRAPPED payload, not the `{payload}` wrapper `safeParsePackage`
 * expects). The camelCase readers in `api/schema.ts` are therefore composed from the shared
 * validator TOOLKIT and typed as `Reader<TheSharedDTO>` so drift still fails the typecheck.
 */

import { type ApiResult, type Reader, safeParse } from "@jurisearch-dashboard/shared";

/** Narrow an arbitrary parsed body to the degraded `{ ok:false }` branch of `ApiResult`. */
function asDegraded(body: unknown): { code?: string; message: string } | null {
  if (typeof body !== "object" || body === null) {
    return null;
  }
  const record = body as Record<string, unknown>;
  if (record.ok !== false) {
    return null;
  }
  const error = record.error;
  if (typeof error === "object" && error !== null) {
    const e = error as Record<string, unknown>;
    const message = typeof e.message === "string" ? e.message : "degraded";
    const code = typeof e.code === "string" ? e.code : undefined;
    return { message, ...(code !== undefined ? { code } : {}) };
  }
  return { message: "degraded" };
}

/** Is this a successful `{ ok:true, data }` envelope? */
function successData(body: unknown): { found: true; data: unknown } | { found: false } {
  if (typeof body === "object" && body !== null) {
    const record = body as Record<string, unknown>;
    if (record.ok === true) {
      return { found: true, data: record.data };
    }
  }
  return { found: false };
}

/**
 * Fetch an `/api/*` endpoint and validate the `ApiResult<T>` body with the supplied shared-toolkit
 * `reader`. Always resolves (never rejects); a failure is the degraded `{ ok:false }` branch.
 */
export async function fetchJson<T>(path: string, reader: Reader<T>): Promise<ApiResult<T>> {
  let body: unknown;
  try {
    const response = await fetch(path, { headers: { Accept: "application/json" } });
    if (!response.ok) {
      return {
        ok: false,
        error: { code: "transport", message: `HTTP ${response.status} for ${path}` },
      };
    }
    body = await response.json();
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    return { ok: false, error: { code: "transport", message } };
  }

  // The server's OWN degraded envelope passes through untouched (one provider failed upstream).
  const degraded = asDegraded(body);
  if (degraded !== null) {
    return { ok: false, error: degraded };
  }

  const success = successData(body);
  if (!success.found) {
    return { ok: false, error: { code: "schema", message: `malformed envelope from ${path}` } };
  }

  const parsed = safeParse(reader, success.data);
  if (!parsed.ok) {
    return { ok: false, error: { code: "schema", message: parsed.error } };
  }
  return { ok: true, data: parsed.value };
}
