#!/usr/bin/env bash
# dist.sh — JuriSearch release packaging (work/10 M6).
#
# Builds role-distinct release bundles under the REPOSITORY-LOCAL ./dist/ directory:
#   - update-server : jurisearch-producer + producer config example + systemd service/timer templates
#   - site-server   : jurisearch, jurisearch-syncd, jurisearchctl + site systemd templates + site.toml example
#   - cli           : jurisearch-client (thin client)
# Plus ./dist/manifest.toml, ./dist/README.md, per-bundle SHA256SUMS, and a .tar.zst per bundle.
#
# Hard rules enforced by this script (see the macro plan, M6):
#   * Writes ONLY to "$REPO_ROOT/dist". Never to the absolute filesystem path /dist.
#   * Bundles are role-distinct: no role's binaries leak into another bundle.
#   * Bundles EXCLUDE huge/runtime assets (databases, vector indexes, legal archives, runtime corpus
#     packages/manifests, model weights, tokenizer files, credentials). A bundle audit FAILS the build
#     if any such asset is present.
#   * First release format: Linux x86_64-unknown-linux-gnu .tar.zst. Debian packages are deferred.
#
# Usage:
#   ./dist.sh                 Build all bundles into ./dist/ (recreated fresh each run).
#   ./dist.sh --audit-only D  Run ONLY the forbidden-asset/role audit against directory D, then exit.
#                             (Used to prove the audit bites; exits non-zero on any violation.)
#
# Upgrade/rollback: NOT implemented in this release. `jurisearchctl site upgrade|rollback` are the
# planned operator surface (plan 01 Phase 9); until they land this script ships binaries + templates
# only. See the README "Upgrade / rollback" section, which documents the explicit not-implemented stub.

set -euo pipefail

# ---------------------------------------------------------------------------------------------------
# Paths & guards: derive REPO_ROOT from this script's own location; refuse to ever touch absolute /dist.
# ---------------------------------------------------------------------------------------------------
SCRIPT_PATH="$(cd "$(dirname "${BASH_SOURCE[0]}")" >/dev/null 2>&1 && pwd)"
REPO_ROOT="$SCRIPT_PATH"
DIST_DIR="$REPO_ROOT/dist"

# The whole point of M6: outputs are repository-local. Refuse anything that could write to /dist.
case "$REPO_ROOT" in
  ""|"/") echo "dist.sh: refusing to operate with REPO_ROOT='$REPO_ROOT'" >&2; exit 2;;
esac
case "$DIST_DIR" in
  "/dist"|"/dist/") echo "dist.sh: refusing to operate on absolute /dist" >&2; exit 2;;
esac
if [ "$DIST_DIR" != "$REPO_ROOT/dist" ]; then
  echo "dist.sh: DIST_DIR ('$DIST_DIR') must be exactly \$REPO_ROOT/dist" >&2; exit 2
fi
if [ ! -f "$REPO_ROOT/Cargo.toml" ] || [ ! -f "$REPO_ROOT/dist.sh" ]; then
  echo "dist.sh: REPO_ROOT ('$REPO_ROOT') does not look like the jurisearch repo root" >&2; exit 2
fi

# ---------------------------------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------------------------------
TARGET="x86_64-unknown-linux-gnu"

# Single source of truth for the version: the root Cargo.toml `[workspace.package]` `version = "..."`,
# which every crate inherits via `version.workspace = true`. (The only line-anchored `version = "` in the
# root manifest is the workspace one — workspace.dependencies pin versions inline as `crate = { version ...`.)
# This is NOT overridable: the binaries derive their `--version` from `env!("CARGO_PKG_VERSION")` (the same
# workspace version), and the post-build audit below compares their `--version` to $VERSION. An override
# that differed from the manifest could never pass that audit, so we single-source it here to keep
# `--version`, the manifest, and the audit consistent.
workspace_version() {
  grep -m1 -E '^version = "' "$REPO_ROOT/Cargo.toml" | sed -E 's/.*"([^"]+)".*/\1/'
}
VERSION="$(workspace_version)"
if [ -z "$VERSION" ]; then
  echo "dist.sh: could not derive workspace version from $REPO_ROOT/Cargo.toml [workspace.package]" >&2
  exit 2
fi

# Binaries that belong in each role bundle (bin name -> appears under <bundle>/bin/).
# NOTE: UPDATE_SERVER_BINS / SITE_SERVER_BINS / CLI_BINS list the CARGO binaries only — the ones built by
# `cargo build --bin` and copied via copy_bin from $BIN_SRC. The update-server bundle ALSO ships the
# dashboard, which is NOT a Cargo bin (it is built by Bun; see below), so its bundle membership is tracked
# separately by UPDATE_SERVER_BUNDLE_BINS to keep "cargo build set" and "bundle membership" cleanly apart.
UPDATE_SERVER_BINS=(jurisearch-producer)
SITE_SERVER_BINS=(jurisearch jurisearch-syncd jurisearchctl)
CLI_BINS=(jurisearch-client)

