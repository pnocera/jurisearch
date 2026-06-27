# P4 Incremental Vertical Slice Re-review r3

## Findings

None.

## Checked r2 Fixes

- WARN 1 is resolved. `build_baseline` now acquires the dedicated exclusive outbox fence, starts the repeatable-read read-only transaction, makes `current_change_seq_with_client(&mut tx)` the first query, and releases the fence immediately after freezing `hi` (`crates/jurisearch-package-build/src/baseline.rs:117-132`). The expensive `corpus_table_digests_with_client` pass, COPY loop, BM25 index inventory, and schema-bundle digest all run after release on the same repeatable-read transaction (`crates/jurisearch-package-build/src/baseline.rs:132-190`). That keeps the fence critical section minimal while preserving snapshot/`hi` coherence: the exclusive fence waits out existing shared emitters, no new emitter can allocate while `hi` is read, and the rest of the baseline materializes from the fixed snapshot.

- WARN 2 is resolved. `emit_change` now performs `pg_advisory_xact_lock_shared` in a `fence` CTE that feeds the `INSERT ... SELECT ... FROM fence ... RETURNING change_seq` statement (`crates/jurisearch-storage/src/outbox.rs:165-197`). A raw autocommit caller therefore holds the transaction-scoped shared advisory lock through the statement that allocates `change_seq`; transaction-wrapped callers still hold it until their surrounding transaction commits or rolls back.

## Checked P4 Invariants

- The D7 fence still prevents the allocation-order skip P4 is guarding against. A builder freezes `hi` only while holding the exclusive fence. Emitters allocate `change_seq` only after taking the conflicting shared fence, so a later-committing lower `change_seq` cannot fall under the frozen `hi` while remaining absent from the build snapshot.

- The incremental producer still maps global outbox sequence to per-corpus package sequence through the catalog. `build_incremental_inner` reads the latest catalog row for `lo`, freezes `hi` under the fence, reads scopes in `(lo, hi]`, and writes the next package catalog row with both window bounds and the chain link (`crates/jurisearch-package-build/src/incremental.rs:139-188`, `:407-528`).

- The r1 compatibility-stamp fix remains intact. The producer refuses an ordinary incremental when the requested embedding fingerprint, builder versions, or storage schema version differ from the latest catalog stamps before it opens the build snapshot (`crates/jurisearch-package-build/src/incremental.rs:147-173`). The applier locks `corpus_state`, compares signed preconditions for schema, embedding fingerprint, builder versions, baseline id, and active generation before applying any payload rows, and only advances the cursor after in-transaction postconditions pass (`crates/jurisearch-syncd/src/apply.rs:766-932`).

- Apply-side idempotency and chain-link checks remain intact. A committed re-apply is skipped only when both `package_id` and package digest match the cursor at the result sequence; gaps, ahead-of-cursor packages, and previous-package id/digest mismatches are rejected before mutation (`crates/jurisearch-syncd/src/apply.rs:789-829`).

- The incremental payload path still validates payload bytes and convergence before cursor movement. Per-file and aggregate digests are recomputed from bytes read from disk (`crates/jurisearch-syncd/src/apply.rs:456-518`), replace-set scopes are delete-then-inserted and checked by `set_digest`, full-corpus table digests are validated inside the apply transaction, and `advance_corpus_cursor` runs only after those checks (`crates/jurisearch-syncd/src/apply.rs:887-932`, `:941-997`).

## Validation

I did not rerun the full managed-Postgres suites in this pass; the r3 brief reports the relevant fmt, clippy, storage outbox, and package-build tests green. I inspected the source and the changed regression tests covering the r2 fence fixes and P4 acceptance path.

VERDICT: GO
