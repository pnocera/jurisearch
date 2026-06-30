/**
 * PackagesProvider — the served `core/manifest.json` (a `Signed<RemoteManifest>` = `{payload,
 * signature}`) → `PackageManifestDTO`. The shared `parsePackage` unwraps `payload`, tolerates an
 * EMPTY `payload.packages` (Spike B §6: zero increments published yet) and ignores forward-compat
 * fields. The signature is carried verbatim (the dashboard does not re-verify it — design §4).
 */

import { type PackageManifestDTO, safeParsePackage } from "@jurisearch-dashboard/shared";
import type { FileSource } from "../adapters/types.ts";
import type { DashboardConfig } from "../config.ts";
import { type DataProvider, ProviderError, parseJson, unwrap } from "./types.ts";

const SOURCE = "packages";
/** The served manifest's path under the packages dir (Spike B: `<corporaDir>/core/manifest.json`). */
const MANIFEST_RELATIVE = "core/manifest.json";

export function manifestPath(config: DashboardConfig): string {
  return `${config.corporaDir}/${MANIFEST_RELATIVE}`;
}

/** PURE: manifest file text → validated `PackageManifestDTO`. Typed `ProviderError` on bad input. */
export function parseManifestText(text: string): PackageManifestDTO {
  return unwrap(SOURCE, safeParsePackage(parseJson(SOURCE, text)));
}

export class PackagesProvider implements DataProvider<PackageManifestDTO> {
  constructor(
    private readonly files: FileSource,
    private readonly config: DashboardConfig,
  ) {}

  async get(): Promise<PackageManifestDTO> {
    let text: string;
    try {
      text = await this.files.read(manifestPath(this.config));
    } catch (error) {
      throw new ProviderError(SOURCE, `${SOURCE}: cannot read ${manifestPath(this.config)}`, {
        cause: error,
      });
    }
    return parseManifestText(text);
  }
}
