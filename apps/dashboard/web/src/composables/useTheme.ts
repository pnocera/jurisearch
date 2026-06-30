/**
 * web/ — light/dark theme (design §6.3). A module-level singleton so every toggle stays in sync; the
 * choice persists to `localStorage` and falls back to the OS preference. The `.dark` class on
 * `<html>` flips the token set in `assets/main.css`.
 */

import { readonly, ref } from "vue";

type Theme = "light" | "dark";
const STORAGE_KEY = "juridia-theme";

const isDark = ref(false);

function apply(theme: Theme): void {
  isDark.value = theme === "dark";
  if (typeof document !== "undefined") {
    document.documentElement.classList.toggle("dark", isDark.value);
  }
}

/** Resolve the initial theme from storage → OS preference → light. Call once at startup. */
export function initTheme(): void {
  let theme: Theme = "light";
  if (typeof window !== "undefined") {
    const stored = window.localStorage.getItem(STORAGE_KEY);
    if (stored === "light" || stored === "dark") {
      theme = stored;
    } else if (window.matchMedia?.("(prefers-color-scheme: dark)").matches) {
      theme = "dark";
    }
  }
  apply(theme);
}

export function useTheme() {
  function toggle(): void {
    const next: Theme = isDark.value ? "light" : "dark";
    apply(next);
    if (typeof window !== "undefined") {
      window.localStorage.setItem(STORAGE_KEY, next);
    }
  }
  return { isDark: readonly(isDark), toggle };
}
