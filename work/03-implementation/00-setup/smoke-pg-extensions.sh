#!/usr/bin/env bash
set -euo pipefail

# Validate the PostgreSQL extension runtime jurisearch needs for the 0.3 backend spike:
#   - pgrx-managed PostgreSQL 18
#   - pg_search installed in that prefix
#   - pgvector copied/installed into that same prefix
#   - CREATE EXTENSION vector; CREATE EXTENSION pg_search;
#
# The cluster is disposable and is removed after the smoke completes.

PG_MAJOR="${PG_MAJOR:-18}"
PG_CONFIG="${PG_CONFIG:-}"

if [[ -z "$PG_CONFIG" || ! -x "$PG_CONFIG" ]]; then
  PG_CONFIG="$(find "$HOME/.pgrx" -path "*/bin/pg_config" -type f 2>/dev/null \
    | grep -E "/(${PG_MAJOR}|${PG_MAJOR}[.][0-9]+)/" \
    | sort -V \
    | tail -1)"
fi

if [[ -z "${PG_CONFIG:-}" || ! -x "$PG_CONFIG" ]]; then
  echo "error: no executable pg_config found for PostgreSQL $PG_MAJOR under ~/.pgrx" >&2
  echo "hint: run ./build-pg-search.sh first" >&2
  exit 4
fi

BINDIR="$("$PG_CONFIG" --bindir)"
PKGLIBDIR="$("$PG_CONFIG" --pkglibdir)"
SHAREDIR="$("$PG_CONFIG" --sharedir)"

if [[ ! -f "$PKGLIBDIR/pg_search.so" || ! -f "$SHAREDIR/extension/pg_search.control" ]]; then
  echo "error: pg_search is not installed in $("$PG_CONFIG" --version) prefix" >&2
  echo "hint: run ./build-pg-search.sh first" >&2
  exit 4
fi

if [[ ! -f "$PKGLIBDIR/vector.so" || ! -f "$SHAREDIR/extension/vector.control" ]]; then
  if [[ ! -f /usr/lib64/pgsql/vector.so || ! -f /usr/share/pgsql/extension/vector.control ]]; then
    echo "error: pgvector is neither installed in the pgrx prefix nor available under /usr" >&2
    exit 4
  fi
  cp -av /usr/lib64/pgsql/vector.so "$PKGLIBDIR/"
  cp -av /usr/share/pgsql/extension/vector* "$SHAREDIR/extension/"
fi

TMPDIR="$(mktemp -d /tmp/jurisearch-pg-smoke.XXXXXX)"
PGDATA="$TMPDIR/data"
SOCKDIR="$TMPDIR/sock"
LOG="$TMPDIR/postgres.log"
PORT="${JURISEARCH_PG_SMOKE_PORT:-$(shuf -i 55432-59999 -n 1)}"
mkdir -p "$SOCKDIR"

cleanup() {
  status=$?
  "$BINDIR/pg_ctl" -D "$PGDATA" -m fast stop >/dev/null 2>&1 || true
  if [[ "$status" -eq 0 ]]; then
    rm -rf "$TMPDIR"
  else
    echo "smoke failed; preserving temp dir: $TMPDIR" >&2
    if [[ -f "$LOG" ]]; then
      echo "postgres log:" >&2
      sed -n '1,160p' "$LOG" >&2 || true
    fi
  fi
  exit "$status"
}
trap cleanup EXIT

"$BINDIR/initdb" -D "$PGDATA" --auth=trust --username=postgres >/dev/null
cat >> "$PGDATA/postgresql.conf" <<EOF
shared_preload_libraries = 'pg_search'
listen_addresses = '127.0.0.1'
port = $PORT
unix_socket_directories = '$SOCKDIR'
EOF

"$BINDIR/pg_ctl" -D "$PGDATA" -l "$LOG" start -w >/dev/null

"$BINDIR/psql" -h 127.0.0.1 -p "$PORT" -U postgres -d postgres -v ON_ERROR_STOP=1 <<'SQL'
CREATE EXTENSION vector;
CREATE EXTENSION pg_search;
SELECT extname, extversion
FROM pg_extension
WHERE extname IN ('vector', 'pg_search')
ORDER BY extname;

CREATE TABLE docs (id serial PRIMARY KEY, body text, embedding vector(3));
INSERT INTO docs (body, embedding)
VALUES
  ('responsabilite civile article 1240', '[1,0,0]'),
  ('recette de tarte aux pommes', '[0,1,0]');
SELECT id, body, embedding <-> '[1,0,0]' AS distance
FROM docs
ORDER BY embedding <-> '[1,0,0]'
LIMIT 1;
SQL

echo "PostgreSQL extension smoke passed with PG_CONFIG=$PG_CONFIG" >&2
