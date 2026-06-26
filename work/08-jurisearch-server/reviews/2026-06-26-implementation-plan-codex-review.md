# Review: central-ingest package-distribution implementation plan

Target reviewed: `work/08-jurisearch-server/2026-06-26-central-ingest-package-distribution-implementation-plan.md`

## Findings

### BLOCKER - Phase 1 / Phase 4 conflate package sequence with outbox watermarks

Plan location: Phase 1 "Outbox read API" (`scopes changed for corpus C since package_sequence N`, lines 236-238), Phase 1 acceptance ("between two arbitrary sequences", lines 249-250), and Phase 4 incremental builder ("since the corpus's last package_sequence", lines 361-366).

The design is explicit that `package_change_log.change_seq` is a global audit/build order and that `from_sequence` / `to_sequence` / `corpus_state.sequence` are a separate per-corpus package sequence. The plan correctly types these as distinct in P0, but then defines the outbox read API in terms of `package_sequence`. That is not directly implementable against the outbox table as designed, because outbox rows do not live in package-sequence space. Without a producer-side package-chain/catalog state that records which `change_seq` range was included in each package sequence, the incremental builder can duplicate or miss scopes during concurrent ingest and cannot prove the chain boundary it publishes.

Recommended fix: add an explicit producer package-build catalog/state deliverable before or inside Phase 4, keyed by corpus, that records `package_sequence`, `previous_package_id`/digest, `included_change_seq_low/high` or an equivalent frozen outbox watermark, baseline id, build status, and publish status. Change the outbox read API to read by `change_seq` bounds derived from that catalog; keep package sequence only in manifests, remote listing, and client cursor checks. Add an acceptance test with interleaved `core`/`inpi` outbox rows and concurrent ingest during package build to prove no false `sequence_gap`, no duplicate scopes, and no dropped scopes.

### WARN - Phase 1 hook inventory omits replicated write paths

Plan location: Phase 1 "Emit hooks at every projection boundary" (lines 227-235) and Phase 1 acceptance (lines 244-250).

The design's replicated table set includes `legi_metadata_roots`, `decision_legislation_citations`, and `legislation_citation_resolutions`, in addition to documents/chunks/edges, embeddings, zones, and `official_api_responses`. The plan's concrete hook list names `projection/legi.rs`, `projection/decisions.rs`, `projection/embeddings.rs`, `zone_units.rs`, `decision_zones.rs`, `hierarchy_backfill.rs`, and `official_api_archive.rs`, but it does not name `projection/metadata.rs` or `legislation_citations.rs`. Source inspection confirms both are real replicated writers: `insert_legi_metadata_roots_with_client` upserts `legi_metadata_roots`, and `legislation_citations.rs` inserts/updates the two citation tables that depend on `official_api_responses.response_id`.

The generic coverage-test risk mitigation helps, but the plan should not rely on a later grep-backed inventory to discover tables already fixed by the design contract. As written, an implementer could satisfy the visible hook list and still ship incrementals that miss metadata-root changes or citation extraction/resolution changes.

Recommended fix: extend Phase 1 deliverables and acceptance to explicitly include outbox hooks for `projection/metadata.rs` and `legislation_citations.rs`, and add `ingest collect-legislation-citations`, `ingest enrich-legislation-citations`, and `ingest backfill-legi-hierarchy` to the fixture command set. The coverage test should assert every table in design §4.2's replicated set has exactly one owned writer/outbox hook or is explicitly client-built/control-only.

### WARN - The plan mischaracterizes the existing `sync` command as disposable STUB surface

Plan location: Component map "CLI surface" (line 81) and Phase 9 client update CLI (lines 580-581).

The CLI enum comment still labels `sync` as "STUB", but the implementation is not just an inert placeholder. `crates/jurisearch-cli/src/ingest.rs` implements `sync_payload` as local official-source delta ingest, using archive filters and the existing LEGI/JURI ingest paths. The analysis document also calls out this distinction: local official-source/archive delta sync exists today; server-to-client package distribution does not.

Treating `sync` as a stub to retire risks deleting or obscuring working local archive-delta functionality, and it reopens the terminology confusion the analysis had already settled.

Recommended fix: reword the plan to say that existing `jurisearch sync` is a local official-source archive-delta command and is not the new server-to-client package updater. Add the new `update` / `subscribe` / `corpus status` surface without removing local `sync` unless a separate deprecation/rename plan preserves that functionality and compatibility.

