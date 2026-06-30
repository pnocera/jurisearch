// Offline, self-hosted fonts (design pass): @fontsource ships the woff2 inside the package, so Vite
// bundles them into web/dist/assets (no Google-Fonts CDN / <link>) — they embed in the M5 binary and
// M3's AssetSource serves them on CT 111 with NO network. Latin subset covers French accents + the
// em dash. Display/body = Space Grotesk (grotesk), data = JetBrains Mono.
import "@fontsource/space-grotesk/latin-400.css";
import "@fontsource/space-grotesk/latin-500.css";
import "@fontsource/space-grotesk/latin-600.css";
import "@fontsource/space-grotesk/latin-700.css";
import "@fontsource/jetbrains-mono/latin-400.css";
import "@fontsource/jetbrains-mono/latin-500.css";
import "@fontsource/jetbrains-mono/latin-700.css";
import { createApp } from "vue";
import App from "./App.vue";
import "./assets/main.css";
import { initTheme } from "./composables/useTheme.ts";
import { router } from "./router/index.ts";

initTheme();
createApp(App).use(router).mount("#app");
