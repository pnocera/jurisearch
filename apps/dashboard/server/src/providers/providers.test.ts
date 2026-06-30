/**
 * Provider tests (DoD: zero real I/O — all fakes). Each provider is driven through IN-MEMORY fakes
 * of `ProcessRunner`/`FileSource` that return fixture bytes/stdout, so there is NO subprocess, NO
 * filesystem, NO network. Asserts: (1) the provider yields the right DTOs from the adapter seam, and
 * (2) a MALFORMED source surfaces a typed `ProviderError` (so M3 degrades one panel) rather than
 * throwing uncaught.
 */

import { beforeAll, describe, expect, test } from "bun:test";
import { resolve } from "node:path";
import type {
  FileSource,
  ProcessRunner,
  RunOptions,
  RunResult,
  StatInfo,
} from "../adapters/types.ts";
import type { DashboardConfig } from "../config.ts";
import { LogsProvider, logsCommand } from "./logs.ts";
import { PackagesProvider } from "./packages.ts";
import { RunsProvider } from "./runs.ts";
import { StatusProvider } from "./status.ts";
import { TimersProvider } from "./timers.ts";
import { ProviderError } from "./types.ts";

// ── In-memory fakes (no real I/O) ──────────────────────────────────────────────────────────────────

/** A `ProcessRunner` that returns a scripted result and records the last argv it was handed. */
class FakeProcessRunner implements ProcessRunner {
  lastCmd: string[] = [];
  constructor(private readonly result: RunResult) {}
  run(cmd: string[], _opts?: RunOptions): Promise<RunResult> {
    this.lastCmd = cmd;
    return Promise.resolve(this.result);
  }
}

/** A `FileSource` backed by in-memory maps; missing paths reject like a real `ENOENT`. */
class FakeFileSource implements FileSource {
  constructor(
    private readonly files: Map<string, string>,
    private readonly dirs: Map<string, string[]>,
  ) {}
  read(path: string): Promise<string> {
    const content = this.files.get(path);
    if (content === undefined) {
      return Promise.reject(new Error(`ENOENT: ${path}`));
    }
    return Promise.resolve(content);
  }
  stat(path: string): Promise<StatInfo> {
    const isDir = this.dirs.has(path);
    const isFile = this.files.has(path);
    if (!isDir && !isFile) {
      return Promise.reject(new Error(`ENOENT: ${path}`));
    }
    return Promise.resolve({ isFile, isDirectory: isDir, size: 0, mtimeMs: 0 });
  }
  list(dir: string): Promise<string[]> {
    const names = this.dirs.get(dir);
    if (names === undefined) {
      return Promise.reject(new Error(`ENOENT: ${dir}`));
    }
    return Promise.resolve(names);
  }
}

