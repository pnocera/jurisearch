/** HTTP — the one `Bun.serve` router + the read-only asset/bind seams (design §5.4). */
export { type AssetResponse, type AssetSource, DevAssetSource } from "./assets.ts";
export { assertExplicitBind, BindGuardError } from "./bind.ts";
export {
  createFetchHandler,
  type FetchHandler,
  type RouterDeps,
  startServer,
} from "./router.ts";
