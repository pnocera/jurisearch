<script setup lang="ts">
import { cva, type VariantProps } from "class-variance-authority";
import { cn } from "@/lib/cn.ts";

const alertVariants = cva(
  "relative w-full rounded-lg border p-3 text-sm [&>svg]:size-4 [&>svg]:shrink-0",
  {
    variants: {
      variant: {
        default: "bg-card text-card-foreground",
        destructive: "border-rag-red/40 bg-rag-red/10 text-rag-red [&>svg]:text-rag-red",
        warning: "border-rag-amber/40 bg-rag-amber/10 text-rag-amber [&>svg]:text-rag-amber",
      },
    },
    defaultVariants: { variant: "default" },
  },
);

type AlertVariants = VariantProps<typeof alertVariants>;

const props = defineProps<{ variant?: AlertVariants["variant"]; class?: string }>();
</script>

<template>
  <div role="alert" :class="cn(alertVariants({ variant: props.variant }), props.class)">
    <div class="flex items-start gap-2">
      <slot />
    </div>
  </div>
</template>
