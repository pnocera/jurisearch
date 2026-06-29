# Code Review: prepare LEGI projection statements once per ingest batch

## Findings

No severity-tagged findings.

I verified the scoped change in commit `4b48aa3` against the parent implementation for the LEGI projection path:

- `prepare_legi_projection_statements` extracts the same three SQL statements previously prepared inside `insert_legi_documents_with_client`: document upsert, chunk upsert, and graph edge upsert.
- `insert_legi_documents_with_statements` preserves the existing document validation, JSON serialization, document execute parameters, chunk execute parameters, publisher-edge insertion call, and `CanonicalInsertReport` accounting.
- `insert_legi_documents_with_client` remains a prepare-then-delegate wrapper, so existing single-call/test callers keep the prior behavior.
- `process_legi_archive_member_batch` prepares the statements after `BEGIN` and `SET LOCAL synchronous_commit TO off`, then reuses them for every member processed on the same transaction. That is sound for the intended postgres client/session lifetime, and the cloned `Statement`s are local handle clones rather than new prepare round-trips.
- The per-member flow is still `record_legi_member(... Parsed)` -> projection insert -> `update_ingest_member_status_with_client(... Inserted)` inside the same batch transaction, so a later failure still rolls back the member record, projection writes, and status transition together. Resume/error attribution behavior is unchanged by the statement threading.

## Residual Risk

I did not rerun the managed PostgreSQL suites listed in the review brief. This review is source-based, with a focused diff and call-path check against `4b48aa3^`.

VERDICT: GO
