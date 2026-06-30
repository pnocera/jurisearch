<script setup lang="ts">
import { TriangleAlert } from "lucide-vue-next";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";

// Renders the `ApiResult` degraded branch (`{ ok:false, error }`) as one self-contained panel — the
// rest of the dashboard keeps rendering (design §5.4). `code` is the upstream provider tag.
const props = defineProps<{
  error: { code?: string; message: string };
  title?: string;
  /** When true, the panel notes that the last good data is still shown beneath it. */
  stale?: boolean;
}>();
</script>

<template>
  <Alert variant="destructive">
    <TriangleAlert />
    <div class="min-w-0">
      <AlertTitle>
        {{ props.title ?? "Panel degraded" }}
        <span v-if="props.error.code" class="font-mono text-xs opacity-80">
          ({{ props.error.code }})
        </span>
      </AlertTitle>
      <AlertDescription>
        <p class="break-words">{{ props.error.message }}</p>
        <p v-if="props.stale" class="mt-1 text-xs opacity-75">
          Showing the last successfully fetched data.
        </p>
      </AlertDescription>
    </div>
  </Alert>
</template>
