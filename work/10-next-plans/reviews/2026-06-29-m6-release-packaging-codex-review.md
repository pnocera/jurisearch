# Codex Review: M6 release packaging

## Findings

### BLOCKER: Missing target silently produces mislabeled artifacts

`dist.sh` is required to produce the first release format for `x86_64-unknown-linux-gnu`, and the review brief explicitly asks for a clean failure when the required target is missing. Instead, lines 169-180 fall back to a host build when `rustup target list --installed` does not contain `x86_64-unknown-linux-gnu`, while the tarball names and manifest still say `x86_64-unknown-linux-gnu` at lines 322-324 and 330-336. On a non-`x86_64-unknown-linux-gnu` host this ships the wrong binaries under the required target label; even on a compatible host it violates the expected clean-failure contract.

Fix: make the target mandatory. Fail with an actionable error if the target is not installed, remove `USING_TARGET`/host fallback, and always run `cargo build --release --target "$TARGET" ...`.

### BLOCKER: Cargo can write outside the repository before the script fails

The script derives `BIN_SRC` as `$REPO_ROOT/target/...` at lines 173 and 179, but the `cargo build` invocations at lines 172 and 178 do not pin `--target-dir "$REPO_ROOT/target"` or sanitize `CARGO_TARGET_DIR`. If a caller has `CARGO_TARGET_DIR=/tmp/jurisearch-target` or another absolute path in the environment, Cargo writes build outputs outside the repository before this script looks in `$REPO_ROOT/target` and fails to find the binaries. That violates the "writes nothing outside the repo except repo-local target/dist scaffolding" constraint.

Fix: call Cargo with an explicit `--target-dir "$REPO_ROOT/target"` or export `CARGO_TARGET_DIR="$REPO_ROOT/target"` after validating that `$REPO_ROOT/target` is not a symlink escaping the repo.

### BLOCKER: The audit permits arbitrary `.tar.zst` payload archives inside bundles

The forbidden archive list intentionally omits `*.tar.zst` at lines 65-72 so the generated release archives can exist beside each bundle. However, the audit is run before `make_tarball` at lines 279-281, 300-302, and 313-315, and `make_tarball` includes the bundle's `bin`, `config`, `systemd`, `completions`, and `SHA256SUMS` members at lines 226-232. A forbidden legal/corpus archive such as `config/corpus.tar.zst` would pass the audit and be included in the release tarball because the rule allows every `.tar.zst`, not just the generated role archive.

Fix: forbid `*.tar.zst` in payload directories during the pre-tar audit, or allow only the exact generated top-level release tarball basename during the final post-tar audit. The audit should distinguish release artifacts from payload files by path, not by blanket extension allowance.

### BLOCKER: SQLite WAL/SHM database sidecars are not detected

The database/WAL rule at lines 66-69 catches `*.wal`, but SQLite sidecars are commonly named `name.sqlite-wal`, `name.sqlite-shm`, `name.db-wal`, and `name.db-shm`. Those basenames do not match `*.wal`, `*.sqlite`, `*.sqlite3`, or `*.db`, so a live database sidecar can slip into a bundle without failing the audit.

Fix: add database sidecar globs such as `*.sqlite-wal`, `*.sqlite-shm`, `*.sqlite3-wal`, `*.sqlite3-shm`, `*.db-wal`, `*.db-shm`, `*-wal`, and `*-shm`, or use a stricter database filename classifier for SQLite sidecars.

### BLOCKER: Role-leak audit only checks files directly under `bin/`

The role-distinctness audit at lines 106-122 only scans `find "$dir/bin" -maxdepth 1 -type f`. A foreign role binary named `jurisearch-producer`, `jurisearch`, `jurisearch-syncd`, `jurisearchctl`, or `jurisearch-client` placed anywhere else under a payload member, for example `config/jurisearch-producer` or `systemd/helpers/jurisearch-client`, would be tarred by `make_tarball` but would not be reported as a role leak. The review brief treats a role leak the audit would not catch as a blocker.

Fix: scan every regular file under the bundle for basenames matching known role binaries, then allow only the expected paths for that role, e.g. `bin/<allowed-name>`.

### BLOCKER: Credential audit misses common secret filenames

The credential rules at lines 79-80 catch some extensions and exact names, but they miss common credential files such as `.env.local`, `.env.production`, `.pgpass`, `.netrc`, `credentials`, `credentials.json`, `client_secret.json`, `service-account.json`, and token files such as `*.token`. Because the non-negotiable constraint says credentials must be excluded and the audit must fail if they are present, those are false negatives.

Fix: extend the credential denylist to cover common basename and extension patterns, especially `.env.*`, `.pgpass`, `.netrc`, `credentials*`, `*credential*`, `*secret*`, `client_secret*.json`, `service-account*.json`, and `*.token`. Consider matching paths as well as basenames if credentials might be nested in conventional directories.

### WARN: `target/` may be a symlink that moves scratch outside the repo

The scratch directory is created with `mktemp -d "$REPO_ROOT/target/dist-scratch.XXXXXX"` at lines 196-198. If `$REPO_ROOT/target` already exists as a symlink to another filesystem location, `mkdir -p "$REPO_ROOT/target"` succeeds and `mktemp` creates the scratch directory outside the repository. That is outside the stated "scratch under gitignored `./target`" boundary even though cleanup later removes it.

Fix: before using `target`, verify it is either absent or a real directory under the repository, not a symlink. Alternatively create a dedicated repo-local scratch directory after resolving and validating its physical path.

## Notes

The script does correctly derive `DIST_DIR` from the script location rather than `$PWD`, refuses an empty or `/` repo root, refuses absolute `/dist`, and removes an existing `dist` symlink instead of following it because `rm -rf "$DIST_DIR"` is invoked without a trailing slash. The role bundle copy lists are otherwise role-distinct in the current assembly code: `update-server` copies only `jurisearch-producer`, `site-server` copies only `jurisearch`, `jurisearch-syncd`, and `jurisearchctl`, and `cli` copies only `jurisearch-client`.

I did not run `./dist.sh` because the review request asked me not to modify any files other than this review, and a full run would create or replace repo-local `dist/` and build outputs under `target/`.

VERDICT: FIXES_REQUIRED
