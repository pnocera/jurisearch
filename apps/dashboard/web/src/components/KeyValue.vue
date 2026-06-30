<script setup lang="ts">
import { cn } from "@/lib/cn.ts";
import { EMPTY } from "@/lib/format.ts";

// A labelled value row reused across cards/detail panels. `value` is optional — pass content via the
// default slot when it is richer than a string (e.g. a badge).
const props = defineProps<{
  label: string;
  value?: string | number | null;
  mono?: boolean;
  class?: string;
}>();
</script>

<template>
  <div :class="cn('flex items-baseline justify-between gap-3 text-sm', props.class)">
    <span class="shrink-0 text-muted-foreground">{{ props.label }}</span>
    <span
      :class="
        cn(
          'min-w-0 truncate text-right',
          props.mono ? 'font-mono text-xs tabular-nums' : 'font-medium',
        )
      "
      :title="props.value === undefined || props.value === null ? '' : String(props.value)"
    >
      <slot>{{ props.value === null || props.value === undefined ? EMPTY : props.value }}</slot>
    </span>
  </div>
</template>
