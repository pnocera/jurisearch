import { afterEach, expect, test } from "bun:test";
import type { ApiResult } from "@jurisearch-dashboard/shared";
import { effectScope, nextTick, ref } from "vue";
import { useLogs } from "@/composables/resources.ts";
import { usePolling } from "@/composables/usePolling.ts";

const tick = (): Promise<void> => new Promise((resolve) => setTimeout(resolve, 0));
const flush = async (): Promise<void> => {
  await nextTick();
  await tick();
};

const realFetch = globalThis.fetch;
afterEach(() => {
  globalThis.fetch = realFetch;
});

test("refresh: success sets data, degrade keeps stale data + sets error, recovery clears error", async () => {
  let next: ApiResult<number> = { ok: true, data: 1 };
  const handle = usePolling<number>(async () => next, 0);

  await handle.refresh();
  expect(handle.data.value).toBe(1);
  expect(handle.error.value).toBeNull();
  expect(handle.lastUpdated.value).not.toBeNull();
  expect(handle.loading.value).toBe(false);

  next = { ok: false, error: { code: "x", message: "degraded" } };
  await handle.refresh();
  expect(handle.error.value?.message).toBe("degraded");
  expect(handle.data.value).toBe(1); // stale-while-degraded: last good value kept

  next = { ok: true, data: 2 };
  await handle.refresh();
  expect(handle.data.value).toBe(2);
  expect(handle.error.value).toBeNull();
});

test("coalescing: a refresh during in-flight is NOT dropped — it runs exactly one follow-up, no overlap", async () => {
  let concurrent = 0;
  let maxConcurrent = 0;
  let calls = 0;
  const releases: Array<() => void> = [];
  const fetcher = async (): Promise<ApiResult<number>> => {
    calls += 1;
    concurrent += 1;
    maxConcurrent = Math.max(maxConcurrent, concurrent);
    await new Promise<void>((resolve) => releases.push(resolve));
    concurrent -= 1;
    return { ok: true, data: calls };
  };
  const handle = usePolling<number>(fetcher, 0);

  const first = handle.refresh();
  await tick();
  expect(calls).toBe(1); // first in flight

  handle.refresh(); // requested while in flight → coalesced, NOT dropped
  await tick();
  expect(calls).toBe(1); // still no overlap: the follow-up has not started

  releases[0]?.(); // release the first fetch
  await tick();
  expect(calls).toBe(2); // the coalesced follow-up started only after the first settled

  releases[1]?.();
  await first;
  expect(maxConcurrent).toBe(1); // the two fetches never overlapped
  expect(handle.data.value).toBe(2);
});

test("gated param change: the OLD-param result is superseded; final data is from the NEW params, no overlap", async () => {
  let concurrent = 0;
  let maxConcurrent = 0;
  const seenGroups: string[] = [];
  const releases: Array<() => void> = [];
  globalThis.fetch = (async (input: string | URL | Request) => {
    const url = new URL(String(input), "http://harness");
    const group = url.searchParams.get("group") ?? "all";
    seenGroups.push(group);
    concurrent += 1;
    maxConcurrent = Math.max(maxConcurrent, concurrent);
    await new Promise<void>((resolve) => releases.push(resolve));
    concurrent -= 1;
    return Response.json({
      ok: true,
      data: [
        { timestamp: 1, priority: 6, unit: `jurisearch-producer-${group}.service`, message: group },
      ],
    });
  }) as typeof fetch;

  const group = ref<string | undefined>(undefined); // "all"
  const handle = useLogs({ group });

  const first = handle.refresh(); // fetch for group=all, gated (in flight)
  await flush();
  expect(seenGroups).toEqual(["all"]);

  group.value = "jurisprudence"; // change params mid-flight → watcher calls refresh()
  await flush();
  expect(seenGroups).toEqual(["all"]); // no overlap: the new fetch has not started yet

  releases[0]?.(); // release the OLD-param fetch (its result must NOT commit)
  await flush();
  expect(seenGroups).toEqual(["all", "jurisprudence"]); // coalesced follow-up uses the NEW params

  releases[1]?.();
  await first;
  await flush();
  expect(maxConcurrent).toBe(1); // the two fetches never overlapped
  expect(handle.data.value?.[0]?.message).toBe("jurisprudence"); // final data is from the NEW params
  expect(handle.error.value).toBeNull();
  handle.stop();
});

