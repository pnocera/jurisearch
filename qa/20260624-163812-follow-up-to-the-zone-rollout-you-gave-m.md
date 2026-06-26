# Q&A — 20260624-163812

## Question

Follow-up to the zone rollout (you gave me the runbook earlier; Q&A 20260624-162034). I ran the tiny
smoke on the clone and found things that change the strategy. Need your decision.

FACTS (clone = phase2-full-juridic.zone-rollout-20260624, schema migrated to v15):
- `enrich-zones --source cass --limit 20 --concurrency 4` => considered=20, official_ok=0, fallback=20,
  errors=0. All 20 cached as status=`invalid_offsets`.
- Root cause (verified via raw_json): those 20 are the OLDEST cass decisions
  (cass:JURITEXT000006952288 …). Their Judilibre `/decision` response has NO `zones` field at all
  (keys: chamber,…,text,themes,…,visa). So normalize_judilibre_zones finds nothing -> invalid_offsets.
  This is NOT a code bug; Judilibre simply has no zone annotations for old decisions.
- Proof zones DO exist for recent decisions: the 2 pre-existing `ok` rows are recent
  (cass:JURITEXT000051743650, inca:JURITEXT000050043900) and their raw_json HAS
  zones={expose,moyens,dispositif,motivations,introduction}; normalized zones non-empty (1 and 2
  motivations frags).
- cass document_id range: JURITEXT000006951557 (oldest) .. JURITEXT000052970388 (newest). The zoned
  `ok` cass row is JURITEXT000051743650 — i.e. near the NEWEST end. JURITEXT ids are assigned over time,
  so document_id ASC == chronological oldest->newest.
- enrich_zone_candidates_json keysets `ORDER BY document_id` ASC (oldest first). So a full backfill
  spends most of ~14h / ~990k PISTE calls on OLD decisions that yield NO zones, and only reaches the
  valuable recent (zoned) decisions LAST.
- The 2 pre-existing `ok` rows have text_hash IS NULL (the "lazy rows"), so build-zone-units derived 0
  units (load_derivable requires text_hash NOT NULL). They'd be re-enriched by the backfill — but they
  sort last (recent).
- Embedding: a local llama-server bge-m3 on 127.0.0.1:8097 works (fingerprint bge-m3:1024:normalize:true,
  matches the chunk corpus). OPENROUTER_API_KEY is also in env (the fast path via JURISEARCH_EMBED_POOL).

I do NOT yet know what FRACTION of the 494,701 resolver-reachable decisions actually have zones (could
be a small recent slice, or a large retroactively-zoned set). I can't cheaply sample mid/recent
decisions because the CLI enrich keyset only starts from the oldest and has no cursor/order flag.

DECISIONS I need from you, concrete and ordered:
1. Coverage measurement first? Should I measure the zoned fraction BEFORE committing ~14h? If yes, what's
   the cheapest sound way given the CLI keyset is oldest-first with no cursor (e.g. a few direct Judilibre
   /decision probes for recent provider_ids, or a one-off SQL+script over a sampled document_id stride)?
2. Ordering: oldest-first wastes most of the run on zoneless old decisions and delays any eval. Is it worth
   a small code change (a `--order recent|oldest` flag or DESC keyset on enrich candidates, with a fresh
   codex review) so the valuable zoned decisions are enriched FIRST (eval-able quickly, and I can stop when
   recent decisions are exhausted)? Or just run oldest-first as the plan says and accept the cost?
3. Smoke: to validate build->embed->search->eval cheaply now, is it acceptable to manually set text_hash on
   the 2 real `ok` rows (their zones_json is real Judilibre data) on the throwaway clone, run the downstream
   pipeline, confirm wiring, then run the real backfill? Or insist the smoke use only real enrich output?
4. Go/no-go: does recent-only (possibly small) coverage change whether this rollout is worth promoting now?
   The artifact is measured-only and the status block reports honest coverage, so partial coverage is
   by-design — but is there a minimum coverage you'd want before a production swap?
5. Embed for the full run: local bge-m3 (8097) vs OpenRouter pool — given the unit count is unknown until
   enrichment, what would you set?

Be concrete. I'm executing autonomously and will follow your recommendation.

## Answer

# Follow-up Decision: Zone Rollout After Zoneless Old-Decision Smoke

## Recommendation

Do **not** run the full oldest-first backfill now.

New execution path:

1. Measure zone coverage first with a stratified, read-only sample.
2. Make a small code change: `ingest enrich-zones --order oldest|recent`, defaulting to current `oldest`.
3. Use `--order recent` for the rollout so the useful recent zone coverage is enriched first.
4. Use a synthetic `text_hash` only for a throwaway downstream wiring smoke, not as promotion evidence.
5. Promote only if measured coverage is materially useful and `eval france-juris-zones` clears the advisory floor on real enriched rows.

