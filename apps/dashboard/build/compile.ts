#!/usr/bin/env bun

/**
 * M5 compile pipeline → a single self-contained `bun-linux-x64` binary with the SPA + fonts EMBEDDED
 * (no filesystem `web/dist` at runtime). Driven via `bun run compile`. Three ordered steps after the
 * `--version` stamp:
 *
 *   1. `bun run build`     → Vite emits content-hashed assets + index.html to `web/dist`.
 *   2. `bun run gen:embed` → walk `web/dist` → `server/embedded-assets.generated.ts` (the manifest
 *                            of `with { type: "file" }` imports keyed by request path).
 *   3. `bun build server/main.ts --compile … --define process.env.DASHBOARD_EMBEDDED=1` → Bun bundles
 *      the embedded files referenced by the manifest into `dist/jurisearch-dashboard`, and the define
 *      flips the composition root onto `EmbeddedAssetSource`.
 *
 * Run as an arg-array (no shell) so the `--define` value is passed verbatim — avoiding package.json
 * quoting pitfalls. Keeps the `--version` stamp EXACT (step 0 = `bun run stamp`, via the build_*
 * `--define`s already baked through `buildinfo.ts`; this script does not touch the version contract).
 */

import { existsSync } from "node:fs";
import { resolve } from "node:path";

const dashboardRoot = resolve(import.meta.dir, "..");

function step(label: string, cmd: string[]): void {
  const r = Bun.spawnSync(cmd, { cwd: dashboardRoot, stdio: ["inherit", "inherit", "inherit"] });
  if (!r.success) {
    console.error(`compile: step "${label}" failed (exit ${r.exitCode ?? "?"})`);
    process.exit(r.exitCode ?? 1);
  }
}

step("stamp", ["bun", "run", "stamp"]);
step("build", ["bun", "run", "build"]);
step("gen:embed", ["bun", "run", "gen:embed"]);

// Required-dist guard: refuse to compile against a stub manifest. The binary embeds whatever the
// manifest references, so a missing `web/dist/index.html` here would yield a stub binary that fails
// closed at startup (main.ts `selectAssetSource`). Fail at BUILD time instead, with a clear message.
const indexHtml = resolve(dashboardRoot, "web", "dist", "index.html");
if (!existsSync(indexHtml)) {
  console.error(
    `compile: missing ${indexHtml} after build — refusing to compile a stub (no embedded SPA). ` +
      "Ensure `bun run build` produced web/dist.",
  );
  process.exit(1);
}

step("compile", [
  "bun",
  "build",
  "server/main.ts",
  "--compile",
  "--target=bun-linux-x64",
  "--define",
  "process.env.DASHBOARD_EMBEDDED=1",
  "--outfile",
  "dist/jurisearch-dashboard",
]);
