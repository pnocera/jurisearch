# Spike A — Bun `--compile` asset embedding — RESULT

## VERDICT: **PASS**

A single self-contained `bun-linux-x64` binary, built with **Bun 1.3.14**, embeds the
content-hashed Vue front-end and serves the SPA on the real target **CT 111**
(`100.71.35.39`, x86_64, glibc 2.41, hostname `jurisearch-update`, **no Bun installed**) from an
**empty cwd with no filesystem `dist/`**. `/`, a deep-linked SPA route, and hashed JS/CSS assets
all return `200` with correct content-types.

**Gate decision:** M5–M7 proceed as written — **single self-contained binary**. The adjacent-asset
fallback is NOT needed.

---

## Pinned toolchain
- **Bun 1.3.14** (local `/home/pierre/.bun/bin/bun`). Pin this exact version for M5–M7.
- No `bunfig.toml` was required for the spike. (A repo `bunfig.toml` is optional; embedding works
  with default config.)

## Embedding mechanism that worked
Import each asset (and `index.html`) with an **import attribute `with { type: "file" }`**. This is
the directive that makes `bun build --compile` bundle the file's bytes into the executable. Each
import resolves **at runtime to an embedded virtual path string**, which `Bun.file(path)` reads
straight out of the binary — no filesystem `dist/` needed.

Because asset filenames are content-hashed (vary per build), the import list is **generated from
`dist/`** at build time into an `embed.ts` manifest (this is exactly what an M5 build step does):

```ts
// embed.ts — generated from dist/ contents at build time
import index_html from "./dist/index.html" with { type: "file" };
import a0 from "./dist/index-ffzy24qg.css" with { type: "file" };
import a1 from "./dist/index-tdfq8xnt.js"  with { type: "file" };

export const INDEX_HTML: string = index_html;          // runtime virtual path
export const ASSETS: Record<string, string> = {
  "/index-ffzy24qg.css": a0,
  "/index-tdfq8xnt.js":  a1,
};
```

```ts
// server.ts — Bun.serve, exact-match assets + SPA fallback
import { INDEX_HTML, ASSETS } from "./embed";
const indexFile = Bun.file(INDEX_HTML);

Bun.serve({
  port: Number(process.env.PORT ?? 18081),
  hostname: process.env.HOST ?? "127.0.0.1",   // fail-closed bind (never 0.0.0.0)
  async fetch(req) {
    const { pathname } = new URL(req.url);
    const assetPath = ASSETS[pathname];
    if (assetPath) {
      const f = Bun.file(assetPath);
      return new Response(f, { headers: { "Content-Type": f.type } }); // Bun infers CT
    }
    return new Response(indexFile, {                                    // SPA fallback
      headers: { "Content-Type": "text/html; charset=utf-8" },
    });
  },
});
```

### Build pipeline (exact invocations)
```sh
# 1. Web bundle -> content-hashed assets + index.html (Bun's own bundler; no Vite needed)
bun build ./src/index.html --outdir dist --minify
#   produces: dist/index.html, dist/index-<hash>.js, dist/index-<hash>.css
#   NOTE: do NOT pass --entry-naming; it would hash index.html itself. Default keeps index.html.

# 2. Generate embed.ts from dist/ (one import per file, with { type: "file" })

# 3. Compile the single self-contained binary
bun build server.ts --compile --target=bun-linux-x64 --outfile jurisearch-spikeA
```
Resulting binary: ELF 64-bit x86-64, dynamically linked (glibc), ~90 MB, runs on CT 111 with no
Bun present. sha256 was identical local vs. CT 111 after transfer.

## CT 111 probe results (run from empty `/tmp/spikeA-run/`, no `dist/`)
| Request | Status | Content-Type |
|---|---|---|
| `GET /` | 200 | `text/html; charset=utf-8` |
| `GET /runs/123` (deep SPA route) | 200 | `text/html; charset=utf-8` (serves index.html) |
| `GET /index-tdfq8xnt.js` (hashed) | 200 | `text/javascript;charset=utf-8` (92980 bytes) |
| `GET /index-ffzy24qg.css` (hashed) | 200 | `text/css;charset=utf-8` (180 bytes) |
| `GET /index-doesnotexist.js` (missing) | 200 | `text/html; charset=utf-8` — see gotcha |

Raw output: `probe-output.txt`.

## Gotchas M5 MUST know
1. **404 unmatched assets — do NOT fall through to index.html.** The naive SPA fallback returns
   `index.html` (HTTP 200) for *any* unmatched path, including a missing/stale hashed asset like
   `/index-doesnotexist.js`. A browser then loads HTML where it expected JS/CSS. M5 must return
   **404 for unmatched `/assets/*` (or any `*.js`/`*.css`/static-extension) paths**, and only
   fall through to `index.html` for navigation-style routes.
2. **Content-Type comes free from `Bun.file(...).type`** — correct for `.js`→`text/javascript`,
   `.css`→`text/css`, both `;charset=utf-8`. No manual MIME table needed. (For `index.html` we set
   the header explicitly because it is served as a virtual path; relying on `.type` there is also
   fine.)
3. **Asset URL layout.** Bun's HTML bundler emits assets flat at the site root with relative
   `./index-<hash>.js` refs (resolve to `/index-<hash>.js`), not under `/assets/`. The embed map is
   keyed on basenames so it's layout-agnostic; if M5 wants `/assets/*`, set `--public-path`/outdir
   accordingly and key the map to match — the embedding mechanism is unchanged.
4. **`index.html` naming.** Avoid `--entry-naming '[name]-[hash].[ext]'` — it hashes `index.html`
   itself. Default naming keeps `index.html` stable while still hashing JS/CSS.
5. **`.vue` SFCs need a plugin.** Bun's bundler does not compile `.vue` single-file components
   out of the box. The spike used `h()` render functions to stay dependency-free. M5's real app
   will use **Vite** (or a Bun Vue plugin) for the web bundle step; that's fine — only the
   `bun build --compile` step (step 3) is load-bearing for embedding, and it consumes whatever
   `dist/` the web build produces.

## Reproduction artifacts
Local scratch (not committed): `/tmp/claude-1000/.../scratchpad/spikeA/` containing
`src/`, `dist/`, `embed.ts`, `server.ts`, and the compiled `jurisearch-spikeA`.
Source is inlined above; only `RESULT.md` + `probe-output.txt` are kept in-repo.
