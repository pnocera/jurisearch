/**
 * The ONE place a subprocess is spawned (design §5.1). Wraps `Bun.spawn` behind the read-only
 * `ProcessRunner` interface. It captures stdout/stderr/exit and (optionally) enforces a timeout; it
 * does NOT use a shell, so the argv is passed verbatim with no interpolation/injection surface.
 */

import type { ProcessRunner, RunOptions, RunResult } from "./types.ts";

export class ProcessAdapter implements ProcessRunner {
  async run(cmd: string[], opts?: RunOptions): Promise<RunResult> {
    if (cmd.length === 0) {
      throw new Error("ProcessAdapter.run: empty command");
    }
    const proc = Bun.spawn(cmd, {
      stdout: "pipe",
      stderr: "pipe",
      // No shell, no stdin: a pure read invocation.
      stdin: "ignore",
    });

    let timer: ReturnType<typeof setTimeout> | undefined;
    if (opts?.timeoutMs !== undefined) {
      timer = setTimeout(() => proc.kill(), opts.timeoutMs);
    }
    try {
      const [stdout, stderr, code] = await Promise.all([
        new Response(proc.stdout).text(),
        new Response(proc.stderr).text(),
        proc.exited,
      ]);
      return { stdout, stderr, code };
    } finally {
      if (timer !== undefined) {
        clearTimeout(timer);
      }
    }
  }
}
