# Reusable ingestion knowledge from `/home/pierre/Apps/juridocs`

Date: 2026-06-21

Scope: identify which ingestion knowledge from `juridocs` can accelerate `jurisearch`, and how to use it without weakening the locked `jurisearch` design.

Note: CodeGraph is not initialized in either repository, so this analysis is based on direct source and document reads.

## Current `jurisearch` ingestion target

`jurisearch` is still design/planning only. The relevant locked constraints are in:

- `/home/pierre/Work/jurisearch/work/01-design/DESIGN.md`
- `/home/pierre/Work/jurisearch/work/02-conception/CONCEPTION.md`
- `/home/pierre/Work/jurisearch/work/03-implementation/IMPLEMENTATION_PLAN.md`

The ingestion target is:

- official DILA/LEGI XML from day one;
- optional Python only before the canonical-record boundary;
- Rust owns canonical validation, indexing, chunking, manifests, and all query paths;
- canonical `Document`, `Chunk`, and graph-edge records are the contract;
- temporal article versions are mandatory;
- `valid_to = null` is the open-ended validity sentinel;
- structure-aware statutory chunking comes before arbitrary text splitting;
- decision zone chunking must use official publisher zones when available;
- embedded Postgres + `pgvector` + `pg_search` is selected for `jurisearch`, so `juridocs` table shapes should be adapted rather than copied wholesale.

## High-value knowledge to reuse

### 1. Archive naming, precedence, and replay order

Reusable sources:

- `/home/pierre/Apps/juridocs/docs/specs/archive-precedence-and-delta-rules.md`
- `/home/pierre/Apps/juridocs/crates/jd-ingest/src/archive/parser.rs`
- `/home/pierre/Apps/juridocs/crates/jd-ingest/src/archive/planner.rs`
- `/home/pierre/Apps/juridocs/crates/jd-ingest/src/archive/inventory.rs`
- `/home/pierre/Apps/juridocs/tests/unit/tests/test_archive_ordering.rs`

What to reuse:

- Baseline archive pattern: `Freemium_{source}_global_YYYYMMDD-HHMMSS.tar.gz`.
- Delta archive pattern: `{SOURCE}_YYYYMMDD-HHMMSS.tar.gz`.
- Select the latest baseline by full timestamp.
- Apply only deltas strictly after the selected baseline.
- Sort deltas by timestamp ascending, with filename tie-breaker.
- Reject mixed datasets in one ingest plan.

How to use it in `jurisearch`:

- Create a `jurisearch-ingest::archive` module early in Phase 0.
- Port the planner semantics and tests, but replace `jd_core::dataset::Dataset` with a `jurisearch` source enum, initially at least `legi`, later `jorf`, `judilibre`, `ja`, and any DILA jurisprudence bulk families we decide to support.
- Treat archive planning as an explicit ingest artifact recorded in the manifest, not just a runtime detail.
- Use the same replay order for reproducibility and for `sync --since`.

### 2. Streaming archive member processing

Reusable sources:

- `/home/pierre/Apps/juridocs/crates/jd-ingest/src/archive/reader.rs`
- `/home/pierre/Apps/juridocs/crates/jd-ingest/src/ingest/supervisor.rs`
- `/home/pierre/Apps/juridocs/docs/dataflow/legi-ingest-embedding-dataflow.md`

What to reuse:

- Stream `.tar.gz` members through a bounded channel instead of materializing all matching members.
- Filter to XML members.
- Cap member byte size (`MAX_MEMBER_BYTES` in `juridocs`) to avoid runaway payloads.
- Keep human progress observable while preserving machine-readable output discipline.

How to use it in `jurisearch`:

- Use streaming member reads for full LEGI baseline ingestion from the first implementation.
- Make member size caps configurable and recorded in the ingest manifest.
- Preserve deterministic member ordering. If parallel parsing is added, force deterministic write/accounting semantics and test replay snapshots.

### 3. DTD-backed parser contracts and required-field validation

Reusable sources:

- `/home/pierre/Apps/juridocs/docs/specs/legi-dtd-coverage-matrix.md`
- `/home/pierre/Apps/juridocs/crates/jd-ingest/src/legi/model.rs`
- `/home/pierre/Apps/juridocs/crates/jd-ingest/src/legi/parser.rs`
- `/home/pierre/Apps/juridocs/crates/jd-ingest/src/legi/validation.rs`
- `/home/pierre/Apps/juridocs/tests/unit/tests/test_dtd_entity_parsers.rs`
- `/home/pierre/Apps/juridocs/tests/unit/tests/test_dtd_required_fields.rs`

