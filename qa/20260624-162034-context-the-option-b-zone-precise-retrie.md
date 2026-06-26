# Q&A — 20260624-162034

## Question

Context: the Option B zone-precise retrieval plan
(work/03-implementation/04-zones/2026-06-24-zone-precise-retrieval-implementation-plan.md)
has its CODE complete and codex-GO'd through Z5 (status.zone_retrieval block + a measured-only
phase2_zone_benchmark eval; commits b87ac8c..c01f322 on main). The remaining work is the T5.3
"promotion": the actual data rollout. See the plan's §Z2/Z3/Z5 and §2 operational runbook.

Current ground state I just verified:
- Production index `/mnt/models/jurisearch-index/phase2-full-juridic` = 157G; a 157G backup
  `phase2-full-juridic.backup-20260624` exists (made today). NO separate clone. No postgres running.
- 2.0T free on /mnt/models. PISTE creds + OPENROUTER_API_KEY + OPENAI_API_URL/MODEL all set in env.
- `ManagedPostgres::start_durable` AUTO-RUNS migrations on open, so opening ANY index migrates it to
  schema v15 (creates the empty zone_units / zone_unit_embeddings / zone_units_bm25_idx tables).
- I have NOT opened prod yet (to avoid mutating it), so I don't know if zone DATA was ever built.
  The git history shows only CODE commits for Z1–Z4; I believe the enrich/build/embed data steps
  have NOT been run anywhere yet.

The rollout per the plan: clone prod -> migrate -> `ingest enrich-zones --source cass` then
`--source inca` (~990k PISTE calls, ~14h, resumable) -> `ingest build-zone-units` ->
`ingest embed-zone-units` (~1.5M units via OpenRouter bge-m3, single-digit hours) ->
`eval france-juris-zones` (measure recall@10 floor) -> then apply the same on production / swap the
clone in. Every data-mutating step is supposed to run on a CLONE first; prod touched only at promotion.

My question — give me a concrete recommended execution path and the gotchas to resolve BEFORE I
launch anything:
1. Clone strategy: the plan says rsync prod->clone and do all data steps on the clone. Given a 157G
   backup already exists and 2.0T free, is the right move to rsync prod into a NEW working clone dir
   (leaving prod + backup untouched), run the whole pipeline there, validate the eval floor, then
   rsync the validated clone OVER prod (after re-confirming the backup)? Or migrate+build directly on
   prod since the schema change is additive/isolation-proven and a backup exists? What's safest?
2. Is there any risk in the auto-migrate-on-open behavior for a 157G index (e.g. the v15 bm25 index
   build on an empty zone_units table is trivial, but confirm nothing scans the big tables)?
3. Before the full ~14h enrich-zones run, should I validate on a tiny `--limit` slice end-to-end
   (enrich a few -> build -> embed -> eval) on the clone first, to catch a bug cheaply? 
4. enrich-zones resumability / PISTE quota: anything to set (concurrency, auto-restart wrapper) so a
   14h run survives stalls without manual babysitting?
5. The promotion gate: what exactly should I require from `eval france-juris-zones` (recall@10) before
   promoting — and since the floor is "measured, never asserted", how do I treat a low first number?
6. Anything else that would make this go wrong (the standalone-index-mistake class of error)?

Be concrete and ordered. I'm executing autonomously and will follow your recommendation.

## Answer

# Zone-Precise Retrieval Data Rollout: Recommended Execution Path

## Recommendation

Use a **new working clone**. Do not run the data rollout directly on production first.

Keep:

- production: `/mnt/models/jurisearch-index/phase2-full-juridic`
- rollback backup: `/mnt/models/jurisearch-index/phase2-full-juridic.backup-20260624`
- new working clone: e.g. `/mnt/models/jurisearch-index/phase2-full-juridic.zone-rollout-20260624`

Run the full pipeline on the working clone, validate `status.zone_retrieval` and `eval france-juris-zones`, then promote by **directory swap/rename** rather than rsyncing over production in place.

The backup already existing is good, but it should remain an immutable rollback point, not the working substrate.

## Why not mutate production first?

The schema migrations are additive and isolated, but the rollout is not just schema:

- ~990k Judilibre calls;
- millions of writes to `decision_zones`;
- potentially ~1.5M `zone_units`;
- OpenRouter embeddings;
- a finalized ivfflat index;
- eval-driven promotion.

