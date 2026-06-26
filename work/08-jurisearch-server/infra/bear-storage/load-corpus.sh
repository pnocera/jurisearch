#!/usr/bin/env bash
#
# load-corpus.sh — physically load the 165 G PG18 corpus into CT 110's 18/main cluster.
#
# Runs on the BEAR HOST (uses pct). It overwrites the (empty) 18/main data dir with a staged copy of
# the source PG18 PGDATA. Highest-stakes script in this sequence — heavily guarded and fail-closed.
# The source on fedora is NEVER touched (only the bear staging copy), so this is recoverable.
#
# Method (physical copy, no index rebuild): bind-mount the staging read-only into the container, prove
# PG18 is stopped, rsync the staged PGDATA over 18/main, fix ownership, neutralise the source's
# postgresql.auto.conf to just shared_preload_libraries='pg_search' (Debian's /etc config supplies
# port/socket/paths), start PG18, and verify the corpus + extensions actually load.
#
set -Eeuo pipefail

VMID="${VMID:-110}"
STAGING="${STAGING:-/root/jurisearch-staging}"   # bear host path (the rsync'd PG18 PGDATA)
PGVER="${PGVER:-18}"
DATADIR="/var/lib/postgresql/${PGVER}/main"       # inside the container
MNT="/mnt/jurisearch-staging"                      # bind-mount target inside the container

ts(){ date '+%Y-%m-%d %H:%M:%S'; }
log(){ echo "[$(ts)] $*"; }
die(){ echo "[$(ts)] FATAL: $*" >&2; echo "SENTINEL: LOADCORPUS-FAILED rc=1"; exit 1; }
trap 'die "unexpected error on line $LINENO"' ERR
ctexec(){ pct exec "$VMID" -- "$@"; }

# =================================================================================================
# 0. PRECONDITIONS — anything here aborts with nothing changed
# =================================================================================================
[ "$(id -u)" = 0 ] || die "must run as root on the bear host"
command -v pct >/dev/null || die "pct not found — run on the PVE host"
[ "$(pct status "$VMID" 2>/dev/null)" = "status: running" ] || die "CT $VMID is not running"

# 0a. staging must be a stopped, self-contained PG18 PGDATA
[ -f "$STAGING/PG_VERSION" ] || die "$STAGING is not a PGDATA (no PG_VERSION)"
[ "$(cat "$STAGING/PG_VERSION")" = "$PGVER" ] || die "staging PG_VERSION != $PGVER"
[ -d "$STAGING/base" ] && [ -d "$STAGING/global" ] || die "staging missing base/ or global/"
[ ! -f "$STAGING/postmaster.pid" ] || die "staging has postmaster.pid — source not cleanly stopped"
# reject external tablespaces / symlinked WAL: rsync -a would copy dangling symlinks -> broken cluster
[ -z "$(find "$STAGING/pg_tblspc" -mindepth 1 2>/dev/null)" ] || die "staging has external tablespaces (pg_tblspc not empty) — not a self-contained copy"
[ ! -L "$STAGING/pg_wal" ] || die "staging pg_wal is a symlink (external WAL) — not self-contained"
staging_sz=$(du -sh "$STAGING" 2>/dev/null | cut -f1); log "staging is a self-contained PG${PGVER} PGDATA ($staging_sz)"

# 0b. container PG18 stack must be complete (so the copied catalog's extensions actually load)
ctexec pg_lsclusters | awk 'NR>1{print $1,$2}' | grep -qx "${PGVER} main" || die "CT has no ${PGVER}/main cluster"
ctexec test -f "$DATADIR/PG_VERSION" || die "$DATADIR is not a cluster data dir (PG_VERSION missing)"
! ctexec systemctl is-active --quiet pgsearch-build || die "pgsearch-build unit still active — wait for the pg_search build to finish"
pkglib=$(ctexec /usr/lib/postgresql/"${PGVER}"/bin/pg_config --pkglibdir)
sharedir=$(ctexec /usr/lib/postgresql/"${PGVER}"/bin/pg_config --sharedir)
for art in "${pkglib}/pg_search.so" "${pkglib}/vector.so" \
           "${sharedir}/extension/pg_search.control" "${sharedir}/extension/vector.control"; do
  ctexec test -e "$art" || die "missing required extension artifact in CT: $art (build pg_search / install pgvector first)"