# The dashboard is a Bun-compiled, self-contained binary (apps/dashboard → `bun run compile`), NOT a Cargo
# bin. It is a legitimate member of the update-server bundle and must be accepted by the role audit and
# listed in the manifest, but it must NEVER be added to the cargo `--bin` build set (BUILD_BINS).
DASHBOARD_BIN="jurisearch-dashboard"
# Full set of binaries that legitimately live in the update-server bundle (Cargo producer + Bun dashboard).
# Used for role-distinctness (ALL_BINS), the audit allowed-list, and the manifest `binaries` — NOT for the
# cargo build/copy loops, which iterate UPDATE_SERVER_BINS.
UPDATE_SERVER_BUNDLE_BINS=("${UPDATE_SERVER_BINS[@]}" "$DASHBOARD_BIN")

# ---------------------------------------------------------------------------------------------------
# Forbidden-asset audit
# ---------------------------------------------------------------------------------------------------
# Filename globs that must NEVER appear in any bundle. Matched against every file's basename, recursively.
# NOTE on archives: ALL archive payloads are forbidden inside a bundle, INCLUDING *.tar.zst. The ONLY
# permitted *.tar.zst is the single generated top-level release tarball for the bundle's own role, and it
# is allowed by PATH (bundle top level + exact basename), never by a blanket extension allowance. See the
# release-tarball exception in audit_dir().
FORBIDDEN_GLOBS=(
  # databases / db data (incl. SQLite WAL/SHM sidecars under their many naming conventions)
  '*.sqlite' '*.sqlite3' '*.db' '*.mdb' '*.wal'
  '*.sqlite-wal' '*.sqlite-shm' '*.sqlite3-wal' '*.sqlite3-shm' '*.db-wal' '*.db-shm' '*-wal' '*-shm'
  # vector index files
  '*.faiss' '*.hnsw' '*.usearch' '*.ann' '*.index' '*.ivf' '*.vec'
  # legal source archives / corpus archives / any compressed payload archive
  '*.tar.gz' '*.tgz' '*.tar.xz' '*.tar.bz2' '*.zip' '*.tar.zst' '*.tzst' '*.tar'
  # runtime corpus packages / manifests produced at runtime
  'manifest.json' '*.jzpkg' '*.pkg' 'corpus-state.json'
  # model weights
  '*.gguf' '*.bin' '*.safetensors' '*.pt' '*.pth' '*.onnx' '*.ggml'
  # tokenizer files
  'tokenizer*.json' 'tokenizer.model' 'vocab.txt' 'merges.txt'
  # credentials / secrets
  '*.pem' '*.key' '*.secret' '*.seed' '.env' '*.env' '.env.*' 'id_rsa' 'id_ed25519' '*.p12' '*.pfx' '*.crt'
  '.pgpass' '.netrc' 'credentials' 'credentials*' '*credential*' '*secret*'
  'client_secret*.json' 'service-account*.json' '*.token'
)

# Every bin name across all roles — used to assert role-distinctness (no foreign binary in a bundle).
# Uses the update-server BUNDLE bins (incl. the Bun dashboard) so the dashboard is treated as a KNOWN
# role binary: allowed only at update-server/bin/, and flagged as a leak if it appears in any other bundle.
ALL_BINS=("${UPDATE_SERVER_BUNDLE_BINS[@]}" "${SITE_SERVER_BINS[@]}" "${CLI_BINS[@]}")

# audit_dir <dir> <release_tarball_basename> <space-separated allowed bin names>
# Fails (returns non-zero) on any forbidden asset, any role-binary leak anywhere in the bundle, or any
# foreign binary under <dir>/bin/.
#   <release_tarball_basename>: the ONE generated *.tar.zst permitted at the bundle TOP LEVEL only (the
#   role's own release archive). Pass "" if no release tarball is expected (e.g. pre-tar audits or
#   --audit-only of a directory that should contain no archive at all).
audit_dir() {
  local dir="$1"; shift
  local release_tarball="$1"; shift
  local allowed=" $* "
  local violations=0
  local known=" ${ALL_BINS[*]} "

  # 1) Forbidden asset classes anywhere under the bundle. The sole archive exception is the role's own
  #    generated release tarball, allowed ONLY at "$dir/<release_tarball>" (bundle top level), never nested.
  local f base g
  while IFS= read -r -d '' f; do
    base="$(basename "$f")"
    if [ -n "$release_tarball" ] && [ "$base" = "$release_tarball" ] && [ "$f" = "$dir/$release_tarball" ]; then
      continue  # the permitted top-level release artifact for this role — by path, not by extension
    fi
    for g in "${FORBIDDEN_GLOBS[@]}"; do
      # shellcheck disable=SC2053  # glob match against basename is intentional
      if [[ "$base" == $g ]]; then
        echo "AUDIT FAIL: forbidden asset '$base' (matched '$g') in $dir -> $f" >&2
        violations=$((violations + 1))
      fi
    done
  done < <(find "$dir" -type f -print0)

  # 2) Role-leak: scan EVERY regular file in the bundle. A known role binary's basename is permitted ONLY
  #    at this role's expected "$dir/bin/<allowed-name>"; anywhere else (or any other role's binary) is a leak.
  local name
  while IFS= read -r -d '' f; do
    name="$(basename "$f")"
    if [[ "$known" == *" $name "* ]]; then
      if [[ "$allowed" == *" $name "* ]] && [ "$f" = "$dir/bin/$name" ]; then
        continue  # expected role binary at its expected path
      fi
      echo "AUDIT FAIL: role leak — role binary '$name' at '$f' (allowed only at $dir/bin/<{$*}>)" >&2
      violations=$((violations + 1))
    fi
  done < <(find "$dir" -type f -print0)

  # 3) Any other (non role-binary) executable sitting directly in this bundle's bin/ is unexpected.
  if [ -d "$dir/bin" ]; then
    local b
    while IFS= read -r -d '' b; do
      name="$(basename "$b")"
      if [[ "$allowed" != *" $name "* ]] && [[ "$known" != *" $name "* ]]; then
        echo "AUDIT FAIL: unexpected binary '$name' in $dir/bin (allowed: $*)" >&2
        violations=$((violations + 1))
      fi
    done < <(find "$dir/bin" -maxdepth 1 -type f -print0)
  fi

  if [ "$violations" -ne 0 ]; then
    echo "AUDIT FAIL: $violations violation(s) in $dir" >&2
    return 1
  fi
  echo "audit OK: $dir"
  return 0
}

