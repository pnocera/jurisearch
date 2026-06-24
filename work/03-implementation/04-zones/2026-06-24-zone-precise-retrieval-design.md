# Zone-Precise Jurisprudence Retrieval — Design (Option B)

Date: 2026-06-24
Status: DESIGN ONLY (no implementation). This is the build design for the **Option B** parallel
official-zone retrieval subsystem chosen in
`04-zones/reviews/2026-06-24-option-a-vs-b-codex-decision.md`, grounded on the analysis
`04-zones/2026-06-24-zone-precise-retrieval-analysis.md`.
Target index: `/mnt/models/jurisearch-index/phase2-full-juridic` (schema v12).

---

## 0. What is fixed coming in (do not re-litigate)

These are settled by the analysis + the A-vs-B decision and are inputs, not choices, here:

- **Architecture = Option B.** A *separate* zone retrieval subsystem (its own units, dense + BM25
  structures, query path). The main `chunks` / `chunk_embeddings` / `chunks_bm25_idx` path and the
  `HybridCandidateQuery` ranking pool stay **untouched** for non-zone search.
- **Zone units are built from Judilibre zone text, never retrofitted onto the local DILA body**
  (the two-texts divergence, analysis §2.2: offsets index Judilibre's serialization, not ours).
- **Coverage is structurally partial:** official zones exist only for resolver-reachable Cour de
  cassation decisions — **117,674 `cass` + 377,027 `inca` = 494,701 (~43%)** of the 1,144,796-decision
  corpus. `capp` (RG numbers, no Judilibre resolution), `jade` (administrative, not covered), and
  ~31k Cassation decisions lacking a parser-valid pourvoi are **out of scope by construction**.
- **The Phase 2 honesty invariant is preserved.** Bulk jurisprudence chunks remain
  `chunking_provenance="heuristic"` / `zone_accurate=false`
  (`crates/jurisearch-ingest/src/juri/mod.rs:190-196`, gate check
  `crates/jurisearch-cli/src/main.rs:8800-8824`). The zone subsystem is a *separate*,
  `zone_accurate=true`, official-Cassation-only overlay — it never changes what the main-corpus
  chunks mean.

---

## 1. Design goals & non-goals

**Goals**
1. Answer *"find Cour de cassation decisions whose `motivations` (resp. `moyens`, `dispositif`)
   discuss X"* with hybrid retrieval scoped to that official zone, ranked within the zone.
2. Eager, corpus-scale zone DATA for all resolver-reachable Cassation decisions (bulk + incremental
   backfill into the existing `decision_zones` overlay), not the lazy 2-row state of today.
3. A clean provenance/coverage contract: every zone result carries `zone_accurate=true` + provenance;
   every zone response states its Cassation-only, coverage-bounded scope; the gate reports zone
   coverage *separately* and never lets it inflate the "full-juridic" claim.
4. An honest, deterministic eval category so the capability can be measured and regression-guarded.

**Non-goals (v1)**
- No marked-heuristic zone scoping for `capp`/`jade` (would reintroduce the heuristic imprecision the
  feature exists to escape; see §11 decision D2).
- No whole-decision/zone blended ranking surface (strictly zone-local in v1; §11 decision D3).
- No change to default `search`, `fetch`, the main dense/BM25 indexes, or the Phase 2 gate's existing
  `zone_accurate=false` corpus assertion.
- No second Cassation resolver for the ~31k pourvoi-less decisions (open item, analysis §2.1).

---

## 2. Subsystem architecture (the parallel path)

```
                         ┌──────────────────── EXISTING (untouched) ────────────────────┐
  search "X"             │ HybridCandidateQuery → chunks + chunk_embeddings + bm25       │
  (no --zone)  ─────────▶│   → RRF/probes → group by document → candidates               │
                         └──────────────────────────────────────────────────────────────┘

                         ┌──────────────────── NEW (Option B subsystem) ────────────────┐
  search "X"             │ ZoneCandidateQuery → zone_units + zone_unit_embeddings        │
  --zone motivations ───▶│   + zone_units_bm25_idx → RRF/probes → group by decision      │
                         │   → candidates (zone_accurate=true, Cassation-only, coverage) │
                         └──────────────────────────────────────────────────────────────┘

  DATA PIPELINE (eager, 3 separable + resumable stages, mirrors ingest→embed→finalize):

   ① enrich-zones      Judilibre (pourvoi+date)  ──▶  decision_zones   (overlay; also powers
      --source cass|inca  resolver-reachable only        (v12, exists)       fetch --part --online)
                                                              │
   ② build-zone-units  derive from zones_json/raw_json  ──▶  zone_units   (NEW table)
                       (Judilibre text, dedupe, fragments)
                                                              │
   ③ embed-zone-units  bge-m3:1024:normalize:true        ──▶  zone_unit_embeddings (NEW)
      + finalize       (own dense + BM25 indexes)              + zone_units_bm25_idx (NEW)
```

The three stages are deliberately separate (like `ingest juri-archives` → `embed-chunks` →
finalize): each is independently resumable, accountable, and codex-reviewable, and a failure in one
never corrupts the others. `decision_zones` is the durable source of truth for zone text; `zone_units`
+ its indexes are a derived, rebuildable materialization.

---

## 3. Data model (new schema)

The subsystem adds tables physically separate from `chunks`/`chunk_embeddings` so the main ranking
pool, the `chunks_bm25_idx`, and the `load_chunk_embedding_inputs` stale-scan
(`crates/jurisearch-storage/src/dense.rs:34-91`) are never touched. `decision_zones` (v12) is reused
as-is for stage ①.

### 3.1 `zone_units` (derived retrieval units) — migration v13 (sketch)

```sql
CREATE TABLE zone_units (
    zone_unit_id    text PRIMARY KEY,                 -- "<document_id>#<zone>#<fragment_index>"
    document_id     text NOT NULL REFERENCES documents(document_id)      ON DELETE CASCADE,
    zone            text NOT NULL CHECK (zone IN
                       ('motivations','moyens','dispositif','expose','introduction','annexes')),
    fragment_index  integer NOT NULL CHECK (fragment_index >= 0),        -- multi-fragment zones
    body            text NOT NULL,                    -- Judilibre zone fragment text (verbatim)
    search_body     text NOT NULL CHECK (btrim(search_body) <> ''),      -- BM25-analyzed field (§3.3)
    provider        text NOT NULL DEFAULT 'judilibre',
    zone_accurate   boolean NOT NULL DEFAULT true,    -- always true here (official); explicit for honesty
    source          text NOT NULL,                    -- 'cass' | 'inca' (decision source family)
    text_hash       text NOT NULL,                    -- decision_zones.text_hash snapshot, populated by Stage ① (§4)
    zone_unit_builder_version text NOT NULL,          -- derivation-logic version (mirrors chunks.chunk_builder_version)
    zone_schema_version text NOT NULL DEFAULT 'judilibre:v1',
    embedding_fingerprint text,                       -- mirrors chunks.embedding_fingerprint
    created_at      timestamptz NOT NULL DEFAULT now(),
    UNIQUE (document_id, zone, fragment_index)
);
CREATE INDEX zone_units_document_idx ON zone_units(document_id);
CREATE INDEX zone_units_zone_idx     ON zone_units(zone);
```

Notes:
- One row per `(document_id, zone, fragment_index)`. Judilibre warns zones can be non-sequential /
  multi-fragment (analysis §5.1; motivations observed with 2 fragments). v1 indexes **per fragment**
  with a shared zone key (no lossy concatenation), and groups back to one zone hit per decision at
  query time.
- `text_hash` is the deterministic `decision_zones.text_hash` snapshot **newly populated by Stage ①**
  (§4) — it is **NOT** written by today's enrichment helper (which stores `text_hash=NULL`,
  `crates/jurisearch-cli/src/main.rs:3702-3720`). Together with `zone_unit_builder_version` it lets a
  refresh detect and atomically replace stale units for a decision (text changed) **or** force a full
  rebuild on a derivation-logic change (`chunk_builder_version` analogue,
  `crates/jurisearch-ingest/src/juri/mod.rs:286-290`).
