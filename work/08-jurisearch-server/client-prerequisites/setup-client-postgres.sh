#!/usr/bin/env bash
#
# setup-client-postgres.sh — provision THIS Fedora workstation as a jurisearch
# package-distribution CLIENT: a persistent **system** PostgreSQL 18 service with pgvector +
# pg_search, mirroring the bear producer.
#
# ── HOW TO RUN ─────────────────────────────────────────────────────────────────────────────────
#   Run AS YOUR NORMAL USER (NOT `sudo ./setup-client-postgres.sh`):
#       ./setup-client-postgres.sh
#   It builds the extensions as you (your Rust toolchain, the ~/Work/paradedb fork, ~/.pgrx) and uses
#   `sudo` ONLY for the privileged steps (dnf, initdb, systemd, install into /usr, postgres role). You
#   are prompted for your sudo password once (cached/kept alive for the run).
#
# ── "START CLEAN" — what is and isn't removed, and WHY ──────────────────────────────────────────
#   The clean slate that matters is a brand-new CLUSTER (this script purges /var/lib/pgsql/data and
#   re-initdb's). It does NOT do a full package uninstall+reinstall of PostgreSQL, on purpose:
#   Fedora's repo currently ships ONLY postgresql-*-18.3-7, the PG subpackages version-lock together,
#   and postgresql-contrib-18.3-7 is BROKEN (a Python 3.15 dependency conflict). A remove-then-
#   reinstall would drag in that broken contrib after the cluster is already gone and could leave the
#   machine with NO PostgreSQL. Instead this:
#     • removes postgresql-contrib (NOT needed by jurisearch; it is the version-lock blocker),
#     • upgrades the server stack in place to 18.3-7 + installs server-devel  (fails closed here),
#     • THEN purges the cluster dir and re-initdb's (the real clean start).
#   Net effect is the same end state (a fresh PG18 cluster on the current binaries) without the brick
#   risk. Nothing destructive to the cluster happens until the package step has succeeded.
#
# ── ORDER (fail-closed) ─────────────────────────────────────────────────────────────────────────
#   0. preconditions      (not root; cargo/git/make present; the paradedb fork present)
#   1. PREFLIGHT          (non-destructive): pin cargo-pgrx; confirm the pgvector tag; confirm the
#                          repo's postgresql-server candidate is major 18
#   2. PACKAGES           stop the service (iff active) → remove pgadmin4-server + postgresql-contrib
#                          (iff installed) → swap PGDG libpq5 → Fedora libpq → `dnf install --best` the
#                          PG18 stack + build deps  (all BEFORE any cluster purge; fails closed)
#   3. CLEAN CLUSTER      purge /var/lib/pgsql/data → postgresql-setup --initdb   (now packages are correct)
#   4. build + install pg_search 0.24.1   (cargo-pgrx, from the pnocera/paradedb fork — the long step)
#   5. build + install pgvector           (from source, a 0.8.x tag — matches bear)
#   6. shared_preload_libraries='pg_search' + conservative tuning + `postgres` password + loopback
#      scram auth; enable + (re)start the service
#   7. create the `jurisearch` database, CREATE EXTENSION vector + pg_search, verify
#
# ── WHY A PERSISTENT SERVER (not the CLI's embedded pgembed) ────────────────────────────────────
#   The design (§7.1) runs a long-running local service AND the CLI concurrently over ONE persistent
#   DB (advisory-lock coordination, short critical sections), which pgembed's single-owner,
#   ephemeral-cluster-per-index-dir model cannot host. NOTE: the client SOFTWARE that uses this
#   server (syncd + an external-DSN read path) does NOT exist yet — this provisions the E2 ENVIRONMENT
#   prerequisite ahead of that code. The current `jurisearch` CLI still uses pgembed and ignores this
#   server until the package-distribution P-phases land.
#
# Mirrors bear/infra/bear-storage/{build-pg-search.sh,switch-to-pg18.sh}, adapted to Fedora (dnf,
# postgresql-setup, systemd) and to the build-as-user / install-as-root split bear did not need.
#
set -Eeuo pipefail
export LC_ALL=C.UTF-8 LANG=C.UTF-8

