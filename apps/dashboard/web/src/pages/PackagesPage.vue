<script setup lang="ts">
import DegradedPanel from "@/components/DegradedPanel.vue";
import PackagesTable from "@/components/PackagesTable.vue";
import RefreshBar from "@/components/RefreshBar.vue";
import { usePackages } from "@/composables/resources.ts";

// Packages page (design §6.2): the served manifest — active baseline + increment chain.
const { data, error, loading, lastUpdated, refresh } = usePackages();
</script>

<template>
  <section class="space-y-4">
    <div class="flex flex-wrap items-center justify-between gap-2">
      <h2 class="text-2xl font-semibold tracking-tight">Packages</h2>
      <RefreshBar :loading="loading" :last-updated="lastUpdated" @refresh="refresh" />
    </div>

    <DegradedPanel v-if="error && !data" :error="error" title="Manifest unavailable" />

    <template v-if="data">
      <DegradedPanel v-if="error" :error="error" title="Manifest degraded" stale />
      <PackagesTable :manifest="data" />
    </template>
  </section>
</template>
