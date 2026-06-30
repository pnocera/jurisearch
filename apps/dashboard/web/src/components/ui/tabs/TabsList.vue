<script setup lang="ts">
import { TabsList } from "reka-ui";
import { cn } from "@/lib/cn.ts";

const props = defineProps<{ class?: string }>();
</script>

<template>
  <TabsList
    :class="
      cn(
        'inline-flex h-9 items-center justify-center gap-1 rounded-lg bg-muted p-1 text-muted-foreground',
        props.class,
      )
    "
  >
    <slot />
  </TabsList>
</template>
