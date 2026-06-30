<script setup lang="ts">
import { cn } from "@/lib/cn.ts";

const props = defineProps<{ class?: string }>();
</script>

<template>
  <h5 :class="cn('mb-0.5 font-medium leading-none tracking-tight', props.class)">
    <slot />
  </h5>
</template>
