/**
 * web/ — Vue Router in HISTORY mode (design §6.1). History mode means deep links like `/packages`
 * are real URLs; M3's SPA fallback serves `index.html` for these navigation routes while still 404-ing
 * missing static assets (router.ts `serveSpa`). Keep `createWebHistory()` at the site root so the
 * compiled binary (M5) serves the SPA from `/`.
 */

import { createRouter, createWebHistory, type RouteRecordRaw } from "vue-router";

const routes: RouteRecordRaw[] = [
  {
    path: "/",
    name: "overview",
    component: () => import("../pages/OverviewPage.vue"),
    meta: { title: "Overview" },
  },
  {
    path: "/packages",
    name: "packages",
    component: () => import("../pages/PackagesPage.vue"),
    meta: { title: "Packages" },
  },
  {
    path: "/runs",
    name: "runs",
    component: () => import("../pages/RunsPage.vue"),
    meta: { title: "Runs & Errors" },
  },
  {
    path: "/logs",
    name: "logs",
    component: () => import("../pages/LogsPage.vue"),
    meta: { title: "Logs" },
  },
  // Unknown route → Overview (the SPA shell is already served by M3 for any navigation path).
  { path: "/:pathMatch(.*)*", redirect: "/" },
];

export const router = createRouter({
  history: createWebHistory("/"),
  routes,
});
