/**
 * The read-only `AssetSource` seam for the SPA (design §5.4). The router asks for an asset by request
 * pathname or for the SPA `index.html`; HOW those bytes are sourced is hidden behind this interface:
 *   - M3 / dev: `DevAssetSource` reads the on-disk `web/dist` (after `bun run build`).
 *   - M5: an embedded-asset source (Spike A `with { type: "file" }` map) drops in unchanged.
 *   - tests: a `FakeAssetSource` over an in-memory map.
 *
 * Spike A discipline (RESULT.md gotchas) lives in the ROUTER, not here: this seam only resolves an
 * exact asset (or null) and the index document. Content-Type comes from `Bun.file().type` for real
 * assets (no MIME table). The interface exposes NO write surface — the dashboard is a pure observer.
 */

/** A resolved asset: its body and the Content-Type to serve it with. */
export interface AssetResponse {
  body: Uint8Array;
  contentType: string;
}

export interface AssetSource {
  /** The SPA shell for navigation/deep-link fallback, or `null` if no build is present. */
  index(): Promise<AssetResponse | null>;
  /** An exact static asset by request pathname (e.g. `/index-ab12.js`), or `null` if absent. */
  asset(pathname: string): Promise<AssetResponse | null>;
}

const INDEX_CONTENT_TYPE = "text/html; charset=utf-8";

/** Reject paths that could escape the dist root (defence-in-depth; `URL` already normalises `..`). */
function isUnsafePath(pathname: string): boolean {
  return pathname.includes("..") || pathname.includes("\0");
}

/**
 * Dev/M3 asset source: serves files from an on-disk `web/dist` directory via `Bun.file`, which also
 * supplies the Content-Type (`.js`→`text/javascript`, `.css`→`text/css`, …) for free.
 */
export class DevAssetSource implements AssetSource {
  constructor(private readonly distDir: string) {}

  async index(): Promise<AssetResponse | null> {
    const file = Bun.file(`${this.distDir}/index.html`);
    if (!(await file.exists())) {
      return null;
    }
    return { body: await file.bytes(), contentType: INDEX_CONTENT_TYPE };
  }

  async asset(pathname: string): Promise<AssetResponse | null> {
    if (pathname === "/" || isUnsafePath(pathname)) {
      return null;
    }
    const file = Bun.file(`${this.distDir}${pathname}`);
    if (!(await file.exists())) {
      return null;
    }
    // Bun infers the Content-Type from the extension; fall back to octet-stream if unknown.
    return { body: await file.bytes(), contentType: file.type || "application/octet-stream" };
  }
}

/**
 * M5 embedded asset source: serves the SPA + fonts from bytes bundled INTO the compiled binary
 * (Spike A `with { type: "file" }`), so the binary runs standalone with NO on-disk `web/dist`.
 * Backed by the generated manifest (`build/gen-embed.ts`): `index()`/`asset(pathname)` look up the
 * embedded virtual-path string and read it via `Bun.file`, which supplies the Content-Type the same
 * way `DevAssetSource` does (`.js`→`text/javascript`, `.woff2`→`font/woff2`, …). `index.html` is
 * forced to `text/html` because its embedded virtual path may not carry a `.html` extension. The
 * Spike A 404 discipline stays in the ROUTER; this seam only resolves an exact asset or the index.
 * Read-only; constructed ONLY for the compiled binary at the composition root.
 */
export class EmbeddedAssetSource implements AssetSource {
  constructor(
    private readonly indexPath: string | null,
    private readonly assets: Readonly<Record<string, string>>,
  ) {}

  async index(): Promise<AssetResponse | null> {
    if (this.indexPath === null) {
      return null;
    }
    return { body: await Bun.file(this.indexPath).bytes(), contentType: INDEX_CONTENT_TYPE };
  }

  async asset(pathname: string): Promise<AssetResponse | null> {
    if (pathname === "/" || isUnsafePath(pathname)) {
      return null;
    }
    const embeddedPath = this.assets[pathname];
    if (embeddedPath === undefined) {
      return null;
    }
    const file = Bun.file(embeddedPath);
    // An exact GET /index.html keeps text/html; everything else infers from `Bun.file().type`.
    const contentType = pathname.endsWith(".html")
      ? INDEX_CONTENT_TYPE
      : file.type || "application/octet-stream";
    return { body: await file.bytes(), contentType };
  }
}
