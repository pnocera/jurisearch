#!/usr/bin/env bash
# work/09 P6 — single-host operated acceptance for the SHIPPED binaries (`jurisearch serve-site` +
# `jurisearch-client`). Runs the real binaries end-to-end and emits a fill-in-ready OBSERVED block for
# `05-two-host-acceptance.md`. Single-host: producer/site/client collapsed onto one machine.
#
# It ALWAYS runs the legs that need no server/DB (the shipped client's contract + connection diagnostics).
# It runs the data legs (status/fetch/search against a live `serve-site`) only when the two operator
# prerequisites are present, and prints EXACTLY which one is missing otherwise — it never fabricates a
# data-leg result.
#
#   PREREQ-DB     a reachable Postgres (pgvector + pg_search) that is MIGRATED + role-provisioned + has at
#                 least one ACTIVE, readiness-stamped corpus (the syncd catch-up target, or a seed).
#   PREREQ-EMBED  an embedder the site can start from env: JURISEARCH_EMBED_BASE_URL / _MODEL / _API_KEY
#                 (e.g. OpenRouter baai/bge-m3) AND a local tokenizer (JURISEARCH_EMBED_TOKENIZER_JSON or a
#                 cached model), since `serve-site` builds its embedder at startup even though status/fetch
#                 and a bm25 search never call it.
#
# Usage:
#   work/09-jurisearch-cli/scripts/single-host-acceptance.sh [--bind 127.0.0.1:8099] [--fetch-id <id>] \
#       [--db-host H --db-port P --db-name N --db-user U --db-password W]
set -uo pipefail

BIND="127.0.0.1:8099"
FETCH_ID=""
DB_HOST="127.0.0.1"; DB_PORT="5432"; DB_NAME="jurisearch"; DB_USER="jurisearch_read"; DB_PASSWORD="${PGPASSWORD:-}"
while [ $# -gt 0 ]; do
  case "$1" in
    --bind) BIND="$2"; shift 2;;
    --fetch-id) FETCH_ID="$2"; shift 2;;
    --db-host) DB_HOST="$2"; shift 2;;
    --db-port) DB_PORT="$2"; shift 2;;
    --db-name) DB_NAME="$2"; shift 2;;
    --db-user) DB_USER="$2"; shift 2;;
    --db-password) DB_PASSWORD="$2"; shift 2;;
    *) echo "unknown arg: $1" >&2; exit 64;;
  esac
done

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
cd "$ROOT" || { echo "cannot cd to repo root $ROOT" >&2; exit 1; }
SECTION() { printf '\n========== %s ==========\n' "$1"; }

SECTION "build the shipped binaries"
cargo build -p jurisearch-cli -p jurisearch-client --bins || exit 1
JS="$ROOT/target/debug/jurisearch"
JC="$ROOT/target/debug/jurisearch-client"
"$JS" --version 2>/dev/null || true
"$JC" --help 2>&1 | head -3 || true

FAIL=0
# A NEGATIVE leg: the client MUST reject it with non-zero exit AND the EXPECTED diagnostic. Checking the
# exit code alone is not enough — these run against a dead port, so a regression where the client FORWARDS
# `index_dir`/`zone` instead of rejecting it locally would still be non-zero (connection refused). The
# stderr substring is what proves the LOCAL contract rejection (the connection error never contains it).
expect_reject() { local want="$1"; shift; echo "+ $* (expect: rejected — \"$want\")"
  local err; err="$("$@" 2>&1 >/dev/null)"; local rc=$?; echo "$err"; echo "exit=$rc"
  { [ "$rc" -ne 0 ] && printf '%s' "$err" | grep -qF -- "$want"; } \
    || { echo "  !! ACCEPTANCE FAIL: expected a non-zero exit with \"$want\""; FAIL=1; }; echo; }
# A POSITIVE leg: the command MUST succeed (zero exit). Any non-zero is an acceptance FAILURE (never a
# silent green) — this is what makes a malformed-but-signature-matching stamp, or a serve-site that died,
# surface as a failed run instead of a misleading "done".
must_run() { echo "+ $*"; "$@"; local rc=$?; echo "exit=$rc"
  [ "$rc" -eq 0 ] || { echo "  !! ACCEPTANCE FAIL: expected success"; FAIL=1; }; echo; }