done
log "CT PG${PGVER} has pg_search + vector artifacts (.so + .control) installed"
# rsync must exist INSIDE the container (the minimal Debian template omits it) — check now so we fail
# in preconditions, never after stopping PostgreSQL.
ctexec sh -c 'command -v rsync >/dev/null' || die "rsync not installed in CT $VMID — run: pct exec $VMID -- apt-get install -y rsync"

# =================================================================================================
# 1. expose the staging read-only to the container (safe mp1 handling)
# =================================================================================================
# Make the staging tree readable/traversable through the UNPRIVILEGED bind mount: container-mapped
# root cannot otherwise read PostgreSQL's 0700 dirs / 0600 files. The mount is read-only and we chown
# only $DATADIR after the copy; the staging is a transient copy on the tailnet-only host.
log "making staging readable through the unprivileged bind mount (chmod -R a+rX)…"
chmod -R a+rX "$STAGING"

conf="/etc/pve/lxc/${VMID}.conf"
existing_mp1=$(grep -E '^mp1:' "$conf" 2>/dev/null || true)
if [ -n "$existing_mp1" ]; then
  # reuse ONLY if mp1 already points at our exact read-only staging + mountpoint; never clobber a foreign mp1
  if ! echo "$existing_mp1" | grep -qF "${STAGING}," || ! echo "$existing_mp1" | grep -qF "mp=${MNT}" || ! echo "$existing_mp1" | grep -qF "ro=1"; then
    die "mp1 present but not our exact read-only staging mount ('$existing_mp1') — refusing to clobber"
  fi
  log "reusing existing staging bind mount (mp1)"
else
  log "adding read-only bind mount mp1 ($STAGING -> $MNT) and rebooting CT to apply…"
  pct set "$VMID" -mp1 "${STAGING},mp=${MNT},ro=1"
  pct reboot "$VMID"
  for _ in $(seq 1 90); do ctexec test -e "$MNT/PG_VERSION" 2>/dev/null && break; sleep 2; done
fi
# verify the mount: present, the expected PG18 PGDATA, genuinely read-only, and DEEP-readable
ctexec test -f "$MNT/PG_VERSION" || die "bind mount $MNT not visible inside CT"
[ "$(ctexec cat "$MNT/PG_VERSION")" = "$PGVER" ] || die "$MNT is not a PG${PGVER} PGDATA"
opts=$(ctexec findmnt -rno OPTIONS "$MNT" 2>/dev/null || true)
echo ",${opts}," | grep -qE ',ro,' || die "$MNT is not mounted read-only (options: ${opts:-unknown})"
# prove a deep 0600 file is actually readable through the unprivileged mount (not just PG_VERSION)
ctexec test -r "$MNT/global/pg_control" || die "$MNT/global/pg_control not readable inside CT — staging not fully readable through the mount"
log "staging visible at $MNT inside CT, read-only, PG${PGVER}, deep files readable"

# =================================================================================================
# 2. stop PG18, PROVE it is down, then replace 18/main with the corpus
# =================================================================================================
log "stopping PG${PGVER}…"
ctexec pg_ctlcluster "$PGVER" main stop || true
# ASSERT the cluster is actually down before any destructive write (status == 0 means running)
! ctexec pg_ctlcluster "$PGVER" main status >/dev/null 2>&1 || die "PG${PGVER} still running after stop — refusing to overwrite a live data dir"
! ctexec test -f "$DATADIR/postmaster.pid" || die "postmaster.pid present in $DATADIR after stop — cluster may be live"
log "PG${PGVER} confirmed down; copying the corpus over $DATADIR…"

ctexec bash -c "
  set -e
  rsync -a --delete '$MNT/' '$DATADIR/'
  chown -R postgres:postgres '$DATADIR'
  chmod 700 '$DATADIR'
  rm -f '$DATADIR/postmaster.pid'
  printf \"shared_preload_libraries = 'pg_search'\n\" > '$DATADIR/postgresql.auto.conf'
  chown postgres:postgres '$DATADIR/postgresql.auto.conf'
"
log "corpus copied into $DATADIR; postgresql.auto.conf neutralised (pg_search preload only)"

