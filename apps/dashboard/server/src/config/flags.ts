/**
 * CLI flag parsing for the dashboard (design §5.5). Produces the HIGHEST-precedence config layer
 * (flags > env > toml) plus the `--version`/`--config` controls `main.ts` acts on. Kept separate
 * from `resolveConfig` so the precedence merge is tested over plain `PartialConfig` layers.
 *
 * Accepts both `--flag value` and `--flag=value`. Unknown flags are ignored (forward-compatible);
 * `--version` is a bare boolean.
 */

import type { PartialConfig } from "./resolve.ts";

export interface ParsedCli {
  /** `--version` was passed — `main.ts` prints the exact contract line and exits. */
  version: boolean;
  /** `--config <path>` — the `[dashboard]` TOML to load as the lowest explicit layer. */
  configPath?: string;
  /** The flag config layer (highest precedence). */
  overrides: PartialConfig;
}

/** Flags that take a string value, mapped to their `PartialConfig` key. */
const STRING_FLAGS: Record<string, keyof PartialConfig> = {
  "--bind": "bind",
  "--producer-bin": "producerBin",
  "--producer-config": "producerConfig",
  "--state-dir": "stateDir",
  "--corpora-dir": "corporaDir",
};

function parsePort(value: string): number {
  // Anchored: reject suffixed/fractional/empty (`--port=123abc` must NOT parse to 123).
  const trimmed = value.trim();
  const n = /^\d+$/.test(trimmed) ? Number(trimmed) : Number.NaN;
  if (!Number.isSafeInteger(n) || n > 65535) {
    throw new Error(`invalid --port: "${value}"`);
  }
  return n;
}

function parseGroups(value: string): string[] {
  return value
    .split(",")
    .map((g) => g.trim())
    .filter((g) => g !== "");
}

/** Parse `argv` (already sliced of `node`/script) into the CLI controls + flag config layer. */
export function parseCliFlags(argv: readonly string[]): ParsedCli {
  const overrides: PartialConfig = {};
  let version = false;
  let configPath: string | undefined;

  for (let i = 0; i < argv.length; i++) {
    const token = argv[i];
    if (token === undefined) {
      continue;
    }
    if (token === "--version") {
      version = true;
      continue;
    }

    // Split `--flag=value`; otherwise consume the next token as the value.
    const eq = token.indexOf("=");
    const name = eq >= 0 ? token.slice(0, eq) : token;
    const inlineValue = eq >= 0 ? token.slice(eq + 1) : undefined;
    const value = (): string => {
      if (inlineValue !== undefined) {
        return inlineValue;
      }
      const next = argv[++i];
      if (next === undefined) {
        throw new Error(`flag ${name} requires a value`);
      }
      return next;
    };

    const stringKey = STRING_FLAGS[name];
    if (name === "--config") {
      configPath = value();
    } else if (name === "--port") {
      overrides.port = parsePort(value());
    } else if (name === "--groups") {
      const groups = parseGroups(value());
      if (groups.length > 0) {
        overrides.groups = groups;
      }
    } else if (stringKey !== undefined) {
      (overrides as Record<string, unknown>)[stringKey] = value();
    }
    // Unknown flags ignored (forward-compatible).
  }

  return { version, configPath, overrides };
}
