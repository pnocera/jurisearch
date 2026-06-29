# Review - Task 0 Contracts/API Survey r2

## Findings

### WARN - M1-C still has unassigned storage-file dependencies for the external producer path

The r2 survey resolves the earlier `S1` overlap by making M1-B land `DbClientSource` first, then states that M1-B can continue with migrations/provisioning while M1-C starts because "they no longer share files" (`work/10-next-plans/task0-contracts-survey.md:156`, `work/10-next-plans/task0-contracts-survey.md:184`, `work/10-next-plans/task0-contracts-survey.md:187`). The ownership table reinforces that M1-C does not edit storage files after consuming the trait (`work/10-next-plans/task0-contracts-survey.md:168`), and the hidden-dependency check only names `backend.rs`/`runtime.rs`/`migrations.rs` as the shared storage hazard (`work/10-next-plans/task0-contracts-survey.md:224`).

That is still not enough for S4-S6 as written. The proposed reusable APIs take `db: &impl DbClientSource` for ingest, enrichment, and document embedding (`work/10-next-plans/task0-contracts-survey.md:84`, `work/10-next-plans/task0-contracts-survey.md:85`, `work/10-next-plans/task0-contracts-survey.md:86`), but the current implementation path they must extract is not limited to `backend.rs`/`runtime.rs`. It calls several storage helpers that are still `ManagedPostgres`-typed or `execute_sql`-based:

- LEGI ingest calls `backfill_legi_article_hierarchy_from_metadata_scoped(&postgres, ...)` (`crates/jurisearch-cli/src/ingest/legi.rs:245`), whose storage API takes `&ManagedPostgres` and opens its own connection (`crates/jurisearch-storage/src/projection/hierarchy_backfill.rs:41`, `crates/jurisearch-storage/src/projection/hierarchy_backfill.rs:46`).
- Ingest/embed completion refreshes replay snapshots through `maybe_refresh_replay_snapshot(&postgres)` (`crates/jurisearch-cli/src/ingest.rs:451`), which calls a storage API typed as `refresh_replay_snapshot(postgres: &ManagedPostgres)` (`crates/jurisearch-storage/src/ingest_accounting/replay_snapshot.rs:64`).
- Enrichment pages and coverage use `ManagedPostgres` helpers such as `enrich_zone_candidates_json` (`crates/jurisearch-storage/src/zone_units.rs:47`) and `zone_retrieval_coverage_json` (`crates/jurisearch-storage/src/zone_units.rs:689`).
- Legislation citation enrichment has the same issue in `finalize_citation_occurrence_counts`, `load_pending_citation_resolutions_json`, and `legislation_citations_coverage_json` (`crates/jurisearch-storage/src/legislation_citations.rs:205`, `crates/jurisearch-storage/src/legislation_citations.rs:257`, `crates/jurisearch-storage/src/legislation_citations.rs:355`).
- Document/zone embedding relies on storage APIs typed to `&ManagedPostgres`, including `load_chunk_embedding_inputs`, `finalize_dense_rebuild`, and `replace_zone_units_for_document` (`crates/jurisearch-storage/src/dense.rs:57`, `crates/jurisearch-storage/src/dense.rs:116`, `crates/jurisearch-storage/src/zone_units.rs:128`).

So M1-C cannot deliver the survey's `DbClientSource`-based S4-S6 surface against an external producer database without either editing additional storage files or duplicating storage logic outside storage. That reintroduces exactly the hidden shared-file dependency the planning gate is supposed to remove.

Recommended fix: expand the S1/M1-B handoff to include the storage helper generalization M1-C needs before it starts, or explicitly serialize those storage edits. At minimum, assign ownership for `crates/jurisearch-storage/src/projection/hierarchy_backfill.rs`, `crates/jurisearch-storage/src/ingest_accounting/replay_snapshot.rs`, `crates/jurisearch-storage/src/zone_units.rs`, `crates/jurisearch-storage/src/legislation_citations.rs`, and `crates/jurisearch-storage/src/dense.rs`. The clean shape is for M1-B to add client-source or `*_with_client` variants alongside the current `ManagedPostgres` wrappers, then M1-C consumes only those APIs from `jurisearch-pipeline`.

## Prior Findings Re-Verification

The prior root `Cargo.toml` workspace-member collision is genuinely addressed. The root manifest explicitly lists members (`Cargo.toml:1`), the new crates are absent today, and r2 adds a single C0 scaffolding step that writes the root member list and skeleton manifests before fan-out (`work/10-next-plans/task0-contracts-survey.md:101`, `work/10-next-plans/task0-contracts-survey.md:106`, `work/10-next-plans/task0-contracts-survey.md:112`).

The S7 public seam is now a client-factory trait only. This matches the source: `build_incremental` opens a main client and a separate fence connection (`crates/jurisearch-package-build/src/incremental.rs:105`, `crates/jurisearch-package-build/src/incremental.rs:110`), and `build_remote_manifest` opens its own client (`crates/jurisearch-package-build/src/remote_manifest.rs:62`).

The producer config parser ownership is now explicitly reconciled. The macro M1 list includes a minimum producer config parser (`work/10-next-plans/00-macro-implementation-plan.md:129`), while the orchestrator assigns it to the Task 2 producer update agent (`work/10-next-plans/04-claude-orchestrator-instructions.md:281`). The r2 survey records the deferral to M2-B and leaves shared redaction/file-permission primitives with M1-A (`work/10-next-plans/task0-contracts-survey.md:139`, `work/10-next-plans/task0-contracts-survey.md:146`), which matches the requested split.

The `ErrorObject` owner correction is also resolved. The type is owned by `jurisearch-core` (`crates/jurisearch-core/src/error.rs:17`), and `jurisearch-cli` depends on core (`crates/jurisearch-cli/Cargo.toml:15`). The r2 hidden-cycle check now says pipeline must not depend on `jurisearch-cli` while allowing direct use of `jurisearch-core::error::ErrorObject` (`work/10-next-plans/task0-contracts-survey.md:222`).

VERDICT: FIXES_REQUIRED
