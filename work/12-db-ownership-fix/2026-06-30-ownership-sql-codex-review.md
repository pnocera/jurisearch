# Review: reassign-corpus-ownership.sql

## Findings

- WARN: [work/12-db-ownership-fix/reassign-corpus-ownership.sql](/home/pierre/Work/jurisearch/work/12-db-ownership-fix/reassign-corpus-ownership.sql:34) takes `ALTER TABLE ... OWNER` locks with no `lock_timeout`, so an unexpected reader/writer or idle transaction can make the remediation wait indefinitely while an `ACCESS EXCLUSIVE` request is queued. PostgreSQL 18 documents `ALTER TABLE` as taking `ACCESS EXCLUSIVE` unless a subform says otherwise, and `OWNER` is not one of the exceptions. The operation is metadata-only/no rewrite, so the one-transaction approach is otherwise fine when the producer is idle, but production safety should fail-clean if any unexpected lock exists. Fix: immediately after `BEGIN;`, add a short local timeout such as `SET LOCAL lock_timeout = '5s';` (optionally also `SET LOCAL statement_timeout = '60s';`). With `ON_ERROR_STOP` and the single transaction, a timeout aborts atomically with no half-reassigned ownership.

- WARN: [work/12-db-ownership-fix/reassign-corpus-ownership.sql](/home/pierre/Work/jurisearch/work/12-db-ownership-fix/reassign-corpus-ownership.sql:38) uses a bare `GRANT jurisearch_owner TO jurisearch_write;`, which does not repair an existing PG18 membership whose `INHERIT` option was drifted to `FALSE`. The producer source confirms the intended model is owner-owned objects plus writer inheritance: `backend.rs:476-489` gives `jurisearch_write` an inheriting role and grants owner membership; `dense.rs:253-264` and `zone_units.rs:687-698` run `DROP INDEX`/`CREATE INDEX`/`ANALYZE` as the writer without `SET ROLE`; `embed.rs:211-224` and `embed.rs:385-398` call those finalize paths. PostgreSQL 18 documents that omitted role-membership options retain current values for an existing membership, while `pg_has_role(..., 'USAGE')` specifically tests immediate inherited role privileges. If the membership already has `INHERIT FALSE`, the script can reassign ownership, print `ownership_check_will_pass = false`, commit, and the rerun will still fail the owner check. Fix: make the membership convergence explicit before the ownership loop, for example:

```sql
GRANT jurisearch_owner TO jurisearch_write WITH INHERIT TRUE;
GRANT jurisearch_owner TO jurisearch_write WITH SET TRUE;
REVOKE ADMIN OPTION FOR jurisearch_owner FROM jurisearch_write;
```

or at minimum replace the bare grant with `GRANT jurisearch_owner TO jurisearch_write WITH INHERIT TRUE;` and keep the existing admin-option revoke if added. Also consider extending the final catalog check to include `zone_unit_embeddings`, since the source has an equivalent zone-unit ivfflat finalize path.

VERDICT: FIXES_REQUIRED