The smoke changed the strategy: oldest-first is now known to front-load Judilibre calls into a zoneless historical region. Continuing that way is wasteful and delays all useful validation.

## 1. Measure Coverage First

Yes. Measure before spending ~14h / ~990k PISTE calls.

The cheapest sound measurement is a **stratified sample over resolver-reachable cass+inca decisions**, then live Judilibre search+decision probes. Do not use the CLI keyset for this; it is currently biased oldest-first.

### Sampling shape

On the clone, export a deterministic stratified sample:

- partition by `source` (`cass`, `inca`);
- order by `document_id`;
- split into chronological buckets, e.g. 20 buckets per source;
- take 10 deterministic rows per bucket by `md5(document_id)`;
- total around 400 decisions, max ~800 HTTP calls.

SQL shape:

```sql
\copy (
WITH reachable AS (
    SELECT
        d.source,
        d.document_id,
        d.valid_from::text AS decision_date,
        (
            SELECT cn
            FROM jsonb_array_elements_text(coalesce(d.canonical_json->'case_numbers', '[]'::jsonb)) AS cn
            WHERE replace(replace(cn, '.', ''), ' ', '') ~ '^[0-9]{2}-[0-9]{4,6}$'
            ORDER BY cn
            LIMIT 1
        ) AS pourvoi
    FROM documents d
    WHERE d.kind = 'decision'
      AND d.source IN ('cass','inca')
      AND EXISTS (
          SELECT 1
          FROM jsonb_array_elements_text(coalesce(d.canonical_json->'case_numbers', '[]'::jsonb)) AS cn
          WHERE replace(replace(cn, '.', ''), ' ', '') ~ '^[0-9]{2}-[0-9]{4,6}$'
      )
),
bucketed AS (
    SELECT *,
           ntile(20) OVER (PARTITION BY source ORDER BY document_id) AS bucket
    FROM reachable
),
ranked AS (
    SELECT *,
           row_number() OVER (PARTITION BY source, bucket ORDER BY md5(document_id)) AS rn
    FROM bucketed
)
SELECT source, bucket, document_id, decision_date, pourvoi
FROM ranked
WHERE rn <= 10
ORDER BY source, bucket, document_id
) TO '/tmp/zone-coverage-sample.tsv' WITH (FORMAT csv, DELIMITER E'\t', HEADER true);
```

Probe each row with the same logic as production:

1. `GET /cassation/judilibre/v1.0/search?query=<pourvoi>&operator=exact&page_size=10`
2. accept result only when normalized `numbers` / `number` contains the pourvoi and `decision_date` equals the local date;
3. `GET /decision?id=<id>`;
4. count as zoned only if any of `zones.motivations`, `zones.moyens`, `zones.dispositif` has at least one fragment.

Report by source and bucket:

```text
source bucket n searched resolved decision_has_zones motivations moyens dispositif
cass   01     ...
cass   20     ...
inca   01     ...
inca   20     ...
```

This gives the key answer: whether zones are a small recent tail or a broad retroactive layer.

## 2. Change Ordering Before Full Backfill

Yes. Add `--order recent|oldest` and run the rollout recent-first.

This is a small, justified code change with high operational value. It does not change zone semantics, caching, normalization, or retrieval. It only changes the keyset ordering of enrichment candidates.

### Implementation

Add CLI arg:

```rust
#[arg(long, default_value = "oldest")]
order: EnrichZoneOrder,
```

Storage helper:

```rust
pub enum EnrichZoneOrder {
    Oldest,
    Recent,
}

pub fn enrich_zone_candidates_json(
    postgres: &ManagedPostgres,
    source: &str,
    after_cursor: Option<&str>,
    since: Option<&str>,
    limit: u32,
    order: EnrichZoneOrder,
) -> Result<String, StorageError>
```

Keyset logic:

```rust
match order {
    Oldest => {
        cursor_predicate = "AND d.document_id > $cursor";
        order_by = "ORDER BY d.document_id ASC";
        next_cursor = "max(document_id)";
    }
    Recent => {
        cursor_predicate = "AND d.document_id < $cursor";
        order_by = "ORDER BY d.document_id DESC";
        next_cursor = "min(document_id)";
    }
}
```

Keep default `oldest` for compatibility, but use:

```bash
ingest enrich-zones --source cass --order recent ...
ingest enrich-zones --source inca --order recent ...
```

Review target:

- ordering/cursor correctness only;
- no changes to resolver, cache row semantics, or `fetch --part --online`;
- unit test for both ASC and DESC keyset pages.

## 3. Smoke With Synthetic `text_hash`

Acceptable, but only on a throwaway clone and only as a wiring smoke.

Your two real `ok` rows contain real Judilibre `zones_json` and `raw_json`, so setting a synthetic non-null `text_hash` is fine to validate:

- `build-zone-units`;
- `embed-zone-units`;
- `search --zone`;
- the eval command shape.

