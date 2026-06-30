/**
 * M5 standalone embed smoke — THE key DoD. Compiles the single self-contained binary (`bun run
 * compile`), copies ONLY the binary into an otherwise-empty scratch dir (NO on-disk `web/dist`),
 * runs it against the committed `fixtures` (M4 dev-harness fakes for producer-bin/state/corpora), and
 * asserts it serves the EMBEDDED SPA + the SPA fallback with the Spike A 404 discipline:
 *   GET /                  → 200 text/html
 *   GET /runs (deep route) → 200 text/html  (index.html fallback)
 *   GET <every file under web/dist, as `/${relpath}`> → 200 + content-type class per extension,
 *                                                       nonempty bytes for non-HTML (COMPLETENESS —
 *                                                       a dropped lazy chunk/nested asset/font fails)
 *   GET /assets/nope.js    → 404           (missing asset never serves HTML)
 * This proves the binary is self-contained AND the manifest is complete so packaging (M6a) can't
 * regress the embed.
 *
 * Heavy (compiles a ~95 MB self-exec). Set DASHBOARD_SKIP_COMPILE_SMOKE=1 to skip on slow hosts.
 */
import { afterAll, beforeAll, expect, test } from "bun:test";
import {
  chmodSync,
  copyFileSync,
  cpSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { relative, resolve } from "node:path";
import { dashboardRoot } from "../scripts/stamp";

const SKIP = process.env.DASHBOARD_SKIP_COMPILE_SMOKE === "1";
const binPath = resolve(dashboardRoot, "dist", "jurisearch-dashboard");
const fixtures = resolve(dashboardRoot, "fixtures");
const distDir = resolve(dashboardRoot, "web", "dist");

/** Expected Content-Type substring per asset extension (the class the router/`Bun.file` must serve). */
const CONTENT_TYPE_BY_EXT: Record<string, string> = {
  ".html": "text/html",
  ".js": "text/javascript",
  ".css": "text/css",
  ".woff2": "font/woff2",
  ".woff": "font/woff",
  ".json": "application/json",
  ".svg": "image/svg",
  ".map": "application/json",
};

/** Every file under `dir`, as request pathnames (`/${posix-relpath}`), matching the manifest keys. */
function walkRequestPaths(dir: string): string[] {
  const out: string[] = [];
  for (const entry of readdirSync(dir)) {
    const full = resolve(dir, entry);
    if (statSync(full).isDirectory()) {
      out.push(...walkRequestPaths(full));
    } else {
      out.push(`/${relative(distDir, full).split("\\").join("/")}`);
    }
  }
  return out;
}

let runDir = "";
let envDir = "";

/** Build the M4-style fake producer env (producer-bin + state-dir + corpora-dir) under `envDir`. */
function makeProducerEnv(): { fakeProducer: string; stateDir: string; corporaDir: string } {
  const fakeProducer = resolve(envDir, "fake-producer.sh");
  writeFileSync(
    fakeProducer,
    `#!/usr/bin/env bash\nif [ "$1" = "status" ]; then\n  cat ${JSON.stringify(resolve(fixtures, "status.json"))}\n  exit 0\nfi\nexit 0\n`,
  );
  chmodSync(fakeProducer, 0o755);

  const stateDir = resolve(envDir, "state");
  const legDir = resolve(stateDir, "runs", "legislation");
  mkdirSync(legDir, { recursive: true });
  cpSync(
    resolve(fixtures, "runrecord-legislation-finished-synthetic.json"),
    resolve(legDir, "runrecord-legislation-finished-synthetic.record.json"),
  );

  const corporaDir = resolve(envDir, "corpora");
  mkdirSync(resolve(corporaDir, "core"), { recursive: true });
  cpSync(
    resolve(fixtures, "manifest-with-increment-synthetic.json"),
    resolve(corporaDir, "core", "manifest.json"),
  );
  return { fakeProducer, stateDir, corporaDir };
}

/** Spawn the standalone binary from `runDir`; resolve the port it reports listening on. */
async function startBinary(): Promise<{ port: number; stop: () => Promise<void> }> {
  const { fakeProducer, stateDir, corporaDir } = makeProducerEnv();
  const proc = Bun.spawn(
    [
      resolve(runDir, "jurisearch-dashboard"),
      "--bind",
      "127.0.0.1",
      "--port",
      "0",
      "--producer-bin",
      fakeProducer,
      "--producer-config",
      "/dev/null",
      "--state-dir",
      stateDir,
      "--corpora-dir",
      corporaDir,
      "--groups",
      "legislation,jurisprudence",
    ],
    { cwd: runDir, stdout: "pipe", stderr: "pipe" },
  );

  const reader = proc.stdout.getReader();
  const dec = new TextDecoder();
  let buf = "";
  const deadline = Date.now() + 20_000;
  let port = 0;
  while (Date.now() < deadline) {
    const { value, done } = await reader.read();
    if (done) {
      break;
    }
    buf += dec.decode(value, { stream: true });
    const m = buf.match(/listening on http:\/\/127\.0\.0\.1:(\d+)/);
    if (m?.[1] !== undefined) {
      port = Number(m[1]);
      break;
    }
  }
  reader.releaseLock();
  if (port === 0) {
    proc.kill();
    throw new Error(`binary did not report a listening port. stdout:\n${buf}`);
  }
  return {
    port,
    stop: async () => {
      proc.kill();
      await proc.exited;
    },
  };
}

beforeAll(() => {
  if (SKIP) {
    return;
  }
  // Build the self-contained embedded binary, then isolate ONLY it in an empty run dir (no web/dist).
  const env: Record<string, string | undefined> = { ...process.env };
  delete env.JURISEARCH_BUILD_COMMIT;
  const r = Bun.spawnSync(["bun", "run", "compile"], { cwd: dashboardRoot, env });
  if (!r.success) {
    throw new Error(`compile failed (code ${r.exitCode}): ${r.stderr.toString()}`);
  }
  runDir = mkdtempSync(resolve(tmpdir(), "m5-run-"));
  envDir = mkdtempSync(resolve(tmpdir(), "m5-env-"));
  copyFileSync(binPath, resolve(runDir, "jurisearch-dashboard"));
  chmodSync(resolve(runDir, "jurisearch-dashboard"), 0o755);
});

afterAll(() => {
  if (runDir) {
    rmSync(runDir, { recursive: true, force: true });
  }
  if (envDir) {
    rmSync(envDir, { recursive: true, force: true });
  }
});

test.skipIf(SKIP)(
  "compiled binary serves EVERY embedded web/dist asset + SPA fallback standalone (no web/dist)",
  async () => {
    // The run dir holds ONLY the binary — there is no filesystem web/dist to fall back to.
    expect(existsSync(resolve(runDir, "web"))).toBe(false);
    expect(readdirSync(runDir)).toEqual(["jurisearch-dashboard"]);

    // Completeness: enumerate every file the web build emitted; the binary must serve all of them.
    const requestPaths = walkRequestPaths(distDir);
    expect(requestPaths).toContain("/index.html");
    // At least one JS, one CSS, and one woff2 font are expected (regression canary on the bundle).
    expect(requestPaths.some((p) => p.endsWith(".js"))).toBe(true);
    expect(requestPaths.some((p) => p.endsWith(".css"))).toBe(true);
    expect(requestPaths.some((p) => p.endsWith(".woff2"))).toBe(true);

    const { port, stop } = await startBinary();
    const base = `http://127.0.0.1:${port}`;
    try {
      const get = (path: string) => fetch(`${base}${path}`);

      // SPA shell + a deep-link route both 200 with text/html (index.html fallback).
      const index = await get("/");
      expect(index.status).toBe(200);
      expect(index.headers.get("content-type")).toContain("text/html");

      const deep = await get("/runs");
      expect(deep.status).toBe(200);
      expect(deep.headers.get("content-type")).toContain("text/html");

      // EVERY emitted asset must be served from the embedded bytes with the right content-type class.
      for (const path of requestPaths) {
        const ext = path.slice(path.lastIndexOf("."));
        const res = await get(path);
        expect(res.status, `status for ${path}`).toBe(200);
        const ct = res.headers.get("content-type") ?? "";
        const expected = CONTENT_TYPE_BY_EXT[ext];
        if (expected !== undefined) {
          expect(ct, `content-type for ${path}`).toContain(expected);
        }
        // Non-HTML assets must carry real bytes (HTML is served from the same shell as `/`).
        if (ext !== ".html") {
          expect((await res.bytes()).length, `bytes for ${path}`).toBeGreaterThan(0);
        }
      }

      // Spike A 404 discipline: a missing static asset must NOT serve HTML.
      const missing = await get("/assets/nope.js");
      expect(missing.status).toBe(404);
    } finally {
      await stop();
    }
  },
  180_000,
);
