<script setup lang="ts">
import { cn } from "@/lib/cn.ts";

const props = defineProps<{ class?: string }>();
</script>

<template>
  <div :class="cn('p-4 pt-0', props.class)">
    <slot />
  </div>
</template>