### WARN - Phase 3 says it does not depend on P1, but uses P1's QA digest helper

Plan location: Phase 1 QA backstop scaffolding (lines 239-240), Phase 3 loopback harness and acceptance (lines 331-337), and Phase 3 dependencies (lines 343-344).

The design permits a baseline apply before the outbox exists, and that sequencing claim is substantively correct for the baseline data path. The plan, however, schedules the per-table row-count/hash digest helper in Phase 1, then uses those QA digests as the Phase 3 loopback proof while declaring that Phase 3 depends only on P0 and P2.

Recommended fix: either move the digest/postcondition helper into P0 or P3 so the baseline vertical slice remains independent of the outbox, or add a narrow P1 dependency for the digest helper while explicitly stating that the outbox hooks themselves are not required for baseline apply.

### NIT - Phase 6 "Realises" cites the wrong design invariants

Plan location: Phase 6 Realises line (line 482) versus the invariant matrix (lines 653-657).

Phase 6 implements signing, manifest verification, version gating, entitlement, reject codes, and no cursor movement on rejection, which maps to design INV-9. It does not implement design INV-5 (`jurisearch_app` / `jurisearch_control` survive every generation) or INV-6 (index materialisation before cursor advance). The invariant matrix assigns those correctly elsewhere, so this looks like a numbering mix-up with the conception document's trust/failure invariants.

Recommended fix: change Phase 6 Realises to `§6.2, §6.3, §10, §11; INV-9` and, if desired, separately mention the conception trust/failure principles without reusing design invariant numbers.

## Verified Correct

- The workspace currently contains only `jurisearch-core`, `jurisearch-cli`, `jurisearch-embed`, `jurisearch-ingest`, `jurisearch-official-api`, and `jurisearch-storage`; the proposed `jurisearch-package`, `jurisearch-crypto`, `jurisearch-package-build`, and `jurisearch-syncd` crates are genuinely new.
- `CURRENT_SCHEMA_VERSION` is 17 in `crates/jurisearch-storage/src/migrations.rs`; the plan's statement that package/topology migrations advance past 17 is grounded.
- The named storage boundaries exist: `insert_legi_documents_with_statements`, `projection/decisions.rs`, `projection/embeddings.rs`, `projection/hierarchy_backfill.rs`, `projection/metadata.rs`, `decision_zones.rs`, `official_api_archive.rs`, and the `replace_zone_units_for_document` writer.
- `replace_zone_units_for_document` mirrors the intended `replace_set` shape: it deletes all `zone_units` for one `document_id` and reinserts the replacement set in a transaction, with `zone_unit_embeddings` removed by cascade.
- `official_api_responses` really uses a server-assigned `response_id`, and the v17 citation tables reference it; the plan's pre-citation apply-order requirement is faithful to the design.
- Dense finalize in `dense.rs` verifies coverage, drops/recreates the IVFFlat index, analyzes, and writes `index_manifest`; the plan's client-build/default indexing path matches the design.
- `serve.rs` is a loopback-oriented, unauthenticated, single-client sequential JSONL daemon shape; it refuses non-loopback TCP binds unless `--allow-remote` is set. The plan correctly treats producer hosting as net-new rather than extending `serve` for external distribution.
- The current CLI has `Sync` and the listed `Ingest` subcommands; `package ...`, `subscribe`, `update`, and `corpus status` are not present today.
- The plan is faithful on the major design contracts: three event kinds including in-place base-row updates; `chunks_with_embeddings` versus `chunk_embeddings`-only replacement; staged baseline/re-baseline generation plus view switch rather than operated `DROP SCHEMA`; index materialisation before cursor advance; soft validated app references; two-tier signed manifests; entitlement as an apply precondition; size-driven catch-up; and the `official_api_responses.response_id` exception.
- The main phase order is buildable after the fixes above: P4 correctly depends on P1 and P3, P5 depends on generation/baseline primitives, P6 hardens existing artifacts, and P7 planner work comes after incrementals, manifest verification, and baseline fallback.

## Overall Assessment

The plan is close and mostly faithful to the three source documents and the live codebase. The remaining issues are not stylistic: the incremental-builder boundary needs an explicit `change_seq` watermark/package-catalog story, Phase 1 needs to enumerate all replicated writers fixed by the design, and the existing local `sync` command must not be treated as an empty stub. Fixing those should make the plan a solid sequencing document.

VERDICT: FIXES_REQUIRED
