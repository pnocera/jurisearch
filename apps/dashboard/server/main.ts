/**
 * jurisearch-dashboard — entrypoint + composition root (DIP: the ONLY place concretes are wired).
 *
 * Flow: parse CLI → (`--version` prints the EXACT contract line and exits) → resolve config
 * (flags > env > toml > CT-111 defaults) → wire real adapters → providers → `Cached` → services →
 * router → fail-closed `Bun.serve`. Every concrete (`ProcessAdapter`/`FileAdapter`, the providers,
 * the caches) is constructed here and nowhere else; the rest of the server depends only on interfaces.
 */

import { DASHBOARD_NAME, type HealthDTO } from "@jurisearch-dashboard/shared";
import { BUILD_COMMIT, BUILD_TARGET, BUILD_VERSION } from "./buildinfo";
import { FileAdapter } from "./src/adapters/file.ts";
import { ProcessAdapter } from "./src/adapters/process.ts";
import { parseCliFlags } from "./src/config/flags.ts";
import { parseEnvConfig, parseTomlConfig, resolveConfig } from "./src/config/resolve.ts";
import { type AssetSource, DevAssetSource, EmbeddedAssetSource } from "./src/http/assets.ts";
import { BindGuardError } from "./src/http/bind.ts";
import { startServer } from "./src/http/router.ts";
import { LogsProvider } from "./src/providers/logs.ts";
import { PackagesProvider } from "./src/providers/packages.ts";
import { RunsProvider } from "./src/providers/runs.ts";
import { StatusProvider } from "./src/providers/status.ts";
import { TimersProvider } from "./src/providers/timers.ts";
import { composeServices } from "./src/services/compose.ts";
import { formatVersionLine } from "./version";

// HARD CONTRACT: this exact line must match dist.sh's release audit (dist.sh:274-293) and
// deploy.sh's compare (deploy.sh:165-170,:502-507): `<bin> <version> (<commit>, <target>)`.
const VERSION_LINE = formatVersionLine(BUILD_VERSION, BUILD_COMMIT, BUILD_TARGET);

/** The generated embed manifest's shape (`build/gen-embed.ts`). */
interface EmbedManifest {
  EMBEDDED_INDEX: string | null;
  EMBEDDED_ASSETS: Record<string, string>;
}

/**
 * Pure selector (testable): choose the SPA `AssetSource` and FAIL CLOSED in embedded mode.
 *
 *  - Embedded (`DASHBOARD_EMBEDDED` truthy — the compiled binary, via `--define`): serve from the
 *    bundled bytes. If the manifest is the empty STUB (`EMBEDDED_INDEX === null`, what
 *    `gen-embed.ts` writes when `web/dist` is absent at build time), THROW — a binary compiled
 *    against a stub has no embedded SPA and must not silently fall back to a filesystem `web/dist`
 *    that does not exist beside it. (`build/compile.ts` also guards against producing such a binary.)
 *  - Dev (`bun server/main.ts`, no `--define`): read the live on-disk `web/dist` via `DevAssetSource`;
 *    the stub is a valid typecheck/dev artifact only on this branch.
 */
export function selectAssetSource(
  embedded: boolean,
  manifest: EmbedManifest,
  devDistDir: string,
): AssetSource {
  if (embedded) {
    if (manifest.EMBEDDED_INDEX === null) {
      throw new Error(
        "embedded build has no assets: the generated manifest is a stub (no web/dist at build " +
          "time). Rebuild the self-contained binary via `bun run compile`.",
      );
    }
    return new EmbeddedAssetSource(manifest.EMBEDDED_INDEX, manifest.EMBEDDED_ASSETS);
  }
  return new DevAssetSource(devDistDir);
}

/**
 * Resolve the SPA `AssetSource`. The compiled binary is built with `--define
 * process.env.DASHBOARD_EMBEDDED=1` (build/compile.ts), which Bun inlines so the embedded branch is
 * taken; the generated manifest's `with { type: "file" }` imports are then bundled into the
 * executable. The manifest is imported LAZILY so the gitignored generated file is never required on
 * the dev path.
 */
async function resolveAssetSource(): Promise<AssetSource> {
  const embedded = Boolean(process.env.DASHBOARD_EMBEDDED);
  const manifest: EmbedManifest = embedded
    ? await import("./embedded-assets.generated.ts")
    : { EMBEDDED_INDEX: null, EMBEDDED_ASSETS: {} };
  return selectAssetSource(embedded, manifest, `${import.meta.dir}/../web/dist`);
}

async function run(argv: readonly string[]): Promise<void> {
  const cli = parseCliFlags(argv);
  if (cli.version) {
    // EXACTLY the contract line, nothing else. Never starts a server.
    console.log(VERSION_LINE);
    return;
  }

  // Resolve config: flags > env > toml > CT-111 defaults.
  const tomlPath = cli.configPath ?? process.env.DASHBOARD_CONFIG;
  const toml = tomlPath ? parseTomlConfig(await Bun.file(tomlPath).text()) : undefined;
  const config = resolveConfig({
    flags: cli.overrides,
    env: parseEnvConfig(process.env),
    toml,
  });

  // ── Composition root: the ONLY place concretes are wired (DIP) ──────────────────────────────────
  const proc = new ProcessAdapter();
  const files = new FileAdapter();

  // raw providers → `Cached` (per-resource TTLs) → services; wall clock in prod.
  const services = composeServices(
    {
      status: new StatusProvider(proc, config),
      runs: new RunsProvider(files, config),
      packages: new PackagesProvider(files, config),
      logs: new LogsProvider(proc, config),
      timers: new TimersProvider(proc, config),
    },
    config.cache,
    Date.now,
  );

  const startedAt = Date.now();
  const health = (): HealthDTO => ({
    status: "ok",
    name: DASHBOARD_NAME,
    version: VERSION_LINE,
    uptimeMs: Date.now() - startedAt,
    now: new Date().toISOString(),
  });

  // SPA assets behind the M3 `AssetSource` seam: embedded bytes for the compiled binary, the live
  // on-disk `web/dist` for dev (`bun server/main.ts`). See `resolveAssetSource`.
  const assets = await resolveAssetSource();

  // Fail-closed bind: `startServer` refuses a wildcard address and never binds it.
  const server = startServer(config, { services, assets, health });
  console.log(
    `${DASHBOARD_NAME} — ${VERSION_LINE} — listening on http://${config.bind}:${server.port}`,
  );
}

// Entrypoint guard: run ONLY when executed directly (`bun server/main.ts` / the compiled binary),
// not when imported by a test (so `selectAssetSource` et al. can be unit-tested without binding).
if (import.meta.main) {
  run(process.argv.slice(2)).catch((error: unknown) => {
    if (error instanceof BindGuardError) {
      console.error(error.message);
    } else {
      console.error(
        `jurisearch-dashboard: fatal: ${error instanceof Error ? error.message : error}`,
      );
    }
    process.exit(1);
  });
}
