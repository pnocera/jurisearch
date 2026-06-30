/**
 * The ONE place the filesystem is touched (design §5.1). Wraps `node:fs/promises` behind the
 * read-only `FileSource` interface — only `readFile`/`stat`/`readdir`, never a write/unlink/mkdir.
 */

import { readdir, readFile, stat } from "node:fs/promises";
import type { FileSource, StatInfo } from "./types.ts";

export class FileAdapter implements FileSource {
  read(path: string): Promise<string> {
    return readFile(path, "utf8");
  }

  async stat(path: string): Promise<StatInfo> {
    const s = await stat(path);
    return {
      isFile: s.isFile(),
      isDirectory: s.isDirectory(),
      size: s.size,
      mtimeMs: s.mtimeMs,
    };
  }

  list(dir: string): Promise<string[]> {
    return readdir(dir);
  }
}
