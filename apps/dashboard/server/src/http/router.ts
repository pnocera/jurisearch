/**
 * The ONE `Bun.serve` router (design §5.4). Two concerns, both read-only:
 *
 *  1. `/api/*` JSON — each handler calls a service and wraps the result in `ApiResult<T>`. A
 *     `ProviderError` (or any throw) becomes `{ ok:false, error }` for THAT endpoint only: the
 *     process stays up and the other endpoints keep serving (per-endpoint isolation, design §5.4).
 *
 *  2. SPA fallback — everything else goes through the `AssetSource` with the Spike A discipline
 *     (RESULT.md gotcha #1): serve an exact asset when it exists; **404** an unmatched path that
 *     looks like a static asset (a missing `*.js`/`*.css` must NOT 200 `index.html`); only fall
 *     through to `index.html` for navigation/deep-link routes. Content-Type comes from the
 *     `AssetSource` (`Bun.file().type`) — no MIME table.
 *
 * No route mutates anything. The handler is pure over its injected `RouterDeps`, so it is driven
 * directly in tests (no socket needed); `startServer` adds the fail-closed bind + `Bun.serve`.
 */

import type { ApiResult, HealthDTO } from "@jurisearch-dashboard/shared";
import type { DashboardConfig } from "../config.ts";
import type { LogsQuery } from "../providers/logs.ts";
import type { RunsQuery } from "../providers/runs.ts";
import { ProviderError } from "../providers/types.ts";
import type { DashboardServices } from "../services/types.ts";
import type { AssetSource } from "./assets.ts";
import { assertExplicitBind } from "./bind.ts";

/** Everything the router needs, injected at the composition root (DIP). */
export interface RouterDeps {
  services: DashboardServices;
  assets: AssetSource;
  /** Builds the dashboard's own liveness payload (design §10); injected for testability. */
  health: () => HealthDTO;
}

export type FetchHandler = (req: Request) => Promise<Response>;

// ── ApiResult helpers ──────────────────────────────────────────────────────────────────────────────

/** Run one endpoint, converting a thrown `ProviderError`/error into the degraded envelope. */
async function endpoint<T>(fn: () => Promise<T>): Promise<ApiResult<T>> {
  try {
    return { ok: true, data: await fn() };
  } catch (error) {
    if (error instanceof ProviderError) {
      return { ok: false, error: { code: error.source, message: error.message } };
    }
    const message = error instanceof Error ? error.message : String(error);
    return { ok: false, error: { message } };
  }
}

/** Serialise an `ApiResult<T>` as JSON. The envelope (not the HTTP status) carries success/failure. */
function json<T>(result: ApiResult<T>): Response {
  return Response.json(result);
}

// ── Query parsing ────────────────────────────────────────────────────────────────────────────────

/**
 * Parse a strictly-positive integer query param; `undefined` when absent or invalid. ANCHORED so a
 * suffixed/fractional value (`?limit=10junk`, `?limit=1.5`) is rejected rather than truncated to 10
 * (Codex M3 NIT).
 */
export function positiveInt(value: string | null): number | undefined {
  if (value === null) {
    return undefined;
  }
  const trimmed = value.trim();
  if (!/^\d+$/.test(trimmed)) {
    return undefined;
  }
  const n = Number(trimmed);
  return Number.isSafeInteger(n) && n > 0 ? n : undefined;
}

function runsQuery(params: URLSearchParams): RunsQuery {
  return { group: params.get("group") ?? undefined, limit: positiveInt(params.get("limit")) };
}

function logsQuery(params: URLSearchParams): LogsQuery {
  return {
    group: params.get("group") ?? undefined,
    limit: positiveInt(params.get("limit")),
    since: params.get("since") ?? undefined,
  };
}

// ── SPA fallback (Spike A discipline) ───────────────────────────────────────────────────────────

/** Does the path's last segment carry a file extension (so a miss must 404, not serve index.html)? */
function looksLikeAsset(pathname: string): boolean {
  const last = pathname.slice(pathname.lastIndexOf("/") + 1);
  return /\.[^./]+$/.test(last);
}

async function serveSpa(pathname: string, assets: AssetSource): Promise<Response> {
  const asset = await assets.asset(pathname);
  if (asset !== null) {
    return new Response(asset.body, { headers: { "Content-Type": asset.contentType } });
  }
  // Miss: a static-asset-shaped path must 404 (never serve HTML where JS/CSS was expected).
  if (looksLikeAsset(pathname)) {
    return new Response("Not Found", { status: 404 });
  }
  // Navigation/deep-link route → the SPA shell.
  const index = await assets.index();
  if (index === null) {
    return new Response("Not Found", { status: 404 });
  }
  return new Response(index.body, { headers: { "Content-Type": index.contentType } });
}

// ── The handler ──────────────────────────────────────────────────────────────────────────────────

export function createFetchHandler(deps: RouterDeps): FetchHandler {
  const { services, assets, health } = deps;

  return async (req: Request): Promise<Response> => {
    // Read-only: only GET/HEAD are ever served.
    if (req.method !== "GET" && req.method !== "HEAD") {
      return new Response("Method Not Allowed", { status: 405 });
    }

    const url = new URL(req.url);
    const { pathname } = url;

    switch (pathname) {
      case "/api/overview":
        return json(await endpoint(() => services.overview()));
      case "/api/runs":
        return json(await endpoint(() => services.runs(runsQuery(url.searchParams))));
      case "/api/packages":
        return json(await endpoint(() => services.packages()));
      case "/api/logs":
        return json(await endpoint(() => services.logs(logsQuery(url.searchParams))));
      case "/api/health":
        return json(await endpoint(async () => health()));
    }

    if (pathname.startsWith("/api/")) {
      return json<never>({
        ok: false,
        error: { code: "router", message: `unknown endpoint: ${pathname}` },
      });
    }

    return serveSpa(pathname, assets);
  };
}

// ── Server lifecycle ────────────────────────────────────────────────────────────────────────────

/**
 * Start `Bun.serve` after the fail-closed bind guard. Throws (and binds nothing) if `config.bind` is
 * a wildcard/unspecified address — a misconfiguration stops the process, never silently exposes it.
 */
export function startServer(
  config: DashboardConfig,
  deps: RouterDeps,
): ReturnType<typeof Bun.serve> {
  const hostname = assertExplicitBind(config.bind);
  return Bun.serve({ hostname, port: config.port, fetch: createFetchHandler(deps) });
}
