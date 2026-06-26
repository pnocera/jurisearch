#!/usr/bin/env bash
#
# switch-to-pg18.sh — replace the PG17 stack with PG18 (PGDG) + PGDG pgvector 0.8.x in CT 110.
#
# Why: the corpus to import is a physical PostgreSQL 18 data dir whose vector extension is at catalog
# version 0.8.0 (built PG18-patched) and pg_search 0.24.1. Debian 13 trixie ships PG17 only, so PG18
# comes from PGDG (apt.postgresql.org).
#
# pgvector version note (important): upstream pgvector **0.8.0 does NOT compile on PG18** (PG18 changed
# the vacuum_delay_point() signature; 0.8.0 predates it — that is exactly why PGDG ships 0.8.3 for
# PG18). pgvector keeps the IVFFlat/HNSW on-disk index format **stable across 0.8.x**, so pgvector
# **0.8.3 reads the source's 0.8.0 indexes natively, with no reindex**. We therefore install the PGDG
# apt package (0.8.x) and accept any 0.8.x. After the physical copy, the catalog (extversion 0.8.0)
# can be synced with `ALTER EXTENSION vector UPDATE` (optional; the 0.8.3 .so already serves the 0.8.0
# catalog objects). (pg_search is built separately by build-pg-search.sh with PGVER=18.)
#
# Ordering (fail-safe): ALL non-destructive setup + a PG18 install-preflight happen BEFORE dropping the
# (empty) PG17 cluster, so a repo/network/availability failure never leaves the container clusterless.
#
# Runs INSIDE CT 110 as root. Fails closed.
#
set -Eeuo pipefail
export DEBIAN_FRONTEND=noninteractive LC_ALL=C.UTF-8 LANG=C.UTF-8

PGVER_NEW=18
PG_CONFIG="/usr/lib/postgresql/${PGVER_NEW}/bin/pg_config"
# Accept any pgvector 0.8.x: 0.8.x share IVFFlat/HNSW on-disk format with the 0.8.0 source corpus,
# and 0.8.0 itself cannot compile on PG18 (so an exact-0.8.0 match is impossible here).
REQUIRE_PGVECTOR_PREFIX="${REQUIRE_PGVECTOR_PREFIX:-0.8.}"

ts(){ date '+%Y-%m-%d %H:%M:%S'; }
log(){ echo "[$(ts)] $*"; }
die(){ echo "[$(ts)] FATAL: $*" >&2; echo "SENTINEL: PG18-SWITCH-FAILED rc=1"; exit 1; }
trap 'die "unexpected error on line $LINENO"' ERR

# ---- 0. prerequisites (non-destructive) ----
[ "$(id -u)" = 0 ] || die "must run as root"
command -v pg_lsclusters >/dev/null || die "postgresql-common missing"
command -v curl >/dev/null || die "curl missing"
# shellcheck source=/dev/null  # /etc/os-release is a runtime file inside the container
. /etc/os-release
[ "${VERSION_CODENAME:-}" = trixie ] || die "expected Debian trixie, found '${VERSION_CODENAME:-?}'"

# ---- 1. configure the PGDG apt repo (non-destructive) ----
log "configuring PGDG apt repo (apt.postgresql.org) for ${VERSION_CODENAME}…"
install -d /usr/share/postgresql-common/pgdg
curl -fsSL https://www.postgresql.org/media/keys/ACCC4CF8.asc \
  -o /usr/share/postgresql-common/pgdg/apt.postgresql.org.asc
echo "deb [signed-by=/usr/share/postgresql-common/pgdg/apt.postgresql.org.asc] https://apt.postgresql.org/pub/repos/apt ${VERSION_CODENAME}-pgdg main" \
  > /etc/apt/sources.list.d/pgdg.list
apt-get update >/dev/null

# ---- 2. PREFLIGHT: confirm PG18 + server-dev + pgvector are installable BEFORE any destructive step ----
log "preflighting that postgresql-${PGVER_NEW} + server-dev + pgvector are installable from PGDG…"
apt-get install -y --simulate \
  "postgresql-${PGVER_NEW}" "postgresql-server-dev-${PGVER_NEW}" "postgresql-${PGVER_NEW}-pgvector" >/dev/null \
  || die "postgresql-${PGVER_NEW}/server-dev/pgvector not installable from PGDG (${VERSION_CODENAME}) — aborting before dropping PG17"
# Confirm the candidate pgvector is a compatible 0.8.x BEFORE dropping anything.
cand=$(apt-cache policy "postgresql-${PGVER_NEW}-pgvector" | awk '/Candidate:/{print $2}')
log "pgvector candidate from PGDG: ${cand}"
case "$cand" in
  "${REQUIRE_PGVECTOR_PREFIX}"*) : ;;
  *) die "pgvector candidate '${cand}' is not ${REQUIRE_PGVECTOR_PREFIX}x — would not be index-compatible with the 0.8.0 source corpus; aborting before dropping PG17" ;;
esac

# ---- 3. drop the empty PG17 cluster (only now that the PG18 stack is confirmed installable) ----
# Done before installing PG18 so 18/main auto-creates on port 5432 (not 5433 behind a live 17/main).
if pg_lsclusters | awk 'NR>1{print $1, $2}' | grep -qx "17 main"; then
  log "dropping the empty PG17 'main' cluster…"
  pg_dropcluster 17 main --stop
fi

# ---- 4. install PG18 + server-dev + pgvector (auto-creates 18/main on port 5432) ----
log "installing postgresql-${PGVER_NEW} + server-dev + pgvector…"
apt-get install -y \
  "postgresql-${PGVER_NEW}" "postgresql-server-dev-${PGVER_NEW}" "postgresql-${PGVER_NEW}-pgvector" >/dev/null
[ -x "$PG_CONFIG" ] || die "$PG_CONFIG missing after install"

# ---- 5. verify the installed pgvector is a 0.8.x compatible with the 0.8.0 source corpus ----
pv=$(dpkg-query -W -f='${Version}' "postgresql-${PGVER_NEW}-pgvector" 2>/dev/null)
ctrl="$("$PG_CONFIG" --sharedir)/extension/vector.control"
[ -f "$ctrl" ] || die "vector.control not installed at $ctrl"
ctrl_ver=$(sed -nE "s/^default_version = '([^']+)'.*/\1/p" "$ctrl")
log "installed pgvector: pkg=${pv} control_default_version=${ctrl_ver}"
case "$pv" in
  "${REQUIRE_PGVECTOR_PREFIX}"*) : ;;
  *) die "installed pgvector '${pv}' is not ${REQUIRE_PGVECTOR_PREFIX}x — incompatible with the 0.8.0 source corpus" ;;
esac

# ---- 6. report ----
log "cluster(s) now:"; pg_lsclusters
log "PG${PGVER_NEW}: $("$PG_CONFIG" --version) ; pgvector pkg ${pv} (0.8.x; reads the 0.8.0 source indexes — no reindex)"
log "post-copy: optionally run 'ALTER EXTENSION vector UPDATE;' to sync the catalog from 0.8.0 to ${ctrl_ver}."
log "NEXT: PGVER=${PGVER_NEW} EXT_FEATURES=pg${PGVER_NEW},deferred_wal bash build-pg-search.sh ; then physical-copy the corpus over 18/main."
echo "SENTINEL: PG18-SWITCH-DONE rc=0"
