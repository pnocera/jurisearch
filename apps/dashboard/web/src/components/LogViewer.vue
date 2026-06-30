<script setup lang="ts">
import { groupFromUnit, type LogLineDTO } from "@jurisearch-dashboard/shared";
import { ScrollArea } from "@/components/ui/scroll-area";
import { cn } from "@/lib/cn.ts";
import { absoluteTime, EMPTY, relativeTime } from "@/lib/format.ts";
import { priorityRag, RAG_PRESENTATION } from "@/lib/severity.ts";

// Presentational log view (design §6.2): a scrollable window over `LogLineDTO[]`, coloured by syslog
// priority. No redaction. Fetching/filtering is the page's job; this only renders.
const props = defineProps<{ lines: LogLineDTO[]; height?: string }>();

// Syslog priority → R/A/G colour, routed through the ONE mapping (`lib/severity.ts`); no rag-* here.
function priorityClass(priority: number | null): string {
  return RAG_PRESENTATION[priorityRag(priority)].textClass;
}

function unitLabel(unit: string | null): string {
  return unit === null ? EMPTY : groupFromUnit(unit);
}
</script>

<template>
  <ScrollArea :style="{ height: props.height ?? '28rem' }" class="rounded-md border">
    <div class="divide-y divide-border/60 font-mono text-xs">
      <p v-if="props.lines.length === 0" class="p-4 text-muted-foreground">
        No log lines in this window.
      </p>
      <div
        v-for="(line, index) in props.lines"
        :key="index"
        class="grid grid-cols-[7rem_6rem_1fr] items-start gap-2 px-3 py-1.5 hover:bg-muted/40"
      >
        <span class="tabular-nums text-muted-foreground" :title="absoluteTime(line.timestamp)">
          {{ relativeTime(line.timestamp) }}
        </span>
        <span :class="cn('truncate', priorityClass(line.priority))" :title="line.unit ?? ''">
          {{ unitLabel(line.unit) }}
        </span>
        <span :class="cn('whitespace-pre-wrap break-words', priorityClass(line.priority))">
          {{ line.message ?? EMPTY }}
        </span>
      </div>
    </div>
  </ScrollArea>
</template>