What to reuse:

- Typed ID wrappers for `LEGITEXT`, `LEGIARTI`, and `LEGISCTA`.
- Structured parse errors split into XML, required-field, invalid-date, and invalid-ID cases.
- DTD requiredness as an implementation contract, not informal documentation.
- Parser models for `TEXTE_VERSION`, `ARTICLE`, `SECTION_TA`, and `TEXTELR`.
- `CONTEXTE` extraction for text and section ancestry.

How to use it in `jurisearch`:

- Use the `juridocs` parser implementation as a concrete reference for the Phase 0 LEGI parser spike.
- Emit `jurisearch` canonical records from parsed models instead of writing directly to `legi_*` tables.
- Keep validation errors actionable and route them to structured ingest errors.
- Treat the `juridocs` DTD matrix as a starting checklist, then re-verify the fields against the current DTD files before making it authoritative in `jurisearch`.

### 4. Temporal versioning rules

Reusable sources:

- `/home/pierre/Apps/juridocs/docs/specs/archive-precedence-and-delta-rules.md`
- `/home/pierre/Apps/juridocs/crates/jd-db/migrations/0002_core_entity_tables.sql`
- `/home/pierre/Apps/juridocs/crates/jd-ingest/src/legi/loader.rs`
- `/home/pierre/Apps/juridocs/tests/integration/tests/test_temporal_query_flow.rs`
- `/home/pierre/Apps/juridocs/tests/integration/tests/test_temporal_query_golden.rs`
- `/home/pierre/Apps/juridocs/fixtures/temporal/LEGIARTI000006590700_versions.json`

What to reuse:

- Article/text versions are keyed by source ID plus `date_debut`.
- Open-ended `date_fin` sentinels are normalized to null at ingestion.
- Query-time temporal semantics are `date_debut <= as_of` and `(date_fin is null or as_of < date_fin)`.
- Historical queries need golden fixtures, not only unit tests.

How to use it in `jurisearch`:

- Map this directly to `Document.id = "legi:<LEGIARTI>@<valid_from>"` and `version_group = <LEGIARTI>`.
- Preserve raw `dateFin` as provenance (`valid_to_raw`) even when normalized to null.
- Add temporal fixture coverage in Phase 0, including current, modified, abrogated, same-day boundary, and sentinel cases.
- Do not use upsert-overwrite semantics for article history. Every article version must remain addressable.

### 5. Source provenance, member accounting, and recovery

Reusable sources:

- `/home/pierre/Apps/juridocs/crates/jd-db/migrations/0006_provenance_tables.sql`
- `/home/pierre/Apps/juridocs/crates/jd-db/migrations/0007_ingest_resume_indexes.sql`
- `/home/pierre/Apps/juridocs/crates/jd-ingest/src/ingest/lifecycle.rs`
- `/home/pierre/Apps/juridocs/crates/jd-ingest/src/ingest/error.rs`
- `/home/pierre/Apps/juridocs/crates/jd-ingest/src/ingest/supervisor.rs`
- `/home/pierre/Apps/juridocs/crates/jd-ingest/src/ingest/quarantine.rs`
- `/home/pierre/Apps/juridocs/tests/integration/tests/test_ingest_run_lifecycle.rs`
- `/home/pierre/Apps/juridocs/tests/integration/tests/test_resume_after_interruption.rs`
- `/home/pierre/Apps/juridocs/tests/integration/tests/test_ingest_provenance_tables.rs`

What to reuse:

- `ingest_run`, `ingest_member`, and `ingest_error` as first-class operational tables.
- Member identity includes run, archive name, member path, and date anchor.
- Resume skips `inserted`/`skipped` members but can retry failed or parsed members.
- Structured error classes distinguish parse, validation, DB, IO, embedding, and unknown failures.
- Optional quarantine of failed payloads by run/archive/member/error.

How to use it in `jurisearch`:

- Implement equivalent tables in the embedded Postgres schema or in the canonical build database.
- Add `parser_version`, `schema_version`, `source_payload_hash`, and `code_version` to the `jurisearch` run/member model. `juridocs` notes warn that recovering after parser/schema changes can preserve stale bad rows.
- Make unsupported roots explicit, e.g. `ignored_unsupported`, not `inserted unknown`.
- Surface this data in `jurisearch status --json` so agents can see corpus freshness, coverage, and ingest health.

