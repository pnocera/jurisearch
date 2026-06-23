# Codex Review - Phase 2.2 Status Coverage

Review target: `HEAD` 7dbfdf84804802cb4a0382cc69c05f084cd19783 on `main`, diff vs parent 3e5d9c6235f2376d0dbc3279f5057c62a827d0bb.

## BLOCKER

None.

## WARN

None.

## NIT

None.

## Verification Notes

- Cost: `corpus_source_coverage_json` in `crates/jurisearch-storage/src/retrieval.rs` reads only `ingest_run`. The SQL uses `SELECT DISTINCT ON (source)` over completed runs and `jsonb_object_agg`; it does not reference `documents`, `chunks`, embeddings, or graph tables, so it preserves the cheap default `status` path.
- Latest-run selection: the `ORDER BY source, completed_at DESC NULLS LAST, run_id DESC` clause is deterministic for each source. Completed runs with the same `completed_at` are tie-broken by `run_id`.
- Wiring: `status_index_and_ingest_health` now returns `(index, ingest_health, corpus_sources)`. The not-configured, not-initialized, and open-failure paths return `Value::Null` for `corpus_sources`; the successful-open path reads coverage from the same `postgres` handle before loading ingest health. If the coverage query errors or the returned text cannot be parsed as JSON, it falls back to `Value::Null`, so `status` still renders.
- Caller coverage: CodeGraph reports `status_payload` as the only caller of `status_index_and_ingest_health`; it destructures the 3-tuple and adds `"corpus_sources"` to the JSON payload.
- Provenance: `zone_accurate`, `chunking_provenance`, `freshness`, `dataset`, `source_version`, and `coverage` are surfaced directly from the stored ingest-run `manifest`. For juri archives, `juri_archive_manifest` records `zone_accurate: false` and `chunking_provenance: "heuristic"`; `corpus_source_coverage_json` does not recompute or upgrade those values.
- SQL injection: `corpus_source_coverage_json` takes no user input and executes a static query.
- Tests: I did not rerun the supplied validation commands during this review, to avoid creating unrelated local build artifacts under the user's "do not modify any other files" constraint. The reviewed test addition in `cli_contract.rs` covers the JADE status surface after a completed no-op replay, including freshness, provenance, and `last_run_coverage.inserted_documents == 0`.

VERDICT: GO