# Compute the generated release-tarball basename for a role bundle (the only *.tar.zst the audit permits
# at that bundle's top level).
release_tarball_for_role() {  # release_tarball_for_role <role>
  case "$1" in
    update-server) echo "jurisearch-update-server-$VERSION-$TARGET.tar.zst";;
    site-server)   echo "jurisearch-site-server-$VERSION-$TARGET.tar.zst";;
    cli)           echo "jurisearch-cli-$VERSION-$TARGET.tar.zst";;
    *)             echo "";;
  esac
}

# --audit-only mode: run the audit against a directory and exit (no build). Used for the negative test.
if [ "${1:-}" = "--audit-only" ]; then
  target_dir="${2:-}"
  if [ -z "$target_dir" ] || [ ! -d "$target_dir" ]; then
    echo "dist.sh --audit-only: need an existing directory" >&2; exit 2
  fi
  # Best-effort allowed-bin inference from the bundle's leaf name; default to all roles' bins.
  role="$(basename "$target_dir")"
  rt="$(release_tarball_for_role "$role")"
  case "$role" in
    update-server) audit_dir "$target_dir" "$rt" "${UPDATE_SERVER_BUNDLE_BINS[@]}";;
    site-server)   audit_dir "$target_dir" "$rt" "${SITE_SERVER_BINS[@]}";;
    cli)           audit_dir "$target_dir" "$rt" "${CLI_BINS[@]}";;
    *)             audit_dir "$target_dir" "" "${ALL_BINS[@]}";;
  esac
  exit $?
fi

# ---------------------------------------------------------------------------------------------------
# Tool checks
# ---------------------------------------------------------------------------------------------------
require_tool() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "dist.sh: required tool '$1' not found on PATH" >&2; exit 3
  fi
}
require_tool cargo
require_tool tar
require_tool zstd
require_tool sha256sum
require_tool python3

# ---------------------------------------------------------------------------------------------------
# Build the release binaries (only the bins each bundle needs).
# ---------------------------------------------------------------------------------------------------
BUILD_BINS=("${UPDATE_SERVER_BINS[@]}" "${SITE_SERVER_BINS[@]}" "${CLI_BINS[@]}")
BIN_ARGS=()
for b in "${BUILD_BINS[@]}"; do BIN_ARGS+=(--bin "$b"); done

# WARN 7 / BLOCKER 2 guard: the build (and later scratch) writes under "$REPO_ROOT/target". Ensure that
# path is absent or a REAL directory physically under the repo — never a symlink that would move build
# outputs (and scratch) outside the repository. Establishes the invariant for the pinned --target-dir below.
TARGET_DIR="$REPO_ROOT/target"
if [ -L "$TARGET_DIR" ]; then
  echo "dist.sh: '$TARGET_DIR' is a symlink; refusing (build outputs could escape the repo). Remove it." >&2
  exit 2
fi
if [ -e "$TARGET_DIR" ] && [ ! -d "$TARGET_DIR" ]; then
  echo "dist.sh: '$TARGET_DIR' exists but is not a directory" >&2; exit 2
fi
mkdir -p "$TARGET_DIR"
REAL_REPO="$(cd "$REPO_ROOT" && pwd -P)"
REAL_TARGET="$(cd "$TARGET_DIR" && pwd -P)"
if [ "$REAL_TARGET" != "$REAL_REPO/target" ]; then
  echo "dist.sh: target dir resolves to '$REAL_TARGET', outside repo-local '$REAL_REPO/target'; refusing." >&2
  exit 2
fi

# BLOCKER 1: the release target is MANDATORY — no silent host fallback (which would mislabel artifacts).
if ! rustup target list --installed 2>/dev/null | grep -qx "$TARGET"; then
  echo "dist.sh: required rustup target '$TARGET' is not installed." >&2
  echo "dist.sh: install it and re-run:  rustup target add $TARGET" >&2
  exit 5
