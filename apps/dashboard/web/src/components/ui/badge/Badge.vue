<script setup lang="ts">
import { cn } from "@/lib/cn.ts";

// Presentational pill. Colour is supplied by the caller via `class` (the R/A/G classes come from
// `lib/severity.ts`, never hard-coded here), so the badge stays policy-free.
const props = defineProps<{ class?: string }>();
</script>

<template>
  <span
    :class="
      cn(
        'inline-flex items-center gap-1 rounded-full border px-2 py-0.5 text-xs font-medium whitespace-nowrap',
        props.class,
      )
    "
  >
    <slot />
  </span>
</template>