### 6. Canonical payload construction and payload hashing

Reusable sources:

- `/home/pierre/Apps/juridocs/crates/jd-ingest/src/legi/canonical_payload.rs`
- `/home/pierre/Apps/juridocs/crates/jd-ingest/src/jurisp/canonical_payload.rs`
- `/home/pierre/Apps/juridocs/crates/jd-ingest/src/legi/provenance_payload.rs`
- `/home/pierre/Apps/juridocs/fixtures/embeddings/canonical_payload/fr_sent_v1_cases.json`

What to reuse:

- Build semantic payloads from a fixed ordered field list.
- Hash the exact source payload used for embeddings and chunk provenance.
- Record source fields and builder flags in deterministic JSON.

How to use it in `jurisearch`:

- Define canonical text assembly as a versioned contract per `Document.kind`.
- Store `source_payload_hash`, `source_fields`, `chunk_builder_version`, and `embedding_fingerprint` per chunk.
- Use fixed field order in tests so re-embedding and regression diffs are explainable.
- Adapt payloads to `jurisearch`'s `Document`/`Chunk` model. Do not inherit `juridocs`' mean-pooled entity vector as the primary retrieval abstraction.

### 7. French sentence splitting and embedding-size guardrails

Reusable sources:

- `/home/pierre/Apps/juridocs/crates/jd-ingest/src/legi/chunking.rs`
- `/home/pierre/Apps/juridocs/crates/jd-ingest/src/legi/guardrail.rs`
- `/home/pierre/Apps/juridocs/fixtures/embeddings/chunking/fr_abbreviations_v1.txt`
- `/home/pierre/Apps/juridocs/fixtures/embeddings/chunking/fr_sentence_cases_v1.json`
- `/home/pierre/Apps/juridocs/docs/dataflow/legi-ingest-embedding-dataflow.md`

What to reuse:

- Pragmatic French sentence segmentation plus deterministic newline boundaries.
- Legal abbreviation repair for forms like `art.`, `L.`, `R.`, numbered enumerations, and similar legal references.
- Hard splitting of oversize unbroken sentences/lines.
- Token-budget guardrail learned from real endpoint overflows: char-based chunking alone is insufficient.

How to use it in `jurisearch`:

- Reuse the French sentence splitter as a subroutine for long article sub-chunking.
- Keep statutory chunking structure-first: article version first, then split only when an article is too large.
- Add a real tokenizer or conservative endpoint-specific preflight before embedding. Keep the `juridocs` guardrail as a fallback estimate, not the only control.
- Record whether a chunk came from structural, zone, or heuristic/hard-split logic.

### 8. Link and reference extraction

Reusable sources:

- `/home/pierre/Apps/juridocs/crates/jd-ingest/src/legi/link_extract.rs`
- `/home/pierre/Apps/juridocs/crates/jd-ingest/src/legi/inline_reference.rs`
- `/home/pierre/Apps/juridocs/crates/jd-ingest/src/ingest/links.rs`
- `/home/pierre/Apps/juridocs/crates/jd-db/migrations/0004_link_tables.sql`
- `/home/pierre/Apps/juridocs/crates/jd-db/migrations/0005_graph_and_search_tables.sql`
- `/home/pierre/Apps/juridocs/tests/integration/tests/test_link_integrity_golden.rs`
- `/home/pierre/Apps/juridocs/tests/integration/tests/test_inline_reference_extraction.rs`

What to reuse:

- Extraction of DILA `LIEN`, `LIEN_ART`, `LIEN_SECTION_TA`, `LIEN_TXT`, and inline HTML anchor references.
- Source-context-aware extraction so links know the source entity and source version date.
- Idempotent/deduplicated link writes.

How to use it in `jurisearch`:

- Convert these extracted relationships into `GraphEdge` canonical records.
- Preserve `edge_source = publisher` for explicit DILA links.
- Add a separate `edge_source = inferred` path only for regex/free-text citation detection.
- Keep link extraction after source entity parsing, but before graph materialization, so graph rebuilds are deterministic from canonical records.

### 9. LEGI hierarchy and context extraction

Reusable sources:

- `/home/pierre/Apps/juridocs/crates/jd-ingest/src/legi/model.rs`
- `/home/pierre/Apps/juridocs/crates/jd-ingest/src/legi/parser.rs`
- `/home/pierre/Apps/juridocs/crates/jd-ingest/src/ingest/models.rs`
- `/home/pierre/Apps/juridocs/crates/jd-ingest/src/legi/loader.rs`
- `/home/pierre/Apps/juridocs/tests/unit/tests/test_dtd_entity_parsers.rs`