# ── tunables (override via env) ─────────────────────────────────────────────────────────────────
PARADEDB_DIR="${PARADEDB_DIR:-$HOME/Work/paradedb}"   # the pnocera/paradedb fork (provides pg_search)
PGVECTOR_REF="${PGVECTOR_REF:-v0.8.3}"                # match bear's pgvector 0.8.x
PG_MAJOR_EXPECTED="${PG_MAJOR_EXPECTED:-18}"          # design requires PostgreSQL 18 (major)
PG_SUPERUSER_PASSWORD="${JURISEARCH_PG_SUPERUSER_PASSWORD:-postgres}"  # `postgres` role password
APP_DB="${JURISEARCH_APP_DB:-jurisearch}"            # database the extensions are created in
PG_FEATURES="${PG_FEATURES:-pg18,deferred_wal}"      # pg_search cargo features (fork default for PG18)

# Fixed Fedora system paths (NOT tunable: postgresql-setup --initdb and postgresql.service use these).
PGDATA="/var/lib/pgsql/data"
PGC="/usr/bin/pg_config"   # hard-pinned to the SYSTEM server-devel pg_config (never the ~/.pgrx one)

# Fedora package set to ENSURE installed (upgraded in place to the repo candidate). server-devel
# provides pg_config/PGXS/headers and pulls postgresql-private-devel; the rest is the toolchain to
# build pg_search (pgrx/bindgen needs libclang) and pgvector (JIT bitcode needs clang/llvm).
# DELIBERATELY ABSENT: postgresql-contrib (unused, and its repo -7 rebuild is broken) and libpq-devel
# (conflicts with postgresql-private-devel on fc45).
PKGS=(
  postgresql-server postgresql postgresql-server-devel
  clang clang-devel llvm llvm-devel gcc gcc-c++ make cmake pkgconf-pkg-config
  openssl-devel readline-devel zlib-devel libicu-devel
)

ts(){ date '+%Y-%m-%d %H:%M:%S'; }
log(){ echo "[$(ts)] $*"; }
die(){ echo "[$(ts)] FATAL: $*" >&2; echo "SENTINEL: CLIENT-PG-FAILED rc=1"; exit 1; }
trap 'die "unexpected error on line $LINENO"' ERR

BUILD_TMP=""
SUDO_KEEPALIVE=""
cleanup(){
  [ -n "$SUDO_KEEPALIVE" ] && kill "$SUDO_KEEPALIVE" 2>/dev/null || true
  [ -n "$BUILD_TMP" ] && rm -rf "$BUILD_TMP" 2>/dev/null || true
}
trap cleanup EXIT

# Stop postgresql.service only if it is actually running; a missing unit is fine, a FAILED stop is not.
stop_pg_if_active(){
  if [ "$(systemctl is-active postgresql.service 2>/dev/null || true)" = "active" ]; then
    log "stopping running postgresql.service…"
    sudo systemctl stop postgresql.service
    [ "$(systemctl is-active postgresql.service 2>/dev/null || true)" != "active" ] \
      || die "postgresql.service is still active after stop — refusing to continue"
  fi
}

# ── 0. preconditions (no system changes) ────────────────────────────────────────────────────────
[ "$(id -u)" != 0 ] || die "run as your NORMAL user, not root/sudo (the Rust build needs your toolchain + ~/.pgrx); the script sudo's the privileged steps itself"
for c in sudo dnf rpm git make cargo systemctl; do command -v "$c" >/dev/null || die "required command not found: $c"; done
[ -d "$PARADEDB_DIR" ] || die "paradedb fork not found at $PARADEDB_DIR (set PARADEDB_DIR=…)"
[ -f "$PARADEDB_DIR/Cargo.toml" ] || die "$PARADEDB_DIR is not a cargo workspace (no Cargo.toml)"
case "$APP_DB" in ""|-*) die "invalid JURISEARCH_APP_DB='$APP_DB' (must be non-empty and not start with '-')";; esac

