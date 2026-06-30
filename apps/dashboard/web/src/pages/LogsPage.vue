<script setup lang="ts">
import { groupFromUnit } from "@jurisearch-dashboard/shared";
import { computed, ref, watch } from "vue";
import DegradedPanel from "@/components/DegradedPanel.vue";
import LogViewer from "@/components/LogViewer.vue";
import RefreshBar from "@/components/RefreshBar.vue";
import { Tabs, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { useLogs } from "@/composables/resources.ts";

// Logs page (design §6.2): a `since`/ring-buffer window per producer service with a group filter.
// `group`/`limit` are SERVER-side params (the provider shells out to `journalctl` per group); the
// `LogViewer` is purely presentational. No redaction.
const activeGroup = ref<string>("all");
const limit = ref<number>(200);

const groupParam = computed(() => (activeGroup.value === "all" ? undefined : activeGroup.value));

const { data, error, loading, lastUpdated, refresh } = useLogs({ group: groupParam, limit });

// Accumulate discovered producer groups so the filter tabs persist across a filtered fetch.
const discovered = ref<Set<string>>(new Set());
watch(
  data,
  (lines) => {
    for (const line of lines ?? []) {
      if (line.unit !== null) {
        discovered.value.add(groupFromUnit(line.unit));
      }
    }
  },
  { immediate: true },
);

const groups = computed(() => [...discovered.value].sort());
const lines = computed(() => data.value ?? []);
</script>

<template>
  <section class="space-y-4">
    <div class="flex flex-wrap items-center justify-between gap-2">
      <h2 class="text-2xl font-semibold tracking-tight">Logs</h2>
      <RefreshBar :loading="loading" :last-updated="lastUpdated" @refresh="refresh" />
    </div>

    <div class="flex flex-wrap items-center justify-between gap-2">
      <Tabs v-model="activeGroup">
        <TabsList>
          <TabsTrigger value="all">All</TabsTrigger>
          <TabsTrigger v-for="group in groups" :key="group" :value="group" class="capitalize">
            {{ group }}
          </TabsTrigger>
        </TabsList>
      </Tabs>
      <label class="flex items-center gap-1.5 text-xs text-muted-foreground">
        window
        <select
          v-model.number="limit"
          class="rounded-md border bg-background px-2 py-1 text-foreground"
        >
          <option :value="100">100</option>
          <option :value="200">200</option>
          <option :value="500">500</option>
        </select>
      </label>
    </div>

    <!-- journald is sparse / unavailable in the dev harness: degrade the panel cleanly. -->
    <DegradedPanel v-if="error" :error="error" title="Logs degraded" :stale="lines.length > 0" />

    <LogViewer :lines="lines" />
  </section>
</template>
