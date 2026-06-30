import { describe, expect, test } from "bun:test";
import {
  EXIT_CLASS_TABLE,
  exitCodeFor,
  FAILURE_CLASSES,
  isSuccess,
  RUNNING_CLASS,
  type RunOutcome,
  SUCCESS_CLASSES,
  severityOf,
} from "./exit-class.ts";

describe("exit-class vocabulary", () => {
  test("covers exactly 22 classes (6 success, 1 running, 15 failure)", () => {
    expect(SUCCESS_CLASSES.length).toBe(6);
    expect(FAILURE_CLASSES.length).toBe(15);
    expect(Object.keys(EXIT_CLASS_TABLE).length).toBe(22);
  });

  test("isSuccess matches the success set and nothing else", () => {
    for (const cls of SUCCESS_CLASSES) {
      expect(isSuccess(cls)).toBe(true);
    }
    for (const cls of FAILURE_CLASSES) {
      expect(isSuccess(cls)).toBe(false);
    }
    expect(isSuccess(RUNNING_CLASS)).toBe(false);
  });

  test("exitCodeFor is a faithful port of exit.rs::exit_code_for", () => {
    for (const cls of SUCCESS_CLASSES) {
      expect(exitCodeFor(cls)).toBe(0);
    }
    expect(exitCodeFor("skipped-lock-held")).toBe(75);
    expect(exitCodeFor("fetch-failed")).toBe(75);
    expect(exitCodeFor("upstream-unreachable")).toBe(75);
    expect(exitCodeFor("integrity-failed")).toBe(65);
    expect(exitCodeFor("producer-db-unprovisioned")).toBe(69);
    expect(exitCodeFor("config-invalid")).toBe(78);
    expect(exitCodeFor("publish-failed")).toBe(70);
    expect(exitCodeFor("needs-rebaseline")).toBe(70);
    expect(exitCodeFor("io-failed")).toBe(70);
  });
});

/** The canonical outcome for a class (how a record/status would actually carry it). */
function canonicalOutcome(cls: string): RunOutcome {
  if (cls === RUNNING_CLASS) return "running";
  if (isSuccess(cls)) return "success";
  return "failure";
}

describe("severityOf — outcome FIRST", () => {
  test("every one of the 22 classes resolves to its table severity under its canonical outcome", () => {
    for (const [cls, info] of Object.entries(EXIT_CLASS_TABLE)) {
      expect(severityOf(canonicalOutcome(cls), cls)).toBe(info.severity);
    }
  });

  test("running outcome is neutral regardless of class string (the running trap)", () => {
    expect(severityOf("running", RUNNING_CLASS)).toBe("neutral");
    // Even a failure-looking class never becomes a permanent failure while outcome=running.
    expect(severityOf("running", "publish-failed")).toBe("neutral");
  });

  test("success outcome is ok regardless of class", () => {
    expect(severityOf("success", "published")).toBe("ok");
    expect(severityOf("success", "no-op")).toBe("ok");
  });

  test("failure outcome buckets via exitCodeFor", () => {
    expect(severityOf("failure", "skipped-lock-held")).toBe("transient");
    expect(severityOf("failure", "integrity-failed")).toBe("data");
    expect(severityOf("failure", "producer-db-unprovisioned")).toBe("unprovisioned");
    expect(severityOf("failure", "config-invalid")).toBe("config");
    expect(severityOf("failure", "publish-failed")).toBe("permanent");
  });

  test("null/unknown outcome (never ran) is neutral, never a permanent failure", () => {
    expect(severityOf(null, null)).toBe("neutral");
    expect(severityOf(null, "running")).toBe("neutral");
    expect(severityOf(undefined, "publish-failed")).toBe("neutral");
  });
});
