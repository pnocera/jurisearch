# P4 Incremental Vertical Slice Re-review r2

## Findings

### WARN 1. Baseline still holds the outbox fence while computing full corpus digests

The incremental path now has the intended fence shape: acquire the dedicated fence connection, open the repeatable-read snapshot, read `hi`, release the fence, then continue materialising from the fixed snapshot (`crates/jurisearch-package-build/src/incremental.rs:107-183`). That resolves the r1 hold-time problem for ordinary incrementals.

The baseline path still holds the same global outbox fence across `corpus_table_digests_with_client` before `hi` is frozen and before the release (`crates/jurisearch-package-build/src/baseline.rs:117-128`). That helper is not a cheap snapshot-establishing read; it iterates every replicated digest spec and runs the count/content-digest query for each table (`crates/jurisearch-storage/src/outbox.rs:374-401`). On a real corpus this can be a substantial full-corpus pass, so outbox emitters remain blocked well beyond the minimal critical section described in the r2 brief. Materialisation continues after release, but the expensive postcondition digest pass is still inside the fence window.

Correctness is conservative, but the r1 WARN is only partially fixed: large baseline builds can still turn the outbox fence into a global ingest commit stall during the digest phase.

Concrete fix: in `build_baseline`, after acquiring the fence and opening the repeatable-read transaction, make `current_change_seq_with_client(&mut tx)` the first snapshot-establishing read, release the fence immediately, and then compute `corpus_table_digests_with_client`, COPY payloads, BM25 inventory, and the schema bundle from that same transaction snapshot.

### WARN 2. The shared fence lock is not held through `change_seq` allocation for raw-client emitters

`emit_change` is generic over `postgres::GenericClient`, so it can be called with either a transaction or a plain `postgres::Client` (`crates/jurisearch-storage/src/outbox.rs:143-147`). The new shared fence lock is acquired with `pg_advisory_xact_lock_shared` in its own `batch_execute` statement (`crates/jurisearch-storage/src/outbox.rs:148-157`), and the outbox row that allocates `change_seq` is inserted in a later statement (`crates/jurisearch-storage/src/outbox.rs:163-191`).

That is safe when the caller passed an explicit transaction: the xact-scoped shared lock survives through the insert and commit. It is not safe for a raw-client caller, because autocommit releases the transaction-scoped lock at the end of the lock statement, before the `INSERT ... RETURNING change_seq`. The API still permits that shape, and public writer helpers such as `insert_official_api_response_with_client<C: postgres::GenericClient>` call `emit_change` through the same generic client (`crates/jurisearch-storage/src/official_api_archive.rs:45-49`, `:71-124`).

The failure mode is the same allocation-order hazard the fence is meant to close: raw emitter A can allocate a lower `change_seq` in an autocommit insert without holding the shared fence, emitter B can commit a higher `change_seq`, and a builder can acquire the exclusive fence and freeze `hi` at B while A is still invisible. When A commits later, its lower row is `<= hi` but absent from the build snapshot, so it can be skipped.

Concrete fix: make the fence impossible to misuse. Prefer changing `emit_change` to require a transaction-shaped API for outbox-enabled writers and update tests/helpers to wrap raw clients in `in_outbox_txn` or local transactions. Alternatively, combine the advisory-lock call and the outbox insert into one SQL statement so even autocommit callers hold the xact-scoped shared lock through `change_seq` allocation and statement commit; the mutation/outbox rollback-coupling still needs explicit transactions for multi-statement writers.

## Checked r1 Items

- The r1 compatibility-stamp blocker is resolved on the producer side. `LatestPackage` now returns `schema_version`, `embedding_fingerprint`, and `builder_versions`; `build_incremental_inner` rejects mismatched incremental params before opening the snapshot or reading scopes (`crates/jurisearch-storage/src/package_catalog.rs:136-178`, `crates/jurisearch-package-build/src/incremental.rs:139-173`).
- The consumer now locks `corpus_state`, reads the same stamps, and rejects mismatched `manifest.apply.preconditions` with `SchemaAhead`, `EmbeddingFingerprintMismatch`, or `BuilderVersionMismatch` before `apply_incremental_files` mutates rows (`crates/jurisearch-syncd/src/apply.rs:766-887`).
- The dedicated fence connection and early release are correct for `build_incremental`; the remaining hold-time issue is specific to the baseline digest ordering.

## Validation

- `cargo fmt --check` passed.
- `cargo test -p jurisearch-storage --test outbox an_emitter_blocked_by_the_fence_commits_above_the_frozen_high_water -- --nocapture` passed.
- `cargo test -p jurisearch-package-build --test incremental_loopback incremental_build_rejects_an_embedding_fingerprint_boundary -- --nocapture` passed.
- `cargo test -p jurisearch-package-build --test incremental_loopback incremental_apply_rejects_a_tampered_fingerprint_precondition -- --nocapture` passed.
- I did not rerun the full managed-PG suite or full clippy pass in this re-review; the r2 brief reports those green.

VERDICT: FIXES_REQUIRED
