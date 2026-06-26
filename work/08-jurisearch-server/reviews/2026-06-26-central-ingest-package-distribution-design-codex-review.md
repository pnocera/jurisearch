# Central ingest package-distribution design review

## BLOCKER

### 1. Per-corpus package streams are incompatible with a single global generation/view switch as written

The design repeatedly fixes package distribution as per-corpus (`core`, `inpi`, etc.) and says one client DB can hold the mix of corpora the client is entitled to (`§1.4`, `§6.1`, `§11.3`). However, the generation model is described as one physical schema `jurisearch_server_gNNNN` behind one stable `jurisearch_server` view namespace (`§4.1`, `§4.3`, `§7.4`). A baseline/rebaseline then loads a new physical schema and repoints the stable views to that generation.

That is underspecified and potentially wrong for the decided architecture. If `core` is rebaselined while `inpi` is already installed, a global `jurisearch_server` view switch to a new `core` generation would either hide/drop the `inpi` corpus or require the new generation to contain a merged copy of every other active corpus. The document does not define either behavior. The per-corpus `jurisearch_control.corpus_state` cursor makes the ambiguity sharper: the cursor is per corpus, but the view switch is described as global.

Recommended fix: define generation granularity explicitly. The lowest-risk design is per-corpus physical generations, e.g. `jurisearch_server_core_g000124`, `jurisearch_server_inpi_g000031`, with `jurisearch_server` stable views/functions selecting or unioning the active generation per corpus. The control registry should map `(corpus -> active_generation, sequence, baseline_id, schema_version, fingerprints)`, and a rebaseline should switch only that corpus's generation without changing visibility of other corpora. If the intended design is instead a whole-server generation containing every installed corpus, say so and specify how a per-corpus package/baseline is merged into the global generation without losing other corpora.

## WARN

### 1. `updated_at` constraint C7 overstates the current schema

The C7 row says "only `documents` has" `updated_at`. That is not true in the current migrations. `documents` has `updated_at` at `crates/jurisearch-storage/src/migrations.rs:43`, but replicated tables also include `legi_metadata_roots.updated_at` at `migrations.rs:207` and `legislation_citation_resolutions.updated_at` at `migrations.rs:689`. The source also confirms those columns are actively stamped: `insert_legi_metadata_roots_with_client` updates `updated_at = now()` on conflict in `crates/jurisearch-storage/src/projection/metadata.rs:43-56`, and citation resolution maintenance updates `updated_at` in `crates/jurisearch-storage/src/legislation_citations.rs:142-146` and `:206-209`.

The design consequence is still correct: there is no uniform high-water mark across the replicated set because `chunks`, `chunk_embeddings`, `graph_edges`, `zone_units`, `zone_unit_embeddings`, `decision_zones`, and `official_api_responses` do not provide a consistent update watermark, and LEGI chunk/graph upserts do not stamp an update time (`projection/legi.rs:73-105`).

Recommended fix: change C7 from "only `documents` has it" to a precise statement such as: "`updated_at` is not uniform across replicated tables: some base/metadata tables have it, but key replicated and derived tables do not, and several upsert paths do not stamp it. Therefore a generic `updated_at` cursor cannot drive package diffs." Include `legi_metadata_roots` and `legislation_citation_resolutions` in the examples so future implementers do not build a false assumption into the outbox design.

### 2. Cursor advancement and index-build activation are internally inconsistent

`§6.2.2` says the package has an `index_build` contract and that the default is "not advertised active until indexes built and manifests written." `§11.1` says the cursor advances only after all data, indexes, and postcondition checks pass. But `§7.1` orders service work as apply -> advance the control cursor -> background reference validation -> index build, and `§7.3` advances `corpus_state.sequence` after data postconditions without stating whether index work has completed.

That creates two possible readings: either a package can be marked applied before the client is query-ready, or index work is actually part of apply/activation. The latter matches the manifest contract and the current finalize discipline: `finalize_dense_rebuild` verifies coverage, drops/recreates the IVFFlat index, analyzes tables, and writes `index_manifest` (`crates/jurisearch-storage/src/dense.rs:122-190`), and `finalize_zone_dense_rebuild` mirrors that for zone units (`crates/jurisearch-storage/src/zone_units.rs:461-524`).

Recommended fix: make the apply state machine explicit. For baselines/rebaselines, keep the current rule that table load, BM25, IVFFlat finalize, `ANALYZE`, `index_manifest`, and validation all complete before view switch and cursor activation. For incrementals, state whether ordinary row-level index maintenance is sufficient or whether any package can require explicit post-apply index/finalize work. If explicit index work is required, cursor advancement and active advertisement must wait for it. If it is not required for ordinary incrementals, say that clearly and reserve `index_build` for baselines/rebaselines or schema/index-definition packages.

## NIT

None.

## Code-grounding notes

The main C1-C9 facts otherwise check out against source: migrations are unqualified and schema version 17; `run_migrations` rejects `SchemaVersionAhead`; LEGI documents/chunks/graph edges use upserts; LEGI and jurisprudence document IDs are deterministic; zone-unit and hierarchy paths perform scoped deletes/rebuild invalidation; IVFFlat indexes are finalize-time drop/recreate products; the FK cascades cited in C8 exist; and the current `serve` daemon is sequential JSONL over local TCP/Unix socket with unauthenticated non-loopback binding refused unless explicitly allowed.

VERDICT: FIXES_REQUIRED
