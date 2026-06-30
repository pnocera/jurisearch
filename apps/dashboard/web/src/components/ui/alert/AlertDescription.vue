<script setup lang="ts">
import { cn } from "@/lib/cn.ts";

const props = defineProps<{ class?: string }>();
</script>

<template>
  <div :class="cn('text-sm opacity-90 [&_p]:leading-relaxed', props.class)">
    <slot />
  </div>
</template>
