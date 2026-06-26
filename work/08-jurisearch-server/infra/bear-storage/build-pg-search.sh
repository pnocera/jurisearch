#!/usr/bin/env bash
#
# build-pg-search.sh — build + install ParadeDB pg_search for the SYSTEM PostgreSQL 17, from the
# pnocera/paradedb fork. Runs INSIDE the Debian 13 LXC (CT 110) as root.
#
# Why source-build: ParadeDB ships pg_search only as RPM/macOS packages (no Debian .deb), and we use
# a local fork. So we build the cdylib via cargo-pgrx against the system PG17's pg_config.
#
# Pins (from the fork @ 0.24.1): Rust 1.96.0 (rust-toolchain.toml, already installed), pgrx =0.18.1,
# pg_search default features ["pg18","deferred_wal"] -> for PG17 we use
#   --no-default-features --features pg17,deferred_wal
# which mirrors the repo Makefile target `cargo pgrx install --package pg_search --release` but pins
# the PG major to 17 (the Makefile defaults to pg18).
#
# pg_search requires shared_preload_libraries='pg_search' + a restart before CREATE EXTENSION.
#
# Safety: idempotent where practical; fails closed; touches only this container (no host actions).
#
set -Eeuo pipefail
export LC_ALL=C.UTF-8 LANG=C.UTF-8

PARADEDB_DIR="${PARADEDB_DIR:-/root/paradedb}"
PGVER="${PGVER:-17}"
PG_CONFIG="/usr/lib/postgresql/${PGVER}/bin/pg_config"
EXT_FEATURES="${EXT_FEATURES:-pg17,deferred_wal}"

ts(){ date '+%Y-%m-%d %H:%M:%S'; }
log(){ echo "[$(ts)] $*"; }
die(){ echo "[$(ts)] FATAL: $*" >&2; echo "SENTINEL: PGSEARCH-FAILED rc=1"; exit 1; }
trap 'die "unexpected error on line $LINENO"' ERR

# ---- 0. prerequisites ----
[ "$(id -u)" = 0 ] || die "must run as root"
[ -d "$PARADEDB_DIR" ] || die "paradedb fork not found at $PARADEDB_DIR"
[ -x "$PG_CONFIG" ] || die "$PG_CONFIG not found — install postgresql-server-dev-${PGVER}"
# shellcheck source=/dev/null  # .cargo/env exists only at runtime inside the container
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
command -v cargo >/dev/null || die "cargo not on PATH (rustup not installed?)"
pg_lsclusters | awk '{print $1, $2}' | grep -qx "${PGVER} main" || die "PG ${PGVER} 'main' cluster not found"

# ---- 1. determine the pinned pgrx version from the fork ----
PGRX_VERSION=$(grep -m1 -E '^pgrx[[:space:]]*=' "$PARADEDB_DIR/Cargo.toml" | sed -E 's/.*"=?([0-9][0-9.]*)".*/\1/')
[ -n "$PGRX_VERSION" ] || die "could not determine pinned pgrx version from $PARADEDB_DIR/Cargo.toml"
log "fork pins pgrx = $PGRX_VERSION ; building pg_search for PG${PGVER} (features: $EXT_FEATURES)"

# ---- 2. install cargo-pgrx (pinned, EXACT match) if needed ----
if ! cargo pgrx --version 2>/dev/null | grep -qx "cargo-pgrx $PGRX_VERSION"; then
  log "installing cargo-pgrx $PGRX_VERSION (compiles cargo-pgrx; a few minutes)…"
  cargo install --locked cargo-pgrx --version "$PGRX_VERSION"
fi
log "cargo-pgrx: $(cargo pgrx --version)"

# ---- 2b. register the SYSTEM PG${PGVER} with pgrx (REQUIRED before `cargo pgrx install`) ----
# cargo-pgrx install calls Pgrx::from_config(), which needs ~/.pgrx/config.toml (or PGRX_PG_CONFIG_PATH).
# `init --pgNN=<existing pg_config>` just records the system pg_config (no PG download/build). Idempotent.
log "registering system PG${PGVER} pg_config with pgrx (cargo pgrx init)…"
cargo pgrx init "--pg${PGVER}=${PG_CONFIG}"

# ---- 3. build + install pg_search against system PG${PGVER} (the long build) ----
cd "$PARADEDB_DIR"
log "building pg_search (datafusion + tantivy — this is the long step, ~20-40 min)…"
cargo pgrx install --package pg_search --release \
  --no-default-features --features "$EXT_FEATURES" \
  --pg-config "$PG_CONFIG"
log "pg_search installed into PG${PGVER} (pkglibdir=$("$PG_CONFIG" --pkglibdir), sharedir=$("$PG_CONFIG" --sharedir))"

# ---- 4. enable shared_preload_libraries='pg_search' (idempotent via ALTER SYSTEM) + restart ----
log "ensuring shared_preload_libraries includes pg_search, then restarting PG${PGVER}…"
cur_spl=$(su - postgres -c "psql -tAc 'show shared_preload_libraries'" | tr -d '[:space:]')
if echo ",${cur_spl}," | grep -q ",pg_search,"; then
  new_spl="$cur_spl"                       # already present
elif [ -z "$cur_spl" ]; then
  new_spl="pg_search"
else
  new_spl="${cur_spl},pg_search"           # preserve any existing preload libs
fi
su - postgres -c "psql -tAc \"ALTER SYSTEM SET shared_preload_libraries = '${new_spl}';\""
pg_ctlcluster "$PGVER" main restart
ready=0
for _ in $(seq 1 30); do
  if su - postgres -c "psql -tAc 'select 1'" >/dev/null 2>&1; then ready=1; break; fi
  sleep 1
done
[ "$ready" = 1 ] || die "PG${PGVER} did not come back after restart"
spl=$(su - postgres -c "psql -tAc 'show shared_preload_libraries'")
log "shared_preload_libraries = $spl"

# ---- 5. verify: create both extensions in a throwaway DB, report versions, drop it ----
log "verifying CREATE EXTENSION vector + pg_search…"
cat > /tmp/ext_smoke.sql <<'SQL'
CREATE EXTENSION IF NOT EXISTS vector;
CREATE EXTENSION IF NOT EXISTS pg_search;
SELECT extname, extversion FROM pg_extension WHERE extname IN ('vector','pg_search') ORDER BY 1;
SQL
chmod 644 /tmp/ext_smoke.sql
su - postgres -c "psql -tAc \"DROP DATABASE IF EXISTS ext_smoke;\""
su - postgres -c "psql -tAc \"CREATE DATABASE ext_smoke;\""
su - postgres -c "psql -v ON_ERROR_STOP=1 -d ext_smoke -f /tmp/ext_smoke.sql"
su - postgres -c "psql -tAc \"DROP DATABASE ext_smoke;\""
rm -f /tmp/ext_smoke.sql
log "pg_search + pgvector verified on PG${PGVER}"
echo "SENTINEL: PGSEARCH-DONE rc=0"
