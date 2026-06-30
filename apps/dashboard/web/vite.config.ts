import vue from "@vitejs/plugin-vue";
import { defineConfig } from "vite";

// M0: minimal build that emits static assets to web/dist. M5 embeds these into the compiled
// binary (the build output is the SPA the server will serve).
export default defineConfig({
  plugins: [vue()],
  build: {
    outDir: "dist",
    emptyOutDir: true,
  },
});
