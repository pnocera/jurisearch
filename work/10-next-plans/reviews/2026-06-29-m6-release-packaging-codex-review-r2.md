# Review: M6 dist.sh release packaging (r2)

## Findings

No BLOCKER/WARN/NIT findings.

## Verification Notes

Reviewed `git diff main -- dist.sh`; `dist.sh` is a new release-packaging script.

The prior blockers appear addressed:

- Target is mandatory: the script always builds with `--release --target x86_64-unknown-linux-gnu --target-dir "$REPO_ROOT/target"` and exits 5 before build if the rustup target is not installed (`dist.sh:225`-`dist.sh:238`). I found no host-build fallback path.
- Cargo output is pinned repo-local: `CARGO_TARGET_DIR` is overwritten, `--target-dir` is passed explicitly, and `BIN_SRC` is derived from that validated directory (`dist.sh:209`-`dist.sh:238`).
- `target/` escape is guarded: symlink/non-directory targets are refused, the directory is created only after those checks, and `pwd -P` must resolve to `$REAL_REPO/target` before build or scratch use (`dist.sh:209`-`dist.sh:223`, `dist.sh:252`-`dist.sh:255`).
- Archive allowance is path-based: `*.tar.zst` is still forbidden by basename, with the only exception being the exact generated top-level role tarball path (`dist.sh:69`-`dist.sh:87`, `dist.sh:105`-`dist.sh:120`, `dist.sh:156`-`dist.sh:164`). Pre-tar audits pass an empty release tarball name, so nested or foreign archives cannot enter the tarball (`dist.sh:336`-`dist.sh:371`).
- SQLite WAL/SHM sidecars, credentials, model/tokenizer artifacts, runtime package/manifests, vector indexes, and archives are covered by the denylist used against every regular file recursively (`dist.sh:69`-`dist.sh:120`).
- Role-leak detection now scans every regular file under each bundle and only permits a known role binary at its exact expected `bin/<name>` path (`dist.sh:122`-`dist.sh:145`).
- Positive release invariants remain present: repo-root is derived from the script location, `/dist` is refused, role copy lists are distinct, `dist/` is recreated under the repo root, and upgrade/rollback are documented as not implemented rather than silently implied (`dist.sh:32`-`dist.sh:48`, `dist.sh:56`-`dist.sh:59`, `dist.sh:249`-`dist.sh:250`, `dist.sh:523`-`dist.sh:529`).

Commands run:

- `bash -n dist.sh`
- `shellcheck dist.sh`
- `git diff --check main -- dist.sh`
- `rustup target list --installed | rg '^x86_64-unknown-linux-gnu$'`
- `./dist.sh --audit-only` against temporary fixtures for nested `corpus.tar.zst`, SQLite `data.sqlite-wal`/`data.db-shm`, nested foreign role binary, `.pgpass`, and `client_secret.json`; all failed as expected.
- `./dist.sh --audit-only` against a temporary `cli` bundle containing only `bin/jurisearch-client` plus the exact top-level generated CLI tarball basename; it passed as expected.
- `./dist.sh --audit-only` against a temporary `cli` bundle containing both the exact top-level generated CLI tarball basename and a nested same-named tarball; it failed as expected.

I did not run full `./dist.sh` because the review request also said not to modify any other files, and a full run would recreate repo-local build/dist outputs. The static review plus `--audit-only` fixtures covered the r2 risk areas from the prior review.

VERDICT: GO
