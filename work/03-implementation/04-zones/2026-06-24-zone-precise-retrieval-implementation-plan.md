# Zone-Precise Jurisprudence Retrieval — Implementation Plan (Option B)

Date: 2026-06-24
Status: IMPLEMENTATION PLAN. Builds the design
`04-zones/2026-06-24-zone-precise-retrieval-design.md` (codex r1→r2 GO,
`reviews/2026-06-24-zone-precise-retrieval-design-codex-review*.md`), which realizes the Option B
architecture decision (`reviews/2026-06-24-option-a-vs-b-codex-decision.md`).
Target index: `/mnt/models/jurisearch-index/phase2-full-juridic` (schema v12).

This is the build order. It does **not** run anything; execution is gated task-by-task by a codex review
before any code is executed, and any live PISTE/index mutation runs only after GO, on a **clone** first.

---

## 0. Preconditions & assumptions

- **Product decisions carried from the design (proceeding on these defaults; flag if you disagree
  before Z2):** D2 = **Cassation-only, official-only** (no heuristic capp/jade zone tier); D3 =
  **strictly zone-local ranking** (no whole-decision blend); D4 = index **motivations + moyens +
  dispositif** only (the three the normalizer writes to `zones_json` today). These are the
  precision-first "best for jurisearch" reading; changing them changes Z3/Z4 scope.
- **Reused as-is (no change):** the Judilibre resolver + client
  (`PisteClient::judilibre_search_params`/`judilibre_decision`), `find_matching_judilibre_id`,
  `normalize_judilibre_zones`, `is_judilibre_cassation_source`
  (`crates/jurisearch-cli/src/main.rs:3542-3802`); `rrf_weights` (public,
  `crates/jurisearch-storage/src/retrieval.rs:23-35`).
- **Reused at the HTTP layer only (NOT the insert path):** the OpenRouter embedding *generation* is
  reusable, but `embed_and_insert_chunks_with_pool` is chunk-specific — it builds `ChunkEmbeddingInsert`
  and calls `insert_chunk_embeddings`, which writes `chunks.embedding_fingerprint` + `chunk_embeddings`
  (`crates/jurisearch-cli/src/main.rs:6294-6371`, `crates/jurisearch-storage/src/projection.rs:827-942`).
  Zone embedding needs its own input/insert types + writer (T1.2, T3.2).
- **Private retrieval helpers needing `pub(crate)` extraction before T4** (they are NOT public, so a
  sibling module cannot call them): `format_sql_f64` (`retrieval.rs:37-41`), `RRF_K` (`:13`),
  `DecisionFilters::predicate` (`:104-156`), `HybridCandidateQuery::effective_probes` (`:168-170`).
  Handled by T1.3.
- **Option B isolation invariant (verify in every review):** **no change to default retrieval
  behavior or candidate SQL output** — `chunks`, `chunk_embeddings`, `chunks_bm25_idx`,
  `hybrid_candidates_json`, `load_chunk_embedding_inputs`, `finalize_dense_rebuild`, and the Phase 2
  gate's `zone_accurate=false` check (`crates/jurisearch-cli/src/main.rs:8800-8824`) stay behaviorally
  identical. The **only** permitted `retrieval.rs` edit is the T1.3 `pub(crate)` helper extraction,
  proven output-identical by a golden test.

## 1. Sequencing (dependency graph)

```
Z1 schema+storage ──▶ Z2 enrich backfill ──▶ Z3 derive+embed+finalize ──▶ Z4 query path + CLI ──▶ Z5 eval+gate
   (migrations,          (text_hash fix +        (zone_units +               (zone_candidates_json    (motivations_retrieval
    zone_units.rs)        enrich-zones cmd)        zone_unit_embeddings)       + search --zone)         + status.zone_retrieval)
```

Z1–Z3 produce data with **zero** effect on existing search until Z4 wires the query path. Each Zn is
independently shippable + codex-reviewed before execution. A clone of the production index is the
substrate for every data-mutating step (Z1 migrations, Z2 backfill, Z3 build); production is touched
only after the clone is validated end-to-end.

