`canonical_json` is `NOT NULL` — the `row.get::<String>` is safe. My review is complete.

# Claude Review - LEGI Hierarchy Backfill

Verdict: GO

The slice is correct, conservatively scoped, and safely advances Phase 1.1. The backfill, its embedding-invalidation side effects, and the join semantics all line up with the surrounding ingest/embed/retrieval machinery, and tests cover the meaningful paths. Working tree is clean against `0b64ae7` (only the untracked review file and `.codegraph/` present).

## What I verified

- **Join semantics are sound.** `backfill_legi_article_hierarchy_from_metadata` (`projection.rs:247`) joins `documents`(`source='legi'`,`kind='article'`) → `graph_edges`(`edge_source='publisher'`, `payload->>'source_tag'='LIEN_SECTION_TA'`) → `legi_metadata_roots`(`root_kind='SECTION_TA'`) on `section.source_uid = edge.payload->>'to_source_uid'`. Confirmed the edge serde fields (`source_tag`, `to_source_uid`, `CanonicalGraphEdge` at `legi/mod.rs:220`) and `target_source_uid()` (`legi/mod.rs:1136`) return the `LEGISCTA…` id, matching `ParsedSectionTa.section_id` (`legi/mod.rs:103`). Section JSON exposes `hierarchy_path` + `title`, both consumed by `enriched_article_hierarchy_json`.
- **Determinism / archive-order robustness.** `DISTINCT ON (d.document_id) … ORDER BY d.document_id, section.valid_from DESC NULLS LAST, section.metadata_key` is deterministic and order-independent. The backfill runs once after full member traversal (`main.rs:711`) and scans the whole DB, so article-before-section, section-before-article, and cross-archive splits all converge. Indexes back the joins (`graph_edges_from_idx`, `legi_metadata_roots_kind_source_idx`).
- **contextualized_body format matches the original.** `contextualized_article_body` (`projection.rs:464`) reproduces `article_chunk_context` + `format!("{context}\n\n{body}")` from `build_article_chunks` (`legi/mod.rs:1043`) exactly (`hierarchy > … > title\n\nbody`), so re-embedded text stays consistent with first-pass ingest.
- **Embedding invalidation is coherent end-to-end.** `contextualized_body` lives only inside `canonical_json` (not the `chunks` table), and the embed pipeline reads it from there (`dense.rs:70`). The backfill updates `canonical_json`, `DELETE`s `chunk_embeddings`, and `NULL`s `chunks.embedding_fingerprint` — which makes `embed-chunks` re-embed (its `insert_chunk_embeddings` guard allows `NULL` fingerprints) and makes `finalize_dense_rebuild` (`dense.rs:111-130`) refuse to advertise the index until re-embedded. Coverage gating (`ingest_accounting.rs:617`) also correctly drops. No stale-embedding window is exposed.
- **Idempotency.** The `hierarchy.len() <= current_hierarchy.len()` guard (`projection.rs:420`) means a second run computes the same enriched length, returns `None`, and writes/invalidates nothing.
- **Tests.** CLI end-to-end asserts `hierarchy_backfilled_documents=1`, `hierarchy_path[3]="Titre preliminaire"`, and contextualized body containing `Titre preliminaire…Article 1240`. Storage test exercises the `embeddings_invalidated=1` path with `chunk_embeddings` deleted and fingerprint cleared. Both align with the verification commands listed.

## Non-blocking suggestions

1. **Temporal join imprecision** (`projection.rs:254-266`). Each article version is matched to the *latest* section version (`valid_from DESC`) regardless of the article's own validity window, so a historical article version can receive a hierarchy from a later reorganization. The strictly-longer guard limits the blast radius and this is fine for 1.1, but worth revisiting when full temporal hierarchy assembly lands. Consider a comment documenting the "latest section wins" choice.
2. **Unbounded full-corpus rescan per ingest** (`projection.rs:252-279`). The query scans every legi article-with-`LIEN_SECTION_TA` and parses each `canonical_json` in Rust on *every* run, even when nothing changes (writes are guarded, reads are not). At corpus scale this is O(all articles) per ingest. Consider scoping to documents touched this run, or pushing a path-length pre-filter into SQL. Relatedly, `updates` holds all enriched JSON in memory before the commit — empty at steady state, but potentially large on a first full backfill; batching would bound it.
3. **Single-section assumption** (`DISTINCT ON`). An article with multiple `LIEN_SECTION_TA` edges silently takes one section. Reasonable, but a one-line comment noting this would help future readers.
4. **Idempotency test gap.** Consider adding a second `backfill_…` call asserting `documents_updated == 0` / `embeddings_invalidated == 0` to lock in the guard's behavior against regressions.

## Verification commands

Inspected (as already run locally) and recommend keeping in CI gating:
- `cargo test -p jurisearch-storage --test legi_metadata_projection`
- `cargo test -p jurisearch-cli ingest_legi_archives_records_accounting_and_quarantines_failures --test cli_contract`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`
- `git diff --check`

One suggested addition for a follow-up: an integration assertion that an `ingest → embed-chunks → re-ingest-with-richer-section → embed-chunks` cycle leaves `finalize_dense_rebuild` reporting zero missing embeddings, to guard the invalidation→re-embed contract at the command level.
