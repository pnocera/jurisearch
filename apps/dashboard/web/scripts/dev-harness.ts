#!/usr/bin/env bun

/**
 * web/ dev harness — stands up the committed M3 server against the committed fixtures so the SPA
 * renders REAL `ApiResult` envelopes end-to-end through the shared validators (M4 DoD).
 *
 * It builds a throwaway environment under `web/scripts/.runtime/` (gitignored):
 *   - a fake `producer-bin` that prints `fixtures/status.json` on `status`;
 *   - a `state-dir` with the run-record fixtures under `runs/<group>/*.record.json`;
 *   - a `corpora-dir` with the manifest fixture at `core/manifest.json`.
 * then launches `server/main.ts` bound to loopback. `logs`/`timers` shell out to journalctl/systemctl
 * (absent here) so those two panels degrade cleanly — exactly as asserted by the SPA.
 *
 * Usage:  bun run web/scripts/dev-harness.ts            # serves SPA + API on :8799
 *         PORT=9000 bun run web/scripts/dev-harness.ts
 * Then open http://127.0.0.1:8799/ (run `bun run build` first so web/dist exists).
 */

import { spawn } from "node:child_process";
import { chmodSync, cpSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";

const here = import.meta.dir;
const dashRoot = resolve(here, "../.."); // apps/dashboard
const fixtures = resolve(dashRoot, "fixtures");
const work = resolve(here, ".runtime");
const groups = ["legislation", "jurisprudence"];

rmSync(work, { recursive: true, force: true });
mkdirSync(work, { recursive: true });

// Fake producer: prints the status fixture on `status`, no-ops otherwise.
const fakeProducer = resolve(work, "fake-producer.sh");
writeFileSync(
  fakeProducer,
  `#!/usr/bin/env bash\nif [ "$1" = "status" ]; then\n  cat ${JSON.stringify(resolve(fixtures, "status.json"))}\n  exit 0\nfi\nexit 0\n`,
);
chmodSync(fakeProducer, 0o755);

// state-dir: runs/<group>/*.record.json (only the .record.json suffix is part of the contract).
const stateDir = resolve(work, "state");
const recordsByGroup: Record<string, string[]> = {
  legislation: [
    "runrecord-legislation-running-real.json",
    "runrecord-legislation-finished-synthetic.json",
    "runrecord-legislation-noop-synthetic.json",
  ],
  jurisprudence: ["runrecord-running-synthetic.json"],
};
for (const group of groups) {
  const dir = resolve(stateDir, "runs", group);
  mkdirSync(dir, { recursive: true });
  for (const file of recordsByGroup[group] ?? []) {
    cpSync(resolve(fixtures, file), resolve(dir, file.replace(/\.json$/, ".record.json")));
  }
}

// corpora-dir: core/manifest.json (the served signed manifest).
const corporaDir = resolve(work, "corpora");
mkdirSync(resolve(corporaDir, "core"), { recursive: true });
cpSync(
  resolve(fixtures, "manifest-with-increment-synthetic.json"),
  resolve(corporaDir, "core", "manifest.json"),
);

const bind = "127.0.0.1";
const port = process.env.PORT ?? "8799";

const child = spawn(
  "bun",
  [
    "server/main.ts",
    "--bind",
    bind,
    "--port",
    port,
    "--producer-bin",
    fakeProducer,
    "--producer-config",
    "/dev/null",
    "--state-dir",
    stateDir,
    "--corpora-dir",
    corporaDir,
    "--groups",
    groups.join(","),
  ],
  { cwd: dashRoot, stdio: "inherit" },
);

const shutdown = (): void => {
  child.kill("SIGTERM");
};
process.on("SIGINT", shutdown);
process.on("SIGTERM", shutdown);
child.on("exit", (code) => process.exit(code ?? 0));
