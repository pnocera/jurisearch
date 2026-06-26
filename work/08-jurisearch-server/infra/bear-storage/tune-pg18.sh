#!/usr/bin/env bash
#
# tune-pg18.sh — tune the standalone PG18 in CT 110 (the SERVER/producer profile) for performance, and
# rebuild the chunk_embeddings IVFFlat index at a corpus-sized `lists`. Runs INSIDE CT 110 as root.
#
# Scope note: these are SERVER values (CT 110 is a dedicated 48c/192 GB producer). Client machines are
# weaker and share RAM with local LLMs/embedding, so the jurisearch CODE defaults must be conservative
# and tunable — that is a separate change; this script is bear-only.
#
# Context: corpus DB `jurisearch` ~163 GB (4.64 M chunk embeddings); was on Debian defaults; the IVFFlat
# had lists=32 (→ ~13 s dense queries). This applies RAM/core-appropriate config and rebuilds the index
# at lists≈sqrt(rows).
#
# Reversible: config via ALTER SYSTEM (postgresql.auto.conf); the index rebuild is ATOMIC (a failed
# CREATE rolls back the DROP, keeping the old index). No corpus data is modified. Fails closed.
#
set -Eeuo pipefail
export LC_ALL=C.UTF-8 LANG=C.UTF-8

PGVER="${PGVER:-18}"
DB="${DB:-jurisearch}"
SHARED_BUFFERS="${SHARED_BUFFERS:-48GB}"
EFFECTIVE_CACHE="${EFFECTIVE_CACHE:-160GB}"
WORK_MEM="${WORK_MEM:-128MB}"                 # persistent default — safe with high parallelism (per review)
MAINT_WORK_MEM="${MAINT_WORK_MEM:-2GB}"       # persistent routine maintenance ceiling
AUTOVAC_WORK_MEM="${AUTOVAC_WORK_MEM:-1GB}"   # cap so autovacuum workers don't each grab MAINT_WORK_MEM
BUILD_MAINT_WORK_MEM="${BUILD_MAINT_WORK_MEM:-16GB}"  # session-local, only for the IVFFlat build
MAX_PAR_WORKERS="${MAX_PAR_WORKERS:-48}"
MAX_PAR_PER_GATHER="${MAX_PAR_PER_GATHER:-16}"
MAX_PAR_MAINT="${MAX_PAR_MAINT:-12}"
VERIFY_PROBES="${VERIFY_PROBES:-64}"

ts(){ date '+%Y-%m-%d %H:%M:%S'; }
log(){ echo "[$(ts)] $*"; }
die(){ echo "[$(ts)] FATAL: $*" >&2; echo "SENTINEL: TUNE-FAILED rc=1"; exit 1; }
trap 'die "unexpected error on line $LINENO"' ERR

# argv-style (no shell parsing of args/SQL), no login shell (avoids the locale banner). SQL via stdin.
psqlf(){
  if [ -n "${1:-}" ]; then runuser -u postgres -- psql -v ON_ERROR_STOP=1 -P pager=off -d "$1" -f -
  else runuser -u postgres -- psql -v ON_ERROR_STOP=1 -P pager=off -f -; fi
}
psqlc(){ runuser -u postgres -- psql -v ON_ERROR_STOP=1 -P pager=off -d "$1" -tAc "$2"; }

# ---- 0. preconditions ----
[ "$(id -u)" = 0 ] || die "must run as root in CT 110"
command -v pg_lsclusters >/dev/null || die "postgresql-common missing"
command -v runuser >/dev/null || die "runuser (util-linux) missing"
pg_lsclusters | awk 'NR>1{print $1,$2}' | grep -qx "${PGVER} main" || die "no ${PGVER}/main cluster"
psqlc "$DB" "select 1" >/dev/null || die "cannot connect to ${DB}"
rows=$(psqlc "$DB" "SELECT count(*) FROM chunk_embeddings")
[ "${rows:-0}" -gt 0 ] || die "chunk_embeddings is empty"
log "chunk_embeddings rows: ${rows}"