fi

# Bun preflight: the dashboard binary is built by Bun (`bun run compile`), not Cargo. Mirror the rustup
# preflight — require Bun up front and ANNOUNCE the pinned toolchain (apps/dashboard/bunfig.toml +
# package.json `packageManager` → bun@1.3.14), so a release fails clearly HERE rather than midway through
# packaging. The hard release gate remains the exact `--version` audit below; a version drift only WARNs.
EXPECTED_BUN="1.3.14"
if ! command -v bun >/dev/null 2>&1; then
  echo "dist.sh: required tool 'bun' is not installed (needed to build $DASHBOARD_BIN; pin: bun@$EXPECTED_BUN)." >&2
  echo "dist.sh: install Bun $EXPECTED_BUN and re-run (see apps/dashboard/bunfig.toml)." >&2
  exit 5
fi
BUN_VERSION="$(bun --version 2>/dev/null || true)"
if [ "$BUN_VERSION" != "$EXPECTED_BUN" ]; then
  echo "dist.sh: WARNING: bun '$BUN_VERSION' detected, expected pinned '$EXPECTED_BUN' (apps/dashboard pin)." >&2
else
  echo "dist.sh: bun $BUN_VERSION (pinned) OK."
fi

# BLOCKER 2: pin Cargo's output dir to the repo-local (symlink-validated) target/, so an inherited
# CARGO_TARGET_DIR=/abs/path cannot push build outputs outside the repo. Both the explicit flag and the
# exported env point at the same validated directory; BIN_SRC is derived from it.
export CARGO_TARGET_DIR="$TARGET_DIR"

# Stamp a DETERMINISTIC build commit into every binary's --version. jurisearch-buildinfo's build.rs reads
# this override (its precedence: non-empty JURISEARCH_BUILD_COMMIT → git → "unknown"), so a release build
# pins the same 12-char short commit the --version audit asserts below — regardless of working-tree state.
# Falls back to a short of any visible full commit, else "unknown" for a no-git build. The manifest's full
# [release] git_commit (GIT_COMMIT, below) stays the un-shortened value.
BUILD_COMMIT="$(git -C "$REPO_ROOT" rev-parse --short=12 HEAD 2>/dev/null || true)"
if [ -z "$BUILD_COMMIT" ]; then
  full_commit="$(git -C "$REPO_ROOT" rev-parse HEAD 2>/dev/null || echo unknown)"
  if [ "$full_commit" = "unknown" ]; then BUILD_COMMIT="unknown"; else BUILD_COMMIT="${full_commit:0:12}"; fi
fi
export JURISEARCH_BUILD_COMMIT="$BUILD_COMMIT"

echo "dist.sh: building release for target $TARGET (target-dir $TARGET_DIR, commit $BUILD_COMMIT) ..."
cargo build --release --target "$TARGET" --target-dir "$TARGET_DIR" "${BIN_ARGS[@]}"
BIN_SRC="$TARGET_DIR/$TARGET/release"

for b in "${BUILD_BINS[@]}"; do
  if [ ! -x "$BIN_SRC/$b" ]; then
    echo "dist.sh: expected built binary missing: $BIN_SRC/$b" >&2; exit 4
  fi
done

# Version-stamp audit: every role binary must answer --version with the single workspace $VERSION, the
# pinned short commit, and this release TARGET. clap prints --version and exits BEFORE any app logic, and
# these are native x86_64-linux binaries on an x86_64-linux host, so they run here. A mismatch (a binary
# built without the buildinfo wiring, or a stale/relabelled artifact) FAILS the release.
echo "dist.sh: verifying binary --version stamps ($VERSION ($BUILD_COMMIT, $TARGET)) ..."
for b in "${BUILD_BINS[@]}"; do
  # EXACT-match the whole clap line — a substring check would pass mislabels (e.g. `10.1.0` contains
  # `0.1.0 (`). clap prints `<binary-name> <version-string>`, and each binary's `--version` prefix is its
  # own filename `$b` (verified for all five), so the full expected line is deterministic.
  expected="$b $VERSION ($BUILD_COMMIT, $TARGET)"
  ver_out="$("$BIN_SRC/$b" --version 2>/dev/null || true)"
  ver_out="${ver_out%$'\r'}"   # tolerate a stray trailing CR
  if [[ "$ver_out" != "$expected" ]]; then
    echo "dist.sh: --version stamp mismatch for '$b'" >&2
    echo "  expected exactly: '$expected'" >&2
    echo "  got:              '$ver_out'" >&2
    exit 6
  fi
  echo "  ok: $b -> $ver_out"
done

