# Codex Re-Review: Automated Single-Source Binary Versioning (r2)

## Findings

No BLOCKER/WARN/NIT findings.

## Verification

Prior BLOCKER resolved: the `dist.sh` release audit now compares each binary's complete `--version` line against the exact expected string, including the binary name, workspace version, pinned short commit, and target triple. The relevant check is at `/home/pierre/Work/jurisearch/dist.sh:283` through `/home/pierre/Work/jurisearch/dist.sh:290`, where `expected="$b $VERSION ($BUILD_COMMIT, $TARGET)"` is compared with `[[ "$ver_out" != "$expected" ]]`. That rejects the previous mislabeled-binary case because substring matches such as `10.1.0` containing `0.1.0 (` no longer pass.

Prior BLOCKER resolved: `stamp()` now emits `rerun-if-changed` for the active branch ref as well as HEAD and packed refs. The implementation asks git for `HEAD`, `symbolic-ref -q HEAD`, the branch ref's `--git-path`, and `packed-refs` at `/home/pierre/Work/jurisearch/crates/jurisearch-buildinfo/src/lib.rs:76` through `/home/pierre/Work/jurisearch/crates/jurisearch-buildinfo/src/lib.rs:92`. The build-script output for the reviewed branch includes `cargo:rerun-if-changed=../../.git/refs/heads/main`, so a normal branch commit moves a watched path and reruns the stamp.

Prior WARN resolved: the `JURISEARCH_DIST_VERSION` override is gone. `dist.sh` derives `VERSION` only from the root workspace package version at `/home/pierre/Work/jurisearch/dist.sh:62` through `/home/pierre/Work/jurisearch/dist.sh:69`, and all changed crates inherit `version.workspace = true`.

Release stamping is coherent: `dist.sh` exports `JURISEARCH_BUILD_COMMIT="$BUILD_COMMIT"` before the release build at `/home/pierre/Work/jurisearch/dist.sh:257` through `/home/pierre/Work/jurisearch/dist.sh:265`, and `jurisearch-buildinfo::stamp()` gives that non-empty override precedence at `/home/pierre/Work/jurisearch/crates/jurisearch-buildinfo/src/lib.rs:97` through `/home/pierre/Work/jurisearch/crates/jurisearch-buildinfo/src/lib.rs:103`. I verified the override path by building with `JURISEARCH_BUILD_COMMIT=OVERRIDE123456`; all five role binaries reported the override in `--version`.

All five release-facing binaries are wired through the stamping macro and a local build script: `jurisearch`, `jurisearch-client`, `jurisearch-producer`, `jurisearch-syncd`, and `jurisearchctl` report `0.1.0 (389d17e0437b-dirty, x86_64-unknown-linux-gnu)` in the current dirty dev tree, with the expected binary-name prefix on each line.

Checks run:

- `cargo check --bins`
- `cargo build --bins`
- `JURISEARCH_BUILD_COMMIT=OVERRIDE123456 cargo build --bins`
- `bash -n dist.sh`
- `shellcheck dist.sh`
- `git diff --check`

I did not run the full `dist.sh` packaging flow because that would rewrite the repository's `dist/` artifacts; the release-script logic relevant to this review was checked by syntax/lint inspection and targeted build/version verification.

VERDICT: GO