The risk is not v15 DDL. The risk is discovering an operational/data bug halfway through a long external backfill. A working clone keeps production query behavior and rollback clean while you learn the real numbers.

## Auto-Migrate-on-Open Risk

Opening the clone with current HEAD will migrate it to v15. That is acceptable.

Source check:

- v13 creates `zone_units` plus small indexes on an empty table.
- v14 creates `zone_unit_embeddings` plus an `embedding_fingerprint` index on an empty table.
- v15 creates `zone_units_bm25_idx` on empty `zone_units`.
- none of v13-v15 backfills from `documents`, `chunks`, or `chunk_embeddings`;
- the zone ivfflat index is built later by `ingest embed-zone-units`, not by migration.

So migration itself should be quick and should not scan the 157G corpus.

Gotcha: `status.zone_retrieval` includes `zone_resolver_reachable_json`, which does scan cass/inca decisions to compute the honest denominator. That is an operator status cost, not a migration cost. It is fine after clone migration, but do not confuse it with opening the DB.

## Ordered Runbook

### 0. Preflight

Confirm no server owns either source or clone path:

```bash
/home/pierre/.pgrx/18.4/pgrx-install/bin/pg_ctl \
  -D /mnt/models/jurisearch-index/phase2-full-juridic/pg/data status || true
```

Keep the existing backup untouched:

```bash
du -sh /mnt/models/jurisearch-index/phase2-full-juridic \
       /mnt/models/jurisearch-index/phase2-full-juridic.backup-20260624
```

Build the current CLI once so the long jobs do not pay compile time repeatedly:

```bash
cargo build --release -p jurisearch-cli
BIN=target/release/jurisearch
```

### 1. Create a new working clone

Use rsync from production into a new path:

```bash
PROD=/mnt/models/jurisearch-index/phase2-full-juridic
CLONE=/mnt/models/jurisearch-index/phase2-full-juridic.zone-rollout-20260624

mkdir -p "$CLONE"
rsync -aH --numeric-ids --info=progress2 "$PROD"/ "$CLONE"/
```

Do not use `--delete` on the first copy unless the target already exists from a prior failed attempt. If reusing a partial clone, use:

```bash
rsync -aH --numeric-ids --delete --info=progress2 "$PROD"/ "$CLONE"/
```

### 2. Open/migrate the clone

Run status once on the clone. This opens the clone and auto-runs v13-v15:

```bash
"$BIN" --index-dir "$CLONE" status > /tmp/zone-rollout-status-after-migrate.json
```

Expected:

- schema becomes v15;
- `status.zone_retrieval.zone_units.total` is `0` unless data already exists;
- normal `phase2_gate` remains open/unchanged if the existing Phase 2 benchmark env is available;
- no zone search readiness yet.

### 3. Tiny end-to-end smoke on the clone

Yes, do this before the 14h run.

Use a small slice from each source:

```bash
"$BIN" --index-dir "$CLONE" ingest enrich-zones --source cass --limit 20 --concurrency 4 \
  | tee /tmp/zone-smoke-enrich-cass.json

"$BIN" --index-dir "$CLONE" ingest enrich-zones --source inca --limit 20 --concurrency 4 \
  | tee /tmp/zone-smoke-enrich-inca.json

"$BIN" --index-dir "$CLONE" ingest build-zone-units --limit 40 \
  | tee /tmp/zone-smoke-build-units.json
```

For the smoke embedding step, `embed-zone-units --limit` is allowed only if it can embed all currently pending units under that limit. Pick a high enough smoke limit:

```bash
"$BIN" --index-dir "$CLONE" ingest embed-zone-units --limit 10000 --index-lists 32 \
  --batch-size 32 --pool-concurrency 8 \
  | tee /tmp/zone-smoke-embed-units.json
```

Then test both readiness and query plumbing:

```bash
"$BIN" --index-dir "$CLONE" search --kind decision --zone motivations --mode hybrid \
  --top-k 5 "responsabilité licenciement motivation" \
  | tee /tmp/zone-smoke-search.json

"$BIN" --index-dir "$CLONE" eval france-juris-zones --mode hybrid \
  --motivations 5 --moyens 5 --dispositif 5 \
  --out /tmp/zone-smoke-eval.json \
  | tee /tmp/zone-smoke-eval.stdout.json
```

