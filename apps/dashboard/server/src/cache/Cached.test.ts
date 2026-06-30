/**
 * `Cached<T>` unit tests — fully deterministic via an INJECTED clock (no `sleep`, no wall-clock).
 * Covers the four behaviours the decorator promises: TTL hit, TTL expiry re-invoke, in-flight
 * dedupe, no-cache-on-rejection, plus the keyed (parameterised) variant.
 */

import { describe, expect, test } from "bun:test";
import { Cached } from "./Cached.ts";

describe("Cached", () => {
  test("within TTL: returns the cached value without re-invoking the source", async () => {
    let calls = 0;
    let t = 1000;
    const cached = new Cached(
      () => {
        calls += 1;
        return Promise.resolve(calls);
      },
      100,
      () => t,
    );

    expect(await cached.get()).toBe(1);
    t = 1050; // still < expiry (1000 + 100)
    expect(await cached.get()).toBe(1);
    expect(calls).toBe(1);
  });

  test("after TTL: re-invokes the source once the clock passes expiry", async () => {
    let calls = 0;
    let t = 1000;
    const cached = new Cached(
      () => {
        calls += 1;
        return Promise.resolve(calls);
      },
      100,
      () => t,
    );

    expect(await cached.get()).toBe(1);
    t = 1200; // past expiry
    expect(await cached.get()).toBe(2);
    expect(calls).toBe(2);
  });

  test("in-flight dedupe: concurrent calls share a single source invocation", async () => {
    let calls = 0;
    let release!: (value: number) => void;
    const gate = new Promise<number>((resolve) => {
      release = resolve;
    });
    const cached = new Cached(
      () => {
        calls += 1;
        return gate;
      },
      100,
      () => 0,
    );

    const first = cached.get();
    const second = cached.get();
    release(42);

    expect(await first).toBe(42);
    expect(await second).toBe(42);
    expect(calls).toBe(1);
  });

  test("a rejection is NOT cached: the next call re-invokes (degraded source retried)", async () => {
    let calls = 0;
    const cached = new Cached(
      () => {
        calls += 1;
        return calls === 1 ? Promise.reject(new Error("boom")) : Promise.resolve(calls);
      },
      10_000,
      () => 0,
    );

    await expect(cached.get()).rejects.toThrow("boom");
    expect(await cached.get()).toBe(2);
    expect(calls).toBe(2);
  });

  test("keyed: distinct arguments get independent TTL slots", async () => {
    let calls = 0;
    const cached = new Cached<string, [string]>(
      (k) => {
        calls += 1;
        return Promise.resolve(`${k}:${calls}`);
      },
      100,
      () => 0,
      (k) => k,
    );

    expect(await cached.get("a")).toBe("a:1");
    expect(await cached.get("b")).toBe("b:2");
    expect(await cached.get("a")).toBe("a:1"); // served from a's slot
    expect(calls).toBe(2);
  });
});
