<script setup lang="ts">
import { cn } from "@/lib/cn.ts";

const props = defineProps<{ class?: string }>();
</script>

<template>
  <h3 :class="cn('font-semibold leading-none tracking-tight', props.class)">
    <slot />
  </h3>
</template>
