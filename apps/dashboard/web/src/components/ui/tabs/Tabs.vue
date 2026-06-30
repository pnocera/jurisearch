<script setup lang="ts">
import { TabsRoot } from "reka-ui";
import { cn } from "@/lib/cn.ts";

const props = defineProps<{ class?: string; modelValue?: string; defaultValue?: string }>();
const emits = defineEmits<{ "update:modelValue": [value: string] }>();
</script>

<template>
  <TabsRoot
    :model-value="props.modelValue"
    :default-value="props.defaultValue"
    :class="cn('w-full', props.class)"
    @update:model-value="(value) => emits('update:modelValue', String(value))"
  >
    <slot />
  </TabsRoot>
</template>