- `zone_accurate`/`provider`/`source` are carried on the unit so a result is self-describing without a
  join — every zone candidate can assert official provenance.

### 3.2 `zone_unit_embeddings` (own dense space) — migration v14 (sketch)

```sql
CREATE TABLE zone_unit_embeddings (
    zone_unit_id  text PRIMARY KEY REFERENCES zone_units(zone_unit_id) ON DELETE CASCADE,
    embedding_fingerprint text NOT NULL,
    embedding     vector(1024) NOT NULL,
    model         text NOT NULL,
    dimension     integer NOT NULL CHECK (dimension = 1024),
    created_at    timestamptz NOT NULL DEFAULT now()
);
-- finalized after backfill, lists sized to corpus (≈ √rows), query-time ivfflat.probes reused:
CREATE INDEX zone_unit_embeddings_ivfflat_idx
    ON zone_unit_embeddings USING ivfflat (embedding vector_l2_ops) WITH (lists = <N>);
```

Same locked fingerprint `bge-m3:1024:normalize:true`, same `vector(1024)`, same `vector_l2_ops` /
`ivfflat.probes` machinery — a **separate physical index**, not the `chunk_embeddings` one.

### 3.3 `zone_units_bm25_idx` (own lexical space) — migration v15 (sketch)

Mirror the **current** production lexical contract, not the obsolete v2 shape. `chunks_bm25_idx` was
moved to `contextualized_body` in migration v8 and recreated with a French legal analyzer (ascii-folding
+ French stemmer + French stopwords) in migration v9
(`crates/jurisearch-storage/src/migrations.rs:317-369`); the lexical arm queries
`contextualized_body @@@ …`, not `body` (`crates/jurisearch-storage/src/retrieval.rs:571-581`). The zone
index uses the **same analyzer** over the zone units' `search_body`:

