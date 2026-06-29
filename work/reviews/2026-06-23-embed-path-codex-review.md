# Review: embedding-path set-based insert and streaming load

## Findings

No severity-tagged findings.

## Notes

- P1 #7 guard equivalence looks preserved. `insert_chunk_embeddings` stages one row per chunk in a temporary table with `chunk_id text PRIMARY KEY`, then updates `chunks` only when the chunk exists and its fingerprint is `NULL` or already equal to the staged fingerprint. The post-update count check at `crates/jurisearch-storage/src/projection.rs:695-731` fails the same missing-chunk and conflicting-fingerprint cases as the old per-row `updated != 1` guard. A row whose fingerprint is already equal still matches the `UPDATE` predicate, and PostgreSQL counts matched `UPDATE` rows, so idempotent re-runs do not spuriously fail.
- P1 #7 vector handling is equivalent to the old `$3::text::vector` path. The new code stores the pgvector literal in a `text` staging column and casts with `embedding::vector` during the final insert at `crates/jurisearch-storage/src/projection.rs:734-747`; the destination schema remains `vector(1024)`, so malformed vectors or wrong dimensions still fail inside the same transaction.
- P1 #7 atomicity is intact. Stage creation, population, guarded chunk update, vector upsert, and commit all happen within one transaction at `crates/jurisearch-storage/src/projection.rs:668-749`, with the temp table declared `ON COMMIT DROP`. Duplicate `chunk_id` values inside a batch now fail at the stage primary key instead of being processed twice; that is a hard error rather than silent data loss and is acceptable for this batch API.
- P1 #7 array binding shape is correct for `postgres`: the code passes same-length `Vec<&str>` columns for `text[]` and `Vec<i32>` for `int[]` into the typed multi-argument `unnest` at `crates/jurisearch-storage/src/projection.rs:646-689`.
- P1 #6 streaming termination and completeness look sound. The no-limit path repeatedly loads a bounded pending page at `crates/jurisearch-cli/src/main.rs:2619-2627`, embeds and inserts that page at `2631-2639`, then stops only when the pending query returns empty. Successful inserts remove chunks from the pending set because `load_chunk_embedding_inputs` filters against `chunk_embeddings` fingerprint/model/dimension at `crates/jurisearch-storage/src/dense.rs:47-69`. Failed pages return `Err` before finalization. Finalization still runs once at `crates/jurisearch-cli/src/main.rs:2650-2662` and independently rejects missing coverage via `finalize_dense_rebuild` at `crates/jurisearch-storage/src/dense.rs:122-140`, so I do not see an under-embed-yet-finalize path. The embedding client also rejects empty or size-mismatched batch responses at `crates/jurisearch-embed/src/lib.rs:451-458`, avoiding a silent partial-success loop.
- P1 #6 `--limit` behavior is preserved. The limit branch still loads `limit + 1`, errors when more than `limit` rows are pending, returns `no_results` when empty, and embeds a single bounded batch otherwise at `crates/jurisearch-cli/src/main.rs:2578-2605`.
- P1 #6 stats merging matches the stated behavior. `merge_embedding_endpoint_stats` only consumes each page run once, sums `requests`, `chunks`, `truncated_inputs`, and `failures` per `base_url`, and leaves the single-page case unchanged because the accumulator starts empty and the first page entries are pushed as-is.

Tests were not rerun during this review; I reviewed the specified commits and current source against the validation stated in the review brief.

VERDICT: GO