log "this upgrades the PostgreSQL stack to the repo's PG${PG_MAJOR_EXPECTED} candidate, removes postgresql-contrib, and PURGES $PGDATA for a fresh cluster."
log "Nothing destructive to the cluster happens until the package step (2) succeeds. Ctrl-C within 5s to abort…"; sleep 5

log "priming sudo (you'll be prompted for your password once; kept alive through the long build)…"
sudo -v
# Keep the sudo timestamp fresh so the ~20-40 min pg_search build does not cause a re-prompt mid-run.
( while true; do sudo -n true 2>/dev/null; sleep 50; kill -0 "$$" 2>/dev/null || exit 0; done ) &
SUDO_KEEPALIVE=$!

# ── 1. PREFLIGHT — validate cheaply, BEFORE any system change ───────────────────────────────────
log "PREFLIGHT: pinning cargo-pgrx to the fork's pgrx version…"
PGRX_VERSION="$(grep -m1 -E '^pgrx[[:space:]]*=' "$PARADEDB_DIR/Cargo.toml" | sed -E 's/.*"=?([0-9][0-9.]*)".*/\1/')"
[ -n "$PGRX_VERSION" ] || die "could not read the pinned pgrx version from $PARADEDB_DIR/Cargo.toml"
if ! cargo pgrx --version 2>/dev/null | grep -qx "cargo-pgrx $PGRX_VERSION"; then
  log "installing cargo-pgrx $PGRX_VERSION (compiles cargo-pgrx; a few minutes)…"
  cargo install --locked cargo-pgrx --version "$PGRX_VERSION"
fi
log "cargo-pgrx: $(cargo pgrx --version)"

log "PREFLIGHT: confirming the pgvector tag '$PGVECTOR_REF' exists upstream…"
git ls-remote --exit-code --tags https://github.com/pgvector/pgvector.git "refs/tags/$PGVECTOR_REF" >/dev/null \
  || die "pgvector tag '$PGVECTOR_REF' not found upstream (set PGVECTOR_REF=…)"

log "PREFLIGHT: confirming the repo postgresql-server candidate is major $PG_MAJOR_EXPECTED…"
CAND_VER="$(dnf -q repoquery --available --latest-limit=1 --qf '%{version}' postgresql-server 2>/dev/null | head -1)"
[ -n "$CAND_VER" ] || die "no postgresql-server candidate found in the enabled dnf repos"
[ "${CAND_VER%%.*}" = "$PG_MAJOR_EXPECTED" ] || die "repo postgresql-server candidate '$CAND_VER' is not major $PG_MAJOR_EXPECTED"
log "PREFLIGHT passed (repo postgresql-server candidate: $CAND_VER)."

# ── 2. settle PACKAGES before any cluster destruction ───────────────────────────────────────────
# This host carries the PGDG `libpq5` (it owns /usr/lib64/libpq.so*, and pgadmin4-server requires it),
# which file-conflicts with Fedora's postgresql-private-devel (pulled in by postgresql-server-devel).
# Per the chosen approach, we drop pgadmin4-server and swap PGDG libpq5 → Fedora libpq, then install
# the server stack. ALL of this is BEFORE the cluster purge (step 3), so it fails closed — a failed
# package step leaves the cluster intact.
stop_pg_if_active
for blocker in pgadmin4-server postgresql-contrib; do
  if rpm -q "$blocker" >/dev/null 2>&1; then
    log "removing $blocker (consented: clears the PGDG libpq5 conflict / drops unused contrib)…"
    sudo dnf remove -y "$blocker"
  fi