```sql
CREATE INDEX zone_units_bm25_idx
ON zone_units USING bm25 (zone_unit_id, search_body)
WITH (
    key_field = 'zone_unit_id',
    text_fields = '{ "search_body": { "tokenizer": {
        "type": "default", "ascii_folding": true,
        "stemmer": "French", "stopwords_language": "French" } } }'
);
```

`search_body` is populated by Stage ② = the zone fragment `body` (the `contextualized_body` analogue;
v8 seeds `contextualized_body` from `body` when absent and enforces non-empty). A **separate** physical
pg_search index from `chunks_bm25_idx`, so Option B's isolation holds — but now analyzer-equivalent for
accents/morphology/French legal terms, avoiding a silent French-text quality regression in zone search.

`CURRENT_SCHEMA_VERSION` advances 12 → 15 (or one combined migration; split shown for review clarity).
`zone_schema_version='judilibre:v1'` already exists on `decision_zones` and is propagated to units.

---

## 4. Stage ① — eager zone backfill (`enrich-zones`)

Promotes today's lazy 2-row `decision_zones` into an eager corpus-scale overlay, reusing the **exact
proven resolver and enrichment helper** already shipped for `fetch --part --online` (commit 0126879 /
28670a2): pourvoi (first parser-valid `NN-NNNN..` case number) + decision date → Judilibre
`search`/`decision`, char-safe zone normalization, `upsert_decision_zones`
(`crates/jurisearch-storage/src/decision_zones.rs:98-162`).

**New CLI (design):** `jurisearch enrich-zones --source <cass|inca> [--limit N] [--since <update_date>]
[--resume] [--concurrency C]`.

- **Candidate set:** decisions where `source ∈ {cass,inca}` and
  `decision_resolution_metadata_json` yields a non-null `pourvoi`
  (`crates/jurisearch-storage/src/decision_zones.rs:51-76`). The ~31k pourvoi-less Cassation decisions
  are skipped and **counted** (coverage honesty), not silently dropped.
- **Status taxonomy:** reuse the existing `decision_zones.status` CHECK domain
  (`ok|not_found|unsupported|invalid_offsets|upstream_error`,
  `crates/jurisearch-storage/src/migrations.rs:456`). Every attempt writes a row (even misses), so a
  resume skips already-attempted decisions and a coverage report is a single `GROUP BY status` scan.
- **Deterministic `text_hash` — a REQUIRED enrichment change (not free today).** The shipped helper
  writes `decision_zones.text_hash=NULL` for every row, `ok` included
  (`crates/jurisearch-cli/src/main.rs:3702-3720`); the target index confirms `text_hash_not_null=0`.
  So `enrich-zones` must **add** population of a deterministic hash for each `ok` row, defined as
  `sha256(judilibre_text ‖ normalized_zones_json ‖ provider_decision_id ‖ upstream_update_date)`. This
  hash is the snapshot key the derived `zone_units` carry (§3.1) and the refresh/rebuild predicate (§5,
  D6). Pre-existing rows with `text_hash=NULL` (the 2 lazy rows + any written before this change) are
  treated as **stale** and re-enriched before derivation — Stage ② never derives from a NULL-hash row.
