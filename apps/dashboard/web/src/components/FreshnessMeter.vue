<script setup lang="ts">
import type { OverviewFreshnessDTO } from "@jurisearch-dashboard/shared";
import { computed } from "vue";
import { Badge } from "@/components/ui/badge";
import { Tooltip } from "@/components/ui/tooltip";
import { cn } from "@/lib/cn.ts";
import { EMPTY, orEmpty } from "@/lib/format.ts";
import { RAG_PRESENTATION } from "@/lib/severity.ts";

// Presentational view of a group's freshness inputs (design §6.2): adopted-vs-fetched baseline per
// source, the latest fetched-archive cursor (the pending-delta lag), and the two staleness flags.
const props = defineProps<{ freshness: OverviewFreshnessDTO }>();

const cursorBySource = computed(() => {
  const map = new Map<string, string>();
  for (const cursor of props.freshness.fetchCursors) {
    const latest = cursor.latestFileName ?? cursor.latestCompactTimestamp;
    if (latest !== null && latest !== undefined) {
      map.set(cursor.source, latest);
    }
  }
  return map;
});

interface Row {
  source: string;
  state: string;
  /** A fetched baseline newer than the adopted one means the source is behind. */
  behind: boolean;
  adopted: string | null;
  fetched: string | null;
  lag: string;
}

const rows = computed<Row[]>(() =>
  props.freshness.baselines.map((baseline) => ({
    source: baseline.source,
    state: baseline.state,
    behind:
      baseline.fetchedBaseline !== null &&
      baseline.adoptedBaseline !== null &&
      baseline.fetchedBaseline !== baseline.adoptedBaseline,
    adopted: baseline.adoptedBaseline,
    fetched: baseline.fetchedBaseline,
    lag: cursorBySource.value.get(baseline.source) ?? EMPTY,
  })),
);

const currentCount = computed(() => rows.value.filter((row) => row.state === "current").length);
const total = computed(() => rows.value.length);
const allCurrent = computed(() => total.value > 0 && currentCount.value === total.value);
</script>

<template>
  <div class="space-y-2">
    <div class="flex items-center justify-between">
      <span class="text-xs font-medium text-muted-foreground">Freshness</span>
      <div class="flex items-center gap-1.5">
        <Badge
          v-if="props.freshness.rebaselinePending"
          :class="RAG_PRESENTATION.amber.badgeClass"
        >
          rebaseline pending
        </Badge>
        <Badge v-if="props.freshness.staleByAge" :class="RAG_PRESENTATION.amber.badgeClass">
          stale by age
        </Badge>
        <span class="text-xs tabular-nums text-muted-foreground">
          {{ currentCount }}/{{ total }} current
        </span>
      </div>
    </div>

    <div class="h-1.5 w-full overflow-hidden rounded-full bg-muted">
      <div
        :class="
          cn(
            'h-full rounded-full transition-all',
            allCurrent && !props.freshness.staleByAge && !props.freshness.rebaselinePending
              ? RAG_PRESENTATION.green.dotClass
              : RAG_PRESENTATION.amber.dotClass,
          )
        "
        :style="{ width: total === 0 ? '0%' : `${(currentCount / total) * 100}%` }"
      />
    </div>

    <ul class="space-y-1">
      <li
        v-for="row in rows"
        :key="row.source"
        class="flex items-center justify-between gap-2 text-xs"
      >
        <span class="font-mono">{{ row.source }}</span>
        <div class="flex items-center gap-1.5">
          <Tooltip
            :text="`adopted: ${orEmpty(row.adopted)}\nfetched: ${orEmpty(row.fetched)}\nlatest: ${row.lag}`"
          >
            <span
              :class="
                cn(
                  'rounded px-1 py-0.5',
                  row.behind ? RAG_PRESENTATION.amber.textClass : 'text-muted-foreground',
                )
              "
            >
              {{ row.state }}{{ row.behind ? " · behind" : "" }}
            </span>
          </Tooltip>
        </div>
      </li>
    </ul>
  </div>
</template>
