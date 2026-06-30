/**
 * `selectAssetSource` fail-closed contract (M5 Codex WARN): embedded mode must NEVER silently fall
 * back to `DevAssetSource` on a STUB manifest (`EMBEDDED_INDEX === null`, what `gen-embed.ts` writes
 * when `web/dist` is absent). A binary compiled against such a stub has no embedded SPA and must
 * fail at startup rather than look for a filesystem `web/dist` that does not exist beside it.
 */
import { expect, test } from "bun:test";
import { selectAssetSource } from "./main.ts";
import { DevAssetSource, EmbeddedAssetSource } from "./src/http/assets.ts";

const STUB = { EMBEDDED_INDEX: null, EMBEDDED_ASSETS: {} };
const POPULATED = {
  EMBEDDED_INDEX: "/$bunfs/root/index.html",
  EMBEDDED_ASSETS: { "/index.html": "/$bunfs/root/index.html" },
};

test("embedded mode + stub manifest THROWS (no silent DevAssetSource fallback)", () => {
  expect(() => selectAssetSource(true, STUB, "/nonexistent/web/dist")).toThrow(
    /embedded build has no assets/,
  );
});

test("embedded mode + populated manifest → EmbeddedAssetSource", () => {
  const src = selectAssetSource(true, POPULATED, "/nonexistent/web/dist");
  expect(src).toBeInstanceOf(EmbeddedAssetSource);
});

test("dev mode (not embedded) → DevAssetSource, even with a stub manifest", () => {
  const src = selectAssetSource(false, STUB, "/some/web/dist");
  expect(src).toBeInstanceOf(DevAssetSource);
});
