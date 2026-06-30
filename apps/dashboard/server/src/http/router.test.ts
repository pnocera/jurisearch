/**
 * HTTP router tests — drive the handler with the M2 in-memory fakes (fixture-backed) and assert:
 *   - each `/api/*` returns the right `ApiResult<T>` DTO shape/values,
 *   - per-endpoint isolation: a throwing provider degrades ONE endpoint while the others stay ok,
 *   - the SPA-fallback Spike A discipline (navigation → index.html; missing asset → 404; present
 *     hashed asset → 200 + content-type) over a fake `AssetSource`,
 *   - the fail-closed bind guard (wildcard refused & never bound; explicit address binds + serves).
 */

import { describe, expect, test } from "bun:test";
import { resolve } from "node:path";
import type {
  ApiResult,
  HealthDTO,
  LogLineDTO,
  OverviewDTO,
  PackageManifestDTO,
  RunRecordDTO,
} from "@jurisearch-dashboard/shared";
import type { FileSource, ProcessRunner, RunResult, StatInfo } from "../adapters/types.ts";
import type { DashboardConfig } from "../config.ts";
import { LogsProvider } from "../providers/logs.ts";
import { PackagesProvider } from "../providers/packages.ts";
import { RunsProvider } from "../providers/runs.ts";
import { StatusProvider } from "../providers/status.ts";
import { TimersProvider } from "../providers/timers.ts";
import { ProviderError } from "../providers/types.ts";
import { OverviewService } from "../services/OverviewService.ts";
import type { DashboardServices } from "../services/types.ts";
import { type AssetResponse, type AssetSource, DevAssetSource } from "./assets.ts";
import { assertExplicitBind, BindGuardError } from "./bind.ts";
import { createFetchHandler, positiveInt, startServer } from "./router.ts";

// ── Fixtures + fakes ───────────────────────────────────────────────────────────────────────────────

const FIXTURES = resolve(import.meta.dir, "../../../fixtures");
const fixtureText = (name: string): Promise<string> => Bun.file(resolve(FIXTURES, name)).text();

/** Read an `/api/*` body as its typed `ApiResult<T>` (the endpoint envelope). */
async function apiResult<T>(res: Response): Promise<ApiResult<T>> {
  return (await res.json()) as ApiResult<T>;
}

/** Assert the envelope is `ok` and return the payload (fails the test otherwise). */
function dataOf<T>(result: ApiResult<T>): T {
  if (!result.ok) {
    throw new Error(`expected ok:true, got error: ${result.error.message}`);
  }
  return result.data;
}

