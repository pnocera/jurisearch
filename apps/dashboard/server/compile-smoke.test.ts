/**
 * Compiled-binary `--version` smoke — covers the SAME path dist.sh's M6a release audit will exercise:
 * compile via `bun run compile`, run `dist/jurisearch-dashboard --version`, and assert it equals the line
 * derived from the root Cargo.toml version + the resolved commit (default and override). This is the only
 * test that proves the end-to-end stamp→compile→binary contract, not just the pure helpers.
 *
 * Heavier than the unit tests (it compiles a ~95 MB self-exec twice). Set DASHBOARD_SKIP_COMPILE_SMOKE=1
 * to skip on slow hosts; it runs by default so the gate covers the real contract.
 */
import { expect, test } from "bun:test";
import { resolve } from "node:path";
import {
  BUILD_TARGET,
  cargoTomlPath,
  dashboardRoot,
  gitOutput,
  parseWorkspaceVersion,
  resolveCommit,
} from "../scripts/stamp";
import { formatVersionLine } from "./version";

const SKIP = process.env.DASHBOARD_SKIP_COMPILE_SMOKE === "1";
const binPath = resolve(dashboardRoot, "dist", "jurisearch-dashboard");

/** Run `bun run compile` in the dashboard root with a controlled JURISEARCH_BUILD_COMMIT. */
function compile(overrideCommit: string | null): void {
  const env: Record<string, string | undefined> = { ...process.env };
  delete env.JURISEARCH_BUILD_COMMIT;
  if (overrideCommit !== null) {
    env.JURISEARCH_BUILD_COMMIT = overrideCommit;
  }
  const r = Bun.spawnSync(["bun", "run", "compile"], { cwd: dashboardRoot, env });
  if (!r.success) {
    throw new Error(`compile failed (code ${r.exitCode}): ${r.stderr.toString()}`);
  }
}

function runVersion(): string {
  const r = Bun.spawnSync([binPath, "--version"]);
  if (!r.success) {
    throw new Error(`binary --version failed: ${r.stderr.toString()}`);
  }
  return r.stdout.toString().trim();
}

async function workspaceVersion(): Promise<string> {
  return parseWorkspaceVersion(await Bun.file(cargoTomlPath).text());
}

test.skipIf(SKIP)(
  "compiled binary --version (default commit) == Cargo.toml version + git short HEAD",
  async () => {
    const version = await workspaceVersion();
    const commit = resolveCommit({
      override: undefined,
      gitShort: () => gitOutput(["rev-parse", "--short=12", "HEAD"]),
      gitFull: () => gitOutput(["rev-parse", "HEAD"]),
    });
    compile(null);
    expect(runVersion()).toBe(formatVersionLine(version, commit, BUILD_TARGET));
  },
  120_000,
);

test.skipIf(SKIP)(
  "compiled binary --version honors the JURISEARCH_BUILD_COMMIT override",
  async () => {
    const version = await workspaceVersion();
    compile("deadbeefcafe");
    expect(runVersion()).toBe(formatVersionLine(version, "deadbeefcafe", BUILD_TARGET));
  },
  120_000,
);