- **Rate discipline:** honor live PISTE limits (~20 req/s burst, ~1M per ~14.6h). Default a safe
  steady concurrency; `enrich-zones` is restartable so a quota stall just resumes. Budget:
  ~494,701 × ~2 calls ≈ ~990k calls ≈ ~14h for the one-shot bulk.
- **Accounting / resume / quarantine:** mirror the `ingest juri-archives` operational model (run
  manifest, processed set, resumable) so a multi-hour backfill is safe to interrupt.
- **Side benefit:** because `enrich-zones` writes the same `decision_zones` overlay, eager backfill
  also makes `fetch --part --online` a cache hit for the whole Cassation subset.

**Refresh (incremental):** `--since <update_date>` re-enriches decisions whose Judilibre
`upstream_update_date` advanced or whose (newly-populated) `text_hash` changed. `decision_zones`
already populates `upstream_update_date` / `expires_at`; `text_hash` is filled by the enrichment change
above (it is NULL today). Changed decisions cascade to a stage-②/③ rebuild of just their units (via
`text_hash` mismatch).

---

## 5. Stage ② — zone-unit derivation (`build-zone-units`)

Pure, offline, deterministic transform from `decision_zones` → `zone_units`. No network.

- **Source fields:** v1 indexes `motivations`, `moyens`, `dispositif` — the three the normalizer
  already writes to `decision_zones.zones_json` (analysis §5.1). `expose`/`introduction`/`annexes` are
  present only in `raw_json`; the schema admits them, but indexing them is deferred to a normalizer
  extension + re-derive (§11 decision D4).
- **Multi-fragment:** emit one `zone_units` row per fragment with a shared `(document_id, zone)` and an
  incrementing `fragment_index`; preserve all fragments verbatim (no concatenation).
- **Idempotent rebuild:** delete-and-reinsert a decision's units when **either** the
  `decision_zones.text_hash` differs from the units' `text_hash` (text changed) **or** the current
  builder version differs from the units' `zone_unit_builder_version` (derivation logic changed) — so a
  normalizer/fragmentation change forces a rebuild even when the text is unchanged. A `decision_zones`
  row with `text_hash IS NULL` is re-enriched (Stage ①) **before** derivation, never derived as-is.
  Decisions with `status<>'ok'` produce no units; ③ re-embeds only the changed units.
- **Builder versioning:** `zone_units.zone_unit_builder_version` (NOT NULL, §3.1; the
  `chunks.chunk_builder_version` analogue, validated like
  `crates/jurisearch-ingest/src/juri/mod.rs:286-290`) is part of the stale-unit criterion above.

## 6. Stage ③ — embedding + index finalize (`embed-zone-units`)

A parallel of the existing dense pipeline, pointed at the zone tables:

- **Stale-scan loader** mirroring `load_chunk_embedding_inputs`
  (`crates/jurisearch-storage/src/dense.rs:34-91`) but `zone_units LEFT JOIN zone_unit_embeddings`
  (missing or fingerprint/model/dimension drift). Embedding text = the zone fragment `body`.
- **Embed** via the proven OpenRouter `baai/bge-m3` path, fingerprint `bge-m3:1024:normalize:true`
  (see [[embedding-via-openrouter]]). ~494,701 × ~3 zones ≈ ~1.5M units → single-digit hours.
- **Finalize** mirrors `finalize_dense_rebuild` (`crates/jurisearch-storage/src/dense.rs:93+`):
  build `zone_unit_embeddings_ivfflat_idx` (lists ≈ √rows) and ensure `zone_units_bm25_idx`. Same
  finalize-gap caution as the main index ([[embed-chunks-finalize-gap]]): never leave the ANN index
  unbuilt after an aborted run.

## 7. Stage ④ — zone retrieval query path

A **new** `zone_candidates_json(ZoneCandidateQuery)` alongside `hybrid_candidates_json`
(`crates/jurisearch-storage/src/retrieval.rs:269`), NOT a new branch inside it — Option B keeps the
two query builders separate. It reuses the *mechanics* (RRF constants `RRF_K`/weights via
`rrf_weights()` / `RetrievalOptions`, `ivfflat.probes`, keyset cursors, the BM25⊕dense CTE shape) but
over the zone tables:

