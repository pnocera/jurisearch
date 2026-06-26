# Central ingest package-distribution design final review

## BLOCKER

None.

## WARN

### 1. Per-corpus package sequence semantics are still ambiguous relative to the outbox sequence

The design correctly makes package streams and `jurisearch_control.corpus_state` per-corpus (`work/08-jurisearch-server/2026-06-26-central-ingest-package-distribution-design.md:146`, `:455`, `:469`) and requires ordered cursor checks with `from_sequence - 1` semantics (`:480`). However, the only sequence defined in the outbox contract is a global `change_seq bigserial PRIMARY KEY` plus a `corpus` column (`:213`). If package `from_sequence`/`to_sequence` are derived from that global outbox sequence, changes for other corpora will create gaps in an individual corpus's cursor and make a valid next package fail `sequence_gap`. If package sequences are a separate per-corpus chain, the document does not define where that sequence is assigned or how it maps back to `package_change_log.change_seq`.

Recommended fix: define the sequencing layers explicitly. The lowest-risk fix is to keep `change_seq` as a global audit/build cursor, add a per-corpus monotonic sequence (`corpus_change_seq` or a package-chain `package_sequence`) assigned without cross-corpus gaps, and state that remote manifests, embedded manifests, `corpus_state.sequence`, `from_sequence`, and `to_sequence` all use that per-corpus sequence. If the intended design is global sequencing instead, require no-op cursor advances or package entries for unaffected corpora and say that manifests/catch-up ranges are global, not per-corpus.

### 2. Chunk-set changes are not safely represented by replacing only `chunk_embeddings`

Section 5.3 says `chunk_embeddings` use document-scoped replacement whenever chunk membership, contextualized body, or fingerprint changes, and says this prevents stale rows (`work/08-jurisearch-server/2026-06-26-central-ingest-package-distribution-design.md:294`). That is incomplete for membership/body changes because `chunks` themselves are replicated, searchable rows, not just embedding parents. The current schema stores chunks as rows keyed by `chunk_id` with `document_id` and `chunk_index` (`crates/jurisearch-storage/src/migrations.rs:46`), BM25 indexes those chunk rows (`crates/jurisearch-storage/src/migrations.rs:355`), and `chunk_embeddings` only cascade when the parent chunk row is deleted (`crates/jurisearch-storage/src/migrations.rs:60`). The live LEGI projection upserts current chunk rows (`crates/jurisearch-storage/src/projection/legi.rs:73`, `:164`) but does not delete chunks that disappeared from the document's current chunk set. A package that only replaces `chunk_embeddings` can therefore leave stale chunk text visible to BM25/fetch when a source correction or non-rebaseline chunking change shrinks/repartitions a document.

Recommended fix: extend `replace_set` to cover the `chunks` table when chunk membership changes. Define a document-scoped `table_group` such as `chunks` or `chunks_with_embeddings`: delete all `chunks` for the document (letting `chunk_embeddings` cascade), insert the provided current chunk rows, then insert/verify embeddings if dense readiness is required. Keep a narrower `chunk_embeddings`-only replacement for fingerprint/payload corrections where the chunk row set is unchanged. Alternatively, state that any chunk membership change is rebaseline-only and cannot appear in an ordinary incremental package.

### 3. The replicated `official_api_responses` surrogate identity is not specified

The replicated table set includes `official_api_responses` plus citation tables that refer to it (`work/08-jurisearch-server/2026-06-26-central-ingest-package-distribution-design.md:167`, `:170`), while the event description emphasizes deterministic PKs and `source_payload_hash` idempotence for upserts (`:251`). That does not cover the current enrichment/provenance schema. `official_api_responses` uses `response_id bigserial PRIMARY KEY` (`crates/jurisearch-storage/src/migrations.rs:593`); the writer appends rows and returns the server-assigned `response_id` (`crates/jurisearch-storage/src/official_api_archive.rs:37`, `:60`, `:67`, `:92`). The v17 citation tables store those IDs as FKs (`crates/jurisearch-storage/src/migrations.rs:650`, `:676`), and the citation extraction code treats the highest `response_id` per decision as the latest archived response (`crates/jurisearch-storage/src/legislation_citations.rs:11`, `:27`, `:38`). A client package apply needs an explicit identity contract for those rows, or it cannot preserve FK/latest-response semantics idempotently.

Recommended fix: add a table-specific identity rule for `official_api_responses`: either preserve the producer's `response_id` as an immutable replicated key by inserting it explicitly, applying `official_api_responses` before dependent citation tables, and ensuring the local sequence is never used for client writes; or introduce a deterministic response key based on provider/endpoint/request fingerprint/body digest and define how package payloads remap the existing FK fields. Also include this exception in the manifest/apply-order contract so implementers do not assume every replicated row is keyed like `documents`.

## NIT

None.

## Verification notes

I did not find design-plan drift: the document stays at contracts, formats, namespaces, and protocols. The analysis decisions and the six consultation directions are incorporated aside from the gaps above. The major C1-C9 code-grounding claims otherwise check out: unqualified migrations and schema version 17 (`crates/jurisearch-storage/src/migrations.rs:23`, `:3`), `SchemaVersionAhead` (`:726`), LEGI upsert behavior (`crates/jurisearch-storage/src/projection/legi.rs:45`, `:80`, `:97`), deterministic LEGI/decision document IDs (`crates/jurisearch-ingest/src/legi/canonical.rs:52`, `crates/jurisearch-ingest/src/juri/types.rs:180`), derived rebuild/delete paths (`crates/jurisearch-storage/src/zone_units.rs:120`, `crates/jurisearch-storage/src/decision_zones.rs:195`, `crates/jurisearch-storage/src/projection/hierarchy_backfill.rs:216`), finalize-time IVFFlat/index manifests (`crates/jurisearch-storage/src/dense.rs:93`, `crates/jurisearch-storage/src/zone_units.rs:431`), BM25 migrations (`crates/jurisearch-storage/src/migrations.rs:103`, `:355`, `:559`), and the current single-client sequential JSONL `serve` shape (`crates/jurisearch-cli/src/serve.rs:1`, `:72`, `:125`).

VERDICT: FIXES_REQUIRED