const config: DashboardConfig = {
  bind: "127.0.0.1",
  port: 0,
  producerBin: "/usr/local/bin/jurisearch-producer",
  producerConfig: "/etc/jurisearch/producer.toml",
  stateDir: "/state",
  corporaDir: "/packages",
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

const ok = (stdout: string): RunResult => ({ stdout, stderr: "", code: 0 });

/** A `ProcessRunner` that dispatches by command: status / journalctl / systemctl. */
class FakeProcess implements ProcessRunner {
  constructor(
    private readonly statusOut: string,
    private readonly journalOut: string,
    private readonly timersOut: string,
  ) {}
  run(cmd: string[]): Promise<RunResult> {
    if (cmd[1] === "status") return Promise.resolve(ok(this.statusOut));
    if (cmd[0] === "journalctl") return Promise.resolve(ok(this.journalOut));
    if (cmd[0] === "systemctl") return Promise.resolve(ok(this.timersOut));
    return Promise.resolve({ stdout: "", stderr: "unknown command", code: 1 });
  }
}

class FakeFiles implements FileSource {
  constructor(
    private readonly files: Map<string, string>,
    private readonly dirs: Map<string, string[]>,
  ) {}
  read(path: string): Promise<string> {
    const v = this.files.get(path);
    return v === undefined ? Promise.reject(new Error(`ENOENT: ${path}`)) : Promise.resolve(v);
  }
  stat(path: string): Promise<StatInfo> {
    const isFile = this.files.has(path);
    const isDirectory = this.dirs.has(path);
    if (!isFile && !isDirectory) return Promise.reject(new Error(`ENOENT: ${path}`));
    return Promise.resolve({ isFile, isDirectory, size: 0, mtimeMs: 0 });
  }
  list(dir: string): Promise<string[]> {
    const v = this.dirs.get(dir);
    return v === undefined ? Promise.reject(new Error(`ENOENT: ${dir}`)) : Promise.resolve(v);
  }
}

const fakeHealth = (): HealthDTO => ({
  status: "ok",
  name: "Juridia — Update Server",
  version: "jurisearch-dashboard 0.1.0 (abc, x86_64-unknown-linux-gnu)",
  uptimeMs: 1,
  now: "2026-06-30T13:26:08Z",
});

/** Build a real services bundle wired over the fixture-backed fakes. */
async function buildServices(): Promise<DashboardServices> {
  const statusOut = await fixtureText("status.json");
  // journalctl emits NDJSON (one object per line); the fixture is a single pretty-printed object.
  const journalOut = `${JSON.stringify(await Bun.file(resolve(FIXTURES, "journal-legislation.json")).json())}\n`;
  const timersOut = await fixtureText("timers.json");
  const proc = new FakeProcess(statusOut, journalOut, timersOut);

  const files = new Map<string, string>([
    ["/packages/core/manifest.json", await fixtureText("manifest.json")],
    [
      "/state/runs/legislation/running.record.json",
      await fixtureText("runrecord-legislation-running-real.json"),
    ],
  ]);
  const dirs = new Map<string, string[]>([["/state/runs/legislation", ["running.record.json"]]]);
  const fileSrc = new FakeFiles(files, dirs);

  const statusProvider = new StatusProvider(proc, config);
  const runsProvider = new RunsProvider(fileSrc, config);
  const packagesProvider = new PackagesProvider(fileSrc, config);
  const logsProvider = new LogsProvider(proc, config);
  const timersProvider = new TimersProvider(proc, config);
  const overview = new OverviewService(statusProvider, runsProvider, timersProvider);

  return {
    overview: () => overview.get(),
    runs: (q) => runsProvider.get(q),
    packages: () => packagesProvider.get(),
    logs: (q) => logsProvider.get(q),
  };
}

/** An asset source over an in-memory map. */
class FakeAssets implements AssetSource {
  constructor(
    private readonly idx: AssetResponse | null,
    private readonly map: Map<string, AssetResponse>,
  ) {}
  index(): Promise<AssetResponse | null> {
    return Promise.resolve(this.idx);
  }
  asset(pathname: string): Promise<AssetResponse | null> {
    return Promise.resolve(this.map.get(pathname) ?? null);
  }
}

const asset = (text: string, contentType: string): AssetResponse => ({
  body: new TextEncoder().encode(text),
  contentType,
});

const noAssets = new FakeAssets(null, new Map());

const handlerWith = async (
  assets: AssetSource = noAssets,
): Promise<ReturnType<typeof createFetchHandler>> =>
  createFetchHandler({ services: await buildServices(), assets, health: fakeHealth });

// ── /api/* DTO shapes ──────────────────────────────────────────────────────────────────────────────

describe("/api/* endpoints (fixture-backed fakes)", () => {
  test("GET /api/overview ⇒ ApiResult<OverviewDTO>", async () => {
    const handler = await handlerWith();
    const data = dataOf(
      await apiResult<OverviewDTO>(await handler(new Request("http://x/api/overview"))),
    );
    expect(data.corpus).toBe("core");
    expect(data.groups.map((g) => g.group)).toEqual(["legislation", "jurisprudence"]);
    expect(data.groups[0]?.severity).toBe("neutral"); // running ⇒ neutral
  });

  test("GET /api/runs ⇒ ApiResult<RunRecordDTO[]>; honours group+limit", async () => {
    const handler = await handlerWith();
    const data = dataOf(
      await apiResult<RunRecordDTO[]>(
        await handler(new Request("http://x/api/runs?group=legislation&limit=1")),
      ),
    );
    expect(data.length).toBe(1);
    expect(data[0]?.group).toBe("legislation");
    expect(data[0]?.exitClass).toBe("running");
  });

  test("GET /api/packages ⇒ ApiResult<PackageManifestDTO>", async () => {
    const handler = await handlerWith();
    const data = dataOf(
      await apiResult<PackageManifestDTO>(await handler(new Request("http://x/api/packages"))),
    );
    expect(data.headSequence).toBe(1);
    expect(data.packages).toEqual([]);
  });

  test("GET /api/logs ⇒ ApiResult<LogLineDTO[]>", async () => {
    const handler = await handlerWith();
    const data = dataOf(
      await apiResult<LogLineDTO[]>(
        await handler(new Request("http://x/api/logs?group=legislation")),
      ),
    );
    expect(data.length).toBe(1);
    expect(data[0]?.unit).toBe("jurisearch-producer-legislation.service");
  });

  test("GET /api/health ⇒ ApiResult<HealthDTO>", async () => {
    const handler = await handlerWith();
    const data = dataOf(
      await apiResult<HealthDTO>(await handler(new Request("http://x/api/health"))),
    );
    expect(data.status).toBe("ok");
    expect(data.name).toBe("Juridia — Update Server");
  });

  test("non-GET is refused (read-only) with 405", async () => {
    const handler = await handlerWith();
    const res = await handler(new Request("http://x/api/overview", { method: "POST" }));
    expect(res.status).toBe(405);
  });
});

describe("positiveInt query parsing is anchored", () => {
  test("rejects suffixed/fractional/empty/zero/negative; accepts a clean positive integer", () => {
    expect(positiveInt("10junk")).toBeUndefined();
    expect(positiveInt("1.5")).toBeUndefined();
    expect(positiveInt("")).toBeUndefined();
    expect(positiveInt("0")).toBeUndefined();
    expect(positiveInt("-5")).toBeUndefined();
    expect(positiveInt(null)).toBeUndefined();
    expect(positiveInt("50")).toBe(50);
    expect(positiveInt(" 50 ")).toBe(50);
  });
});

// ── Per-endpoint isolation (degraded panel) ─────────────────────────────────────────────────────

describe("degraded path: one bad source degrades ONE endpoint", () => {
  test("a throwing packages provider ⇒ /api/packages {ok:false} while /api/overview stays {ok:true}", async () => {
    const good = await buildServices();
    const services: DashboardServices = {
      ...good,
      packages: () =>
        Promise.reject(new ProviderError("packages", "packages: cannot read manifest")),
    };
    const handler = createFetchHandler({ services, assets: noAssets, health: fakeHealth });

    const pkg = await apiResult<PackageManifestDTO>(
      await handler(new Request("http://x/api/packages")),
    );
    expect(pkg.ok).toBe(false);
    if (pkg.ok) {
      throw new Error("expected degraded packages");
    }
    expect(pkg.error.code).toBe("packages");
    expect(pkg.error.message).toContain("cannot read manifest");

    const overview = await apiResult<OverviewDTO>(
      await handler(new Request("http://x/api/overview")),
    );
    expect(overview.ok).toBe(true); // other endpoints unaffected — process stays up
  });

  test("a non-ProviderError throw still degrades (never crashes the handler)", async () => {
    const good = await buildServices();
    const services: DashboardServices = {
      ...good,
      logs: () => Promise.reject(new Error("kaboom")),
    };
    const handler = createFetchHandler({ services, assets: noAssets, health: fakeHealth });
    const body = await apiResult<LogLineDTO[]>(await handler(new Request("http://x/api/logs")));
    expect(body.ok).toBe(false);
    if (body.ok) {
      throw new Error("expected degraded logs");
    }
    expect(body.error.message).toBe("kaboom");
  });
});

// ── SPA fallback (Spike A discipline) ───────────────────────────────────────────────────────────

describe("SPA fallback over a fake AssetSource", () => {
  const assets = new FakeAssets(
    asset("<!doctype html>", "text/html; charset=utf-8"),
    new Map([["/index-abc123.js", asset("console.log(1)", "text/javascript;charset=utf-8")]]),
  );

  test("navigation/deep-link route ⇒ index.html (200)", async () => {
    const handler = await handlerWith(assets);
    for (const path of ["/", "/runs", "/logs/123"]) {
      const res = await handler(new Request(`http://x${path}`));
      expect(res.status).toBe(200);
      expect(res.headers.get("content-type")).toContain("text/html");
      expect(await res.text()).toContain("<!doctype html>");
    }
  });

  test("present hashed asset ⇒ 200 + its content-type", async () => {
    const handler = await handlerWith(assets);
    const res = await handler(new Request("http://x/index-abc123.js"));
    expect(res.status).toBe(200);
    expect(res.headers.get("content-type")).toContain("text/javascript");
  });

  test("missing static asset ⇒ 404 (never 200 index.html for a missing *.js)", async () => {
    const handler = await handlerWith(assets);
    const res = await handler(new Request("http://x/index-missing.js"));
    expect(res.status).toBe(404);
  });

  test("missing *.css ⇒ 404 too", async () => {
    const handler = await handlerWith(assets);
    const res = await handler(new Request("http://x/styles-gone.css"));
    expect(res.status).toBe(404);
  });
});

describe("DevAssetSource over the committed web/dist", () => {
  const dist = resolve(import.meta.dir, "../../../web/dist");
  const src = new DevAssetSource(dist);

  test("serves index.html", async () => {
    const idx = await src.index();
    expect(idx).not.toBeNull();
    expect(idx?.contentType).toContain("text/html");
  });

  test("serves a real hashed asset with a JS content-type", async () => {
    // web/dist is content-addressed (hashes change every build), so discover a real hashed JS
    // asset from the committed dist rather than pinning a build-specific hash.
    const { readdirSync } = await import("node:fs");
    const assetsDir = resolve(dist, "assets");
    const jsName = readdirSync(assetsDir).find((f) => f.endsWith(".js"));
    expect(jsName).toBeDefined();
    const a = await src.asset(`/assets/${jsName}`);
    expect(a).not.toBeNull();
    expect(a?.contentType).toContain("javascript");
  });

  test("a missing asset ⇒ null (router turns this into a 404)", async () => {
    expect(await src.asset("/assets/nope.js")).toBeNull();
  });
});

// ── Fail-closed bind guard ──────────────────────────────────────────────────────────────────────

// Every all-interfaces spelling Bun.serve would otherwise accept (Codex M3 BLOCKER).
const WILDCARD_SPELLINGS = [
  "0.0.0.0",
  "000.000.000.000",
  "0",
  "0.0",
  "0.0.0",
  "0x0",
  "0x0.0x0.0x0.0x0",
  "::",
  "[::]",
  "::0",
  "0:0:0:0:0:0:0:0",
  "0000:0000:0000:0000:0000:0000:0000:0000",
  "[0:0:0:0:0:0:0:0]",
  "::ffff:0.0.0.0",
  "::ffff:0:0",
  "",
  "   ",
  "*",
];

describe("bind guard (fail closed)", () => {
  test("refuses every wildcard/unspecified spelling (normalize + classify, not a deny-list)", () => {
    for (const bad of WILDCARD_SPELLINGS) {
      expect(() => assertExplicitBind(bad)).toThrow(BindGuardError);
    }
  });

  test("accepts an explicit tailnet address, loopback, hostname, and a concrete IPv6", () => {
    expect(assertExplicitBind("100.71.35.39")).toBe("100.71.35.39");
    expect(assertExplicitBind("127.0.0.1")).toBe("127.0.0.1");
    expect(assertExplicitBind("jurisearch-update")).toBe("jurisearch-update");
    expect(assertExplicitBind("::1")).toBe("::1"); // IPv6 loopback
    expect(assertExplicitBind("::ffff:127.0.0.1")).toBe("::ffff:127.0.0.1");
  });

  test("startServer refuses EVERY wildcard spelling and never binds (no server created)", async () => {
    const deps = { services: await buildServices(), assets: noAssets, health: fakeHealth };
    for (const bad of WILDCARD_SPELLINGS) {
      let server: ReturnType<typeof startServer> | undefined;
      expect(() => {
        server = startServer({ ...config, bind: bad, port: 0 }, deps);
      }).toThrow(BindGuardError);
      // The guard throws BEFORE Bun.serve, so nothing was bound; clean up just in case it didn't.
      expect(server).toBeUndefined();
      server?.stop(true);
    }
  });

  test("startServer on loopback actually binds and serves /api/health", async () => {
    const deps = { services: await buildServices(), assets: noAssets, health: fakeHealth };
    const server = startServer({ ...config, bind: "127.0.0.1", port: 0 }, deps);
    try {
      const res = await fetch(`http://127.0.0.1:${server.port}/api/health`);
      const body = await apiResult<HealthDTO>(res);
      expect(body.ok).toBe(true);
      expect(dataOf(body).status).toBe("ok");
    } finally {
      server.stop(true);
    }
  });
});
