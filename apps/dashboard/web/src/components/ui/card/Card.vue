<script setup lang="ts">
import { cn } from "@/lib/cn.ts";

const props = defineProps<{ class?: string }>();
</script>

<template>
  <div :class="cn('rounded-lg border bg-card text-card-foreground shadow-sm', props.class)">
    <slot />
  </div>
</template>