done
# Swap the PGDG libpq5 → Fedora libpq as a SINGLE atomic transaction. Target the Fedora package by
# NAME.ARCH (`libpq.x86_64`): a bare `libpq` spec is a virtual capability that PGDG libpq5 already
# Provides, so dnf would just keep/upgrade libpq5 and the /usr/lib64/libpq.so conflict would recur.
# Guard first that nothing still requires the libpq5 *package name* (pgadmin4-server was the only one,
# removed above), then verify the swap landed. Fedora libpq still owns /usr/lib64/libpq.so.5 (psql, gdal).
if rpm -q libpq5 >/dev/null 2>&1; then
  if rpm -q --whatrequires libpq5 2>/dev/null | grep -v '^no package requires ' | grep -q .; then
    die "libpq5 is still required by an installed package after removing pgadmin4-server — resolve that dependency before the swap"
  fi
  log "replacing PGDG libpq5 with Fedora libpq (explicit name.arch; bare 'libpq' would resolve back to PGDG libpq5)…"
  sudo dnf swap -y --best --allowerasing libpq5 libpq.x86_64
  rpm -q libpq >/dev/null 2>&1 || die "Fedora libpq is not installed after the swap"
  if rpm -q libpq5 >/dev/null 2>&1; then die "PGDG libpq5 is still installed after the swap"; fi
fi
log "installing/upgrading the PostgreSQL 18 stack + build deps (--best; fails closed, cluster intact)…"
sudo dnf install -y --best "${PKGS[@]}"

[ -x "$PGC" ] || die "pg_config not found at $PGC after install (expected from postgresql-server-devel)"
PKGLIB="$("$PGC" --pkglibdir)"
SHARE="$("$PGC" --sharedir)"
case "$PKGLIB" in
  "$HOME"/*|"$HOME") die "pg_config $PGC reports pkglibdir under \$HOME ($PKGLIB) — refusing to target the pgrx-managed PostgreSQL" ;;
esac
PG_VERSION_FULL="$("$PGC" --version)"
PG_MAJOR="$(printf '%s' "$PG_VERSION_FULL" | sed -E 's/^PostgreSQL ([0-9]+).*/\1/')"
[ "$PG_MAJOR" = "$PG_MAJOR_EXPECTED" ] || die "installed PostgreSQL major is '$PG_MAJOR', expected $PG_MAJOR_EXPECTED ($PG_VERSION_FULL)"
log "system PostgreSQL: $PG_VERSION_FULL  (pkglibdir=$PKGLIB, sharedir=$SHARE)"

# ── 3. CLEAN the cluster (packages are now correct) ─────────────────────────────────────────────
stop_pg_if_active   # a package scriptlet may have started it
log "purging the cluster directory $PGDATA for a fresh start…"
sudo rm -rf "$PGDATA"
log "initialising the cluster ($PGDATA) via postgresql-setup --initdb…"
sudo postgresql-setup --initdb

# ── 4. build + install pg_search from the fork (the long step) ──────────────────────────────────
log "building + staging pg_search for PG${PG_MAJOR} (datafusion + tantivy — the long step, ~20-40 min)…"
( cd "$PARADEDB_DIR" && PGRX_PG_CONFIG_PATH="$PGC" cargo pgrx package --package pg_search \
    --no-default-features --features "$PG_FEATURES" --pg-config "$PGC" )

