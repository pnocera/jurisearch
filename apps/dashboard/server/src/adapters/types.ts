/**
 * The Dependency-Inversion seam (design §5.1). Providers depend ONLY on these narrow, READ-ONLY
 * interfaces — never on Bun/`node:fs`/subprocess — so they unit-test against in-memory fakes with
 * zero real I/O. The interfaces expose NO write/delete/spawn-arbitrary-mutation method: the whole
 * dashboard is a pure observer (design §1, §5.4 "adapters expose no write methods").
 */

/** Options for a single read command. Kept minimal: only a timeout (no shell, no mutation). */
export interface RunOptions {
  /** Kill the command after this many ms (defence against a wedged subprocess). */
  timeoutMs?: number;
}

/** The captured result of running ONE fixed read command. */
export interface RunResult {
  stdout: string;
  stderr: string;
  /** Process exit code (`null` only if the process was signalled). */
  code: number | null;
}

/**
 * Runs a fixed read command and captures its output. The ONLY implementation that spawns a process
 * is `ProcessAdapter`; providers build the argv from fixed templates (`status`, `journalctl`,
 * `systemctl list-timers`). There is deliberately no `write`/`kill-others`/shell-eval surface.
 */
export interface ProcessRunner {
  run(cmd: string[], opts?: RunOptions): Promise<RunResult>;
}

/** A minimal stat result — enough to tell files from dirs and order records by mtime. */
export interface StatInfo {
  isFile: boolean;
  isDirectory: boolean;
  size: number;
  mtimeMs: number;
}

/**
 * Reads from the filesystem. The ONLY implementation that touches disk is `FileAdapter`. READ-ONLY
 * by contract — no `write`/`unlink`/`mkdir`/`rm`: the dashboard never mutates `state_dir`/`corpora_dir`.
 */
export interface FileSource {
  /** Read a file's full contents as UTF-8 text. */
  read(path: string): Promise<string>;
  /** Stat a path (throws if it does not exist). */
  stat(path: string): Promise<StatInfo>;
  /** List the entry names (not full paths) in a directory (throws if it does not exist). */
  list(dir: string): Promise<string[]>;
}
