## Findings

### BLOCKER: the strengthened producer write probe is not a valid insert

The new postcondition now probes `public.official_api_responses`, but the inserted row omits the required `corpus` column (`crates/jurisearch-storage/src/provision.rs:279`). Migration 18 makes that column `NOT NULL` (`crates/jurisearch-storage/src/migrations.rs:795`), and the runtime writer either supplies `corpus` or derives it from `subject_document_id` (`crates/jurisearch-storage/src/official_api_archive.rs:73`). This probe has neither an explicit `corpus` nor a subject document, so after privileges are fixed the writer path still fails with a constraint violation, not a privilege result. Because `probe_role_can_write` only converts SQLSTATE `42501` into `Ok(false)`, `provision_external_db` will fail with `StorageError::PostgresClient` instead of returning a successful report (`crates/jurisearch-storage/src/provision.rs:235`).

The new integration gate repeats the same invalid insert: `producer_writer_can_write_replicated_public_working_tables_and_read_cannot` inserts into `public.official_api_responses` without `corpus` (`crates/jurisearch-storage/tests/provision_external.rs:377`). With PG extensions available, this test should fail for the schema constraint rather than proving replicated-table DML and sequence access.

Fix: include a valid `corpus` value in both inserts, e.g. add `corpus` to the column list and `'core'` to the values. That keeps the probe focused on the intended privilege surface: table DML plus `response_id` sequence `USAGE`.

### WARN: default privileges are still additive, so over-granted future-object privileges are not converged

The producer profile now correctly revoke-firsts existing public tables and sequences before granting the intended current-object surface (`crates/jurisearch-storage/src/backend.rs:597`, `crates/jurisearch-storage/src/backend.rs:600`). However, the default privilege branch is grant-only (`crates/jurisearch-storage/src/backend.rs:675`, `crates/jurisearch-storage/src/backend.rs:679`). If a previous run or operator drift left broader default privileges for `{owner}` in `public` to the writer, such as `ALL ON TABLES` or `ALL ON SEQUENCES`, re-provisioning will not remove the extra future-object privileges (`TRUNCATE`/`REFERENCES`/`TRIGGER` on tables, `UPDATE` on sequences). The same issue matters for the site profile if drifted public defaults exist: it never revokes those defaults, so future owner-created public objects can still inherit a broader site-writer surface than the profile intends.

Fix: converge default privileges as well as current privileges. Revoke default table and sequence privileges for `{owner}` in `public` from `{writer}` before profile-specific handling; then the producer profile can re-grant only `SELECT, INSERT, UPDATE, DELETE` on tables and `USAGE, SELECT` on sequences, while the site profile leaves no public default privileges for the writer.

## Notes

The explicit `RoleProfile::Producer` path grants the existing public working schema and public sequences broadly enough for the producer writer (`crates/jurisearch-storage/src/backend.rs:666`, `crates/jurisearch-storage/src/backend.rs:669`), and `provision_external_db` uses that producer profile rather than the site provisioner (`crates/jurisearch-storage/src/provision.rs:102`). The site profile remains narrow for current objects: replicated public tables are SELECT-only, site writable tables are enumerated via `SITE_PUBLIC_WRITE_TABLES`, and public sequence grants are limited through the namespace-scoped `pg_depend` block (`crates/jurisearch-storage/src/backend.rs:610`, `crates/jurisearch-storage/src/backend.rs:621`, `crates/jurisearch-storage/src/backend.rs:643`). Read-role write access is still revoke-first and SELECT-only for current objects (`crates/jurisearch-storage/src/backend.rs:543`, `crates/jurisearch-storage/src/backend.rs:555`, `crates/jurisearch-storage/src/backend.rs:569`).

I did not run the PG-gated tests; the review above is source-based, per the environment note.

VERDICT: FIXES_REQUIRED
