<script setup lang="ts">
import { TabsContent } from "reka-ui";
import { cn } from "@/lib/cn.ts";

const props = defineProps<{ value: string; class?: string }>();
</script>

<template>
  <TabsContent
    :value="props.value"
    :class="cn('mt-4 focus-visible:outline-none', props.class)"
  >
    <slot />
  </TabsContent>
</template>
