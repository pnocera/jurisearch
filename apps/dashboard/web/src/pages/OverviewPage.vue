<script setup lang="ts">
import { computed } from "vue";
import DegradedPanel from "@/components/DegradedPanel.vue";
import GroupCard from "@/components/GroupCard.vue";
import KeyValue from "@/components/KeyValue.vue";
import RefreshBar from "@/components/RefreshBar.vue";
import { Badge } from "@/components/ui/badge";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { useOverview } from "@/composables/resources.ts";
import { EMPTY, formatCount, orEmpty, relativeTime } from "@/lib/format.ts";
import { overallRag, RAG_PRESENTATION } from "@/lib/severity.ts";

// Overview page (design §6.2): a GroupCard per fetch group + a corpus header. Composes the single
// `useOverview` composable and presentational components only — no fetching here.
const { data, error, loading, lastUpdated, refresh } = useOverview();

const overall = computed(() => data.value?.overall ?? null);
</script>

<template>
  <section class="space-y-4">
    <div class="flex flex-wrap items-center justify-between gap-2">
      <h2 class="text-2xl font-semibold tracking-tight">Overview</h2>
      <RefreshBar :loading="loading" :last-updated="lastUpdated" @refresh="refresh" />
    </div>

    <!-- Hard failure with no prior data: the whole page degrades to one panel. -->
    <DegradedPanel
      v-if="error && !data"
      :error="error"
      title="Overview unavailable"
    />

    <template v-if="data">
      <!-- Soft degrade: stale data is still shown beneath the notice. -->
      <DegradedPanel v-if="error" :error="error" title="Overview degraded" stale />

      <Card>
        <CardHeader>
          <div class="flex flex-wrap items-center justify-between gap-2">
            <CardTitle class="flex items-center gap-2">
              Corpus <span class="font-mono">{{ data.corpus }}</span>
              <Badge v-if="overall" :class="RAG_PRESENTATION[overallRag(overall)].badgeClass">
                {{ overall }}
              </Badge>
            </CardTitle>
            <span class="text-xs text-muted-foreground">
              generated {{ relativeTime(data.generatedAt) }}
            </span>
          </div>
        </CardHeader>
        <CardContent class="grid grid-cols-1 gap-x-8 gap-y-1 sm:grid-cols-2 lg:grid-cols-3">
          <KeyValue label="Published head seq" :value="formatCount(data.publishedHeadSequence)" mono />
          <KeyValue label="Active baseline" :value="orEmpty(data.activeBaselineId)" mono />
          <KeyValue
            label="Manifest published"
            :value="data.publishedManifestGeneratedAt ? relativeTime(data.publishedManifestGeneratedAt) : EMPTY"
          />
          <KeyValue label="Update lock" :value="data.updateLockHeld ? 'held' : 'free'" />
          <KeyValue label="Groups" :value="formatCount(data.groups.length)" />
        </CardContent>
      </Card>

      <div class="grid grid-cols-1 gap-4 md:grid-cols-2 xl:grid-cols-3">
        <GroupCard v-for="group in data.groups" :key="group.group" :group="group" />
      </div>
    </template>
  </section>
</template>
