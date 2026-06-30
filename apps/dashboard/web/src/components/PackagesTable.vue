<script setup lang="ts">
import type { PackageManifestDTO } from "@jurisearch-dashboard/shared";
import { computed } from "vue";
import KeyValue from "@/components/KeyValue.vue";
import { Badge } from "@/components/ui/badge";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { Tooltip } from "@/components/ui/tooltip";
import { absoluteTime, formatBytes, formatCount, relativeTime, sequenceRange } from "@/lib/format.ts";
import { RAG_PRESENTATION } from "@/lib/severity.ts";

// Presentational view of the served manifest (design §6.2): the active baseline highlighted, then the
// increment chain. Tolerates an EMPTY `packages` (baseline only — Spike B §6).
const props = defineProps<{ manifest: PackageManifestDTO }>();

function shortDigest(sha256: string): string {
  const hex = sha256.replace(/^sha256:/, "");
  return hex.length > 16 ? `${hex.slice(0, 16)}…` : hex;
}

function rowCountSummary(counts: Record<string, number>): string {
  const entries = Object.entries(counts);
  if (entries.length === 0) {
    return "—";
  }
  return entries.map(([key, value]) => `${key} ${formatCount(value)}`).join(" · ");
}

const baseline = computed(() => props.manifest.activeBaseline);
const hasIncrements = computed(() => props.manifest.packages.length > 0);
</script>

<template>
  <div class="space-y-4">
    <!-- Active baseline -->
    <div :class="['rounded-lg border-l-4 border bg-card p-4', RAG_PRESENTATION.green.accentClass]">
      <div class="mb-2 flex items-center justify-between">
        <div class="flex items-center gap-2">
          <span class="font-semibold">Active baseline</span>
          <Badge :class="RAG_PRESENTATION.green.badgeClass">{{ baseline.baselineId }}</Badge>
        </div>
        <span class="text-xs text-muted-foreground" :title="absoluteTime(manifest.manifestGeneratedAt)">
          manifest {{ relativeTime(manifest.manifestGeneratedAt) }}
        </span>
      </div>
      <div class="grid grid-cols-1 gap-x-6 gap-y-1 sm:grid-cols-2">
        <KeyValue label="Generation" :value="baseline.generation" mono />
        <KeyValue label="Kind" :value="baseline.packageKind" />
        <KeyValue label="Sequence" :value="formatCount(baseline.sequence)" mono />
        <KeyValue label="Schema version" :value="formatCount(baseline.schemaVersion)" mono />
        <KeyValue label="Compressed" :value="formatBytes(baseline.compressedSizeBytes)" />
        <KeyValue label="Uncompressed" :value="formatBytes(baseline.uncompressedSizeBytes)" />
        <KeyValue label="Corpus" :value="manifest.corpus" />
        <KeyValue label="Head sequence" :value="formatCount(manifest.headSequence)" mono />
      </div>
      <div class="mt-2 border-t pt-2">
        <KeyValue label="Digest" :value="baseline.sha256" mono />
      </div>
    </div>

    <!-- Increment chain -->
    <div>
      <h3 class="mb-2 text-sm font-medium text-muted-foreground">
        Increment chain ({{ manifest.packages.length }})
      </h3>
      <p v-if="!hasIncrements" class="rounded-md border border-dashed p-4 text-sm text-muted-foreground">
        No increment packages published yet — the corpus is at the baseline only.
      </p>
      <Table v-else>
        <TableHeader>
          <TableRow>
            <TableHead>Package</TableHead>
            <TableHead>Seq</TableHead>
            <TableHead>Compressed</TableHead>
            <TableHead>Uncompressed</TableHead>
            <TableHead>Rows</TableHead>
            <TableHead>Schema</TableHead>
            <TableHead>Fingerprint</TableHead>
            <TableHead>Digest</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          <TableRow v-for="pkg in manifest.packages" :key="pkg.packageId">
            <TableCell class="font-mono text-xs">{{ pkg.packageId }}</TableCell>
            <TableCell class="tabular-nums">{{ sequenceRange(pkg.fromSequence, pkg.toSequence) }}</TableCell>
            <TableCell class="tabular-nums">{{ formatBytes(pkg.compressedSizeBytes) }}</TableCell>
            <TableCell class="tabular-nums">{{ formatBytes(pkg.uncompressedSizeBytes) }}</TableCell>
            <TableCell class="text-xs">{{ rowCountSummary(pkg.rowCounts) }}</TableCell>
            <TableCell class="tabular-nums">{{ formatCount(pkg.schemaVersion) }}</TableCell>
            <TableCell class="font-mono text-xs">{{ pkg.embeddingFingerprint }}</TableCell>
            <TableCell class="font-mono text-xs">
              <Tooltip :text="pkg.sha256">
                <span>{{ shortDigest(pkg.sha256) }}</span>
              </Tooltip>
            </TableCell>
          </TableRow>
        </TableBody>
      </Table>
    </div>
  </div>
</template>
