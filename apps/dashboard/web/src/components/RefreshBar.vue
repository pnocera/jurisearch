<script setup lang="ts">
import { RefreshCw } from "lucide-vue-next";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/cn.ts";
import { relativeTime } from "@/lib/format.ts";

// The per-page refresh control + freshness line. Wired to one `usePolling` handle's state; emits
// `refresh` for the manual button (the same `refresh()` the interval calls).
const props = defineProps<{ loading: boolean; lastUpdated: number | null }>();
const emit = defineEmits<{ refresh: [] }>();
</script>

<template>
  <div class="flex items-center gap-2 text-xs text-muted-foreground">
    <span v-if="props.lastUpdated !== null">Updated {{ relativeTime(props.lastUpdated) }}</span>
    <span v-else>Loading…</span>
    <Button
      variant="ghost"
      size="icon"
      class="size-7"
      :disabled="props.loading"
      aria-label="Refresh now"
      @click="emit('refresh')"
    >
      <RefreshCw :class="cn('size-3.5', props.loading ? 'animate-spin' : '')" />
    </Button>
  </div>
</template>