```
ZoneCandidateQuery {
    query_text, query_embedding, embedding_fingerprint, retrieval_mode, options, after_cursor,
    zone: Zone,                         // motivations | moyens | dispositif  (required)
    decision_filters: DecisionFilters,  // court/formation/publication/decided_from/decided_to (reused)
    limit, lexical_limit, dense_limit,
}
```

- **Lexical arm:** BM25 over `zone_units_bm25_idx` querying `search_body @@@ …` with the v9 French
  analyzer (mirrors `crates/jurisearch-storage/src/retrieval.rs:571-581`), filtered to `zone = <zone>`.
- **Dense arm:** ivfflat over `zone_unit_embeddings` (probes reused), joined to `zone_units` filtered
  to the zone.
- **Decision metadata + filters:** join `zone_units → documents` so `DecisionFilters`
  (`crates/jurisearch-storage/src/retrieval.rs:90-130`) and citation/court/date all work unchanged
  (decision metadata still lives in `documents`). No `as_of` temporal validity needed — decisions are
  dated, not versioned.
- **Grouping:** group by `document_id`, surface the **best-matching zone fragment** per decision
  (DISTINCT ON pattern from the existing `GroupBy::Document` path,
  `crates/jurisearch-storage/src/retrieval.rs:331-383`), plus the decision's citation/court/date and
  `zone_accurate=true`, `provider`, `zone`, the matched fragment snippet.
- **Ranking:** strictly zone-local in v1 (RRF over zone arms only; no whole-decision blend — D3).

## 8. CLI surface & routing

- `search --zone <motivations|moyens|dispositif> "<query>" [decision filters…]` → routes to the zone
  subsystem. **Explicit opt-in only**; absent `--zone`, behavior is byte-for-byte the current
  `search`. No implicit/automatic routing (never silently switch the user from corpus-wide to a
  Cassation subset).
- `--zone` implies the Cassation-only scope; combining it with `--kind article` (or a non-decision
  filter) is rejected with a clear message.
- **Every zone response is self-labeling:** a `scope` block stating `official Cour de cassation zone
  retrieval (cass+inca)`, the indexed-decision coverage count, and `zone_accurate=true`. A
  corpus-wide-sounding question must never get silently Cassation-only results (analysis §5.5).
- (`compare --zone` optional, same routing; deferred if not needed for v1.)

## 9. Provenance, honesty & the gate

This is the part Option B exists to keep clean.

- **Main corpus invariant unchanged.** The Phase 2 gate check that every bulk jurisprudence source
  reports `zone_accurate=false` (`crates/jurisearch-cli/src/main.rs:8800-8824`) stays exactly as-is —
  zone units are a *separate* subsystem, not bulk chunks, so it is unaffected. No weakening of the
  "bulk is heuristic" meaning.
- **Zone coverage reported separately.** Add a `status.zone_retrieval` block (independent of the
  `phase2_gate` corpus claim): per-source enriched counts by `decision_zones.status`, derived
  `zone_units` counts per zone, embedding coverage, and the resolver-reachable denominator
  (494,701) with the skipped pourvoi-less count surfaced. This is a *coverage report*, not a claim
  that the corpus is zone-accurate.
