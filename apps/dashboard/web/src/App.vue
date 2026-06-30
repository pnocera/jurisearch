<script setup lang="ts">
// Imports from shared/ to prove the workspace wiring (one contract for server + web). The real
// pages/components/composables land in M4.
import { DASHBOARD_NAME } from "@jurisearch-dashboard/shared";
</script>

<template>
  <main>
    <h1>{{ DASHBOARD_NAME }}</h1>
    <p>Scaffold (M0) — operational dashboard UI lands in M4.</p>
  </main>
</template>
