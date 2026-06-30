<script setup lang="ts">
import { cn } from "@/lib/cn.ts";

const props = defineProps<{ class?: string }>();
</script>

<template>
  <tr :class="cn('border-b transition-colors hover:bg-muted/40 data-[state=on]:bg-muted/60', props.class)">
    <slot />
  </tr>
</template>