SECTION "client legs that need NO server (shipped-binary contract + connection diagnostics)"
# The shared contract seam (`Operation::parse_args`) rejects a typo / server-owned / local-only field
# at the client BEFORE any round-trip — the SAME validation the site handler applies. Each leg asserts the
# contract's OWN diagnostic, so a local rejection is distinguished from a mere connection failure.
expect_reject "not a site query operation" "$JC" --server "tcp://127.0.0.1:1" bogus-op '{}'
expect_reject "index_dir"                  "$JC" --server "tcp://127.0.0.1:1" search '{"query":"x","index_dir":"/tmp"}'
expect_reject "zone"                       "$JC" --server "tcp://127.0.0.1:1" search '{"query":"x","zone":"motivations"}'
expect_reject "cannot reach"               "$JC" --server "tcp://127.0.0.1:1" status   # unreachable
expect_reject "tcp://host:port"            "$JC" --server "127.0.0.1:8099"    status   # bare host:port

# ---- preflight the two data-leg prerequisites ------------------------------------------------------
SECTION "preflight: data-leg prerequisites"
DB_OK=0; EMBED_OK=0
export PGPASSWORD="$DB_PASSWORD"
# Validate the SAME writer-owned readiness stamp the site read gate keys on: an ACTIVE corpus AND a
# `public.index_manifest['query_readiness']` whose embedded `signature` still matches the active
# `corpus:active_generation:sequence` topology (so an active-but-unstamped or STALE DB is caught here,
# not deep inside a failing data leg). The gate's coverage recompute stays authoritative at request time.
DB_STATE=$(psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" -tAc "
  WITH sig AS (
    SELECT coalesce(string_agg(corpus||':'||active_generation||':'||sequence::text, ',' ORDER BY corpus),
                    'public') AS active,
           count(*) FILTER (WHERE active_generation IS NOT NULL) AS n
    FROM jurisearch_control.corpus_state),
  stamp AS (
    SELECT value->>'signature' AS sig,
           -- the read gate parses the WHOLE cached object {signature, report:{projection_coverage,
           -- embedding_coverage}}; a stamp with a matching signature but a missing/malformed report is
           -- still rejected there, so require the report sub-objects here too (no false-green).
           (value ? 'signature' AND value ? 'report'
            AND value->'report' ? 'projection_coverage'
            AND value->'report' ? 'embedding_coverage') AS well_formed
    FROM public.index_manifest WHERE key='query_readiness')
  SELECT CASE
    WHEN (SELECT n FROM sig) < 1                              THEN 'no_active'
    WHEN (SELECT sig FROM stamp) IS NULL                      THEN 'unstamped'
    WHEN NOT (SELECT well_formed FROM stamp)                  THEN 'malformed'
    WHEN (SELECT sig FROM stamp) <> (SELECT active FROM sig)  THEN 'stale'
    ELSE 'ready' END;" 2>/dev/null)
case "${DB_STATE:-unreachable}" in
  ready)     echo "PREREQ-DB    OK   (active + well-formed readiness stamp matching the active topology)"; DB_OK=1;;
  no_active) echo "PREREQ-DB    MISSING   no active corpus at $DB_USER@$DB_HOST:$DB_PORT/$DB_NAME — syncd has not caught up";;
  unstamped) echo "PREREQ-DB    MISSING   active corpus but NO query_readiness stamp (writer never stamped / was invalidated)";;
  malformed) echo "PREREQ-DB    MISSING   query_readiness stamp is MALFORMED (signature present but report sub-objects missing)";;
  stale)     echo "PREREQ-DB    MISSING   query_readiness stamp is STALE (signature != active topology) — a re-stamp is due";;
  *)         echo "PREREQ-DB    MISSING   cannot reach a migrated DB at $DB_USER@$DB_HOST:$DB_PORT/$DB_NAME (no jurisearch_control/index_manifest)";;
esac
[ "$DB_OK" = 1 ] || echo "             the preflight is a best-effort early skip; the site read gate stays authoritative at request time."
[ "$DB_OK" = 1 ] || echo "             provision via syncd catch-up to the producer head (shared-server mode), then re-run."
if [ -n "${JURISEARCH_EMBED_BASE_URL:-}" ] && [ -n "${JURISEARCH_EMBED_TOKENIZER_JSON:-}" ]; then
  echo "PREREQ-EMBED OK   (base_url + tokenizer set)"; EMBED_OK=1