Do not use the smoke eval as a promotion metric. It is only to catch wiring, credentials, finalization, and fingerprint mistakes cheaply.

### 4. Full enrich on the clone

Run cass then inca. Default concurrency is `6`; keep it unless the observed error rate is clean. It is intentionally well below the PISTE burst ceiling.

```bash
"$BIN" --index-dir "$CLONE" ingest enrich-zones --source cass --concurrency 6 \
  | tee "$CLONE.enrich-cass.$(date +%Y%m%d-%H%M%S).json"

"$BIN" --index-dir "$CLONE" ingest enrich-zones --source inca --concurrency 6 \
  | tee "$CLONE.enrich-inca.$(date +%Y%m%d-%H%M%S).json"
```

For unattended operation, wrap each source in a restart loop that stops when a run considers zero candidates:

```bash
run_enrich_until_empty() {
  local source="$1"
  while true; do
    out="$CLONE.enrich-${source}.$(date +%Y%m%d-%H%M%S).json"
    "$BIN" --index-dir "$CLONE" ingest enrich-zones --source "$source" --concurrency 6 | tee "$out"
    considered=$(jq -r '.considered // 0' "$out")
    errors=$(jq -r '.errors // 0' "$out")
    [ "$considered" = "0" ] && break
    sleep 300
  done
}

run_enrich_until_empty cass
run_enrich_until_empty inca
```

Operational knobs:

- keep `JURISEARCH_JUDILIBRE_ZONE_ERROR_TTL_SECONDS` short-ish for the rollout, e.g. `600`, so transient API failures become retryable soon;
- keep `JURISEARCH_JUDILIBRE_ZONE_TTL_DAYS=30`;
- avoid pushing `--concurrency` above `8` unless logs show no 429/upstream spikes;
- if you see sustained `upstream_error`, stop increasing concurrency; rerun later rather than baking a high transient-error rate into the cache.

The command is resumable because every attempt writes a `decision_zones` row and the next run skips fresh rows.

### 5. Build zone units on the clone

After both sources enrich:

```bash
"$BIN" --index-dir "$CLONE" ingest build-zone-units \
  | tee "$CLONE.build-zone-units.$(date +%Y%m%d-%H%M%S).json"
```

Re-run once if needed; it should converge toward no additional candidates unless new/changed `decision_zones` rows appeared.

### 6. Embed and finalize zone units on the clone

Run without `--limit` for the real rollout. The command embeds pending zone units and finalizes the separate zone ivfflat index.

```bash
"$BIN" --index-dir "$CLONE" ingest embed-zone-units --index-lists 32 \
  --batch-size 32 --pool-concurrency 8 \
  | tee "$CLONE.embed-zone-units.$(date +%Y%m%d-%H%M%S).json"
```

Gotcha: do not use `--limit` for the full run. The code intentionally refuses partial finalization if more units remain than the limit.

After this, require:

```bash
"$BIN" --index-dir "$CLONE" status > "$CLONE.status.zone-ready.json"
```

Check:

```bash
jq '.zone_retrieval' "$CLONE.status.zone-ready.json"
```

Promotion-ready shape:

- `zone_units.total > 0`;
- `zone_units.decisions > 0`;
- `embeddings.units_pending == 0`;
- `embeddings.total == zone_units.total`;
- `embedding_manifest.embedding_fingerprint == "bge-m3:1024:normalize:true"`;
- `decision_zones.by_source_status` has large `ok` counts for cass/inca and no unexplained huge `upstream_error` bucket.

### 7. Full zone benchmark on the clone

Run the measured benchmark:

```bash
"$BIN" --index-dir "$CLONE" eval france-juris-zones --mode hybrid \
  --motivations 60 --moyens 60 --dispositif 60 \
  --floor 0.8 \
  --out work/03-implementation/02-evidence/2026-06-24-phase2-zone-benchmark-clone.json \
  | tee /tmp/phase2-zone-benchmark-clone.stdout.json
```

Promotion requirement:

- artifact kind is `phase2_zone_benchmark`;
- `state == "measured"`;
- `gate_input == false`;
- `all_meet_proposed_floor == true`;
- each non-empty category has `metric == "recall_at_10"`;
- each category with queries has `queries >= 60` unless a zone legitimately has fewer available qrels;
- each measured category has `value >= 0.8`;
- provenance says `pipeline` is `jurisearch search --zone ...`, not a direct table lookup.

