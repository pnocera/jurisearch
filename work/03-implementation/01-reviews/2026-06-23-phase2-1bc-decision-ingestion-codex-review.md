# Codex Review: Phase 2.1-B/C Decision Projection + Juri Archive Ingest

Reviewed diff: `ba9b50c..6c09cd4`

Scope reviewed:

- `crates/jurisearch-storage/src/projection.rs`
- `crates/jurisearch-storage/tests/decision_projection.rs`
- `crates/jurisearch-cli/src/main.rs`
- `crates/jurisearch-cli/tests/cli_contract.rs`

## BLOCKER

None.

## WARN

### Juri ingest can finish a fatal manifest-update failure as `completed`

Location: `crates/jurisearch-cli/src/main.rs:3868-3885`

The juri path computes `run_status` before attempting `update_ingest_run_manifest_with_client`:

- `run_status` is set from the current `failed_members` / `fatal_error` state at lines 3868-3872.
- The manifest update can then fail and set `fatal_error` at lines 3875-3880.
- `finish_ingest_run_with_client` is still called with the old `run_status` at lines 3883-3885.

That means a run with no member failures can have a fatal manifest-update error, return an error to the caller, but persist `ingest_run.status = 'completed'`. This violates the review requirement that `run_status` be `failed` iff `failed_members > 0 || fatal`, and it diverges from the LEGI reference, which recomputes `run_status` after the manifest update failure path (`crates/jurisearch-cli/src/main.rs:3566-3578`).

Concrete fix:

- Mirror the LEGI structure: use an initial `manifest_run_status` for building the final manifest if needed, run `update_ingest_run_manifest_with_client`, then recompute the terminal `run_status` after any manifest-update error has had a chance to populate `fatal_error`.
- Add a focused contract/unit test that forces `update_ingest_run_manifest_with_client` or the equivalent finalization path to fail after member processing, then asserts the run is not persisted as `completed`.

## NIT

None.

## Verified Non-Findings

- `DocumentProjectionStatements` is a direct alias of `LegiProjectionStatements`, and `prepare_document_projection_statements` forwards to `prepare_legi_projection_statements`; the reused SQL statement targets the same `documents`, `chunks`, and `graph_edges` columns and conflict targets.
- `insert_decision_documents_with_statements` passes the document parameters in the same 15-slot order as the shared LEGI document statement: decision IDs/source/kind/source UID, `version_group = NULL`, citation/title/body, `valid_from = decision_date`, `valid_to = NULL`, `valid_to_raw = NULL`, source URL, payload hash, empty hierarchy path JSON, and serialized canonical decision JSON.
- Decision chunks use the same 13-slot chunk parameter order as LEGI chunks, including JSON serialization for `source_fields` and `hierarchy_path`.
- Decision validation runs before the decision row insert in the projection path, and the CLI calls the projection inside the per-batch transaction.
- Publisher edges correctly reuse the shared `insert_publisher_edge` path; decision edge payloads preserve the canonical edge fields while storing `edge_source = 'publisher'`.
- `record_juri_member` writes the dataset token (`cass`, `capp`, `inca`, or `jade`) to `ingest_member.source`, while projected decisions use `document_id = <source>:<source_uid>` from `CanonicalDecision::validate`.
- `JURI_PARSER_VERSION` and `JURI_CANONICAL_SCHEMA_VERSION` are distinct from the LEGI constants. Resume compatibility uses archive name, member path, parser version, schema version, code version, and payload hash; archive planning also rejects filenames whose source token does not match the selected dataset, so ordinary cross-source resume collision is avoided by the source-specific archive name.
- The same-run skip guard (`resume.previous_run_id.as_deref() != Some(run_id)`) is present on the juri path, preventing an inserted member from being demoted to skipped during a same-run replay.
- `BlockedIncompatible` records a failed member, records an ingest error, optionally quarantines, increments `failed_members`, and returns `Ok(())`, so the rest of the batch can continue.
- Parse failures are recorded and accounted without propagating a batch-fatal error; committed counters are merged only after transaction commit, and `pending_members` / byte counters are cleared only on the successful flush path.
- Honest provenance is hard-coded in both response and manifest: `zone_accurate = false` and `chunking_provenance = "heuristic"`.
- Replay snapshot refresh is gated on terminal `run_status == Completed`.

## Validation

I did not rerun the test suite because this was a review-only task and the instruction was not to modify any files other than the requested review artifact. The review used the live source and the diff from `ba9b50c` to `6c09cd4`.

VERDICT: FIXES_REQUIRED
