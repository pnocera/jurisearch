import { expect, test } from "bun:test";
import {
  BUILD_TARGET,
  cargoTomlPath,
  gitOutput,
  parseWorkspaceVersion,
  renderBuildInfo,
  resolveCommit,
} from "./stamp";

// --- parseWorkspaceVersion -------------------------------------------------------------------------
test("parseWorkspaceVersion reads [workspace.package] version", () => {
  const toml =
    '[workspace]\nmembers = []\n\n[workspace.package]\nversion = "1.2.3"\nedition = "2024"\n';
  expect(parseWorkspaceVersion(toml)).toBe("1.2.3");
});

test("parseWorkspaceVersion ignores inline workspace.dependencies versions", () => {
  const toml =
    '[workspace.package]\nversion = "0.1.0"\n\n[workspace.dependencies]\nserde = { version = "1.0" }\n';
  expect(parseWorkspaceVersion(toml)).toBe("0.1.0");
});

test("parseWorkspaceVersion throws when the section or version is missing", () => {
  expect(() => parseWorkspaceVersion("[workspace]\nmembers = []\n")).toThrow();
  expect(() => parseWorkspaceVersion('[workspace.package]\nedition = "2024"\n')).toThrow();
});

test("the real root Cargo.toml exposes a semver workspace version", async () => {
  const toml = await Bun.file(cargoTomlPath).text();
  expect(parseWorkspaceVersion(toml)).toMatch(/^\d+\.\d+\.\d+/);
});

// --- BUILD_TARGET ----------------------------------------------------------------------------------
test("BUILD_TARGET is dist.sh's GNU triple", () => {
  expect(BUILD_TARGET).toBe("x86_64-unknown-linux-gnu");
});

// --- resolveCommit precedence (mirrors dist.sh / jurisearch-buildinfo) ------------------------------
test("resolveCommit: a non-empty override wins over git", () => {
  expect(
    resolveCommit({
      override: "deadbeefcafe",
      gitShort: () => "aaaaaaaaaaaa",
      gitFull: () => "f".repeat(40),
    }),
  ).toBe("deadbeefcafe");
});

test("resolveCommit: empty / whitespace / undefined override falls through to git short", () => {
  expect(resolveCommit({ override: "", gitShort: () => "bbbbbbbbbbbb", gitFull: () => null })).toBe(
    "bbbbbbbbbbbb",
  );
  expect(
    resolveCommit({ override: "   ", gitShort: () => "cccccccccccc", gitFull: () => null }),
  ).toBe("cccccccccccc");
  expect(
    resolveCommit({ override: undefined, gitShort: () => "dddddddddddd", gitFull: () => null }),
  ).toBe("dddddddddddd");
});

test("resolveCommit: no git short → full HEAD truncated to 12", () => {
  expect(
    resolveCommit({
      override: undefined,
      gitShort: () => null,
      gitFull: () => "0123456789abcdef0123",
    }),
  ).toBe("0123456789ab");
});

test("resolveCommit: nothing available → 'unknown' (never throws)", () => {
  expect(resolveCommit({ override: undefined, gitShort: () => null, gitFull: () => null })).toBe(
    "unknown",
  );
});

test("resolveCommit default path equals `git rev-parse --short=12 HEAD`", () => {
  const expected = gitOutput(["rev-parse", "--short=12", "HEAD"]);
  if (!expected) {
    return; // not a git checkout; the synthetic precedence cases above still cover the logic
  }
  const got = resolveCommit({
    override: undefined,
    gitShort: () => gitOutput(["rev-parse", "--short=12", "HEAD"]),
    gitFull: () => gitOutput(["rev-parse", "HEAD"]),
  });
  expect(got).toBe(expected);
});

// --- renderBuildInfo -------------------------------------------------------------------------------
test("renderBuildInfo emits the three typed constants", () => {
  const out = renderBuildInfo({
    version: "0.1.0",
    commit: "ed259c4f7856",
    target: "x86_64-unknown-linux-gnu",
  });
  expect(out).toContain('export const BUILD_VERSION = "0.1.0";');
  expect(out).toContain('export const BUILD_COMMIT = "ed259c4f7856";');
  expect(out).toContain('export const BUILD_TARGET = "x86_64-unknown-linux-gnu";');
});