# ---------------------------------------------------------------------------------------------------
# Build the dashboard (NON-Cargo: Bun-compiled self-contained binary) and version-audit it identically.
# ---------------------------------------------------------------------------------------------------
# The dashboard is not a Cargo bin, so it is deliberately NOT in BUILD_BINS / the cargo `--bin` loop above.
# It is compiled by Bun with the SAME exported JURISEARCH_BUILD_COMMIT stamp (its build.rs-equivalent stamp
# step reads that override), so its clap-style `--version` line resolves to the exact same release format
# `<bin> $VERSION ($BUILD_COMMIT, $TARGET)` and is held to the SAME exact-match gate below.
#
# WRITE SET: `bun run compile` writes more than dist/jurisearch-dashboard. It also (re)generates, in-tree,
# the gitignored build artifacts apps/dashboard/server/buildinfo.ts (the --version stamp),
# apps/dashboard/server/embedded-assets.generated.ts (the embed manifest), and apps/dashboard/web/dist/
# (the Vite SPA build). All are repo-local and gitignored — they are NOT placed in any bundle and do not
# affect the bundle/checksum/--version audits (only dist/jurisearch-dashboard is installed below). These
# are intentionally NOT redirected under $TARGET_DIR: the Bun toolchain expects them at their in-tree paths.
DASHBOARD_DIR="$REPO_ROOT/apps/dashboard"
DASHBOARD_BUILT="$DASHBOARD_DIR/dist/$DASHBOARD_BIN"
echo "dist.sh: building $DASHBOARD_BIN via Bun (bun run compile, commit $BUILD_COMMIT) ..."
( cd "$DASHBOARD_DIR" && bun run compile )
if [ ! -x "$DASHBOARD_BUILT" ]; then
  echo "dist.sh: expected Bun-built dashboard missing: $DASHBOARD_BUILT" >&2; exit 4
fi
echo "dist.sh: verifying $DASHBOARD_BIN --version stamp ($VERSION ($BUILD_COMMIT, $TARGET)) ..."
dash_expected="$DASHBOARD_BIN $VERSION ($BUILD_COMMIT, $TARGET)"
dash_ver_out="$("$DASHBOARD_BUILT" --version 2>/dev/null || true)"
dash_ver_out="${dash_ver_out%$'\r'}"   # tolerate a stray trailing CR
if [[ "$dash_ver_out" != "$dash_expected" ]]; then
  echo "dist.sh: --version stamp mismatch for '$DASHBOARD_BIN'" >&2
  echo "  expected exactly: '$dash_expected'" >&2
  echo "  got:              '$dash_ver_out'" >&2
  exit 6
fi
echo "  ok: $DASHBOARD_BIN -> $dash_ver_out"
# The exact-match audit above guarantees the dashboard reports the single workspace $VERSION.
VER_DASHBOARD="$VERSION"

# ---------------------------------------------------------------------------------------------------
# Fresh repo-local ./dist/
# ---------------------------------------------------------------------------------------------------
rm -rf "$DIST_DIR"
mkdir -p "$DIST_DIR"

# Keep all transient build scaffolding repo-local (under the gitignored, symlink-validated target/) so
# dist.sh writes nothing outside the repository. ($TARGET_DIR was validated as a real repo-local dir above.)
WORK="$(mktemp -d "$TARGET_DIR/dist-scratch.XXXXXX")"
cleanup() { rm -rf "$WORK"; }
trap cleanup EXIT

GIT_COMMIT="$(git -C "$REPO_ROOT" rev-parse HEAD 2>/dev/null || echo unknown)"

# Every crate now inherits the SINGLE workspace version (`version.workspace = true`), so the per-binary
# manifest versions are all the one $VERSION derived above — no per-crate Cargo.toml grep needed.
VER_PRODUCER="$VERSION"
VER_CLI="$VERSION"
VER_SYNCD="$VERSION"
VER_DEPLOY="$VERSION"
VER_CLIENT="$VERSION"

# ---------------------------------------------------------------------------------------------------
# Helpers to assemble a bundle
# ---------------------------------------------------------------------------------------------------
copy_bin() {  # copy_bin <bundle_dir> <bin_name>
  install -D -m 0755 "$BIN_SRC/$2" "$1/bin/$2"
}

write_checksums() {  # write_checksums <bundle_dir> — SHA256SUMS over payload (excludes SHA256SUMS & tarballs)
  ( cd "$1" && find . -type f ! -name 'SHA256SUMS*' ! -name '*.tar.zst' -print0 \
      | sort -z | xargs -0 sha256sum > SHA256SUMS.tmp && mv SHA256SUMS.tmp SHA256SUMS )
}

make_tarball() {  # make_tarball <bundle_dir> <archive_basename>  -> writes <bundle_dir>/<archive>.tar.zst
  local dir="$1" name="$2"
  local members=()
  for m in bin config systemd completions SHA256SUMS; do
    [ -e "$dir/$m" ] && members+=("$m")
  done
  tar -C "$dir" -cf - "${members[@]}" | zstd -19 -q -o "$dir/$name.tar.zst"
}

# Extract the producer config-example (JSON {"producer_toml": "..."}) into a plain producer.toml.
producer_config_example() {  # producer_config_example <out_file>
  "$BIN_SRC/jurisearch-producer" config-example \
    | python3 -c 'import json,sys; sys.stdout.write(json.load(sys.stdin)["producer_toml"])' > "$1"
}