If the first number is low:

- do **not** promote and do **not** lower the floor just to pass;
- inspect whether the failure is BM25/dense/fingerprint/readiness/query-gold leakage by rerunning:

```bash
"$BIN" --index-dir "$CLONE" eval france-juris-zones --mode bm25 --floor 0.8 --out /tmp/zone-bm25.json
"$BIN" --index-dir "$CLONE" eval france-juris-zones --mode dense --floor 0.8 --out /tmp/zone-dense.json
```

- only treat a lower floor as acceptable if it is written up as measured product evidence, not as a promotion gate for “zone-precise retrieval works”.

For this rollout, I would require the default proposed `0.8` floor before promotion.

### 8. Promote

Before promotion:

- stop any Postgres process for prod/clone;
- re-confirm the immutable backup exists and is readable;
- save clone status + eval artifacts under `work/03-implementation/02-evidence/` or another durable evidence path.

Prefer same-filesystem directory swap over rsync-over-production. It avoids leaving production half-overwritten if rsync is interrupted.

Example:

```bash
PROD=/mnt/models/jurisearch-index/phase2-full-juridic
CLONE=/mnt/models/jurisearch-index/phase2-full-juridic.zone-rollout-20260624
OLD=/mnt/models/jurisearch-index/phase2-full-juridic.pre-zone-$(date +%Y%m%d-%H%M%S)

/home/pierre/.pgrx/18.4/pgrx-install/bin/pg_ctl -D "$PROD/pg/data" -m fast stop || true
/home/pierre/.pgrx/18.4/pgrx-install/bin/pg_ctl -D "$CLONE/pg/data" -m fast stop || true

mv "$PROD" "$OLD"
mv "$CLONE" "$PROD"
```

Then open the promoted production path and verify:

```bash
"$BIN" --index-dir "$PROD" status > /tmp/zone-promoted-status.json
"$BIN" --index-dir "$PROD" search --kind decision --zone motivations --mode hybrid \
  --top-k 5 "responsabilité licenciement motivation" \
  > /tmp/zone-promoted-smoke-search.json
```

Keep `$OLD` until you have run several successful searches/evals against promoted prod. You already have the 157G backup as deeper rollback.

If you prefer rsync promotion, only do it with prod stopped and backup confirmed:

```bash
rsync -aH --numeric-ids --delete --info=progress2 "$CLONE"/ "$PROD"/
```

but directory swap is cleaner.

## Gotchas

### Do not accidentally build a standalone fresh index

Every command must pass the intended index path:

```bash
"$BIN" --index-dir "$CLONE" ...
```

Do not rely on `JURISEARCH_INDEX_DIR` during rollout unless you print it before every run.

### Runtime config is rewritten on open

The clone may contain old `jurisearch.conf` / `postmaster.opts`. That is fine: `open_index()` rewrites runtime config. Do not manually edit these files.

### `status` may be slower after v15

`status.zone_retrieval` computes the resolver-reachable denominator with a cass/inca scan. That is useful for rollout evidence, but do not run status in a tight loop during enrichment.

### `embed-zone-units --limit` is for smoke only

For the full rollout, omit `--limit`; otherwise the command will intentionally refuse to finalize a partial zone dense index.

### `decision_zones` negative rows are not fatal

Some `not_found`, `unsupported`, `invalid_offsets`, or transient `upstream_error` rows are expected. The red flag is a large `upstream_error` bucket that tracks concurrency/quota/network problems.

### Production Phase 2 gate should remain conceptually separate

`phase2_zone_benchmark` is measured-only and `gate_input=false`. Promotion should require it operationally, but it must not be wired into the full-juridic Phase 2 gate or used to claim corpus-wide zone accuracy.

## Short Answer

Safest path: **new clone → migrate/build/enrich/embed/eval on clone → promote by directory swap**.

Do a tiny end-to-end slice first. Require `status.zone_retrieval` embedding completeness and `eval france-juris-zones --mode hybrid --floor 0.8` with `all_meet_proposed_floor=true` before promotion. If the metric is low, stop and diagnose; do not promote and do not paper over the floor.
