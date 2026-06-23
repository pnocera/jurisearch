# Codex Review r2: Phase 2.1-B/C Decision Ingestion

Reviewed HEAD: `6fa9c66`

Scope:

- `crates/jurisearch-cli/src/main.rs`
- `crates/jurisearch-storage/src/ingest_accounting.rs`
- `crates/jurisearch-cli/tests/cli_contract.rs`
- Prior r1 review: `work/03-implementation/01-reviews/2026-06-23-phase2-1bc-decision-ingestion-codex-review.md`

## BLOCKER

None.

## WARN

None.

## NIT

None.

## Verified Fix

The r1 WARN is fixed.

In `ingest_juri_archives_payload`, the manifest body is now built from `manifest_run_status` at `crates/jurisearch-cli/src/main.rs:3868-3878`, then `update_ingest_run_manifest_with_client` runs at `crates/jurisearch-cli/src/main.rs:3879-3885`. If that update fails, the error path mutates `fatal_error` with `fatal_error.get_or_insert_with(|| storage_error_object(error))`.

The terminal `run_status` is recomputed after that mutation point at `crates/jurisearch-cli/src/main.rs:3887-3891`:

```rust
let run_status = if counters.failed_members == 0 && fatal_error.is_none() {
    IngestRunStatus::Completed
} else {
    IngestRunStatus::Failed
};
```

That recomputed value is the value passed to `finish_ingest_run_with_client` at `crates/jurisearch-cli/src/main.rs:3892-3894`. Therefore a manifest-update failure on an otherwise member-successful run now persists `failed`, not stale `completed`.

This matches the LEGI reference control flow at `crates/jurisearch-cli/src/main.rs:3559-3585`: both paths compute `manifest_run_status` for the manifest payload, attempt `update_ingest_run_manifest_with_client`, record any manifest-update failure in `fatal_error`, recompute terminal `run_status`, derive `error_message`, and pass the recomputed status to `finish_ingest_run_with_client`.

`finish_ingest_run_with_client` itself persists exactly the status it receives via `UPDATE ingest_run SET status = $2, error_message = $3, completed_at = now(), updated_at = now() WHERE run_id = $1` at `crates/jurisearch-storage/src/ingest_accounting.rs:301-319`, so there is no later storage-layer remapping that could reintroduce the stale status.

## Verified Non-Findings

- No other JURI terminal-status persistence path was found. The only JURI call to `finish_ingest_run_with_client` is the recomputed-status call in `ingest_juri_archives_payload` at `crates/jurisearch-cli/src/main.rs:3893`.
- `start_ingest_run_with_client` persists only the initial `running` status at `crates/jurisearch-storage/src/ingest_accounting.rs:241-278`; it is not a terminal-status path.
- `update_ingest_run_manifest_with_client` only updates the manifest JSON at `crates/jurisearch-storage/src/ingest_accounting.rs:339-359`; it does not persist a terminal status from `manifest_run_status`.
- Replay snapshot refresh in the JURI path is gated on the recomputed `run_status == IngestRunStatus::Completed` at `crates/jurisearch-cli/src/main.rs:3898-3902`, so a manifest-update fatal error also skips the completed-run refresh path.

## Regression Test Assessment

Skipping the fault-injection regression test is reasonable for this r2 fix.

The existing CLI tests cover ordinary JURI failed-member accounting/quarantine and compatible replay behavior (`crates/jurisearch-cli/tests/cli_contract.rs:4097-4248`), but they do not expose a seam for making only `update_ingest_run_manifest_with_client` fail after member processing succeeds. The CLI path owns a concrete `postgres::Client`, builds valid manifest JSON internally, and calls the generic storage helper directly. The storage helper fails only on PostgreSQL execution failure or a missing `ingest_run` row, neither of which is cheaply injectable through the current CLI contract without adding production fault-injection surface or brittle external database manipulation.

Given that the JURI terminal-status control flow now mirrors the already accepted LEGI path, and that there is no existing deterministic manifest-update failure hook, I would not block this fix on adding that regression test.

## Validation

I did not rerun the test suite for this review because the task requested a review artifact only and no file modifications other than this review file. The review used the live checkout at `6fa9c66`, CodeGraph structural context, focused source reads, and literal usage searches for the finalizer/update paths.

VERDICT: GO