What to reuse:

- `ParsedContexte`, `ParsedContexteTitreTxt`, and `ParsedContexteTitreTm` model how article/section context is present in DILA XML.
- The parser already flattens nested `TITRE_TM` entries into a path-like list.
- Section files can carry parent text hints useful for article ordering and hierarchy.

How to use it in `jurisearch`:

- Build `hierarchy_path` from this context for every statutory `Document`/`Chunk`.
- Include path labels in the embedding prefix while keeping returned text clean.
- Test that `Code -> Livre -> Titre -> Chapitre -> Section -> Article` survives ingestion for fixture articles.

### 10. Derived projection/backfill discipline

Reusable sources:

- `/home/pierre/Apps/juridocs/crates/jd-ingest/src/legi/loader.rs`
- `/home/pierre/Apps/juridocs/apps/jd-cli/src/ingest.rs`
- `/home/pierre/Apps/juridocs/scripts/repair_backfill_data.sh`
- `/home/pierre/Apps/juridocs/scripts/backfill_search_index.sh`
- `/home/pierre/Apps/juridocs/scripts/backfill_embeddings_chunked.sh`
- `/home/pierre/Apps/juridocs/docs/runbooks/ingest-write-path-rollback.md`

What to reuse:

- Separate source-of-truth loads from derived search/reference/embedding projections.
- Fast ingest can defer projections, but query access must be gated until backfills complete.
- Write-path optimizations need a conservative rollback mode.

How to use it in `jurisearch`:

- Let canonical record creation and validated source table writes complete before search/vector/graph projections.
- Make `jurisearch ingest` fail or warn loudly if an index is queryable before derived layers pass integrity gates.
- Add a safe-mode ingest path from day one, even if the initial implementation is simple.

### 11. Jurisprudence bulk XML parser and loader lessons

Reusable sources:

- `/home/pierre/Apps/juridocs/docs/plans/jurisp/jurisprudence-ingestion-macro-plan.md`
- `/home/pierre/Apps/juridocs/docs/plans/juris/status.md`
- `/home/pierre/Apps/juridocs/docs/plans/juris/contracts/jurisprudence-mapping-matrix.md`
- `/home/pierre/Apps/juridocs/crates/jd-ingest/src/jurisp/model.rs`
- `/home/pierre/Apps/juridocs/crates/jd-ingest/src/jurisp/parser.rs`
- `/home/pierre/Apps/juridocs/crates/jd-ingest/src/jurisp/loader.rs`
- `/home/pierre/Apps/juridocs/crates/jd-db/migrations/0016_jurisp_core_tables.sql`
- `/home/pierre/Apps/juridocs/crates/jd-db/migrations/0017_jurisp_shared_table_extensions.sql`
- `/home/pierre/Apps/juridocs/fixtures/jurisprudence/golden/`
- `/home/pierre/Apps/juridocs/tests/unit/tests/test_jurisp_parser.rs`
- `/home/pierre/Apps/juridocs/tests/integration/tests/test_jurisp_loader.rs`
- `/home/pierre/Apps/juridocs/tests/integration/tests/test_jurisp_idempotency.rs`

What to reuse:

- Source taxonomy for DILA jurisprudence bulk archives: `cass`, `inca`, `capp`, `jade`.
- XML roots: `TEXTE_JURI_JUDI` and `TEXTE_JURI_ADMIN`.
- Decision ID validation for `JURITEXT*` and `CETATEXT*`.
- Parsing of `META_COMMUN`, `META_JURI`, source-specific metadata, `BLOC_TEXTUEL`, `SOMMAIRE`, `CITATION_JP`, and `LIENS`.
- Correction-safe upsert logic and replay invariants for redelivered decisions.
- Golden fixture layout for four jurisprudence families.

How to use it in `jurisearch`:

- Use this as the Phase 2 DILA bulk XML ingestion reference.
- Map `JURITEXT`/`CETATEXT`, ECLI, court, solution, publication, text, summaries, and links into `Document(kind=decision)` and graph edges.
- Keep DILA bulk XML support distinct from Judilibre API support. Judilibre offers official zone offsets and transactional history; `juridocs`' DILA parser does not satisfy the official-zone chunking requirement by itself.
- For DILA bulk decisions without publisher zones, chunk as heuristic/fallback and mark provenance accordingly.

### 12. Quality gates, replay invariants, and cutover runbooks

