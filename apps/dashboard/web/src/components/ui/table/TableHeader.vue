<script setup lang="ts">
import { cn } from "@/lib/cn.ts";

const props = defineProps<{ class?: string }>();
</script>

<template>
  <thead :class="cn('[&_tr]:border-b', props.class)">
    <slot />
  </thead>
</template>