# ---------------------------------------------------------------------------------------------------
# Bundle: update-server
# ---------------------------------------------------------------------------------------------------
echo "dist.sh: assembling update-server bundle ..."
US="$DIST_DIR/update-server"
mkdir -p "$US/bin" "$US/config" "$US/systemd"
for b in "${UPDATE_SERVER_BINS[@]}"; do copy_bin "$US" "$b"; done
# The Bun-built dashboard is NOT under $BIN_SRC (not a Cargo bin), so copy_bin can't fetch it — install the
# audited Bun artifact explicitly into the bundle's bin/ (mode 0755, same as copy_bin).
install -D -m 0755 "$DASHBOARD_BUILT" "$US/bin/$DASHBOARD_BIN"

# Producer config example (operator template; secrets are referenced by path/env, never inline).
producer_config_example "$US/config/producer.toml.example"

# Producer systemd service/timer templates: render them via the producer's own renderer, pointed at a
# scratch unit_dir so we capture exactly what `jurisearch-producer install` would write (M3 templates).
# `install` strictly validates the config first, including 0600 secret-file perms, so we stage dummy
# 0600 secret files in scratch and repoint the example's secret/unit paths at them. These dummies are
# build-time scaffolding under $WORK (a mktemp dir) and never enter any bundle.
PRENDER_CFG="$WORK/producer.toml"
producer_config_example "$PRENDER_CFG"
RENDER_OUT="$WORK/producer-units"
SECRETS_DIR="$WORK/secrets"
mkdir -p "$RENDER_OUT" "$SECRETS_DIR"
( umask 077
  : > "$SECRETS_DIR/admin-password"
  : > "$SECRETS_DIR/writer-password"
  : > "$SECRETS_DIR/signing.seed" )
