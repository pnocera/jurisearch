import { fileURLToPath, URL } from "node:url";
import tailwindcss from "@tailwindcss/vite";
import vue from "@vitejs/plugin-vue";
import { defineConfig } from "vite";

// The web SPA build. Emits hashed static assets to `web/dist` (M5 embeds these into the compiled
// binary behind the same `AssetSource` seam). A dev proxy forwards `/api/*` to the M3 server so the
// SPA renders real `ApiResult` envelopes through the shared validators during development.
export default defineConfig({
  plugins: [vue(), tailwindcss()],
  resolve: {
    alias: {
      "@": fileURLToPath(new URL("./src", import.meta.url)),
    },
  },
  build: {
    outDir: "dist",
    emptyOutDir: true,
  },
  server: {
    proxy: {
      // `DASHBOARD_DEV_API` lets the harness point Vite at the live M3 server's port.
      "/api": {
        target: process.env.DASHBOARD_DEV_API ?? "http://127.0.0.1:8787",
        changeOrigin: true,
      },
    },
  },
});
