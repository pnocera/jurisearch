<script setup lang="ts">
import type { Severity } from "@jurisearch-dashboard/shared";
import { computed } from "vue";
import { Badge } from "@/components/ui/badge";
import { cn } from "@/lib/cn.ts";
import { presentationOf, severityLabel } from "@/lib/severity.ts";

// Presentational: maps a backend `Severity` to its R/A/G pill through the ONE mapping in
// `lib/severity.ts`. Never hard-codes a colour.
const props = defineProps<{ severity: Severity; label?: string; pulse?: boolean }>();

const presentation = computed(() => presentationOf(props.severity));
const text = computed(() => props.label ?? severityLabel(props.severity));
</script>

<template>
  <Badge :class="presentation.badgeClass" :title="severityLabel(props.severity)">
    <span
      :class="
        cn('size-1.5 rounded-full', presentation.dotClass, props.pulse ? 'animate-pulse' : '')
      "
    />
    {{ text }}
  </Badge>
</template>
