# Central ingest package-distribution design final-r2 review

## BLOCKER

None.

## WARN

None.

## NIT

None.

## Verification notes

WARN 1 is resolved. The design now states that `package_change_log.change_seq` is only a global build/audit ordering, while `from_sequence`/`to_sequence`, the remote manifest's `head_sequence` / `min_available_sequence` / `catchup_ranges`, and `jurisearch_control.corpus_state.sequence` all use a separate per-corpus package sequence (`work/08-jurisearch-server/2026-06-26-central-ingest-package-distribution-design.md:246`). That contract is consistent with the per-corpus remote manifest (`:384`), the embedded manifest ordering fields (`:431`), the per-corpus cursor row (`:492`), and the ordered apply check against `corpus_state.sequence` (`:516`). This removes the prior cross-corpus `sequence_gap` ambiguity because `change_seq` interleaving is no longer used as the client cursor.

WARN 2 is resolved. Section 5.3 now requires a document-scoped `chunks_with_embeddings` replacement whenever chunk membership, partitioning, or body/contextualized-body changes, deleting all `chunks` for the document so `chunk_embeddings` cascade before inserting the current rows (`work/08-jurisearch-server/2026-06-26-central-ingest-package-distribution-design.md:290`, `:325`). The narrower `chunk_embeddings`-only replacement is limited to unchanged chunk row sets (`:334`), and the summary invariant repeats that chunk-set changes must replace `chunks` as BM25-indexed replicated rows (`:764`). This matches the live schema and writers: `chunks` are stored rows with `chunk_embeddings` cascading from them (`crates/jurisearch-storage/src/migrations.rs:46`, `:60`), BM25 indexes chunk text (`:355`), and the LEGI projection upserts current chunk rows without deleting dropped ones (`crates/jurisearch-storage/src/projection/legi.rs:73`, `:164`).

WARN 3 is resolved. Section 5.2 now defines the producer's `official_api_responses.response_id` as the immutable replicated key, carried verbatim and inserted explicitly by the client, with `official_api_responses` applied before dependent citation tables (`work/08-jurisearch-server/2026-06-26-central-ingest-package-distribution-design.md:274`, `:281`, `:284`). The embedded manifest apply order repeats that table ordering (`:458`). This matches the source contract: `official_api_responses` uses `response_id bigserial PRIMARY KEY` (`crates/jurisearch-storage/src/migrations.rs:593`), the archive writer appends and returns the assigned `response_id` (`crates/jurisearch-storage/src/official_api_archive.rs:37`, `:58`, `:67`, `:92`), citation tables FK to that id (`crates/jurisearch-storage/src/migrations.rs:650`, `:676`), and citation extraction treats the highest `response_id` per decision as the latest archived response (`crates/jurisearch-storage/src/legislation_citations.rs:11`, `:26`, `:38`).

I found no new internal contradiction or stale cross-reference introduced by these edits. The document remains design-only: it continues to declare no implementation plan, phasing, or code at the top (`work/08-jurisearch-server/2026-06-26-central-ingest-package-distribution-design.md:4`, `:8`), lists implementation details as out of scope (`:43`), and confines the remaining "phase" wording to the designed baseline/re-baseline apply protocol rather than a build plan (`:545`).

VERDICT: GO
