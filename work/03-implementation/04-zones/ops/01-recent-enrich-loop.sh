#!/usr/bin/env bash
# Recent-first zone enrichment (now archiving EVERY official-API exchange into official_api_responses,
# v16). Walks newest->oldest in batches; stops a source when 2 consecutive batches yield <1% official_ok,
# when dry, or at the safety backstop. The by-status report is the coverage measurement. Resumable:
# every attempt writes a decision_zones row (+ archive rows), so a re-run skips fresh rows.
# Designed to run inside a visible WezTerm window via run-in-wezterm.sh (stdout is tee'd to a log).
set -u
BIN=${BIN:-/home/pierre/Work/jurisearch/target/release/jurisearch}
CLONE=${CLONE:-/mnt/models/jurisearch-index/phase2-full-juridic.zone-rollout-20260624}
OPS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LOGDIR="$OPS_DIR/logs/enrich-batches"
mkdir -p "$LOGDIR"
BATCH=${BATCH:-5000}
CONC=${CONC:-6}
MAX_BATCHES=${MAX_BATCHES:-40}     # backstop: <=200k decisions/source
LOW_YIELD=0.01                      # <1% official_ok => "collapsed"

run_source() {
  local source="$1"
  local consec_low=0 b=0 total_considered=0 total_ok=0
  echo "=== [$source] recent-first enrichment starting $(date -u +%H:%M:%S) ==="
  while [ "$b" -lt "$MAX_BATCHES" ]; do
    b=$((b+1))
    local out="$LOGDIR/${source}-batch${b}.json"
    "$BIN" --index-dir "$CLONE" ingest enrich-zones \
        --source "$source" --order recent --limit "$BATCH" --concurrency "$CONC" \
        > "$out" 2>"$LOGDIR/${source}-batch${b}.err"
    local rc=$?
    if [ "$rc" -ne 0 ]; then
      echo "[$source] batch $b FAILED rc=$rc; stderr:"; tail -3 "$LOGDIR/${source}-batch${b}.err"
      break
    fi
    local considered official errors
    considered=$(jq -r '.considered // 0' "$out")
    official=$(jq -r '.official_ok // 0' "$out")
    errors=$(jq -r '.errors // 0' "$out")
    if [ "$considered" = "0" ]; then
      echo "[$source] batch $b: dry (no more candidates) -> done"; break
    fi
    total_considered=$((total_considered + considered))
    total_ok=$((total_ok + official))
    local yield
    yield=$(awk -v o="$official" -v c="$considered" 'BEGIN{ if(c>0) printf "%.4f", o/c; else print "0" }')
    echo "[$source] batch $b: considered=$considered official_ok=$official errors=$errors yield=$yield (cum ok=$total_ok) $(date -u +%H:%M:%S)"
    local low
    low=$(awk -v y="$yield" -v t="$LOW_YIELD" 'BEGIN{ print (y < t) ? 1 : 0 }')
    if [ "$low" = "1" ]; then consec_low=$((consec_low+1)); else consec_low=0; fi
    if [ "$consec_low" -ge 2 ]; then
      echo "[$source] yield collapsed (<${LOW_YIELD} x2) -> stop; cum considered=$total_considered ok=$total_ok"; break
    fi
  done
  echo "=== [$source] done: cum considered=$total_considered ok=$total_ok batches=$b $(date -u +%H:%M:%S) ==="
}

run_source cass
run_source inca
echo "=== ALL ENRICH DONE $(date -u +%H:%M:%S) ==="
"$BIN" --index-dir "$CLONE" status 2>/dev/null | jq '.zone_retrieval | {decision_zones, zone_units, embeddings, resolver_reachable}'
echo "=== archive (official_api_responses) counts via status not exposed; see ops/02 for build/embed/eval ==="
