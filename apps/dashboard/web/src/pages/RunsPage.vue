<script setup lang="ts">
import type { RunRecordDTO } from "@jurisearch-dashboard/shared";
import { computed, ref } from "vue";
import DegradedPanel from "@/components/DegradedPanel.vue";
import RefreshBar from "@/components/RefreshBar.vue";
import RunRow from "@/components/RunRow.vue";
import { Tabs, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { useRuns } from "@/composables/resources.ts";
import { cn } from "@/lib/cn.ts";
import { RAG_PRESENTATION } from "@/lib/severity.ts";

// Runs / Errors page (design §6.2): per-group run list with failures PINNED to the top. Group filter
// is client-side over the full list so the tab set is derived from the data itself.
const { data, error, loading, lastUpdated, refresh } = useRuns();

const activeGroup = ref<string>("all");

const groups = computed(() => {
  const names = new Set<string>();
  for (const run of data.value ?? []) {
    names.add(run.group);
  }
  return [...names].sort();
});

const visible = computed<RunRecordDTO[]>(() => {
  const all = data.value ?? [];
  return activeGroup.value === "all" ? all : all.filter((run) => run.group === activeGroup.value);
});

const failures = computed(() => visible.value.filter((run) => run.outcome === "failure"));
const others = computed(() => visible.value.filter((run) => run.outcome !== "failure"));
</script>

<template>
  <section class="space-y-4">
    <div class="flex flex-wrap items-center justify-between gap-2">
      <h2 class="text-2xl font-semibold tracking-tight">Runs &amp; Errors</h2>
      <RefreshBar :loading="loading" :last-updated="lastUpdated" @refresh="refresh" />
    </div>

    <DegradedPanel v-if="error && !data" :error="error" title="Runs unavailable" />

    <template v-if="data">
      <DegradedPanel v-if="error" :error="error" title="Runs degraded" stale />

      <Tabs v-if="groups.length > 1" v-model="activeGroup">
        <TabsList>
          <TabsTrigger value="all">All</TabsTrigger>
          <TabsTrigger v-for="group in groups" :key="group" :value="group" class="capitalize">
            {{ group }}
          </TabsTrigger>
        </TabsList>
      </Tabs>

      <div v-if="failures.length > 0" class="space-y-2">
        <h3 :class="cn('text-sm font-medium', RAG_PRESENTATION.red.textClass)">
          Failures ({{ failures.length }})
        </h3>
        <RunRow v-for="run in failures" :key="run.runId" :run="run" pinned />
      </div>

      <div class="space-y-2">
        <h3 v-if="failures.length > 0" class="text-sm font-medium text-muted-foreground">
          Other runs ({{ others.length }})
        </h3>
        <p v-if="visible.length === 0" class="rounded-md border border-dashed p-4 text-sm text-muted-foreground">
          No run records for this group.
        </p>
        <RunRow v-for="run in others" :key="run.runId" :run="run" />
      </div>
    </template>
  </section>
</template>
