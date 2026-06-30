/**
 * Bun test preload: a minimal `.vue` SFC loader so component tests can `import` an SFC and render it
 * with `vue/server-renderer` (no DOM needed). It compiles `<script setup>` + `<template>` via
 * `@vue/compiler-sfc` (inline template) and hands the result to Bun as TS (Bun strips the types).
 * Styles are irrelevant to behaviour tests and are dropped. Only used by `bun test`; the production
 * build uses `@vitejs/plugin-vue`.
 */

import { readFileSync } from "node:fs";
import { compileScript, parse } from "@vue/compiler-sfc";
import { plugin } from "bun";

plugin({
  name: "vue-sfc-loader",
  setup(build) {
    build.onLoad({ filter: /\.vue$/ }, (args) => {
      const source = readFileSync(args.path, "utf8");
      const { descriptor } = parse(source, { filename: args.path });
      const id = `sfc-${descriptor.filename}`;
      const script = compileScript(descriptor, { id, inlineTemplate: true });
      return { contents: script.content, loader: "ts" };
    });
  },
});
