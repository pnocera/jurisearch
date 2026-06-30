/**
 * Config resolution tests — the precedence contract (flags > env > toml > CT-111 defaults) and each
 * layer parser. Deterministic: env and argv are passed in, never read from the real process.
 */

import { describe, expect, test } from "bun:test";
import { parseCliFlags } from "./flags.ts";
import { DEFAULT_CONFIG, parseEnvConfig, parseTomlConfig, resolveConfig } from "./resolve.ts";

describe("DEFAULT_CONFIG — CT-111 production defaults", () => {
  test("matches the design §5.5 production layout", () => {
    expect(DEFAULT_CONFIG.producerBin).toBe("/usr/local/bin/jurisearch-producer");
    expect(DEFAULT_CONFIG.producerConfig).toBe("/etc/jurisearch/producer.toml");
    expect(DEFAULT_CONFIG.stateDir).toBe("/var/lib/jurisearch-producer");
    expect(DEFAULT_CONFIG.groups).toEqual(["legislation", "jurisprudence"]);
    // Loopback default is dev-safe AND accepted by the bind guard; prod overrides to the tailnet addr.
    expect(DEFAULT_CONFIG.bind).toBe("127.0.0.1");
  });
});

describe("resolveConfig precedence (flags > env > toml > defaults)", () => {
  const toml = parseTomlConfig(
    [
      "[dashboard]",
      'bind = "10.0.0.1"',
      "port = 1111",
      'state_dir = "/toml/state"',
      'groups = ["a", "b"]',
      "[dashboard.cache]",
      "overview_ms = 111",
      "[dashboard.logs]",
      "default_limit = 11",
    ].join("\n"),
  );
  const env = parseEnvConfig({
    DASHBOARD_BIND: "10.0.0.2",
    DASHBOARD_PORT: "2222",
    DASHBOARD_STATE_DIR: "/env/state",
  });
  const flags = parseCliFlags(["--bind", "10.0.0.3", "--port", "3333"]).overrides;

  test("flags win over env and toml", () => {
    const config = resolveConfig({ flags, env, toml });
    expect(config.bind).toBe("10.0.0.3"); // flag
    expect(config.port).toBe(3333); // flag
  });

  test("env wins over toml where no flag is set", () => {
    const config = resolveConfig({ flags, env, toml });
    expect(config.stateDir).toBe("/env/state"); // env over toml /toml/state
  });

  test("toml wins over defaults where no flag/env is set", () => {
    const config = resolveConfig({ flags, env, toml });
    expect(config.groups).toEqual(["a", "b"]); // toml over default
    expect(config.cache.overviewMs).toBe(111); // toml over default 3000
    expect(config.logs.defaultLimit).toBe(11); // toml over default 200
  });

  test("defaults fill anything no layer sets", () => {
    const config = resolveConfig({ flags, env, toml });
    expect(config.producerBin).toBe(DEFAULT_CONFIG.producerBin);
    expect(config.cache.packagesMs).toBe(DEFAULT_CONFIG.cache.packagesMs);
  });

  test("an empty resolve is exactly the defaults", () => {
    expect(resolveConfig({})).toEqual(DEFAULT_CONFIG);
  });
});

describe("layer parsers", () => {
  test("parseEnvConfig reads DASHBOARD_* and comma-splits groups", () => {
    const partial = parseEnvConfig({
      DASHBOARD_BIND: "100.64.0.5",
      DASHBOARD_PORT: "9000",
      DASHBOARD_GROUPS: "legislation, jurisprudence ,",
      DASHBOARD_CACHE_LOGS_MS: "500",
    });
    expect(partial.bind).toBe("100.64.0.5");
    expect(partial.port).toBe(9000);
    expect(partial.groups).toEqual(["legislation", "jurisprudence"]);
    expect(partial.cache?.logsMs).toBe(500);
  });

  test("parseTomlConfig reads the [dashboard] block and ignores unknown keys", () => {
    const partial = parseTomlConfig(
      [
        "[dashboard]",
        'bind = "100.64.0.6"',
        'producer_bin = "/opt/p"',
        'unknown_future_key = "ignored"',
      ].join("\n"),
    );
    expect(partial.bind).toBe("100.64.0.6");
    expect(partial.producerBin).toBe("/opt/p");
  });

  test("parseCliFlags supports --flag value and --flag=value, ignores unknowns", () => {
    const cli = parseCliFlags([
      "--config",
      "/etc/jurisearch/dashboard.toml",
      "--bind=100.64.0.7",
      "--groups",
      "legislation",
      "--unknown",
      "x",
    ]);
    expect(cli.configPath).toBe("/etc/jurisearch/dashboard.toml");
    expect(cli.overrides.bind).toBe("100.64.0.7");
    expect(cli.overrides.groups).toEqual(["legislation"]);
  });

  test("parseCliFlags detects --version", () => {
    expect(parseCliFlags(["--version"]).version).toBe(true);
    expect(parseCliFlags([]).version).toBe(false);
  });
});

describe("numeric parsing is anchored (rejects suffixed/fractional/empty)", () => {
  test("--port: rejects 123abc / 1.5 / empty / out-of-range; accepts a padded integer", () => {
    for (const bad of ["123abc", "1.5", "", "70000", "-1", "0x50"]) {
      expect(() => parseCliFlags(["--port", bad])).toThrow();
    }
    expect(parseCliFlags(["--port", " 80 "]).overrides.port).toBe(80);
    expect(parseCliFlags(["--port=8080"]).overrides.port).toBe(8080);
  });

  test("DASHBOARD_PORT: rejects 123abc / 1.5; accepts a clean integer", () => {
    expect(() => parseEnvConfig({ DASHBOARD_PORT: "123abc" })).toThrow();
    expect(() => parseEnvConfig({ DASHBOARD_PORT: "1.5" })).toThrow();
    expect(parseEnvConfig({ DASHBOARD_PORT: "9000" }).port).toBe(9000);
  });

  test("DASHBOARD_CACHE_*_MS: rejects partial numbers", () => {
    expect(() => parseEnvConfig({ DASHBOARD_CACHE_LOGS_MS: "100junk" })).toThrow();
    expect(parseEnvConfig({ DASHBOARD_CACHE_LOGS_MS: "100" }).cache?.logsMs).toBe(100);
  });
});