# ---- 1. corpus-sized IVFFlat lists (pgvector rule: rows/1000 up to 1M, else sqrt(rows)) ----
if [ "$rows" -le 1000000 ]; then LISTS=$(( rows / 1000 )); else LISTS=$(awk "BEGIN{printf \"%d\", sqrt($rows)}"); fi
[ "$LISTS" -ge 1 ] || LISTS=1
log "target IVFFlat lists = ${LISTS} (was 32)"

# ---- 2. apply SERVER performance config (ALTER SYSTEM -> postgresql.auto.conf) ----
log "applying PG${PGVER} server performance config for 48c / 192 GB…"
psqlf <<SQL
ALTER SYSTEM SET shared_buffers = '${SHARED_BUFFERS}';
ALTER SYSTEM SET effective_cache_size = '${EFFECTIVE_CACHE}';
ALTER SYSTEM SET work_mem = '${WORK_MEM}';
ALTER SYSTEM SET maintenance_work_mem = '${MAINT_WORK_MEM}';
ALTER SYSTEM SET autovacuum_work_mem = '${AUTOVAC_WORK_MEM}';
ALTER SYSTEM SET max_worker_processes = '${MAX_PAR_WORKERS}';
ALTER SYSTEM SET max_parallel_workers = '${MAX_PAR_WORKERS}';
ALTER SYSTEM SET max_parallel_workers_per_gather = '${MAX_PAR_PER_GATHER}';
ALTER SYSTEM SET max_parallel_maintenance_workers = '${MAX_PAR_MAINT}';
ALTER SYSTEM SET random_page_cost = '1.1';
ALTER SYSTEM SET effective_io_concurrency = '256';
ALTER SYSTEM SET max_wal_size = '16GB';
ALTER SYSTEM SET min_wal_size = '2GB';
ALTER SYSTEM SET checkpoint_completion_target = '0.9';
SQL

# ---- 3. restart (shared_buffers / worker pools need a restart) ----
log "restarting PG${PGVER} to apply shared_buffers…"
pg_ctlcluster "$PGVER" main restart
ready=0; for _ in $(seq 1 60); do psqlc "$DB" "select 1" >/dev/null 2>&1 && { ready=1; break; }; sleep 2; done
[ "$ready" = 1 ] || die "PG${PGVER} did not come back after restart"
log "shared_buffers now: $(psqlc "$DB" "show shared_buffers")"

# ---- 4. rebuild the chunk_embeddings IVFFlat at corpus-sized lists — ATOMIC (keep old index on failure) ----
log "rebuilding chunk_embeddings IVFFlat at lists=${LISTS} (atomic; parallel build; a few minutes)…"
psqlf "$DB" <<SQL
BEGIN;
SET LOCAL maintenance_work_mem = '${BUILD_MAINT_WORK_MEM}';
SET LOCAL max_parallel_maintenance_workers = ${MAX_PAR_MAINT};
DROP INDEX IF EXISTS chunk_embeddings_embedding_ivfflat_idx;
CREATE INDEX chunk_embeddings_embedding_ivfflat_idx ON chunk_embeddings USING ivfflat (embedding vector_l2_ops) WITH (lists = ${LISTS});
COMMIT;
SQL
log "index rebuilt; analyzing…"
psqlf "$DB" <<SQL
ANALYZE chunk_embeddings;
ANALYZE chunks;
SQL

# ---- 5. verify (fail-closed): a timed dense query should now be sub-second + use the index ----
log "verifying dense query speed (probes=${VERIFY_PROBES})…"
verify_out=$(psqlf "$DB" <<SQL
SET statement_timeout='90s';
SET ivfflat.probes=${VERIFY_PROBES};
\timing on
WITH seed AS (SELECT embedding FROM chunk_embeddings LIMIT 1)
SELECT count(*) FROM (SELECT chunk_id FROM chunk_embeddings ORDER BY embedding <-> (SELECT embedding FROM seed) LIMIT 10) x;
EXPLAIN (COSTS off) WITH seed AS (SELECT embedding FROM chunk_embeddings LIMIT 1)
SELECT chunk_id FROM chunk_embeddings ORDER BY embedding <-> (SELECT embedding FROM seed) LIMIT 10;
SQL
)
echo "$verify_out" | grep -iE "Time:|rows\)|Index Scan|Seq Scan" | head
echo "SENTINEL: TUNE-DONE rc=0 lists=${LISTS}"
