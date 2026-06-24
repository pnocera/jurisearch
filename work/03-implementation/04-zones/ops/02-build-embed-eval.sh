#!/usr/bin/env bash
# Post-enrichment: derive zone_units -> embed via OpenRouter (fingerprint bge-m3:1024:normalize:true,
# request alias baai/bge-m3) -> verify dense index -> measured eval. OpenRouter pool is used for BOTH
# embed and eval/query so the embedded space matches the query embedder. Run after enrichment (and after
# the Legifrance pass if desired). Designed for a visible WezTerm window via run-in-wezterm.sh.
set -u
BIN=${BIN:-/home/pierre/Work/jurisearch/target/release/jurisearch}
CLONE=${CLONE:-/mnt/models/jurisearch-index/phase2-full-juridic.zone-rollout-20260624}
OPS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LOGDIR="$OPS_DIR/logs/build-embed"
mkdir -p "$LOGDIR"
# Run-in-wezterm launches with cwd=$HOME, so cd to the repo root (ops is at work/03-…/04-zones/ops, 4 up)
# to make the relative `eval --out work/…` artifact path resolve correctly.
REPO="$(cd "$OPS_DIR/../../../.." && pwd)"
cd "$REPO" || { echo "cannot cd to repo root $REPO"; exit 1; }
export JURISEARCH_EMBED_POOL="https://openrouter.ai/api/v1|baai/bge-m3|OPENROUTER_API_KEY"

echo "=== 1. build-zone-units $(date -u +%H:%M:%S) ==="
"$BIN" --index-dir "$CLONE" ingest build-zone-units > "$LOGDIR/build.json" 2>"$LOGDIR/build.err" \
  || { echo "build FAILED"; tail -5 "$LOGDIR/build.err"; exit 1; }
jq -c 'del(.coverage)' "$LOGDIR/build.json"

UNITS=$("$BIN" --index-dir "$CLONE" status 2>/dev/null | jq -r '.zone_retrieval.zone_units.total // 0')
echo "zone_units.total=$UNITS"
[ "$UNITS" = "0" ] && { echo "no zone_units -> stop"; exit 1; }
LISTS=$(awk -v n="$UNITS" 'BEGIN{ l=int(sqrt(n)+0.5); if(l<32)l=32; if(l>2000)l=2000; print l }')

echo "=== 2. embed-zone-units via OpenRouter (lists=$LISTS) $(date -u +%H:%M:%S) ==="
attempt=0
until "$BIN" --index-dir "$CLONE" ingest embed-zone-units \
        --index-lists "$LISTS" --batch-size 32 --pool-concurrency 8 \
        > "$LOGDIR/embed.json" 2>"$LOGDIR/embed.err"; do
  attempt=$((attempt+1)); echo "embed attempt $attempt aborted (likely 429):"; tail -3 "$LOGDIR/embed.err"
  [ "$attempt" -ge 40 ] && { echo "too many embed retries -> stop"; exit 1; }
  sleep 30
done
jq -c 'del(.coverage)' "$LOGDIR/embed.json" 2>/dev/null || cat "$LOGDIR/embed.json"

echo "=== 3. verify dense index $(date -u +%H:%M:%S) ==="
"$BIN" --index-dir "$CLONE" status 2>/dev/null | jq '.zone_retrieval | {zone_units, embeddings, embedding_manifest}'

echo "=== 4. eval france-juris-zones (measured, floor 0.8, OpenRouter query) $(date -u +%H:%M:%S) ==="
"$BIN" --index-dir "$CLONE" eval france-juris-zones --mode hybrid \
  --motivations 60 --moyens 60 --dispositif 60 --floor 0.8 \
  --out work/03-implementation/02-evidence/2026-06-24-phase2-zone-benchmark-clone.json \
  > "$LOGDIR/eval.json" 2>"$LOGDIR/eval.err" || { echo "eval FAILED"; tail -8 "$LOGDIR/eval.err"; exit 1; }
jq '{state, all_meet_proposed_floor, proposed_floor, uses_dense, fingerprint, categories}' "$LOGDIR/eval.json"
echo "=== DONE $(date -u +%H:%M:%S) ==="