SO_SRC="$(find "$PARADEDB_DIR/target" -name pg_search.so -path "*pg_search-pg${PG_MAJOR}*" 2>/dev/null | head -1)"
CTRL_SRC="$(find "$PARADEDB_DIR/target" -name pg_search.control -path "*pg_search-pg${PG_MAJOR}*" 2>/dev/null | head -1)"
[ -n "$SO_SRC" ] && [ -n "$CTRL_SRC" ] || die "pg_search staged artifacts not found under $PARADEDB_DIR/target after package"
EXT_SRC_DIR="$(dirname "$CTRL_SRC")"
log "installing pg_search artifacts into the system PostgreSQL as root (sudo)…"
# Install with explicit root:root ownership + standard modes — NOT `cp -a`, which (= --preserve=all)
# would carry the build user's ownership and SELinux `user_home_t` label into the system extension dir
# and make CREATE EXTENSION fail under enforcing SELinux. `install` also overwrites attributes, so it
# repairs a prior bad run. `restorecon` then resets the SELinux context to the path's system default.
sudo install -D -o root -g root -m 0755 "$SO_SRC" "$PKGLIB/pg_search.so"
sudo install -d -o root -g root -m 0755 "$SHARE/extension"
find "$EXT_SRC_DIR" -maxdepth 1 -type f \
  -exec sudo install -o root -g root -m 0644 -t "$SHARE/extension" {} +
if command -v restorecon >/dev/null 2>&1; then
  sudo restorecon -v "$PKGLIB/pg_search.so" "$SHARE/extension"/pg_search* 2>/dev/null || true
fi

# ── 5. build + install pgvector from source (matches bear's 0.8.x) ──────────────────────────────
log "building + installing pgvector $PGVECTOR_REF from source…"
BUILD_TMP="$(mktemp -d)"
git clone --depth 1 --branch "$PGVECTOR_REF" https://github.com/pgvector/pgvector.git "$BUILD_TMP/pgvector"
make -C "$BUILD_TMP/pgvector" PG_CONFIG="$PGC"
sudo make -C "$BUILD_TMP/pgvector" PG_CONFIG="$PGC" install

# ── 6. configure: preload, tuning, password, TCP password auth; enable + (re)start ──────────────
log "enabling + starting postgresql.service…"
sudo systemctl enable postgresql.service
sudo systemctl start  postgresql.service
for _ in $(seq 1 30); do sudo -u postgres psql -X -tAc 'select 1' >/dev/null 2>&1 && break; sleep 1; done
sudo -u postgres psql -X -tAc 'select 1' >/dev/null 2>&1 || die "PostgreSQL did not become ready after start"

# shared_preload_libraries (preserve any existing entry, append pg_search) — REQUIRED by pg_search
cur_spl="$(sudo -u postgres psql -X -tAc 'show shared_preload_libraries' | tr -d '[:space:]')"
if   echo ",${cur_spl}," | grep -q ",pg_search,"; then new_spl="$cur_spl"
elif [ -z "$cur_spl" ];                          then new_spl="pg_search"
else                                                  new_spl="${cur_spl},pg_search"
fi
log "setting shared_preload_libraries = '$new_spl' + conservative tuning + postgres password (sudo)…"
# Values are passed as psql variables and quoted with :'var' so an env override can't break the SQL.
sudo -u postgres psql -X -v ON_ERROR_STOP=1 -v spl="$new_spl" -v pw="$PG_SUPERUSER_PASSWORD" <<'SQL'
ALTER SYSTEM SET shared_preload_libraries = :'spl';
-- Conservative starting tuning for a workstation client that co-hosts local LLM/embedding services.
-- Adjust freely; these are a small footprint, not a 25%-of-RAM server profile.
ALTER SYSTEM SET shared_buffers = '512MB';
ALTER SYSTEM SET work_mem = '64MB';
ALTER SYSTEM SET maintenance_work_mem = '512MB';
ALTER SYSTEM SET effective_cache_size = '4GB';
ALTER SYSTEM SET max_parallel_workers = '4';
ALTER SYSTEM SET max_parallel_workers_per_gather = '2';
ALTER SYSTEM SET max_parallel_maintenance_workers = '2';
-- postgres superuser password (used by the future client DSN over TCP loopback)
ALTER USER postgres PASSWORD :'pw';
SQL