But do **not** treat this as rollout evidence, and do **not** promote a clone containing manually fabricated hashes unless you reset those rows.

### Safe smoke SQL

On the throwaway clone:

```sql
UPDATE decision_zones
SET text_hash = 'smoke:' || md5(raw_json::text || zones_json::text),
    offset_unit = coalesce(offset_unit, 'char')
WHERE status = 'ok'
  AND text_hash IS NULL
  AND jsonb_typeof(zones_json) = 'object'
  AND (
      jsonb_array_length(coalesce(zones_json->'motivations', '[]'::jsonb)) > 0
      OR jsonb_array_length(coalesce(zones_json->'moyens', '[]'::jsonb)) > 0
      OR jsonb_array_length(coalesce(zones_json->'dispositif', '[]'::jsonb)) > 0
  );
```

Then run:

```bash
ingest build-zone-units --limit 10
ingest embed-zone-units --limit 10000 --index-lists 32
search --zone motivations --mode hybrid ...
eval france-juris-zones --mode hybrid --motivations 1 --moyens 1 --dispositif 0
```

After the smoke, either discard/re-rsync the clone or reset:

```sql
DELETE FROM zone_units
WHERE document_id IN (
    SELECT document_id
    FROM decision_zones
    WHERE text_hash LIKE 'smoke:%'
);

UPDATE decision_zones
SET text_hash = NULL
WHERE text_hash LIKE 'smoke:%';
```

If you want the cleanest path, use the synthetic hash on a separate scratch clone and keep the promotion clone pure.

## 4. Go / No-Go Threshold

Partial coverage is acceptable by design, but “two recent rows” is not a production feature.

Before production swap, require all of:

1. Real enriched rows, not synthetic:
   - `decision_zones.status='ok'` with non-null `text_hash`.
2. Useful scale:
   - at least **25,000 official-ok decisions**, or
   - at least **5% of resolver-reachable cass+inca decisions**.
3. Derived/indexed coverage:
   - `zone_units.decisions > 0`;
   - `embeddings.units_pending == 0`;
   - `zone_unit_embeddings.total == zone_units.total`.
4. Benchmark:
   - `eval france-juris-zones --mode hybrid --floor 0.8`;
   - `all_meet_proposed_floor == true`;
   - non-empty categories have enough qrels; target 60 per zone where possible.
5. Status honesty:
   - `status.zone_retrieval` clearly reports the covered decision count and does not imply corpus-wide zone coverage.

If coverage lands below 25k / 5%, do not promote as a production swap. Keep the code, maybe keep the clone as evidence, but defer data promotion until there is a clearer product value story or a targeted recent-only product mode.

If the sample shows zones only exist in a recent band, use recent-first enrichment and stop once rolling yield collapses. Example operational stop rule:

- run recent-first in batches of 5,000 candidates;
- after each batch, record `official_ok / considered`;
- if two consecutive batches are below 1% official_ok and the stratified sample shows older buckets are also effectively zero, stop the backfill and build/eval the accumulated recent coverage.

## 5. Embedding Choice

Use local bge-m3 for smoke. Use OpenRouter pool for the full promotion candidate unless the measured unit count is small.

Recommended:

- Smoke:
  - local `127.0.0.1:8097`;
  - confirms fingerprint and plumbing cheaply.
- Full run:
  - if `zone_units.total < 100,000`, local is fine;
  - if `zone_units.total >= 100,000`, use the OpenRouter pool for predictable completion time.

In both cases, require the storage fingerprint to stay:

```text
bge-m3:1024:normalize:true
```

The code’s zone readiness checks compare the zone embedding manifest fingerprint against the query embedder. Do not mix local and OpenRouter configs unless they produce the same storage fingerprint, which you said they do.

Full-run default:

```bash
ingest embed-zone-units --index-lists 32 --batch-size 32 --pool-concurrency 8
```

If using local single-server bge-m3, reduce pool concurrency if the server queues badly. If using OpenRouter pool, keep the same retry-safe settings you used for the chunk corpus.

## Updated Execution Order

1. Do synthetic-hash downstream smoke on scratch clone if you want immediate wiring confidence.
2. Run stratified live coverage measurement.
3. Patch `--order recent|oldest`; codex review the small diff.
4. Re-rsync or reset the working clone to a clean state.
5. Run recent-first enrichment on clone in bounded batches.
6. Stop early if measured yield collapses and sampling says older strata are zoneless.
7. Build zone units.
8. Embed zone units.
9. Run `eval france-juris-zones`.
10. Promote only if coverage + benchmark thresholds above are met.

## Bottom Line

The smoke did its job: it showed oldest-first is the wrong operational order. Measure coverage first, add recent-first ordering, and only promote if real official-zone coverage reaches a meaningful scale. Synthetic `text_hash` is acceptable for a throwaway wiring smoke, but not for promotion evidence.
