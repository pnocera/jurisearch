#!/usr/bin/env bash
set -euo pipefail

# Rebuild the local ParadeDB pg_search extension for the jurisearch backend spike.
#
# Default route:
#   - install the cargo-pgrx version pinned by /home/pierre/Work/paradedb
#   - use pgrx-managed PostgreSQL 18 under ~/.pgrx
#   - build pg_search with the matching ~/.pgrx/.../bin/pg_config
#
# This deliberately avoids Fedora system postgresql-server-devel because this
# machine has PGDG libpq5 from the pgAdmin repo, which conflicts with Fedora's
# postgresql-private-devel package.

PARADEDB_DIR="${PARADEDB_DIR:-/home/pierre/Work/paradedb}"
PG_MAJOR="${PG_MAJOR:-18}"
PGRX_INIT_MODE="${PGRX_INIT_MODE:-download}"

if [[ ! -d "$PARADEDB_DIR" ]]; then
  echo "error: PARADEDB_DIR does not exist: $PARADEDB_DIR" >&2
  exit 2
fi

if ! command -v cargo >/dev/null 2>&1; then
  echo "error: cargo is not on PATH" >&2
  exit 4
fi

PGRX_VERSION="${PGRX_VERSION:-$(perl -nE '/^pgrx\s+=\s"=?([^"]+)/ && do { say $1; exit }' "$PARADEDB_DIR/Cargo.toml")}"
if [[ -z "$PGRX_VERSION" ]]; then
  echo "error: could not determine pgrx version from $PARADEDB_DIR/Cargo.toml" >&2
  exit 4
fi

if ! cargo pgrx --version 2>/dev/null | grep -q "cargo-pgrx $PGRX_VERSION"; then
  echo "Installing cargo-pgrx $PGRX_VERSION" >&2
  cargo install --locked cargo-pgrx --version "$PGRX_VERSION"
fi

pg_config_from_pgrx() {
  find "$HOME/.pgrx" -path "*/bin/pg_config" -type f 2>/dev/null \
    | grep -E "/(${PG_MAJOR}|${PG_MAJOR}[.][0-9]+)/" \
    | sort -V \
    | tail -1
}

PG_CONFIG="${PG_CONFIG:-$(pg_config_from_pgrx)}"
if [[ -z "$PG_CONFIG" || ! -x "$PG_CONFIG" ]]; then
  echo "Initializing pgrx-managed PostgreSQL $PG_MAJOR via cargo pgrx init --pg${PG_MAJOR} ${PGRX_INIT_MODE}" >&2
  cargo pgrx init "--pg${PG_MAJOR}" "$PGRX_INIT_MODE"
  PG_CONFIG="$(pg_config_from_pgrx)"
fi

if [[ -z "$PG_CONFIG" || ! -x "$PG_CONFIG" ]]; then
  echo "error: no executable pg_config found under ~/.pgrx for PostgreSQL $PG_MAJOR" >&2
  exit 4
fi

echo "Using PARADEDB_DIR=$PARADEDB_DIR" >&2
echo "Using PG_CONFIG=$PG_CONFIG" >&2
"$PG_CONFIG" --version

cd "$PARADEDB_DIR"
PG_CONFIG="$PG_CONFIG" make pg-version
PG_CONFIG="$PG_CONFIG" make pgrx-version
PG_CONFIG="$PG_CONFIG" make package

cat >&2 <<EOF

pg_search package build completed.
PG_CONFIG used:
  $PG_CONFIG

To repeat exactly:
  PARADEDB_DIR="$PARADEDB_DIR" PG_CONFIG="$PG_CONFIG" "$0"
EOF

