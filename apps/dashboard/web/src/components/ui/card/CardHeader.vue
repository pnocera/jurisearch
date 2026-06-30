<script setup lang="ts">
import { cn } from "@/lib/cn.ts";

const props = defineProps<{ class?: string }>();
</script>

<template>
  <div :class="cn('flex flex-col gap-1.5 p-4', props.class)">
    <slot />
  </div>
</template>