---

## Z1 — Schema + storage helpers

**Goal:** the three new tables/indexes and the Rust read/write helpers, with the main path provably
untouched.

### T1.1 Migrations v13–v15
- File: `crates/jurisearch-storage/src/migrations.rs`. Append three `Migration` entries to
  `MIGRATIONS` and bump `CURRENT_SCHEMA_VERSION` 12 → 15 (`:3`). Each migration ends with the standard
  `index_manifest` schema-version upsert (mirror v12, `:476-481`).
  - **v13 `zone_units`** — the DDL from design §3.1 verbatim: PK `zone_unit_id`, FK `document_id`→
    `documents ON DELETE CASCADE`, `zone` CHECK domain, `fragment_index`, `body`, `search_body` (NOT
    NULL + non-empty CHECK, mirroring `chunks_contextualized_body_not_empty` at `:329-334`), `provider`,
    `zone_accurate`, `source`, `text_hash` NOT NULL, `zone_unit_builder_version` NOT NULL,
    `zone_schema_version`, `embedding_fingerprint`, `UNIQUE (document_id, zone, fragment_index)`, plus
    `zone_units_document_idx` / `zone_units_zone_idx`.
  - **v14 `zone_unit_embeddings`** — design §3.2: PK `zone_unit_id`→`zone_units ON DELETE CASCADE`,
    `embedding_fingerprint`, `embedding vector(1024)`, `model`, `dimension` CHECK=1024. (The ivfflat
    index is built at finalize time in Z3, not in the migration — mirror how the chunk ivfflat index is
    a finalize step, not a base migration.)
  - **v15 `zone_units_bm25_idx`** — design §3.3: `USING bm25 (zone_unit_id, search_body)` with
    `key_field='zone_unit_id'` and the **v9 French analyzer** `text_fields` block (ascii_folding +
    French stemmer + French stopwords), mirroring `:355-369`.
- Acceptance: `run_migrations` on a clone advances `index_manifest` schema to 15;
  `validate_migration_list` passes; the three tables/indexes exist; existing `chunks*` objects
  unchanged (`\d+ chunks`, `\di chunks_bm25_idx` identical pre/post).

### T1.2 Storage module `zone_units.rs`
- New file `crates/jurisearch-storage/src/zone_units.rs`; `pub mod zone_units;` in
  `crates/jurisearch-storage/src/lib.rs` (alongside `decision_zones`).