test("post-stop(): a late resolution is a no-op on the disposed handle", async () => {
  let release: () => void = () => {};
  const gate = new Promise<void>((resolve) => {
    release = resolve;
  });
  const handle = usePolling<number>(async () => {
    await gate;
    return { ok: true, data: 99 };
  }, 0);

  handle.start();
  await tick();
  expect(handle.loading.value).toBe(true); // a fetch is in flight

  handle.stop(); // dispose before the fetch resolves
  const snapshot = {
    data: handle.data.value,
    error: handle.error.value,
    lastUpdated: handle.lastUpdated.value,
    loading: handle.loading.value,
  };
  expect(snapshot.loading).toBe(false); // stop() settles loading

  release(); // the in-flight fetch resolves AFTER disposal
  await tick();
  await tick();

  expect(handle.data.value).toBe(snapshot.data); // unchanged (still null)
  expect(handle.error.value).toBe(snapshot.error);
  expect(handle.lastUpdated.value).toBe(snapshot.lastUpdated);
  expect(handle.loading.value).toBe(snapshot.loading);
});

test("scope disposal (onScopeDispose → stop, the component-unmount path): late resolution is a no-op", async () => {
  let release: () => void = () => {};
  const gate = new Promise<void>((resolve) => {
    release = resolve;
  });
  // An `effectScope` is exactly the scope a component's setup runs in; disposing it fires the same
  // `onScopeDispose(stop)` hook a component unmount fires.
  const scope = effectScope();
  let handle!: ReturnType<typeof usePolling<number>>;
  scope.run(() => {
    handle = usePolling<number>(async () => {
      await gate;
      return { ok: true, data: 7 };
    }, 0);
    handle.start(); // begin the gated fetch within the scope
  });

  await tick();
  expect(handle.loading.value).toBe(true);

  scope.stop(); // dispose the scope → onScopeDispose(stop) runs
  const before = { data: handle.data.value, loading: handle.loading.value };
  expect(before.loading).toBe(false); // stop() settled loading

  release(); // resolves after the scope was disposed
  await tick();
  await tick();

  expect(handle.data.value).toBe(before.data); // unchanged (still null) by the late resolution
  expect(handle.loading.value).toBe(before.loading);
});

test("refresh() after stop() is fully inert: no fetch, loading never flips, all state unchanged", async () => {
  let calls = 0;
  const handle = usePolling<number>(async () => {
    calls += 1;
    return { ok: true, data: 1 };
  }, 0);

  handle.stop();
  await handle.refresh(); // must NOT enter drive(): no fetch, no loading flip

  expect(calls).toBe(0); // no fetch issued
  expect(handle.data.value).toBeNull();
  expect(handle.error.value).toBeNull();
  expect(handle.lastUpdated.value).toBeNull();
  expect(handle.loading.value).toBe(false); // regression: loading must NOT be stuck true
});

test("start() after stop() is a no-op: no interval armed, no fetch", async () => {
  let calls = 0;
  const handle = usePolling<number>(async () => {
    calls += 1;
    return { ok: true, data: 1 };
  }, 10); // a real interval — must never arm on a disposed handle

  handle.stop();
  handle.start(); // dispose-once: no immediate refresh, no interval

  await new Promise((resolve) => setTimeout(resolve, 35)); // span several would-be interval ticks
  expect(calls).toBe(0); // neither the immediate fetch nor any interval tick fired
  expect(handle.loading.value).toBe(false);
});
