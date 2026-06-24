#!/usr/bin/env bash
# Resolve the deduped legislation citations against the Legifrance API, ONE per unique citation, in
# resumable batches (each Legifrance response archived in official_api_responses; each resolution row
# recorded). Sequential (the PisteClient OAuth token cache is not shared across threads). Stops when no
# pending citations remain. Designed for a visible WezTerm window via run-in-wezterm.sh.
set -u
BIN=${BIN:-/home/pierre/Work/jurisearch/target/release/jurisearch}
CLONE=${CLONE:-/mnt/models/jurisearch-index/phase2-full-juridic.zone-rollout-20260624}
OPS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LOGDIR="$OPS_DIR/logs/legislation-batches"
mkdir -p "$LOGDIR"
BATCH=${BATCH:-2000}
MAX_BATCHES=${MAX_BATCHES:-30}      # backstop: <=60k unique citations
# Redo the ~1,300 rows attempted under the old broken body (now upstream_error) alongside the pending
# ones (default on). With --retry-errors the keyset page re-selects pending+error rows; the loop still
# self-terminates because the final batch (only persistent errors left) trips the err==considered break.
RETRY_ERRORS=${RETRY_ERRORS:-1}
RETRY_FLAG=""; [ "$RETRY_ERRORS" = "1" ] && RETRY_FLAG="--retry-errors"

b=0 total_considered=0 total_ok=0 total_nf=0 total_err=0
echo "=== legislation enrichment starting $(date -u +%H:%M:%S) (retry_errors=$RETRY_ERRORS) ==="
while [ "$b" -lt "$MAX_BATCHES" ]; do
  b=$((b+1))
  out="$LOGDIR/batch${b}.json"
  "$BIN" --index-dir "$CLONE" ingest enrich-legislation-citations $RETRY_FLAG --limit "$BATCH" \
      > "$out" 2>"$LOGDIR/batch${b}.err"
  rc=$?
  if [ "$rc" -ne 0 ]; then
    echo "batch $b FAILED rc=$rc:"; tail -3 "$LOGDIR/batch${b}.err"; break
  fi
  considered=$(jq -r '.considered // 0' "$out")
  ok=$(jq -r '.resolved_ok // 0' "$out")
  nf=$(jq -r '.not_found // 0' "$out")
  err=$(jq -r '.errors // 0' "$out")
  if [ "$considered" = "0" ]; then
    echo "batch $b: no pending citations remain -> done"; break
  fi
  total_considered=$((total_considered+considered)); total_ok=$((total_ok+ok))
  total_nf=$((total_nf+nf)); total_err=$((total_err+err))
  echo "batch $b: considered=$considered resolved_ok=$ok not_found=$nf errors=$err (cum ok=$total_ok nf=$total_nf err=$total_err) $(date -u +%H:%M:%S)"
  if [ "$err" = "$considered" ] && [ "$considered" -gt 0 ]; then
    echo "ALL of batch $b errored — likely a Legifrance auth/quota problem; stopping. See $out / note field:"
    jq -r '.note // "(no note)"' "$out"; break
  fi
done
echo "=== LEGISLATION ENRICH DONE: cum considered=$total_considered ok=$total_ok not_found=$total_nf errors=$total_err batches=$b $(date -u +%H:%M:%S) ==="
# Final by-status coverage is in the last batch's report .coverage (no re-collect needed):
[ -f "$out" ] && jq -c '.coverage // empty' "$out" 2>/dev/null || true
