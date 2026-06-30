/**
 * Read-only contract (design §1, §5.4): the adapters are a PURE-OBSERVER seam. This test asserts the
 * concrete adapters expose ONLY their read methods and NO mutation surface (write/unlink/rm/mkdir/
 * append/spawn/kill/…), so the dashboard can never write `state_dir`/`corpora_dir` or run an
 * arbitrary mutating command. Pure surface check — no real I/O.
 */

import { describe, expect, test } from "bun:test";
import { resolve } from "node:path";
import type { DashboardConfig } from "../config.ts";
import { logsCommand } from "../providers/logs.ts";
import { statusCommand } from "../providers/status.ts";
import { timersCommand } from "../providers/timers.ts";
import { FileAdapter } from "./file.ts";
import { ProcessAdapter } from "./process.ts";

/** Every own+prototype method name of an instance (excluding the constructor). */
function methodNames(instance: object): string[] {
  const proto = Object.getPrototypeOf(instance);
  return Object.getOwnPropertyNames(proto).filter(
    (name) =>
      name !== "constructor" && typeof (proto as Record<string, unknown>)[name] === "function",
  );
}

const FORBIDDEN = [
  "write",
  "append",
  "unlink",
  "rm",
  "rmdir",
  "mkdir",
  "delete",
  "remove",
  "create",
  "chmod",
  "chown",
  "rename",
  "spawn",
  "exec",
  "kill",
  "truncate",
];

describe("FileAdapter is read-only", () => {
  const names = methodNames(new FileAdapter());

  test("exposes exactly read/stat/list", () => {
    expect(names.sort()).toEqual(["list", "read", "stat"]);
  });

  test("exposes no mutating method", () => {
    for (const name of names) {
      for (const bad of FORBIDDEN) {
        expect(name.toLowerCase().includes(bad)).toBe(false);
      }
    }
  });
});

describe("ProcessAdapter is read-only", () => {
  const names = methodNames(new ProcessAdapter());

  test("exposes exactly run", () => {
    expect(names.sort()).toEqual(["run"]);
  });

  test("exposes no mutating method", () => {
    for (const name of names) {
      for (const bad of FORBIDDEN) {
        expect(name.toLowerCase().includes(bad)).toBe(false);
      }
    }
  });
});

// ── Source audit — names alone are gameable; pin the actual I/O surface ──────────────────────────

/** Forbidden `node:fs` write/mutation APIs that must never appear in the file adapter's source. */
const FORBIDDEN_FS = [
  "writeFile",
  "appendFile",
  "unlink",
  "rmdir",
  "mkdir",
  "rename",
  "copyFile",
  "truncate",
  "chmod",
  "chown",
  "createWriteStream",
  "open(",
  "rm(",
];

/** Strip block + line comments so the audit inspects CODE, not prose (which may name a verb). */
function stripComments(source: string): string {
  return source.replace(/\/\*[\s\S]*?\*\//g, "").replace(/\/\/.*$/gm, "");
}

describe("FileAdapter source touches only read APIs", () => {
  test("imports/uses no node:fs write API", async () => {
    const src = stripComments(await Bun.file(resolve(import.meta.dir, "file.ts")).text());
    for (const api of FORBIDDEN_FS) {
      expect(src.includes(api)).toBe(false);
    }
    // Positive: the only fs primitives it pulls in are the three read APIs (order-independent).
    const match = src.match(/import\s*{([^}]*)}\s*from\s*"node:fs\/promises"/);
    expect(match).not.toBeNull();
    const imported = (match?.[1] ?? "")
      .split(",")
      .map((s) => s.trim())
      .filter((s) => s.length > 0)
      .sort();
    expect(imported).toEqual(["readFile", "readdir", "stat"]);
  });
});

// ── Command-builder allowlist — a mutating verb can never be assembled ────────────────────────────

const auditConfig: DashboardConfig = {
  bind: "100.64.0.1",
  port: 8787,
  producerBin: "/usr/local/bin/jurisearch-producer",
  producerConfig: "/etc/jurisearch/producer.toml",
  stateDir: "/state",
  corporaDir: "/packages",
  groups: ["legislation", "jurisprudence"],
  cache: { statusMs: 0, overviewMs: 0, runsMs: 0, packagesMs: 0, logsMs: 0, timersMs: 0 },
  logs: { defaultLimit: 200, defaultSince: null },
};

/** Verbs that mutate state — none may appear in ANY assembled command. */
const MUTATING_VERBS = [
  "start",
  "stop",
  "restart",
  "reload",
  "enable",
  "disable",
  "mask",
  "unmask",
  "trigger",
  "kill",
  "set-property",
  "edit",
  "rebaseline",
  "publish",
  "publish-baseline",
  "fetch",
  "ingest",
  "provision",
  "run",
  "--rotate",
  "--vacuum-size",
  "--vacuum-time",
  "--flush",
  "--sync",
];

describe("provider command builders are read-only argv", () => {
  test("statusCommand is exactly the read-only `status --config` invocation", () => {
    expect(statusCommand(auditConfig)).toEqual([
      "/usr/local/bin/jurisearch-producer",
      "status",
      "--config",
      "/etc/jurisearch/producer.toml",
    ]);
  });

  test("timersCommand is exactly `systemctl list-timers … -o json` (never start/stop/trigger)", () => {
    expect(timersCommand()).toEqual([
      "systemctl",
      "list-timers",
      "jurisearch-producer-*",
      "-o",
      "json",
    ]);
  });

  test("logsCommand is a `journalctl -o json` read query", () => {
    const cmd = logsCommand(auditConfig, { group: "legislation", limit: 50, since: "-1h" });
    expect(cmd.slice(0, 3)).toEqual(["journalctl", "-o", "json"]);
  });

  test("NO assembled command contains a mutating verb", () => {
    const commands = [
      statusCommand(auditConfig),
      timersCommand(),
      logsCommand(auditConfig),
      logsCommand(auditConfig, { group: "legislation", limit: 10, since: "-2h" }),
    ];
    for (const cmd of commands) {
      for (const token of cmd) {
        expect(MUTATING_VERBS).not.toContain(token);
      }
    }
  });
});
