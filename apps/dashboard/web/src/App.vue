<script setup lang="ts">
import { DASHBOARD_NAME } from "@jurisearch-dashboard/shared";
import { RouterLink, RouterView } from "vue-router";
import ThemeToggle from "@/components/ThemeToggle.vue";

// App shell: an ops MASTHEAD — a typographic "Juridia" wordmark (display grotesk) + a "/ Update
// Server" mono subtitle, understated page tabs with an ink underline active-state, a SOLID header
// (no glass/blur), theme toggle. Pages own their own data via composables; the shell holds none.
// The two wordmark parts are derived from the single `DASHBOARD_NAME` contract so it stays the source.
const [mark, subtitle] = DASHBOARD_NAME.split("—").map((part) => part.trim());

const nav = [
  { to: "/", label: "Overview" },
  { to: "/packages", label: "Packages" },
  { to: "/runs", label: "Runs" },
  { to: "/logs", label: "Logs" },
];
</script>

<template>
  <div class="min-h-screen bg-background text-foreground">
    <header class="sticky top-0 z-40 border-b border-border bg-background">
      <div
        class="mx-auto flex max-w-7xl flex-wrap items-center gap-x-6 gap-y-1 px-4 py-2.5"
      >
        <RouterLink to="/" class="order-1 flex shrink-0 items-baseline gap-2" :title="DASHBOARD_NAME">
          <span class="font-display text-xl font-bold leading-none tracking-tight">{{ mark }}</span>
          <span
            class="hidden font-mono text-[0.65rem] uppercase leading-none tracking-[0.18em] text-muted-foreground sm:inline"
          >
            / {{ subtitle }}
          </span>
        </RouterLink>

        <ThemeToggle class="order-2 ml-auto shrink-0 sm:order-3" />

        <nav
          class="order-3 -mb-2.5 w-full overflow-x-auto sm:order-2 sm:mb-0 sm:w-auto sm:overflow-visible"
        >
          <div class="flex items-stretch gap-5">
            <RouterLink
              v-for="item in nav"
              :key="item.to"
              :to="item.to"
              class="whitespace-nowrap border-b-2 border-transparent py-2 text-sm text-muted-foreground transition-colors hover:text-foreground sm:py-1"
              exact-active-class="border-foreground text-foreground"
            >
              {{ item.label }}
            </RouterLink>
          </div>
        </nav>
      </div>
    </header>

    <main class="mx-auto max-w-7xl px-4 py-6">
      <RouterView />
    </main>
  </div>
</template>
