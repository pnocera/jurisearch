import { expect, test } from "bun:test";
import {
  type LogLineDTO,
  type OverviewGroupDTO,
  parseLogLine,
  type RunRecordDTO,
  safeParsePackage,
  safeParseRunRecord,
} from "@jurisearch-dashboard/shared";
import { type Component, createSSRApp } from "vue";
import { renderToString } from "vue/server-renderer";
import DegradedPanel from "@/components/DegradedPanel.vue";
import GroupCard from "@/components/GroupCard.vue";
import LogViewer from "@/components/LogViewer.vue";
import PackagesTable from "@/components/PackagesTable.vue";
import RunRow from "@/components/RunRow.vue";
import StatusBadge from "@/components/StatusBadge.vue";
import journalLine from "../../fixtures/journal-legislation.json";
import emptyManifestFixture from "../../fixtures/manifest.json";
import manifestFixture from "../../fixtures/manifest-with-increment-synthetic.json";
import finishedRun from "../../fixtures/runrecord-legislation-finished-synthetic.json";

const render = (component: Component, props: Record<string, unknown>): Promise<string> =>
  renderToString(createSSRApp(component, props));

function unwrap<T>(parsed: { ok: true; value: T } | { ok: false; error: string }): T {
  if (!parsed.ok) {
    throw new Error(parsed.error);
  }
  return parsed.value;
}

const run: RunRecordDTO = unwrap(safeParseRunRecord(finishedRun));
const manifest = unwrap(safeParsePackage(manifestFixture));
const emptyManifest = unwrap(safeParsePackage(emptyManifestFixture));

test("StatusBadge maps severity to its R/A/G label", async () => {
  expect(await render(StatusBadge, { severity: "ok" })).toContain("Healthy");
  expect(await render(StatusBadge, { severity: "permanent" })).toContain("Failed");
});

test("RunRow renders kind, exact exit-class and derived duration", async () => {
  const html = await render(RunRow, { run });
  expect(html).toContain("legislation");
  expect(html).toContain("incremental");
  expect(html).toContain("published");
  expect(html).toContain("12m 17s");
});

test("RunRow surfaces a failure's error text", async () => {
  const failing: RunRecordDTO = {
    ...run,
    outcome: "failure",
    exitClass: "ingest-failed",
    error: "ingest aborted: disk full",
  };
  const html = await render(RunRow, { run: failing, pinned: true });
  expect(html).toContain("ingest-failed");
  expect(html).toContain("disk full");
});

test("GroupCard shows last run + flags a rebaseline kind prominently", async () => {
  const group: OverviewGroupDTO = {
    group: "legislation",
    sources: ["legi"],
    severity: "ok",
    lastRun: {
      runId: run.runId,
      kind: "rebaseline",
      outcome: "success",
      exitClass: "rebaselined",
      startedAt: run.startedAt,
      endedAt: run.endedAt,
      durationMs: 737000,
    },
    freshness: { staleByAge: false, rebaselinePending: false, baselines: [], fetchCursors: [] },
    publishedHeadSequence: 1,
    updateLockHeld: true,
    nextTimer: null,
  };
  const html = await render(GroupCard, { group });
  expect(html).toContain("legislation");
  expect(html).toContain("rebaselined");
  expect(html).toContain("rebaseline");
});

test("PackagesTable highlights the baseline and lists the increment chain", async () => {
  const html = await render(PackagesTable, { manifest });
  expect(html).toContain("core-bootstrap-v1");
  expect(html).toContain("core-1-2");
  expect(html).toContain("Increment chain (1)");
});

test("PackagesTable handles a baseline-only (empty) manifest gracefully", async () => {
  const html = await render(PackagesTable, { manifest: emptyManifest });
  expect(html).toContain("No increment packages");
});

test("LogViewer renders a producer log line", async () => {
  const line: LogLineDTO = parseLogLine(journalLine, "$");
  const html = await render(LogViewer, { lines: [line] });
  expect(html).toContain("Starting jurisearch-producer-legislation");
});

test("LogViewer shows an empty-window state", async () => {
  const html = await render(LogViewer, { lines: [] });
  expect(html).toContain("No log lines");
});

test("DegradedPanel renders the ApiResult error code + message", async () => {
  const html = await render(DegradedPanel, {
    error: { code: "status", message: "producer exited 78" },
  });
  expect(html).toContain("status");
  expect(html).toContain("producer exited 78");
});