const config: DashboardConfig = {
  bind: "100.64.0.1",
  port: 8787,
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

const FIXTURES = resolve(import.meta.dir, "../../../fixtures");
const fixtureText = (name: string): Promise<string> => Bun.file(resolve(FIXTURES, name)).text();
const fixtureJson = (name: string): Promise<unknown> => Bun.file(resolve(FIXTURES, name)).json();

const ok = (stdout: string): RunResult => ({ stdout, stderr: "", code: 0 });

// ── StatusProvider ──────────────────────────────────────────────────────────────────────────────

describe("StatusProvider", () => {
  test("invoke→parse→validate yields StatusDTO; argv is the fixed read command", async () => {
    const proc = new FakeProcessRunner(ok(await fixtureText("status.json")));
    const status = await new StatusProvider(proc, config).get();
    expect(status.overall).toBe("stale");
    expect(status.groups.length).toBe(2);
    expect(proc.lastCmd).toEqual([
      "/usr/local/bin/jurisearch-producer",
      "status",
      "--config",
      "/etc/jurisearch/producer.toml",
    ]);
  });

  test("non-zero exit ⇒ typed ProviderError", async () => {
    const proc = new FakeProcessRunner({ stdout: "", stderr: "boom", code: 1 });
    await expect(new StatusProvider(proc, config).get()).rejects.toBeInstanceOf(ProviderError);
  });

  test("malformed stdout ⇒ typed ProviderError (degrade, not crash)", async () => {
    const proc = new FakeProcessRunner(ok("<<not json>>"));
    await expect(new StatusProvider(proc, config).get()).rejects.toBeInstanceOf(ProviderError);
  });
});

// ── RunsProvider ──────────────────────────────────────────────────────────────────────────────────

describe("RunsProvider", () => {
  test("reads only *.record.json; skips checkpoint/last.json; absent group dir ⇒ no error", async () => {
    const finished = await fixtureText("runrecord-legislation-finished-synthetic.json");
    const noop = await fixtureText("runrecord-legislation-noop-synthetic.json");
    const files = new Map<string, string>([
      ["/state/runs/legislation/a.record.json", finished],
      ["/state/runs/legislation/b.record.json", noop],
      ["/state/runs/legislation/last.json", finished], // NOT a record file → ignored
      ["/state/runs/legislation/checkpoint-787.json", "{ not even valid }"], // checkpoint → ignored
    ]);
    const dirs = new Map<string, string[]>([
      [
        "/state/runs/legislation",
        ["a.record.json", "b.record.json", "last.json", "checkpoint-787.json"],
      ],
      // NOTE: no "/state/runs/jurisprudence" entry → that group has never run.
    ]);
    const provider = new RunsProvider(new FakeFileSource(files, dirs), config);
    const records = await provider.get();
    expect(records.length).toBe(2); // only the two .record.json
    // sorted most-recent-first: no-op (12:30) before finished (11:30)
    expect(records[0]?.exitClass).toBe("no-op");
    expect(records[1]?.exitClass).toBe("published");
  });

  test("group filter + limit", async () => {
    const finished = await fixtureText("runrecord-legislation-finished-synthetic.json");
    const files = new Map([["/state/runs/legislation/a.record.json", finished]]);
    const dirs = new Map([["/state/runs/legislation", ["a.record.json"]]]);
    const provider = new RunsProvider(new FakeFileSource(files, dirs), config);
    const records = await provider.get({ group: "legislation", limit: 1 });
    expect(records.length).toBe(1);
  });

  test("a malformed record file ⇒ typed ProviderError", async () => {
    const files = new Map([["/state/runs/legislation/a.record.json", "}{ broken"]]);
    const dirs = new Map([["/state/runs/legislation", ["a.record.json"]]]);
    const provider = new RunsProvider(new FakeFileSource(files, dirs), config);
    await expect(provider.get({ group: "legislation" })).rejects.toBeInstanceOf(ProviderError);
  });
});

// ── PackagesProvider ────────────────────────────────────────────────────────────────────────────

describe("PackagesProvider", () => {
  test("reads <corporaDir>/core/manifest.json ⇒ PackageManifestDTO (empty packages tolerated)", async () => {
    const files = new Map([["/packages/core/manifest.json", await fixtureText("manifest.json")]]);
    const provider = new PackagesProvider(new FakeFileSource(files, new Map()), config);
    const manifest = await provider.get();
    expect(manifest.headSequence).toBe(1);
    expect(manifest.packages).toEqual([]);
  });

  test("reads a non-empty manifest ⇒ maps the increment entry's fields", async () => {
    const files = new Map([
      ["/packages/core/manifest.json", await fixtureText("manifest-with-increment-synthetic.json")],
    ]);
    const provider = new PackagesProvider(new FakeFileSource(files, new Map()), config);
    const manifest = await provider.get();
    expect(manifest.headSequence).toBe(2);
    expect(manifest.packages.length).toBe(1);
    const entry = manifest.packages[0];
    expect(entry?.packageId).toBe("core-1-2");
    expect(entry?.fromSequence).toBe(1);
    expect(entry?.toSequence).toBe(2);
    expect(entry?.rowCounts).toEqual({ documents: 1284, chunks: 9173, embeddings: 9173 });
    expect(entry?.embeddingFingerprint).toBe("bge-m3:1024:v1");
  });

  test("missing manifest ⇒ typed ProviderError", async () => {
    const provider = new PackagesProvider(new FakeFileSource(new Map(), new Map()), config);
    await expect(provider.get()).rejects.toBeInstanceOf(ProviderError);
  });

  test("malformed manifest ⇒ typed ProviderError", async () => {
    const files = new Map([["/packages/core/manifest.json", "not json"]]);
    const provider = new PackagesProvider(new FakeFileSource(files, new Map()), config);
    await expect(provider.get()).rejects.toBeInstanceOf(ProviderError);
  });
});

// ── LogsProvider ──────────────────────────────────────────────────────────────────────────────────

describe("LogsProvider", () => {
  test("invoke→parse yields LogLineDTO[]; argv carries -u per group", async () => {
    const obj = await fixtureJson("journal-legislation.json");
    const ndjson = `${JSON.stringify(obj)}\n`;
    const proc = new FakeProcessRunner(ok(ndjson));
    const lines = await new LogsProvider(proc, config).get();
    expect(lines.length).toBe(1);
    expect(lines[0]?.unit).toBe("jurisearch-producer-legislation.service");
    expect(proc.lastCmd).toContain("-u");
    expect(proc.lastCmd).toContain("jurisearch-producer-legislation.service");
    expect(proc.lastCmd).toContain("jurisearch-producer-jurisprudence.service");
  });

  test("sparse (empty) journald output ⇒ [] (expected, not an error)", async () => {
    const proc = new FakeProcessRunner(ok(""));
    const lines = await new LogsProvider(proc, config).get();
    expect(lines).toEqual([]);
  });

  test("group filter narrows the unit and adds --since", () => {
    const cmd = logsCommand(config, { group: "legislation", limit: 50, since: "-1h" });
    expect(cmd).toContain("jurisearch-producer-legislation.service");
    expect(cmd).not.toContain("jurisearch-producer-jurisprudence.service");
    expect(cmd).toContain("--since");
    expect(cmd[cmd.indexOf("-n") + 1]).toBe("50");
  });

  test("non-zero exit ⇒ typed ProviderError", async () => {
    const proc = new FakeProcessRunner({ stdout: "", stderr: "no journal access", code: 1 });
    await expect(new LogsProvider(proc, config).get()).rejects.toBeInstanceOf(ProviderError);
  });
});

// ── TimersProvider ────────────────────────────────────────────────────────────────────────────────

describe("TimersProvider", () => {
  let timersStdout: string;
  beforeAll(async () => {
    timersStdout = await fixtureText("timers.json");
  });

  test("invoke→parse yields TimerDTO[] with derived group + µs→ms", async () => {
    const proc = new FakeProcessRunner(ok(timersStdout));
    const timers = await new TimersProvider(proc, config).get();
    expect(timers.map((t) => t.group)).toEqual(["legislation", "jurisprudence"]);
    expect(timers[0]?.nextRun).toBe(1_782_859_358_634);
    expect(proc.lastCmd).toEqual([
      "systemctl",
      "list-timers",
      "jurisearch-producer-*",
      "-o",
      "json",
    ]);
  });

  test("malformed stdout ⇒ typed ProviderError", async () => {
    const proc = new FakeProcessRunner(ok('{"not":"array"}'));
    await expect(new TimersProvider(proc, config).get()).rejects.toBeInstanceOf(ProviderError);
  });
});
