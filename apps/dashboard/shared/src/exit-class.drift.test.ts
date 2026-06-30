/**
 * Build-time DRIFT GUARD: re-derive the producer's exit-class vocabulary AND its class→exit-code
 * buckets straight from the Rust source, and assert the TS `EXIT_CLASS_TABLE`/`exitCodeFor` match.
 *   - SET drift: if the producer adds/renames/removes a class in `exit.rs`/`error.rs`, the SET test
 *     fails.
 *   - BUCKET drift: if the producer re-buckets an EXISTING class in `exit_code_for` (e.g.
 *     `integrity-failed` 65→75), the BUCKET test fails — even though the class set is unchanged —
 *     so the dashboard severity can't silently flip.
 */
import { expect, test } from "bun:test";
import { resolve } from "node:path";
import { EXIT_CLASS_TABLE, exitCodeFor, RUNNING_CLASS } from "./exit-class.ts";

// shared/src → repo root is four levels up (src → shared → dashboard → apps → root).
const REPO_ROOT = resolve(import.meta.dir, "../../../..");
const PRODUCER_SRC = resolve(REPO_ROOT, "crates/jurisearch-producer/src");
const EXIT_RS = resolve(PRODUCER_SRC, "exit.rs");
const ERROR_RS = resolve(PRODUCER_SRC, "error.rs");

/**
 * Extract producer exit-class string literals from Rust source. A class is a double-quoted,
 * lowercase kebab token (`"fetch-failed"`, `"no-op"`, `"ok"`). The "whole quoted content is a
 * single kebab token" shape excludes error MESSAGE strings (spaces), doc backticks, and
 * `snake_case` attribute values (underscores), so only real class literals are collected.
 */
async function extractRustClasses(...files: string[]): Promise<Set<string>> {
  const classes = new Set<string>();
  const pattern = /"([a-z0-9]+(?:-[a-z0-9]+)*)"/g;
  for (const file of files) {
    const text = await Bun.file(file).text();
    for (const match of text.matchAll(pattern)) {
      const token = match[1];
      if (token !== undefined) {
        classes.add(token);
      }
    }
  }
  return classes;
}

test("EXIT_CLASS_TABLE matches the producer's Rust class set (no SET drift)", async () => {
  const rustClasses = await extractRustClasses(EXIT_RS, ERROR_RS);

  // `running` lives in runrecord.rs (RunRecord::started), not in exit.rs/error.rs — we add it.
  expect(rustClasses.has(RUNNING_CLASS)).toBe(false);

  const tableKeys = new Set(Object.keys(EXIT_CLASS_TABLE));
  const expectedFromRust = new Set([...rustClasses, RUNNING_CLASS]);

  expect(tableKeys).toEqual(expectedFromRust);
  // Belt-and-braces: the captured-fixture ground truth is exactly 22 classes.
  expect(tableKeys.size).toBe(22);
  expect(rustClasses.size).toBe(21);
});

/** Slice the body of a free `fn <name>` from Rust source (up to its first column-0 closing brace). */
function functionBody(text: string, signature: string): string {
  const start = text.indexOf(signature);
  if (start < 0) {
    throw new Error(`could not find \`${signature}\` in source`);
  }
  const rest = text.slice(start);
  const end = rest.indexOf("\n}");
  if (end < 0) {
    throw new Error(`could not find end of \`${signature}\``);
  }
  return rest.slice(0, end);
}

/** The class names listed in the `pub const SUCCESS_CLASSES: &[&str] = &[ … ];` block of exit.rs. */
function parseSuccessClasses(text: string): Set<string> {
  const anchor = text.indexOf("SUCCESS_CLASSES: &[&str]");
  const close = text.indexOf("];", anchor);
  const block = text.slice(anchor, close);
  const classes = new Set<string>();
  for (const match of block.matchAll(/"([a-z0-9-]+)"/g)) {
    if (match[1] !== undefined) {
      classes.add(match[1]);
    }
  }
  return classes;
}

/**
 * Re-derive `exit_code_for` as a class→code map FROM SOURCE: the `is_success(c) => 0` arm (success
 * classes), the explicit `"a" | "b" => NN` string arms, and the catch-all `_ => NN` default.
 */
function parseExitCodeFor(text: string): {
  successCode: number;
  defaultCode: number;
  explicit: Map<string, number>;
} {
  const body = functionBody(text, "pub fn exit_code_for");

  const successMatch = body.match(/is_success\(c\)\s*=>\s*(\d+)/);
  const defaultMatch = body.match(/_\s*=>\s*(\d+)/);
  if (!successMatch?.[1] || !defaultMatch?.[1]) {
    throw new Error("could not parse the is_success/default arms of exit_code_for");
  }

  const explicit = new Map<string, number>();
  // Each arm: one-or-more `"class"` separated by `|`, then `=> NN,`.
  const armPattern = /((?:"[a-z0-9-]+"\s*\|\s*)*"[a-z0-9-]+")\s*=>\s*(\d+)/g;
  for (const arm of body.matchAll(armPattern)) {
    const lhs = arm[1];
    const code = Number.parseInt(arm[2] ?? "", 10);
    if (lhs === undefined || Number.isNaN(code)) {
      continue;
    }
    for (const token of lhs.matchAll(/"([a-z0-9-]+)"/g)) {
      if (token[1] !== undefined) {
        explicit.set(token[1], code);
      }
    }
  }

  return {
    successCode: Number.parseInt(successMatch[1], 10),
    defaultCode: Number.parseInt(defaultMatch[1], 10),
    explicit,
  };
}

test("exitCodeFor / EXIT_CLASS_TABLE match the producer's exit_code_for buckets (no BUCKET drift)", async () => {
  const exitText = await Bun.file(EXIT_RS).text();
  const rustClasses = await extractRustClasses(EXIT_RS, ERROR_RS);
  const successClasses = parseSuccessClasses(exitText);
  const { successCode, defaultCode, explicit } = parseExitCodeFor(exitText);

  // Sanity: the parse actually found the source-of-truth buckets we depend on.
  expect(successCode).toBe(0);
  expect(defaultCode).toBe(70);
  expect(explicit.get("integrity-failed")).toBe(65);
  expect(explicit.get("producer-db-unprovisioned")).toBe(69);
  expect(explicit.get("config-invalid")).toBe(78);
  expect(explicit.get("fetch-failed")).toBe(75);
  expect(explicit.size).toBeGreaterThanOrEqual(5);

  /** The exit code Rust would assign `cls`, derived purely from source. */
  const expectedCode = (cls: string): number => {
    if (successClasses.has(cls)) return successCode;
    const arm = explicit.get(cls);
    return arm ?? defaultCode; // ProducerError::class classes not matched explicitly → default 70.
  };

  for (const cls of rustClasses) {
    const want = expectedCode(cls);
    // The TS port must agree with the source bucket…
    expect(`${cls}=${exitCodeFor(cls)}`).toBe(`${cls}=${want}`);
    // …and so must the derived table (running excluded — it carries exitCode null).
    expect(`${cls}=${EXIT_CLASS_TABLE[cls]?.exitCode}`).toBe(`${cls}=${want}`);
  }
});