python3 - "$PRENDER_CFG" "$RENDER_OUT" "$SECRETS_DIR" <<'PY'
import sys, re, pathlib
cfg, out, secrets = sys.argv[1], sys.argv[2], sys.argv[3]
text = pathlib.Path(cfg).read_text()
text = re.sub(r'(?m)^unit_dir = .*$', f'unit_dir = "{out}"', text)
text = re.sub(r'(?m)^admin_password_file = .*$',  f'admin_password_file = "{secrets}/admin-password"', text)
text = re.sub(r'(?m)^writer_password_file = .*$', f'writer_password_file = "{secrets}/writer-password"', text)
text = re.sub(r'(?m)^signing_key_seed_file = .*$', f'signing_key_seed_file = "{secrets}/signing.seed"', text)
pathlib.Path(cfg).write_text(text)
PY
"$BIN_SRC/jurisearch-producer" install --config "$PRENDER_CFG" >/dev/null
cp "$RENDER_OUT"/*.service "$RENDER_OUT"/*.timer "$US/systemd/"

write_checksums "$US"
audit_dir "$US" "" "${UPDATE_SERVER_BUNDLE_BINS[@]}"
make_tarball "$US" "jurisearch-update-server-$VERSION-$TARGET"

# ---------------------------------------------------------------------------------------------------
# Bundle: site-server
# ---------------------------------------------------------------------------------------------------
echo "dist.sh: assembling site-server bundle ..."
SS="$DIST_DIR/site-server"
mkdir -p "$SS/bin" "$SS/config" "$SS/systemd"
for b in "${SITE_SERVER_BINS[@]}"; do copy_bin "$SS" "$b"; done

# Site config example.
"$BIN_SRC/jurisearchctl" site config-example > "$SS/config/site.toml.example"

# Site systemd templates (checked-in reference units for site, syncd, bge-m3).
cp "$REPO_ROOT"/deploy/systemd/jurisearch-site.service \
   "$REPO_ROOT"/deploy/systemd/jurisearch-syncd.service \
   "$REPO_ROOT"/deploy/systemd/jurisearch-bge-m3.service \
   "$SS/systemd/"

write_checksums "$SS"
audit_dir "$SS" "" "${SITE_SERVER_BINS[@]}"
make_tarball "$SS" "jurisearch-site-server-$VERSION-$TARGET"

# ---------------------------------------------------------------------------------------------------
# Bundle: cli (thin client)
# ---------------------------------------------------------------------------------------------------
echo "dist.sh: assembling cli bundle ..."
CL="$DIST_DIR/cli"
mkdir -p "$CL/bin"
for b in "${CLI_BINS[@]}"; do copy_bin "$CL" "$b"; done
# Completions/manpage are not generated by jurisearch-client in this release (no completions subcommand).

write_checksums "$CL"
audit_dir "$CL" "" "${CLI_BINS[@]}"
make_tarball "$CL" "jurisearch-cli-$VERSION-$TARGET"

# ---------------------------------------------------------------------------------------------------
# Top-level manifest.toml
# ---------------------------------------------------------------------------------------------------
echo "dist.sh: writing manifest.toml ..."
tarball_sha() { sha256sum "$1" | awk '{print $1}'; }
US_TAR="$US/jurisearch-update-server-$VERSION-$TARGET.tar.zst"
SS_TAR="$SS/jurisearch-site-server-$VERSION-$TARGET.tar.zst"
CL_TAR="$CL/jurisearch-cli-$VERSION-$TARGET.tar.zst"

{
  echo "# JuriSearch release manifest — generated by dist.sh (work/10 M6). Do not edit by hand."
  echo ""
  echo "[release]"
  echo "version = \"$VERSION\""
  echo "git_commit = \"$GIT_COMMIT\""
  echo "targets = [\"$TARGET\"]"
  echo "format = \"tar.zst\""
  echo "debian_packages = \"deferred\""
  echo ""
  echo "[binary_versions]"
  echo "jurisearch-producer = \"$VER_PRODUCER\""
  echo "jurisearch-dashboard = \"$VER_DASHBOARD\""
  echo "jurisearch = \"$VER_CLI\""
  echo "jurisearch-syncd = \"$VER_SYNCD\""
  echo "jurisearchctl = \"$VER_DEPLOY\""
  echo "jurisearch-client = \"$VER_CLIENT\""
  echo ""
  echo "[[bundle]]"
  echo "role = \"update-server\""
  echo "binaries = [\"jurisearch-producer\", \"jurisearch-dashboard\"]"
  echo "tarball = \"update-server/$(basename "$US_TAR")\""
  echo "tarball_sha256 = \"$(tarball_sha "$US_TAR")\""
  echo "files = ["
  ( cd "$US" && find . -type f ! -name '*.tar.zst' | sort | sed 's#^\./#  "update-server/#; s#$#",#' )
  echo "]"
  echo ""
  echo "[[bundle]]"
  echo "role = \"site-server\""
  echo "binaries = [\"jurisearch\", \"jurisearch-syncd\", \"jurisearchctl\"]"
  echo "tarball = \"site-server/$(basename "$SS_TAR")\""
  echo "tarball_sha256 = \"$(tarball_sha "$SS_TAR")\""
  echo "files = ["
  ( cd "$SS" && find . -type f ! -name '*.tar.zst' | sort | sed 's#^\./#  "site-server/#; s#$#",#' )
  echo "]"
  echo ""
  echo "[[bundle]]"
  echo "role = \"cli\""
  echo "binaries = [\"jurisearch-client\"]"
  echo "tarball = \"cli/$(basename "$CL_TAR")\""
  echo "tarball_sha256 = \"$(tarball_sha "$CL_TAR")\""
  echo "files = ["
  ( cd "$CL" && find . -type f ! -name '*.tar.zst' | sort | sed 's#^\./#  "cli/#; s#$#",#' )
  echo "]"
  echo ""
  echo "# External prerequisites — provisioned/fetched separately, NEVER bundled (see README)."
  echo "[prerequisites]"
  echo "update_server = ["
  echo "  \"External PostgreSQL (producer DB; pgvector + pg_search extensions)\","
  echo "  \"zstd + tar to unpack this bundle\","
  echo "  \"systemd (for the rendered producer service/timer units)\","
  echo "  \"PISTE / OpenRouter credentials supplied via env or 0600 files (never in the bundle)\","
  echo "]"
  echo "site_server = ["
  echo "  \"External PostgreSQL (site DB; pgvector + pg_search extensions)\","
  echo "  \"Local loopback bge-m3 endpoint (llama-server + model/tokenizer assets, fetched separately)\","
  echo "  \"zstd + tar to unpack this bundle\","
  echo "  \"systemd (for the rendered site/syncd/bge-m3 units)\","
  echo "]"
  echo "cli = ["
  echo "  \"Network reachability to a JuriSearch site URL (trusted LAN / Tailscale)\","
  echo "]"
} > "$DIST_DIR/manifest.toml"

# ---------------------------------------------------------------------------------------------------
# Top-level README.md
# ---------------------------------------------------------------------------------------------------
echo "dist.sh: writing README.md ..."
cat > "$DIST_DIR/README.md" <<EOF
# JuriSearch release bundles ($VERSION)

Generated by \`./dist.sh\` (work/10 M6). Target: \`$TARGET\`. Format: \`.tar.zst\`.
Git commit: \`$GIT_COMMIT\`. Debian packages are deferred.

Every output here is **repository-local** (\`./dist/\`); \`dist.sh\` never writes to the absolute path
\`/dist\`. Re-running \`./dist.sh\` recreates \`./dist/\` from scratch.

## Bundles (role-distinct)

| Role | Binaries | Tarball |
|---|---|---|
| \`update-server\` | \`jurisearch-producer\`, \`jurisearch-dashboard\` | \`update-server/$(basename "$US_TAR")\` |
| \`site-server\` | \`jurisearch\`, \`jurisearch-syncd\`, \`jurisearchctl\` | \`site-server/$(basename "$SS_TAR")\` |
| \`cli\` | \`jurisearch-client\` | \`cli/$(basename "$CL_TAR")\` |

Each bundle directory also contains \`SHA256SUMS\` (per-file checksums of the payload) and, for the
server roles, \`config/\` examples and \`systemd/\` unit templates.

## EXCLUDED assets (never in any bundle)

The bundle audit in \`dist.sh\` FAILS the build if any of these leak in:

- databases / DB data incl. SQLite WAL/SHM sidecars (\`*.sqlite\`, \`*.db\`, \`*.wal\`, \`*.sqlite-wal\`,
  \`*.sqlite-shm\`, \`*.db-wal\`, \`*.db-shm\`, \`*-wal\`, \`*-shm\`, ...)
- vector indexes (\`*.faiss\`, \`*.hnsw\`, \`*.usearch\`, \`*.index\`, ...)
- archive payloads of any kind (\`*.tar.gz\`, \`*.zip\`, \`*.tar.zst\`, ...) — the ONLY permitted
  \`.tar.zst\` is the role's own generated release tarball at the bundle top level
- runtime corpus packages / manifests (\`manifest.json\`, \`*.pkg\`, ...)
- model weights (\`*.gguf\`, \`*.safetensors\`, \`*.bin\`, ...)
- tokenizer files (\`tokenizer*.json\`, \`vocab.txt\`, ...)
- credentials / secrets (\`*.pem\`, \`*.key\`, \`*.seed\`, \`.env\`, \`.env.*\`, \`.pgpass\`, \`.netrc\`,
  \`credentials*\`, \`*credential*\`, \`*secret*\`, \`client_secret*.json\`, \`service-account*.json\`, \`*.token\`, ...)

These are provisioned or fetched separately and referenced by config/manifests. See
\`manifest.toml\` → \`[prerequisites]\`.

## Prerequisites (provisioned separately, not bundled)

- **update-server**: external PostgreSQL (producer DB with \`pgvector\` + \`pg_search\`); \`systemd\`;
  \`zstd\`/\`tar\`; PISTE / OpenRouter credentials supplied via env or \`0600\` files.
- **site-server**: external PostgreSQL (site DB with \`pgvector\` + \`pg_search\`); a local loopback
  \`bge-m3\` endpoint (\`llama-server\` + model/tokenizer assets, fetched separately); \`systemd\`;
  \`zstd\`/\`tar\`.
- **cli**: network reachability to a JuriSearch site URL (trusted LAN / Tailscale).

## Install (per role)

\`\`\`sh
# update-server
tar --use-compress-program=unzstd -xf update-server/$(basename "$US_TAR") -C /opt/jurisearch-update
install -m0755 /opt/jurisearch-update/bin/jurisearch-producer /usr/local/bin/
# copy & edit config/producer.toml.example -> /etc/jurisearch/producer.toml, then:
jurisearch-producer provision-db --config /etc/jurisearch/producer.toml
jurisearch-producer install --config /etc/jurisearch/producer.toml   # renders the systemd units
# This bundle also ships bin/jurisearch-dashboard; its service/config install is completed by deploy.sh.

# site-server
tar --use-compress-program=unzstd -xf site-server/$(basename "$SS_TAR") -C /opt/jurisearch-site
install -m0755 /opt/jurisearch-site/bin/jurisearch /opt/jurisearch-site/bin/jurisearch-syncd \\
  /opt/jurisearch-site/bin/jurisearchctl /usr/local/bin/
# copy & edit config/site.toml.example -> /etc/jurisearch/site.toml, then follow:
jurisearchctl site provision-db --config /etc/jurisearch/site.toml
jurisearchctl site install --config /etc/jurisearch/site.toml

# cli (thin client)
tar --use-compress-program=unzstd -xf cli/$(basename "$CL_TAR") -C /opt/jurisearch-cli
install -m0755 /opt/jurisearch-cli/bin/jurisearch-client /usr/local/bin/
jurisearch-client configure --server tcp://<site-host>:8099
\`\`\`

Verify a bundle's payload after extraction with: \`sha256sum -c SHA256SUMS\`.

## Upgrade / rollback

**Not implemented in this release.** \`jurisearchctl site upgrade --bundle <tarball>\` and
\`jurisearchctl site rollback --to <version>\` are the planned operator surface (plan 01 Phase 9).
Until they ship, upgrade by installing a newer bundle's binaries and re-running the role's
\`provision-db\` / \`install\` steps. \`dist.sh\` does not silently no-op an upgrade; it only produces
artifacts.
EOF

# ---------------------------------------------------------------------------------------------------
# Final verification: re-audit every bundle and verify checksums on the real output.
# ---------------------------------------------------------------------------------------------------
echo "dist.sh: final verification ..."
audit_dir "$US" "$(release_tarball_for_role update-server)" "${UPDATE_SERVER_BUNDLE_BINS[@]}"
audit_dir "$SS" "$(release_tarball_for_role site-server)" "${SITE_SERVER_BINS[@]}"
audit_dir "$CL" "$(release_tarball_for_role cli)" "${CLI_BINS[@]}"
for d in "$US" "$SS" "$CL"; do
  ( cd "$d" && sha256sum -c --quiet SHA256SUMS )
done

echo ""
echo "dist.sh: done. Bundles under $DIST_DIR:"
( cd "$DIST_DIR" && find . -maxdepth 2 -type f | sort )
