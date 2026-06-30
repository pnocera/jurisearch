# Codex Review: Automated Single-Source Binary Versioning

## Findings

### BLOCKER: `dist.sh` can accept a mislabeled `--version` because the audit only checks substrings

Reference: `/home/pierre/Work/jurisearch/dist.sh:276`

The release audit checks that `ver_out` contains `"$VERSION ("`, `"$BUILD_COMMIT"`, and `"$TARGET"` somewhere, but it does not require the exact clap output shape or the expected binary name. This can miss real mislabels. For example, with the current `VERSION=0.1.0`, a binary that reports `jurisearch 10.1.0 (389d17e0437b, x86_64-unknown-linux-gnu)` still contains the substring `0.1.0 (` and would pass the version check. A binary can also pass with extra/misordered stamp text as long as those three substrings appear.

The fix is to compare against the exact expected line for each binary, e.g. `expected="$b $VERSION ($BUILD_COMMIT, $TARGET)"` and `[[ "$ver_out" == "$expected" ]]`, possibly after trimming a single trailing newline/CR if needed. This is in the "dist.sh audit fails to catch a mislabeled binary" class called out in the review instructions.

### BLOCKER: dev builds can keep a stale commit after a normal branch commit

Reference: `/home/pierre/Work/jurisearch/crates/jurisearch-buildinfo/src/lib.rs:64`

`stamp()` only emits `rerun-if-changed` for `.git/HEAD` and `.git/packed-refs`. In a normal checkout on `main`, `.git/HEAD` contains `ref: refs/heads/main` and does not change when a new commit is made; `.git/refs/heads/main` is the file that changes. Because the build script emits explicit rerun hints, Cargo will not rerun it for arbitrary source changes. The next build can therefore compile the binary with the previous `JURISEARCH_BUILD_COMMIT`, producing a wrong `--version` until the build script is forced to rerun.

The fix is to also watch the resolved HEAD ref path. A robust approach is to ask git for paths (`git rev-parse --git-path HEAD`, `git symbolic-ref -q HEAD`, then `git rev-parse --git-path <ref>`) and emit rerun hints for both HEAD and the active branch ref when symbolic. That also naturally handles linked worktrees better than looking only for a `.git` directory.

### WARN: `JURISEARCH_DIST_VERSION` is documented as kept, but any real override will fail the new audit

Reference: `/home/pierre/Work/jurisearch/dist.sh:62`

The script still lets `JURISEARCH_DIST_VERSION` override `$VERSION`, and the comment says this is for ad-hoc/test packaging. The binaries, however, get their version from `env!("CARGO_PKG_VERSION")`, i.e. the workspace package version, and the audit compares their output to the overridden `$VERSION`. If `JURISEARCH_DIST_VERSION` differs from the root `Cargo.toml` version, the release build should fail at the audit even though the override path is still advertised.

Either remove the dist version override to preserve the single-source contract, or make the override update the actual Cargo package version before building. Keeping an override that cannot pass the audit is likely to confuse release/test packaging.

## Verified Behavior

- `cargo check --bins` passes.
- `cargo build --bins` passes, and all five binaries print `0.1.0 (389d17e0437b-dirty, x86_64-unknown-linux-gnu)` in this dirty working tree.
- `JURISEARCH_BUILD_COMMIT=OVERRIDE123456 cargo build --bins` passes, and all five binaries print the override commit.
- `cargo build --target x86_64-unknown-linux-gnu --bins` passes, and all five target binaries print the target triple.
- `bash -n dist.sh` passes.
- `shellcheck dist.sh` passes.
- All current uses of `jurisearch_buildinfo::version!()` are in the five binary crates that have their own `build.rs` calling `jurisearch_buildinfo::stamp()`.

## Notes

- `git_dirty()` is not inverted: a clean `git status --porcelain` produces empty stdout, `git_output()` returns `None`, and `git_dirty()` returns `false`; a dirty tree returns non-empty stdout and `true`.
- The no-git/tarball path is best-effort and should degrade to `unknown`: failed `git` commands return `None`, and `stamp()` still emits both compile-time env vars.
- The `.git` file case in linked worktrees currently only skips rerun hints; commit resolution itself uses `git rev-parse` and is worktree-safe. The stale-ref blocker above is broader than worktrees and should be fixed at the same time.

VERDICT: FIXES_REQUIRED