else
  echo "PREREQ-EMBED MISSING   set JURISEARCH_EMBED_BASE_URL/_MODEL/_API_KEY + a local tokenizer"
  echo "             (JURISEARCH_EMBED_TOKENIZER_JSON, or a cached model). serve-site builds its"
  echo "             embedder at startup even though status/fetch/bm25 never call it."
fi

if [ "$DB_OK" != 1 ] || [ "$EMBED_OK" != 1 ]; then
  SECTION "data legs SKIPPED — a prerequisite is missing (see above)"
  echo "The shipped serve-site SERVICE path (handlers, dispatch, read gate, full op set, render parity)"
  echo "is proven by the AUTOMATED in-process E2E; the shipped serve-site PROCESS run needs the two"
  echo "prerequisites above (run this script where they exist):"
  echo "  cargo test -p jurisearch-cli --bins site::tests::"
  echo "  cargo test -p jurisearch-client            # cli_acceptance + dependency_cone"
  # A SKIP is legitimate, but a regressed NEGATIVE client leg above is still a hard failure.
  [ "$FAIL" -eq 0 ] || { echo; echo "!! ACCEPTANCE FAIL: a client diagnostic leg regressed (see !! above)"; exit 1; }
  exit 0
fi

# ---- data legs: a live serve-site + the shipped client --------------------------------------------
# A complete shipped-serve-site capture REQUIRES a known document id (the fetch + fetch-hash legs are part
# of the acceptance, per the runbook). Refuse to enter the data-leg branch without one rather than print
# "all legs passed" having silently skipped fetch/hash.
if [ -z "$FETCH_ID" ]; then
  SECTION "ACCEPTANCE FAILED — data legs need --fetch-id <a known document id> for the fetch + hash leg"
  echo "Prerequisites are present, but a complete capture is status + fetch + fetch-hash + bm25 search."
  echo "Pick a real id from the corpus (e.g. via a status/DB lookup) and re-run with --fetch-id <id>."
  exit 1
fi

SECTION "start the shipped serve-site against the live DB"
PW_ARGS=(); [ -n "$DB_PASSWORD" ] && PW_ARGS=(--db-password "$DB_PASSWORD")  # omit on peer/trusted-socket auth
"$JS" serve-site --tcp "$BIND" \
  --db-host "$DB_HOST" --db-port "$DB_PORT" --db-name "$DB_NAME" --db-user "$DB_USER" "${PW_ARGS[@]}" \
  >/tmp/jurisearch-site.out 2>/tmp/jurisearch-site.err &
SITE_PID=$!
trap '[ -n "${SITE_PID:-}" ] && kill "$SITE_PID" 2>/dev/null' EXIT
# Wait for a real bind, and FAIL HARD if serve-site exits or never binds (embedder-from-env error, DB
# refusal, bad bind) — never proceed to "answer" legs against a dead server and call it done.
BOUND=0
for _ in $(seq 1 40); do
  grep -q "listening on" /tmp/jurisearch-site.err 2>/dev/null && { BOUND=1; break; }
  kill -0 "$SITE_PID" 2>/dev/null || break          # the process exited before binding
  sleep 0.3
done
cat /tmp/jurisearch-site.err
if [ "$BOUND" != 1 ]; then
  echo "  !! ACCEPTANCE FAIL: serve-site did not bind (see stderr above) — data legs not attempted"
  exit 1
fi

SITE_URL="tcp://$BIND"
SECTION "data legs via the shipped client (status / fetch / bm25 search) — each MUST succeed"
must_run "$JC" --server "$SITE_URL" status
must_run "$JC" --server "$SITE_URL" fetch "{\"ids\":[\"$FETCH_ID\"]}"
echo "fetch sha256 (host C, this client):"
if ! "$JC" --server "$SITE_URL" fetch "{\"ids\":[\"$FETCH_ID\"]}" | sha256sum; then
  echo "  !! ACCEPTANCE FAIL: fetch for the hash leg failed"; FAIL=1
fi
# A bm25 search exercises the read path WITHOUT the dense embedder (lexical only).
must_run "$JC" --server "$SITE_URL" search '{"query":"responsabilite","mode":"bm25","kind":"decision"}'

if [ "$FAIL" -ne 0 ]; then
  SECTION "ACCEPTANCE FAILED — one or more legs did not behave as required (see !! lines above)"
  exit 1
fi
SECTION "done — all legs passed; paste the captures above into the OBSERVED block of 05-two-host-acceptance.md"
