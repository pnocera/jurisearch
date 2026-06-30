<script setup lang="ts">
import { type RunRecordDTO, severityOf } from "@jurisearch-dashboard/shared";
import { computed } from "vue";
import StatusBadge from "@/components/StatusBadge.vue";
import { Badge } from "@/components/ui/badge";
import { cn } from "@/lib/cn.ts";
import { absoluteTime, EMPTY, relativeTime, runDuration } from "@/lib/format.ts";
import { presentationOf, RAG_PRESENTATION } from "@/lib/severity.ts";

// One run record row (design §6.2 Runs/Errors). Severity colour comes from the shared `severityOf`
// (outcome-first), mapped to R/A/G via the ONE `lib/severity.ts` mapping.
const props = defineProps<{ run: RunRecordDTO; pinned?: boolean }>();

const severity = computed(() => severityOf(props.run.outcome, props.run.exitClass));
const isRunning = computed(() => props.run.outcome === "running");
const duration = computed(() =>
  isRunning.value ? "in progress" : runDuration(props.run.startedAt, props.run.endedAt),
);
</script>

<template>
  <div
    :class="
      cn(
        'grid grid-cols-[auto_1fr_auto] items-center gap-x-3 gap-y-1 rounded-md border px-3 py-2',
        props.pinned ? cn('border-l-2', presentationOf(severity).accentClass) : 'border-transparent',
      )
    "
  >
    <StatusBadge :severity="severity" :pulse="isRunning" />

    <div class="min-w-0">
      <div class="flex flex-wrap items-center gap-x-2 gap-y-1 text-sm">
        <span class="font-medium capitalize">{{ run.group }}</span>
        <Badge class="border-border bg-muted text-muted-foreground">{{ run.kind }}</Badge>
        <span :class="cn('font-mono text-xs', presentationOf(severity).textClass)">
          {{ run.exitClass }}
        </span>
      </div>
      <p
        v-if="run.error"
        :class="cn('mt-0.5 break-words text-xs', RAG_PRESENTATION.red.textClass)"
      >
        {{ run.error }}
      </p>
    </div>

    <div class="text-right text-xs text-muted-foreground">
      <div class="tabular-nums">{{ duration }}</div>
      <div :title="absoluteTime(run.startedAt)">{{ relativeTime(run.startedAt) || EMPTY }}</div>
    </div>
  </div>
</template>
