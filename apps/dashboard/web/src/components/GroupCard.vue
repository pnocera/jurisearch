<script setup lang="ts">
import type { OverviewGroupDTO } from "@jurisearch-dashboard/shared";
import { Lock, RefreshCw } from "lucide-vue-next";
import { computed } from "vue";
import FreshnessMeter from "@/components/FreshnessMeter.vue";
import KeyValue from "@/components/KeyValue.vue";
import StatusBadge from "@/components/StatusBadge.vue";
import { Badge } from "@/components/ui/badge";
import { Card, CardContent, CardHeader } from "@/components/ui/card";
import { cn } from "@/lib/cn.ts";
import { EMPTY, formatCount, formatDuration, orEmpty, relativeTime } from "@/lib/format.ts";
import { groupRag, RAG_PRESENTATION } from "@/lib/severity.ts";

// One fetch group's GroupCard (design §6.2). Presentational over the composed `OverviewGroupDTO`;
// the card's R/A/G accent combines the run severity with freshness via the ONE `groupRag` mapping.
const props = defineProps<{ group: OverviewGroupDTO }>();

const rag = computed(() => groupRag(props.group.severity, props.group.freshness));
const accent = computed(() => RAG_PRESENTATION[rag.value]);
const lastRun = computed(() => props.group.lastRun);
const isRebaseline = computed(() => props.group.lastRun?.kind === "rebaseline");
const isRunning = computed(() => props.group.severity === "neutral");
const when = computed(() =>
  relativeTime(props.group.lastRun?.endedAt ?? props.group.lastRun?.startedAt ?? null),
);
</script>

<template>
  <Card :class="cn('border-l-4', accent.accentClass)">
    <CardHeader>
      <div class="flex items-start justify-between gap-2">
        <div class="min-w-0">
          <h3 class="font-semibold capitalize leading-tight">{{ group.group }}</h3>
          <p class="truncate font-mono text-xs text-muted-foreground">
            {{ group.sources.join(", ") || EMPTY }}
          </p>
        </div>
        <div class="flex shrink-0 flex-col items-end gap-1">
          <StatusBadge :severity="group.severity" :pulse="isRunning" />
          <Badge v-if="isRebaseline" :class="RAG_PRESENTATION.amber.badgeClass">
            <RefreshCw class="size-3" /> rebaseline
          </Badge>
        </div>
      </div>
    </CardHeader>

    <CardContent class="space-y-3">
      <div class="space-y-1 rounded-md bg-muted/40 p-2">
        <template v-if="lastRun">
          <KeyValue label="Last run">
            <span :class="cn('font-medium', accent.textClass)">
              {{ orEmpty(lastRun.exitClass) }}
            </span>
          </KeyValue>
          <KeyValue label="Kind" :value="orEmpty(lastRun.kind)" />
          <KeyValue
            label="Duration"
            :value="lastRun.durationMs === null ? (isRunning ? 'in progress' : EMPTY) : formatDuration(lastRun.durationMs)"
          />
          <KeyValue label="When" :value="when" />
        </template>
        <p v-else class="text-sm text-muted-foreground">Never run</p>
      </div>

      <FreshnessMeter :freshness="group.freshness" />

      <div class="space-y-1 border-t pt-2">
        <KeyValue label="Published head seq" :value="formatCount(group.publishedHeadSequence)" mono />
        <KeyValue label="Next run" :value="group.nextTimer ? relativeTime(group.nextTimer.nextRun) : EMPTY" />
        <KeyValue v-if="group.updateLockHeld" label="Update lock">
          <span class="inline-flex items-center gap-1 text-muted-foreground">
            <Lock class="size-3" /> held
          </span>
        </KeyValue>
      </div>
    </CardContent>
  </Card>
</template>
