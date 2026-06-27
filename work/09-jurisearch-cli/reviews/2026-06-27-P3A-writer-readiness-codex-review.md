# P3A Writer Readiness Review

## Findings

### BLOCKER: readiness can stamp dense coverage for the wrong active fingerprint

`compute_generation_coverage` counts a chunk as embedded when `chunk_embeddings.embedding_fingerprint = chunks.embedding_fingerprint`, but it never checks that either value equals the active generation fingerprint stamped into `jurisearch_control.corpus_state` (`ActivationStamps.embedding_fingerprint`). The new preflight then compares the query only to `corpus_state.embedding_fingerprint`, while the dense SQL filters `chunk_embeddings` by the query fingerprint.

Relevant source:

- `crates/jurisearch-storage/src/ingest_accounting/readiness.rs:286-297` counts dense coverage against the chunk row fingerprint only.
- `crates/jurisearch-storage/src/ingest_accounting/readiness.rs:331-337` stamps readiness after reading only the aggregate active signature and generation schema.
- `crates/jurisearch-storage/src/retrieval/hybrid.rs:29-42` accepts a dense/hybrid query whose fingerprint matches `corpus_state.embedding_fingerprint`.
- `crates/jurisearch-storage/src/retrieval/sql.rs:120-124` and `crates/jurisearch-storage/src/retrieval/sql.rs:218-222` then search only `chunk_embeddings` rows with that query fingerprint.

That leaves this false-ready state possible: a generation's chunks and embeddings are internally consistent with `fp-old`, activation stamps `corpus_state.embedding_fingerprint = fp-new`, `stamp_query_readiness` reports dense coverage complete, the query preflight accepts `fp-new`, and dense retrieval finds zero vectors. Hybrid silently falls back to lexical and explicit dense returns false no-results, which is exactly the failure P3A is meant to close.

The modified tests already demonstrate the hole. In `crates/jurisearch-storage/tests/generations.rs:29-37` and `crates/jurisearch-storage/tests/generations.rs:41-47`, the seed data uses fingerprint `fp` while activation stamps `bge-m3:1024:cls:normalize=true`; similar mismatches appear at `crates/jurisearch-storage/tests/generations.rs:609-615`, `crates/jurisearch-storage/tests/generations.rs:690-696`, and `crates/jurisearch-storage/tests/shared_server_roles.rs:21-47`. These seeds are not actually dense-query-ready for the active fingerprint, but the new gate accepts them.

Actionable fix: make the stamp helper validate dense coverage against the active corpus fingerprint, not just chunk self-consistency. The cleanest shape is to pass `corpus` or expected `embedding_fingerprint` into `stamp_query_readiness`, or resolve the single active row for `generation` in the transaction, then count embedded chunks only when `c.embedding_fingerprint = active_fingerprint` and `ce.embedding_fingerprint = active_fingerprint`. Update the test seeds to use the same fingerprint as `ActivationStamps`, and add a negative test where chunk/embedding rows are internally consistent but differ from the active stamp; activation/incremental restamp should fail.

### BLOCKER: the implementation writes a single-corpus coverage report under a multi-corpus signature

`stamp_query_readiness` explicitly records the aggregate `active_read_signature` while computing coverage for only the `generation` argument (`crates/jurisearch-storage/src/ingest_accounting/readiness.rs:327-337`). That is unsafe once more than one active corpus exists: `rebuild_server_views` unions all active physical schemas (`crates/jurisearch-storage/src/generations.rs:844-866`), and `execute_read_sql` routes multi-corpus reads through `jurisearch_server` (`crates/jurisearch-storage/src/runtime.rs:293-307`). A stamp produced by activating or incrementally updating one corpus can therefore authorize fetch/BM25 reads over the aggregate view without proving aggregate projection/dense readiness.

The new `rebaseline_loopback` change makes this concrete by installing `inpi` directly into `corpus_state` and rebuilding the union views while bypassing the readiness gate (`crates/jurisearch-package-build/tests/rebaseline_loopback.rs:133-152`). A later `core` stamp under the aggregate signature can still contain only `core` coverage. Dense/hybrid eventually fails closed on multi-corpus at the fingerprint preflight, but fetch and BM25 gates can pass using the single-corpus report.

Actionable fix: because P3A is explicitly single-corpus, fail closed before stamping or before lookup when `count(*) from jurisearch_control.corpus_state` is greater than one. If the rebaseline test needs a second-corpus preservation fixture, keep that as a non-queryable setup and avoid creating a valid-looking aggregate readiness stamp, or move the aggregate-readiness implementation into 3C and test it there.

### WARN: the negative incremental coverage gate is not tested end to end

The source placement is directionally right: activation stamps inside the switch transaction (`crates/jurisearch-storage/src/generations.rs:1208-1215`), and incremental apply restamps after `advance_corpus_cursor` but before `commit` (`crates/jurisearch-syncd/src/apply.rs:1053-1066`). However, the new negative test in `crates/jurisearch-storage/tests/query_readiness_p3a.rs:109-137` calls `stamp_query_readiness` directly after deleting embeddings. That proves the helper rejects incomplete coverage, but it does not prove an incremental diff that creates incomplete coverage rolls back the row mutations and leaves the cursor unchanged.

The review instructions call out this exact invariant for both activation and incremental. The successful shared-writer loopback only covers the happy path after an incremental (`crates/jurisearch-package-build/tests/shared_writer_loopback.rs:206-236`).

Actionable fix: add an apply-level incremental test whose package postconditions are otherwise valid but whose resulting active generation is missing a chunk embedding or has a stale active fingerprint. Assert `apply_incremental` errors, `corpus_state.sequence` is unchanged, and the generation rows are rolled back.

## Notes

I reviewed the working-tree diff and the relevant source paths. I did not rerun the claimed validation suite.

VERDICT: FIXES_REQUIRED