- Helpers (mirror the `decision_zones.rs` read-via-`execute_sql` / write-via-parameterized-client
  split):
  - `enrich_zone_candidates_json(postgres, source, after_cursor, limit)` → the next page of
    enrichable decisions: `documents` where `source ∈ {cass,inca}` AND
    `jurisearch_normalized_case_numbers` yields a parser-valid pourvoi (reuse the predicate from
    `decision_resolution_metadata_json`, `crates/jurisearch-storage/src/decision_zones.rs:63-69`) AND
    the decision is **not fresh-and-derivable**:
    `LEFT JOIN decision_zones z … WHERE z.status IS NULL OR z.expires_at <= now() OR (z.status IN
    ('ok','invalid_offsets') AND z.text_hash IS NULL)` — the last clause re-enriches the 2 lazy rows
    and any row written before the T2.1 hash fix **regardless of TTL** (the BLOCKER: a fresh NULL-hash
    `ok` row would otherwise be skipped by both this predicate and `load_derivable_decision_zones_json`,
    which requires `text_hash IS NOT NULL`, leaving it permanently un-derived). Negative rows stay
    hashless and non-derivable. Ordered by `document_id` (keyset). Also a `count` companion for the
    denominator.
  - `ReplaceZoneUnits` struct + `replace_zone_units_for_document(client, document_id, units, …)`:
    a single transaction that deletes the decision's existing `zone_units` and inserts the derived set
    (idempotent rebuild). Parameterized inserts (jsonb-free; all text/int), `$n::text` casts as in
    `upsert_decision_zones_with_client` (`:114-160`).
  - `load_derivable_decision_zones_json(postgres, limit, builder_version)`: page decisions whose
    `decision_zones.status='ok'` AND `text_hash IS NOT NULL` AND (`zone_units` missing for the doc OR
    any unit's `text_hash`≠the row's OR any unit's `zone_unit_builder_version`≠`builder_version`) —
    returns `document_id`, `source`, `text_hash`, `zones_json` for the derivation pass.
  - `load_zone_unit_embedding_inputs(postgres, fingerprint, model, dimension, limit)`,
    `insert_zone_unit_embeddings(client, inserts)`, and `finalize_zone_dense_rebuild(postgres, spec)` —
    **copies** of the `dense.rs:34-91` / `:93-192` + `insert_chunk_embeddings`
    (`projection.rs:827-942`) shapes pointed at `zone_units`/`zone_unit_embeddings` and the
    `zone_unit_embeddings_ivfflat_idx` (lists ≈ √rows). `insert_zone_unit_embeddings` inserts into
    `zone_unit_embeddings` and sets `zone_units.embedding_fingerprint` (upsert; idempotent on
    conflicting/missing units). Embedding text = `zone_units.body` (= `search_body`). Kept as separate
    functions so `dense.rs`/`projection.rs` chunk paths are provably untouched (decision reason #4).
  - `zone_retrieval_coverage_json(postgres)` — the §Z5 report query (counts by `decision_zones.status`
    per source, `zone_units` per zone, embedding coverage, resolver-reachable denominator).
- Acceptance: unit tests for the SQL builders (literal-injection safety, predicate shape) following the
  `decision_zones`/`retrieval` test style; a round-trip integration test on a temp index (insert a
  decision + a fake `ok` `decision_zones` row with `text_hash` → derive → 1+ `zone_units` rows →
  embed a stub vector → finalize builds the ivfflat index).

### T1.3 Shared retrieval helpers (`pub(crate)` extraction) — unblocks T4.1
- The zone query path (T4.1) needs `format_sql_f64`, `RRF_K`, `DecisionFilters::predicate`, and the
  probes logic, all currently **private** to `retrieval.rs` (`:13`, `:37-41`, `:104-156`, `:168-170`).
  A sibling `zone_retrieval.rs` cannot call them, and duplication would risk silent drift (esp.
  `DecisionFilters::predicate`).
- Chosen approach: **extract** the shared SQL helpers into a `crates/jurisearch-storage/src/
  retrieval_common.rs` (or mark them `pub(crate)` in place) — `format_sql_f64`, `RRF_K`,
  `DecisionFilters` + its `predicate`, and a free `effective_probes(options, mode)` — and have both
  `hybrid_candidates_json` and `zone_candidates_json` call them. This is the **only** permitted
  `retrieval.rs` change.
- Acceptance: a golden test proves `hybrid_candidates_json` emits byte-identical SQL/JSON before vs
  after the extraction (the isolation invariant); existing retrieval tests pass unchanged.

### T1.4 Review gate Z1
- codex review of the Z1 diff (minimal instructions: scope = the migrations + `zone_units.rs` + the
  T1.3 helper extraction; verify the DDL against the live schema, the analyzer block against v9, that
  the extraction leaves `hybrid_candidates_json` output identical, and that no chunk dense/BM25 path is
  altered). Apply all severities; r2 to GO if FIXES_REQUIRED.

---

## Z2 — Enrichment backfill (`enrich-zones`)

**Goal:** eager `decision_zones` coverage for the resolver-reachable Cassation subset, with the
**deterministic `text_hash` now populated** (design §4 BLOCKER fix), accounted + resumable.

### T2.1 Populate `text_hash` in the enrichment writers (the BLOCKER fix)
- File: `crates/jurisearch-cli/src/main.rs`. Today `enrich_decision_from_judilibre` writes
  `text_hash: None` (`:3714`) and `cache_zone_status` writes `text_hash: None` (`:3831`).
- Add `fn zone_text_hash(decision: &Value, provider_id: &str) -> String` =
  a stable digest over `decision["text"]` ‖ `normalize_judilibre_zones(decision).0` (the normalized
  zones_json string) ‖ `provider_id` ‖ `decision["update_date"]`. Use the workspace's existing content
  digest (the one behind `source_payload_hash` / `france_juris_index_revision`); **verify which crate**
  (md5 today) and prefer a 256-bit hash — add `sha2` to `jurisearch-cli` if not already a dep. Set it
  on the `ok`/`invalid_offsets` upsert at `:3705-3720`. Negative rows (`cache_zone_status`) keep
  `text_hash=None` (they derive nothing).
- Acceptance: a unit test that two calls on the same `decision`+`provider_id` produce the same hash and
  a changed `text`/`update_date` changes it; the enrichment upsert now writes a non-null `text_hash`
  for `ok`.

### T2.2 `ingest enrich-zones` command
- File: `crates/jurisearch-cli/src/main.rs`. Add `IngestSubcommand::EnrichZones { source, limit,
  since, resume, concurrency }` next to `EmbedChunks` (`:880`), dispatched in `emit_ingest`
  (`:4294`-style match). Mirror the `embed-chunks` paged-streaming + accounting shape (`:5979-6063`):
  loop `enrich_zone_candidates_json` pages → for each decision call the **existing**
  `enrich_decision_from_judilibre` (now hash-populating) → accumulate status counts. `--source`
  restricts to `cass`/`inca`; reject others. `--since` filters candidates by `upstream_update_date`
  advance / `text_hash` change (refresh mode). Honor PISTE limits: a conservative steady
  `--concurrency` (default well under ~20 req/s; ≈2 calls per decision); restartable so a quota stall
  just resumes (every attempt writes a `decision_zones` row, so a resumed run skips fresh rows).
- Emit a JSON report (command, source, considered, by-status counts, skipped-no-pourvoi, elapsed) like
  the other ingest commands.
- Acceptance: `--limit 5` dry-run on a clone writes 5 `decision_zones` rows with non-null `text_hash`
  for `ok`; a second run is a near-no-op (fresh rows skipped); status counts reconcile. **Plus the
  BLOCKER-1 test:** seed a fresh (`expires_at` in the future) `ok` `decision_zones` row with
  `text_hash IS NULL` and prove `enrich-zones` selects and rewrites it with a non-null hash (TTL is not
  honored while the hash is NULL).

### T2.3 Review gate Z2 + the live backfill
- codex review of the Z2 diff (scope: the `text_hash` writers + the new command; verify the hash input
  is deterministic and the candidate predicate matches the resolver). Apply, r2 to GO.
- Then run the live backfill **on a clone**: `enrich-zones --source cass` then `--source inca`,
  ~494,701 decisions × ~2 calls ≈ ~14h at the quota (see [[phase2-jurisprudence-progress]] PISTE
  limits). Monitor via the by-status report; resume on any stall. (This is the one multi-hour external
  step; it is pure cache population — no retrieval impact yet.)

---

## Z3 — Derive + embed + finalize

**Goal:** materialize `zone_units` from the enriched cache and build their dense + BM25 indexes.

### T3.1 `ingest build-zone-units`
- `IngestSubcommand::BuildZoneUnits { limit, rebuild }`. Pages `load_derivable_decision_zones_json`
  (T1.2) with the current `ZONE_UNIT_BUILDER_VERSION` const; for each decision, derive one
  `zone_units` row per `(zone, fragment_index)` from `zones_json` (motivations/moyens/dispositif
  fragments, verbatim `body`; `search_body=body`; carry `text_hash`, `source`,
  `zone_unit_builder_version`); `replace_zone_units_for_document` per decision (idempotent). A
  `--rebuild` forces all (builder-version bump path).
- Acceptance: on the clone, `zone_units` count ≈ Σ enriched-ok × non-empty zones; re-running is a
  no-op; bumping `ZONE_UNIT_BUILDER_VERSION` rederives all.

### T3.2 `ingest embed-zone-units`
- `IngestSubcommand::EmbedZoneUnits { limit, index_lists, batch_size, pool_concurrency }`. Mirror the
  `embed-chunks` paged-streaming control flow (`:5979-6063`), but the embed/insert helper is
  **zone-specific** — `embed_and_insert_chunks_with_pool` builds `ChunkEmbeddingInsert` →
  `insert_chunk_embeddings` (writes `chunks`/`chunk_embeddings`) and **cannot** populate
  `zone_unit_embeddings`. Add:
  - CLI: `ZoneUnitEmbeddingInput { zone_unit_id, embedding_text }`,
    `ZoneUnitEmbeddingInsert { zone_unit_id, embedding, model, dimension }`, and
    `embed_and_insert_zone_units_with_pool(...)` — same OpenRouter HTTP generation as the chunk pool
    (the reusable part), but writing via `insert_zone_unit_embeddings` (T1.2). Factor the shared HTTP
    pool driver if practical; otherwise a parallel wrapper is acceptable (CLI-local, no main-path risk).
  - Then paged load (`load_zone_unit_embedding_inputs`) → embed/insert → `finalize_zone_dense_rebuild`
    builds `zone_unit_embeddings_ivfflat_idx`, **same `bge-m3:1024:normalize:true` fingerprint**. Same
    finalize-gap guard ([[embed-chunks-finalize-gap]]): never leave the ANN index unbuilt after an
    aborted run.
- Acceptance: unit tests for missing/conflicting zone units and idempotent re-insert; 100% of
  `zone_units` embedded under the fingerprint; the ivfflat index exists and is non-empty; `~1.5M` units
  ⇒ single-digit hours at proven throughput.

### T3.3 Review gate Z3
- codex review of the derivation + embed/finalize diff (scope: fragment handling incl. multi-fragment,
  the stale/rebuild predicate using both `text_hash` and `zone_unit_builder_version`, and that the
  finalize touches only the zone tables). Apply, r2 to GO. Then run T3.1→T3.2 on the clone.

---

## Z4 — Zone retrieval query path + CLI

**Goal:** `search --zone <motivations|moyens|dispositif>` over the zone subsystem, isolated from the
default path, self-labeling its Cassation-only coverage.

### T4.1 `zone_candidates_json`
- New file `crates/jurisearch-storage/src/zone_retrieval.rs` (keeps `retrieval.rs`/
  `hybrid_candidates_json` untouched — decision reason #1). Add `Zone` enum + `ZoneCandidateQuery`
  (design §7): `query_text`, `query_embedding`, `embedding_fingerprint`, `retrieval_mode`, `options`,
  `after_cursor`, `zone`, `decision_filters`, limits. `zone_candidates_json` calls the **T1.3
  `pub(crate)` helpers** (`rrf_weights`, `format_sql_f64`, `RRF_K`, `DecisionFilters` + `predicate`,
  `effective_probes`) — none are reachable today (all private to `retrieval.rs`), so T1.3 must land
  first — and builds CTEs over: lexical = `zone_units_bm25_idx` `search_body @@@ …` filtered
  `zone=<zone>`; dense = `zone_unit_embeddings` ivfflat joined to `zone_units` filtered `zone=<zone>`;
  join `zone_units→documents` for citation/court/date + the shared `DecisionFilters` predicate; group
  by `document_id` (DISTINCT-ON best zone fragment, mirror
  `retrieval.rs:331-383`). No `as_of` temporal arm (decisions are dated). Each candidate carries
  `zone_accurate=true`, `provider`, `zone`, the matched fragment snippet, and a keyset `cursor`.
- Acceptance: unit tests of the CTE/predicate builders; an integration query on the clone returns the
  seeded decision under its zone and **nothing** for `--kind article`-style inputs.

### T4.2 `search --zone` routing + readiness gate + scope block
- File: `crates/jurisearch-cli/src/main.rs`.
  - **Both arg surfaces (WARN):** add `zone` to `SearchArgs` (`:263`) **and** `SessionSearchArgs`
    (`:388-423`), and thread it through `session_search_payload` (which rebuilds a `SearchArgs` field by
    field, `:4054-4085`) and the help/schema output — so the agent-facing JSONL/session/serve path is
    not silently missing the capability. Add session tests for the new field.
  - **Dedicated routing + readiness (WARN):** today `search_with_postgres` calls
    `ensure_query_readiness` against the **chunk** corpus, then `hybrid_candidates_json`
    (`:3002-3057`). When `zone` is set, route through a new `zone_search_payload` that **bypasses** the
    chunk readiness check and calls an explicit `ensure_zone_retrieval_readiness` (asserts `zone_units`,
    `zone_unit_embeddings`, `zone_units_bm25_idx`, and the requested fingerprint/model/dimension are
    present) before `zone_candidates_json`. Absent `zone`, the path is byte-identical to today (explicit
    opt-in). Reject `zone` combined with `--kind article` or a non-decision filter with a clear message.
  - **Scope block:** wrap the response with `mode:"official_zone_retrieval"`,
    `coverage:"cour_de_cassation (cass+inca)"`, the indexed-decision count (coverage helper),
    `zone_accurate:true`. (Optionally mirror onto `compare --zone`; defer if not needed for v1.)
- Acceptance: `search --zone motivations "<q>"` (direct AND session) returns zone hits with the scope
  block; default `search`/session output unchanged (golden-output diff); zone search fails clearly when
  zone embeddings/indexes are missing (readiness), independent of chunk readiness; invalid combinations
  rejected.

### T4.3 Review gate Z4
- codex review (scope: the new query module + routing; verify isolation from `hybrid_candidates_json`,
  the `DecisionFilters` reuse, and that default search is unchanged). Apply, r2 to GO.

---

## Z5 — Eval + coverage/gate reporting

**Goal:** an honest, deterministic zone metric and a separate coverage surface that never inflates the
full-juridic claim.

### T5.1 `status.zone_retrieval` coverage block
- Files: `crates/jurisearch-storage/src/zone_units.rs` (`zone_retrieval_coverage_json`, T1.2) +
  `crates/jurisearch-cli/src/main.rs` status payload. A **separate** block (NOT folded into
  `phase2_gate`): per-source `decision_zones` status counts, derived `zone_units` per zone, embedding
  coverage %, and the resolver-reachable denominator (494,701) with the skipped-no-pourvoi count.
- Acceptance: `status` shows `zone_retrieval` with the clone's real numbers; `phase2_gate` output and
  its `zone_accurate=false` corpus assertion are unchanged.

### T5.2 `motivations_retrieval` eval category
- Files: `crates/jurisearch-storage/src/france_juris.rs` (+ gold builder) and the `eval france-juris`
  runner (`EvalFranceJurisArgs`, `:765`). New deterministic category: gold = an identifier-stripped
  excerpt of a decision's official `motivations` zone text (from `zone_units`) → query under `--zone
  motivations` → recall@10 that the source decision is retrieved. Same official-fields-only, no-human/
  no-LLM construction as the shipped france-juris gold. (Optionally add `moyens_retrieval`/
  `dispositif_retrieval`.) Emit a `phase2_zone_benchmark` artifact under
  `work/03-implementation/02-evidence/`, mirroring the france-juris artifact.
- Acceptance: `eval france-juris` runs the new category on the clone and reports recall@10; propose a
  measured floor (≈0.8, calibrate on first run) — measured, never asserted.

### T5.3 Review gate Z5 + promotion
- codex review (scope: the eval gold construction + the coverage block; verify the gold is leak-free
  and the coverage report does not overstate the corpus claim). Apply, r2 to GO.
- **Promotion:** after the full clone passes Z1–Z5 end-to-end (backfill + units + index + eval floor),
  apply the same migrations + run the same backfill/build on production
  `phase2-full-juridic` (refresh the 157G backup first), or swap the validated clone in. Update
  [[phase2-jurisprudence-progress]].

---

## 2. Operational runbook (data-mutating steps)

1. **Clone first** (the standing rule, see [[ask-codex-before-important-decisions]]): rsync
   `phase2-full-juridic` → a working clone; do all of Z1 (migrate), Z2 (backfill), Z3 (build) on the
   clone. Production is touched only at T5.3 promotion.
2. **Backfill throughput:** ~990k PISTE calls ≈ ~14h at ~20 req/s / ~1M-per-14.6h; conservative
   concurrency; resume on stall; watch the by-status report for `upstream_error` spikes.
3. **Embedding:** OpenRouter `baai/bge-m3`, C tuned as in the main run (429-safe), auto-restart wrapper
   ([[embedding-via-openrouter]]); confirm the ivfflat finalize ran ([[embed-chunks-finalize-gap]]).
4. **Idempotence:** every stage is re-runnable and converges; a builder-version bump forces a clean
   rederive; `--since` drives incremental refresh.

## 3. Testing strategy

- **Unit:** SQL builders (injection safety, predicate shape) for every new query helper; `zone_text_hash`
  determinism; fragment derivation (incl. multi-fragment, empty-zone skip); `--zone` arg validation.
- **Integration (temp index):** seed decision → fake `ok` `decision_zones` (with `text_hash`) → derive
  → embed stub → finalize → `zone_candidates_json` returns it; assert default `search`/`fetch` and the
  Phase 2 gate are byte-unchanged (the isolation invariant) in the same test run.
- **End-to-end (clone):** the Z5 eval floor is the regression guard.

## 4. Risks & rollback

- **Isolation regression** (the one that matters): every review explicitly checks that default
  retrieval behavior / candidate SQL output is unchanged — the only permitted `retrieval.rs` edit is
  the T1.3 `pub(crate)` helper extraction, proven output-identical by a golden test; no chunk
  dense/BM25/gate path changes. Rollback = drop the three zone tables/indexes; the main index is
  unaffected by construction.
- **Backfill partiality/freshness:** surfaced by `status.zone_retrieval`; `--since` refresh keeps it
  current; never folded into the full-juridic claim.
- **PISTE quota / transient errors:** resumable backfill + cached negative TTLs (existing
  `cache_zone_status` policy) absorb them.
- **`text_hash` migration of pre-existing rows:** the 2 lazy NULL-hash rows (and any pre-T2.1 rows) are
  re-enriched before derivation via the T1.2 candidate predicate's `status IN ('ok','invalid_offsets')
  AND text_hash IS NULL` clause (TTL-independent); no special migration needed.

## 5. Implementation review-gate notes (from codex r2 — enforce during the per-Zn diff reviews)

- **T1.3:** if `DecisionFilters` moves into `retrieval_common.rs`, keep a compatibility re-export (or
  update all CLI/storage test imports) **in the same diff** — mechanical, but must not break callers.
- **T1.2/T3.2:** `insert_zone_unit_embeddings` must mirror the chunk writer's missing/conflicting-unit
  guard semantics (fail/handle explicitly), **not** silently skip bad staged IDs — the existing
  missing/conflicting tests must assert this.
- **T4.2:** the generic cursor parser is tied to chunk/document cursor tags; the Z4 review must verify
  zone cursors are either parsed exactly by the reused parser or parsed in the zone route **before**
  entering the old `search_with_postgres` flow.

## 6. Decisions to confirm before Z2 execution

- D2 (Cassation-only vs marked-heuristic capp/jade) and D3 (zone-local vs blended ranking) — proceeding
  on the design defaults (Cassation-only, zone-local). Confirm, or redirect before the backfill, since
  they change Z3/Z4 scope. Everything else is settled by the design + A-vs-B decision.