Reusable sources:

- `/home/pierre/Apps/juridocs/scripts/audit_ingest_quality.sh`
- `/home/pierre/Apps/juridocs/scripts/audit_jurisp_ingest_quality.sh`
- `/home/pierre/Apps/juridocs/scripts/run_jurisp_quality_gates.sh`
- `/home/pierre/Apps/juridocs/scripts/run_jurisp_replay_invariants.sh`
- `/home/pierre/Apps/juridocs/scripts/verify_write_path_integrity.sh`
- `/home/pierre/Apps/juridocs/scripts/run_jurisp_e2e_lane.sh`
- `/home/pierre/Apps/juridocs/docs/runbooks/jurisp-cutover.md`
- `/home/pierre/Apps/juridocs/tests/e2e/tests/test_jurisp_end_to_end_ingest_search.rs`
- `/home/pierre/Apps/juridocs/tests/e2e/tests/test_jurisp_quality_gates.rs`
- `/home/pierre/Apps/juridocs/tests/e2e/tests/test_jurisp_acceptance_reports.rs`

What to reuse:

- Gate on latest completed ingest runs per source.
- Gate failed-member percentage and ingest-error counts.
- Gate projection and embedding coverage.
- Gate replay by snapshot/diff, not by trusting row-count summaries alone.
- Keep cutover blocked when mandatory gates fail.

How to use it in `jurisearch`:

- Build `jurisearch` Phase 0 eval/ingest gates before full feature work.
- Add replay snapshots over canonical records, chunks, graph edges, embeddings, and manifest fields.
- Report gates as markdown plus machine-readable JSON.
- Make `jurisearch status` derive its freshness/coverage claims from the same gate data.

## Important things not to copy directly

Do not inherit these as-is:

- `search_document` and `reference_index` as the core search contract. `jurisearch` wants `Document`, `Chunk`, graph edges, command schemas, and an agent-facing CLI contract.
- The fixed `vector(768)` assumption in `juridocs` migrations. `jurisearch` must take vector dimension from the embedding model fingerprint.
- The current `jd_embed`/mean-pooled entity vector model as the retrieval design. `jurisearch` should rank chunks directly, with exact temporal and jurisdiction filters before fusion.
- `juridocs` jurisprudence sentence chunking as a substitute for official Judilibre/justice-admin zones. Use it only as fallback when official offsets are missing.
- Any path where embedding failure makes the source-of-truth entity look failed without a clear retry/backfill distinction. In `jurisearch`, core canonical ingestion and embedding/index projection failures should be accounted separately.
- The loose `unknown` unsupported-root behavior described in older `juridocs` docs. Unsupported roots need explicit classification and counters.
- Any derived dataset shortcut. `jurisearch` is official-source-only for authoritative records.

## Recommended `jurisearch` implementation sequence using this knowledge

1. Implement `jurisearch-ingest::archive` from the `juridocs` archive parser/planner/reader patterns, with tests copied/adapted from `test_archive_ordering.rs`.
2. Define canonical `Document`, `Chunk`, `GraphEdge`, `Manifest`, and `IngestError` schemas before writing database tables.
3. Port/adapt LEGI parser model and validation concepts, but make the parser emit canonical records first.
4. Add member-level run accounting, quarantine, and replay/resume before full-corpus ingestion.
5. Implement temporal normalization and fixture tests around `LEGIARTI` version histories.
6. Implement hierarchy extraction from `CONTEXTE` and `TEXTELR` before chunking, so every article chunk carries scope.
7. Implement structural statutory chunking. Use the `juridocs` French sentence splitter and guardrails only for long-article sub-splitting.
8. Materialize graph edges from DILA links and inline references as derived records that can be rebuilt from canonical records.
9. Add embedding payload hashing and chunk provenance. Include provider/model/dimension/normalization in the manifest.
10. Add ingestion quality gates and replay snapshot gates before claiming Phase 1 LEGI quality.
11. For Phase 2, adapt `juridocs` `jurisp` parser for DILA bulk JURI/JADE data, but separately implement Judilibre official-zone ingestion.

## Bottom line

`juridocs` is most valuable as a battle-tested ingestion playbook: archive ordering, streaming, parser validation, temporal semantics, provenance, replay, quality gates, and operational runbooks. It is less valuable as a direct storage/search design for `jurisearch`, because `jurisearch` has a stricter canonical-record boundary, chunk-first retrieval model, selected embedded Postgres/`pg_search` backend, and CLI agent contract.

