<script setup lang="ts">
import { cn } from "@/lib/cn.ts";

const props = defineProps<{ class?: string }>();
</script>

<template>
  <td :class="cn('px-3 py-2 align-middle', props.class)">
    <slot />
  </td>
</template>