# Enable scram password auth over loopback TCP (first-match wins → prepend before the distro rules).
# Local socket connections keep peer auth, so `sudo -u postgres psql` stays password-less for admin.
HBA="$PGDATA/pg_hba.conf"
if ! sudo grep -q 'jurisearch client: loopback password auth' "$HBA" 2>/dev/null; then
  log "enabling scram password auth for 127.0.0.1/::1 in pg_hba.conf…"
  { printf '%s\n' \
      '# jurisearch client: loopback password auth (prepended; pg_hba is first-match)' \
      'host    all   all   127.0.0.1/32   scram-sha-256' \
      'host    all   all   ::1/128        scram-sha-256'; \
    sudo cat "$HBA"; } | sudo tee "${HBA}.jurisearch.new" >/dev/null
  sudo install -o postgres -g postgres -m 0600 "${HBA}.jurisearch.new" "$HBA"
  sudo rm -f "${HBA}.jurisearch.new"
fi

log "restarting postgresql.service to apply preload + pg_hba…"
sudo systemctl restart postgresql.service
for _ in $(seq 1 30); do sudo -u postgres psql -X -tAc 'select 1' >/dev/null 2>&1 && break; sleep 1; done
sudo -u postgres psql -X -tAc 'select 1' >/dev/null 2>&1 || die "PostgreSQL did not become ready after restart"
log "shared_preload_libraries = $(sudo -u postgres psql -X -tAc 'show shared_preload_libraries')"

# ── 7. create the app database + extensions, verify ─────────────────────────────────────────────
# Probe via stdin (NOT `psql -c`): `-c` requires a server-parsable string and does not process psql's
# `:'var'` interpolation, so the quoting must run through script input like the ALTER SYSTEM block.
db_exists="$(sudo -u postgres psql -X -d postgres -tA -v ON_ERROR_STOP=1 -v app_db="$APP_DB" <<'SQL'
SELECT 1 FROM pg_database WHERE datname = :'app_db';
SQL
)"
if [ "$db_exists" != "1" ]; then
  log "creating database '${APP_DB}'…"
  sudo -u postgres createdb -- "${APP_DB}"
fi
log "creating extensions vector + pg_search in '${APP_DB}'…"
sudo -u postgres psql -X -v ON_ERROR_STOP=1 -d "${APP_DB}" <<'SQL'
CREATE EXTENSION IF NOT EXISTS vector;
CREATE EXTENSION IF NOT EXISTS pg_search;
SQL

VEC_VER="$(sudo -u postgres psql -X -tAc "SELECT extversion FROM pg_extension WHERE extname='vector'" -d "${APP_DB}")"
SRCH_VER="$(sudo -u postgres psql -X -tAc "SELECT extversion FROM pg_extension WHERE extname='pg_search'" -d "${APP_DB}")"

# Verify postgres/postgres works over TCP loopback (the future client DSN path).
log "verifying TCP loopback password auth (host=127.0.0.1 user=postgres)…"
PGPASSWORD="$PG_SUPERUSER_PASSWORD" psql -X -h 127.0.0.1 -U postgres -d "${APP_DB}" -tAc "select 'tcp-auth-ok'" \
  | grep -qx 'tcp-auth-ok' || die "TCP loopback password auth failed for postgres/<password>"

echo
log "──────────────────────────────────────────────────────────────────────────"
log "CLIENT PostgreSQL ready:"
log "  engine     : $PG_VERSION_FULL"
log "  pgvector   : $VEC_VER   (bear/producer: 0.8.3)"
log "  pg_search  : $SRCH_VER   (bear/producer: 0.24.1)"
log "  database   : ${APP_DB}   (extensions: vector, pg_search)"
log "  connect    : host=127.0.0.1 port=5432 user=postgres password=<set> dbname=${APP_DB}"
log "  admin      : sudo -u postgres psql -X -d ${APP_DB}   (local peer, no password)"
log "──────────────────────────────────────────────────────────────────────────"
echo "SENTINEL: CLIENT-PG-DONE rc=0"
