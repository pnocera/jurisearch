#!/usr/bin/env bash
# Resolve the deduped legislation citations against the Legifrance API, ONE per unique citation, in
# resumable batches (each Legifrance response archived in official_api_responses; each resolution row
# recorded). Sequential (the PisteClient OAuth token cache is not shared across threads). Designed for a
# visible WezTerm window via run-in-wezterm.sh.
#
# TWO PASSES (codex r2 fix). Pass 1 resolves all `pending` rows; only when none remain does pass 2 retry
# rows previously left `upstream_error`/`parse_error` (e.g. the ~1,300 from the old broken body). Why not
# one combined --retry-errors loop: each shell batch is a fresh CLI process that keyset-selects from the
# START of the citation_key space, and the `err==considered` stop only inspects the SELECTED prefix. With
# pending+error mixed, a batch that happened to be all-error could trip that stop while pending rows sorted
# after it — stranding pending work. Pending-first removes the hazard: in pass 1 a batch is all-error only
# on a real auth/quota outage (the intended stop); in pass 2 there are no pending rows left to strand
# (only persistent errors re-loop, and they correctly trip err==considered). Set RETRY_ERRORS=0 to skip
# pass 2.
set -u
BIN=${BIN:-/home/pierre/Work/jurisearch/target/release/jurisearch}
CLONE=${CLONE:-/mnt/models/jurisearch-index/phase2-full-juridic.zone-rollout-20260624}
OPS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LOGDIR="$OPS_DIR/logs/legislation-batches"
mkdir -p "$LOGDIR"
BATCH=${BATCH:-2000}
MAX_BATCHES=${MAX_BATCHES:-30}      # backstop per pass: <=60k attempts
RETRY_ERRORS=${RETRY_ERRORS:-1}

# run_pass <label> [--retry-errors]
# Loops --limit BATCH invocations until a batch reports considered=0 (queue drained) or err==considered
# (every selected row errored). Returns non-zero only on a CLI failure.
run_pass () {
  local label="$1"; shift
  local extra=( "$@" )            # empty, or (--retry-errors)
  local b=0 c_total=0 ok_total=0 nf_total=0 err_total=0 out=""
  echo "--- pass [$label] starting $(date -u +%H:%M:%S) ---"
  while [ "$b" -lt "$MAX_BATCHES" ]; do
    b=$((b+1))
    out="$LOGDIR/${label}-batch${b}.json"
    "$BIN" --index-dir "$CLONE" ingest enrich-legislation-citations "${extra[@]}" --limit "$BATCH" \
        > "$out" 2>"$LOGDIR/${label}-batch${b}.err"
    local rc=$?
    if [ "$rc" -ne 0 ]; then
      echo "pass [$label] batch $b FAILED rc=$rc:"; tail -3 "$LOGDIR/${label}-batch${b}.err"; return 1
    fi
    local considered ok nf err
    considered=$(jq -r '.considered // 0' "$out")
    ok=$(jq -r '.resolved_ok // 0' "$out")
    nf=$(jq -r '.not_found // 0' "$out")
    err=$(jq -r '.errors // 0' "$out")
    if [ "$considered" = "0" ]; then
      echo "pass [$label] batch $b: queue drained -> pass done"; break
    fi
    c_total=$((c_total+considered)); ok_total=$((ok_total+ok)); nf_total=$((nf_total+nf)); err_total=$((err_total+err))
    echo "pass [$label] batch $b: considered=$considered ok=$ok nf=$nf err=$err (cum ok=$ok_total nf=$nf_total err=$err_total) $(date -u +%H:%M:%S)"
    if [ "$err" = "$considered" ] && [ "$considered" -gt 0 ]; then
      echo "pass [$label] batch $b: ALL errored — stopping pass (auth/quota outage, or only persistent errors remain). note:"
      jq -r '.note // "(no note)"' "$out"; break
    fi
  done
  echo "--- pass [$label] DONE: considered=$c_total ok=$ok_total nf=$nf_total err=$err_total batches=$b $(date -u +%H:%M:%S) ---"
  [ -f "$out" ] && jq -c '.coverage // empty' "$out" 2>/dev/null || true
}

echo "=== legislation enrichment starting $(date -u +%H:%M:%S) (retry_errors=$RETRY_ERRORS) ==="
run_pass pending
if [ "$RETRY_ERRORS" = "1" ]; then
  run_pass retry --retry-errors
fi
echo "=== LEGISLATION ENRICH DONE $(date -u +%H:%M:%S) ==="