- **The "full-juridic" claim is NOT extended.** Zone retrieval is presented as a distinct,
  coverage-bounded mode — never folded into the headline corpus claim (codex decision reason #5).
- **Self-describing results.** `zone_accurate=true` + `provider=judilibre` + `zone` on every candidate
  so an agent can never equate an official-zone hit with a heuristic body hit.

## 10. Evaluation (`eval france-juris` extension)

A new deterministic, official-fields-only category, slotting into the existing `france_juris.rs` /
`eval france-juris` pattern (no human/LLM, like the shipped gate):

- **`motivations_retrieval` (and `moyens_retrieval`/`dispositif_retrieval`):** gold = an
  identifier-stripped excerpt of a decision's official `motivations` zone text (from `zone_units`);
  query it under `--zone motivations`; measure **recall@10** that the source decision is retrieved.
- **Floor:** propose ≥ 0.8 recall@10 for `motivations_retrieval` (calibrate on a first run, like the
  judicial/administrative floors were); a measured floor, never asserted.
- **Guards** against a silently-degraded zone index (e.g. finalize gap) and gives the capability a
  claimable number. Emitted as a `phase2_zone_benchmark` artifact under `02-evidence/`, mirroring the
  `phase2_france_juris_benchmark` artifact.

## 11. Open decisions — resolved here, flag for confirmation

| # | Question (analysis §8) | Design decision (v1) | Why |
|---|---|---|---|
| D1 | Option A vs B | **B** (settled) | preserves proven path + honesty boundary (decision doc) |
| D2 | Marked-heuristic zone scoping for capp/jade? | **No — Cassation-only, official-only** | the feature's value is precision; a heuristic tier reintroduces the imprecision it escapes + a 2nd provenance tier (analysis §5.5) |
| D3 | Strictly zone-local vs whole-decision blend | **Strictly zone-local** | simplest honest semantics; blend is an unproven enhancement, deferrable |
| D4 | Which zones to index | **motivations + moyens + dispositif** (already normalized) | highest legal value; expose/intro/annexes need a normalizer extension + re-derive, deferred |
| D5 | Gate/coverage reporting | **Separate `status.zone_retrieval` block; don't extend full-juridic claim** | keeps the corpus claim honest (decision reason #5) |
| D6 | Backfill operating model | **One-shot bulk, then `--since` incremental refresh** keyed on `update_date`/`text_hash` | `decision_zones` carries `update_date`/`expires_at`; enrichment must ADD deterministic `text_hash` (§4) |

D2 and D3 are genuine product calls; the recommended defaults above are the "best for jurisearch"
(precision-first) reading. Confirm before building if there is a competing product preference.

## 12. Build phases (each gated by a codex review before execution)

Design-level sequencing only — no code here. Per the standing rule, **every stage gets a codex review
before any code is executed**, and live API/index mutation only after GO.

1. **Z1 — schema:** migrations v13–v15 (`zone_units`, `zone_unit_embeddings`, `zone_units_bm25_idx`),
   `CURRENT_SCHEMA_VERSION` bump, applied to a **clone** of the production index (never the live one
   first). Storage helpers for upsert/load/finalize of zone units.
2. **Z2 — backfill (`enrich-zones`):** the eager Judilibre overlay populate, with accounting/resume,
   **including the new deterministic `text_hash` population for `ok` rows** (§4 — the BLOCKER fix; the
   current helper writes NULL). Dry-run on a small `--limit` slice, codex-review the diff, then run the
   full ~14h bulk.
3. **Z3 — derive + embed (`build-zone-units`, `embed-zone-units` + finalize):** materialize and index
   the units; verify the ANN index is built (no finalize gap).
4. **Z4 — query path + CLI (`zone_candidates_json`, `search --zone`):** the parallel retrieval path
   and routing, with the self-labeling scope/coverage block.
5. **Z5 — eval + gate report:** the `motivations_retrieval` category and `status.zone_retrieval`
   coverage block; emit the zone benchmark artifact.

Each phase is independently shippable and reviewable; Z1–Z3 produce data with zero impact on existing
search until Z4 wires the query path.

## 13. Risks accepted (from the decision) & mitigations

- **Second retrieval index to operate** (the accepted cost of B): localized to the zone tables;
  refresh + coverage are explicit, reported, and resumable.
- **External dependency / freshness:** completeness tracks the Judilibre backfill; `--since` refresh +
  the newly-populated `text_hash` (§4) / `update_date` + a coverage report make staleness visible, not
  silent.
- **Two-texts divergence:** units are Judilibre-derived by construction; the product never implies
  offset-alignment with the local body.
- **Structural partiality (~43%, Cassation-only):** surfaced in every response scope block and the
  coverage report; never folded into the full-juridic claim.
- **Switch condition (from the decision):** only if a single unified whole-decision⊕zone ranking
  surface is later required would we revisit — and then via **A2**, never A1, and only after the schema
  grows explicit unit-type/provenance/exclusion contracts. Out of scope here.

---

## 14. Explicitly out of scope (this design)

- A second Cassation resolver for the ~31k pourvoi-less decisions (open research item).
- capp/jade zone coverage of any kind.
- Whole-decision/zone blended ranking.
- Indexing expose/introduction/annexes (needs normalizer extension first).
- Any change to default `search`, `fetch`, the main dense/BM25 indexes, or the existing Phase 2 gate
  corpus assertion.
