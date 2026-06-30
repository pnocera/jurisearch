/** Adapters — the read-only I/O seam (design §5.1). */
export { FileAdapter } from "./file.ts";
export { ProcessAdapter } from "./process.ts";
export type {
  FileSource,
  ProcessRunner,
  RunOptions,
  RunResult,
  StatInfo,
} from "./types.ts";