# ---- 2b. ensure the corpus's libc locale(s) exist, else PG${PGVER} refuses to start ----
# (the corpus was initialised with a libc locale, e.g. en_US.UTF-8, that the minimal CT may lack)
collate=$(ctexec /usr/lib/postgresql/"${PGVER}"/bin/pg_controldata "$MNT" 2>/dev/null | sed -nE 's/^LC_COLLATE: +(.+)/\1/p' | tr -d '[:space:]')
ctype=$(ctexec /usr/lib/postgresql/"${PGVER}"/bin/pg_controldata "$MNT" 2>/dev/null | sed -nE 's/^LC_CTYPE: +(.+)/\1/p' | tr -d '[:space:]')
log "corpus locale: LC_COLLATE=${collate:-?} LC_CTYPE=${ctype:-?}"
need_gen=0
for loc in "$collate" "$ctype"; do
  case "$loc" in ""|C|POSIX|C.UTF-8|C.utf8) continue ;; esac
  ctexec bash -c "grep -qxiF '$loc UTF-8' /etc/locale.gen 2>/dev/null || echo '$loc UTF-8' >> /etc/locale.gen"
  need_gen=1
done
if [ "$need_gen" = 1 ]; then
  log "generating the corpus libc locale(s) in the container…"
  ctexec bash -c "DEBIAN_FRONTEND=noninteractive apt-get install -y locales >/dev/null 2>&1; locale-gen >/dev/null 2>&1"
fi

# =================================================================================================
# 3. start PG18 on the corpus and VERIFY it really works
# =================================================================================================
log "starting PG${PGVER} on the copied corpus…"
ctexec pg_ctlcluster "$PGVER" main start
ready=0
for _ in $(seq 1 60); do
  if ctexec su - postgres -c "psql -tAc 'select 1'" >/dev/null 2>&1; then ready=1; break; fi
  sleep 2
done
[ "$ready" = 1 ] || die "PG${PGVER} did not accept connections after the swap — check /var/log/postgresql"

log "=== verification ==="
ctexec su - postgres -c "psql -P pager=off -c \"SELECT datname, pg_size_pretty(pg_database_size(datname)) FROM pg_database WHERE datistemplate=false ORDER BY pg_database_size(datname) DESC;\""
corpusdb=$(ctexec su - postgres -c "psql -P pager=off -tAc \"SELECT datname FROM pg_database WHERE datistemplate=false AND datname<>'postgres' ORDER BY pg_database_size(oid) DESC LIMIT 1;\"" | tr -d '[:space:]')
[ -n "$corpusdb" ] || die "no corpus database found after the copy"
log "corpus database: ${corpusdb} — extensions:"
ctexec su - postgres -c "psql -P pager=off -d ${corpusdb} -c '\\dx'"
# force the vector C library to actually load (a copied catalog can LIST vector even if vector.so is broken)
ctexec su - postgres -c "psql -d ${corpusdb} -tAc \"SELECT '[1,2,3]'::vector;\"" >/dev/null || die "vector type failed to load in ${corpusdb} (vector.so problem)"
# pg_search is loaded at startup via shared_preload_libraries (the server would not have started otherwise);
# confirm the extension is present in the corpus catalog
ctexec su - postgres -c "psql -d ${corpusdb} -tAc \"SELECT extversion FROM pg_extension WHERE extname='pg_search';\"" | grep -q . \
  || die "pg_search extension missing from ${corpusdb} catalog"
log "verified: ${corpusdb} online; vector loads; pg_search present"
# the corpus may have been built on a different glibc collation version — sync it to this OS so PG
# stops warning (a conservative REINDEX of collation-dependent btree indexes is an optional follow-up).
log "refreshing collation version on all databases…"
ctexec su - postgres -c "psql -tAc \"SELECT datname FROM pg_database WHERE datallowconn\"" | tr -d '[:space:]\r' | while read -r db; do
  [ -n "$db" ] || continue
  printf 'ALTER DATABASE "%s" REFRESH COLLATION VERSION;\n' "$db"
done | ctexec su - postgres -c "psql -f -" >/dev/null 2>&1 || true
echo "SENTINEL: LOADCORPUS-DONE rc=0 corpusdb=${corpusdb}"
log "NEXT: sample a real BM25/vector query on '${corpusdb}'; optionally 'ALTER EXTENSION vector UPDATE;'."
log "      remove staging mount when satisfied: pct set $VMID --delete mp1 && pct reboot $VMID && rm -rf $STAGING"
