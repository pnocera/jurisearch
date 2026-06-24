#!/usr/bin/env bash
# Resolve the deduped legislation citations against the Legifrance API, ONE per unique citation, in
# resumable batches (each Legifrance response archived in official_api_responses; each resolution row
# recorded). Sequential (the PisteClient OAuth token cache is not shared across threads). Designed for a
# visible WezTerm window via run-in-wezterm.sh.
#
# TWO GATED PASSES (codex r2/r3 fix). Pass 1 resolves all `pending` rows; pass 2 retries rows previously
# left `upstream_error`/`parse_error` (e.g. the ~1,300 from the old broken body) ONLY IF pass 1 actually
# drained the pending queue. Why: each shell batch is a fresh CLI keyset-selecting from the START of the
# citation_key space, and the `err==considered` stop only inspects the SELECTED prefix. If pending+error
# were mixed (or pass 2 ran before pending drained), an all-error batch could trip that stop while pending
# rows sorted after it — stranding pending work. Gating pass 2 on `pending drained` removes the hazard.
# run_pass reports WHY it stopped in STOP_REASON: drained | all_error | cli_fail | max_batches.
#   - pending: only `drained` proceeds to retry; all_error (auth/quota outage) / max_batches / cli_fail abort.
#   - retry:   `drained` and `all_error` (only persistent errors remain) are both acceptable terminal states.
# Set RETRY_ERRORS=0 to skip pass 2. All passes are resumable (re-run continues from the remaining queue).
set -u
BIN=${BIN:-/home/pierre/Work/jurisearch/target/release/jurisearch}
CLONE=${CLONE:-/mnt/models/jurisearch-index/phase2-full-juridic.zone-rollout-20260624}
OPS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LOGDIR="$OPS_DIR/logs/legislation-batches"
mkdir -p "$LOGDIR"
BATCH=${BATCH:-2000}
MAX_BATCHES=${MAX_BATCHES:-30}      # backstop per pass: <=60k attempts
RETRY_ERRORS=${RETRY_ERRORS:-1}

STOP_REASON=""

# run_pass <label> [--retry-errors]
# Loops --limit BATCH invocations until a batch reports considered=0 (drained) or err==considered
# (all_error). Sets STOP_REASON to drained|all_error|cli_fail|max_batches. Returns non-zero only on
# cli_fail (so `run_pass … || abort` catches a real CLI failure); the caller inspects STOP_REASON for the
# rest.
run_pass () {
  local label="$1"; shift
  local extra=( "$@" )            # empty, or (--retry-errors)
  local b=0 c_total=0 ok_total=0 nf_total=0 err_total=0 out=""
  STOP_REASON="max_batches"       # default if the while loop exhausts its batch budget
  echo "--- pass [$label] starting $(date -u +%H:%M:%S) ---"
  while [ "$b" -lt "$MAX_BATCHES" ]; do
    b=$((b+1))
    out="$LOGDIR/${label}-batch${b}.json"
    "$BIN" --index-dir "$CLONE" ingest enrich-legislation-citations "${extra[@]}" --limit "$BATCH" \
        > "$out" 2>"$LOGDIR/${label}-batch${b}.err"
    local rc=$?
    if [ "$rc" -ne 0 ]; then
      echo "pass [$label] batch $b FAILED rc=$rc:"; tail -3 "$LOGDIR/${label}-batch${b}.err"
      STOP_REASON="cli_fail"; return 1
    fi
    local considered ok nf err
    considered=$(jq -r '.considered // 0' "$out")
    ok=$(jq -r '.resolved_ok // 0' "$out")
    nf=$(jq -r '.not_found // 0' "$out")
    err=$(jq -r '.errors // 0' "$out")
    if [ "$considered" = "0" ]; then
      echo "pass [$label] batch $b: queue drained"; STOP_REASON="drained"; break
    fi
    c_total=$((c_total+considered)); ok_total=$((ok_total+ok)); nf_total=$((nf_total+nf)); err_total=$((err_total+err))
    echo "pass [$label] batch $b: considered=$considered ok=$ok nf=$nf err=$err (cum ok=$ok_total nf=$nf_total err=$err_total) $(date -u +%H:%M:%S)"
    if [ "$err" = "$considered" ] && [ "$considered" -gt 0 ]; then
      echo "pass [$label] batch $b: ALL errored. note:"; jq -r '.note // "(no note)"' "$out"
      STOP_REASON="all_error"; break
    fi
  done
  echo "--- pass [$label] DONE (stop=$STOP_REASON): considered=$c_total ok=$ok_total nf=$nf_total err=$err_total batches=$b $(date -u +%H:%M:%S) ---"
  [ -f "$out" ] && jq -c '.coverage // empty' "$out" 2>/dev/null || true
  return 0
}

echo "=== legislation enrichment starting $(date -u +%H:%M:%S) (retry_errors=$RETRY_ERRORS) ==="

run_pass pending || { echo "=== ABORT: pending pass CLI failure ==="; exit 1; }
if [ "$STOP_REASON" != "drained" ]; then
  echo "=== ABORT: pending pass did NOT drain (stop=$STOP_REASON) -> NOT starting retry pass (would risk stranding pending). Fix the cause and re-run. ==="
  exit 1
fi

if [ "$RETRY_ERRORS" = "1" ]; then
  run_pass retry --retry-errors || { echo "=== ABORT: retry pass CLI failure ==="; exit 1; }
  case "$STOP_REASON" in
    drained|all_error) ;;  # acceptable: queue empty, or only persistent errors remain
    max_batches) echo "=== NOTE: retry pass hit MAX_BATCHES — some persistent errors may remain; re-run to continue ===" ;;
  esac
fi

echo "=== LEGISLATION ENRICH DONE $(date -u +%H:%M:%S) ==="
