<script setup lang="ts">
import { Moon, Sun } from "lucide-vue-next";
import { Button } from "@/components/ui/button";
import { useTheme } from "@/composables/useTheme.ts";

const { isDark, toggle } = useTheme();
</script>

<template>
  <Button
    variant="ghost"
    size="icon"
    :title="isDark ? 'Switch to light theme' : 'Switch to dark theme'"
    aria-label="Toggle colour theme"
    @click="toggle"
  >
    <Moon v-if="isDark" class="size-4" />
    <Sun v-else class="size-4" />
  </Button>
</template>
