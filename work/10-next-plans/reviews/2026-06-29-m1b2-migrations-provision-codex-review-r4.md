# Review

No findings.

Verified by source:

- The r3 write-probe fix is present and valid against the current schema. `run_write_probe` now supplies `corpus = 'core'` when inserting `public.official_api_responses` (`crates/jurisearch-storage/src/provision.rs:283`), and the table's current constraints are satisfied: `provider = 'local'`, `http_method = 'LOCAL'`, `outcome = 'ok'`, required request/response fields are present, and the explicit `corpus` column is `NOT NULL` with only the non-null check (`crates/jurisearch-storage/src/migrations.rs:614`, `crates/jurisearch-storage/src/migrations.rs:795`). The table intentionally has no FK; the only FK exercised by the probe is `ingest_member.run_id`, and the matching `ingest_run` row is inserted earlier in the same rolled-back transaction (`crates/jurisearch-storage/src/migrations.rs:155`, `crates/jurisearch-storage/src/provision.rs:266`).
- The producer grant model is still the full public working schema: `provision_external_db` calls `provision_producer_roles` (`crates/jurisearch-storage/src/provision.rs:102`), and the producer profile revoke-firsts the writer's public tables/sequences before granting `SELECT, INSERT, UPDATE, DELETE` on all public tables plus `USAGE, SELECT` on all public sequences (`crates/jurisearch-storage/src/backend.rs:597`, `crates/jurisearch-storage/src/backend.rs:680`).
- Default privileges are now converged rather than grant-only. Both profiles issue `ALTER DEFAULT PRIVILEGES FOR ROLE <owner> IN SCHEMA public REVOKE ALL` on tables and sequences before profile-specific handling (`crates/jurisearch-storage/src/backend.rs:611`). The producer profile then re-grants only DML table defaults and `USAGE, SELECT` sequence defaults (`crates/jurisearch-storage/src/backend.rs:689`); the site profile does not re-grant public defaults.
- The site profile remains narrow: it grants SELECT on replicated public templates and DML only on the enumerated `SITE_PUBLIC_WRITE_TABLES`, with sequence usage limited to sequences owned by those tables via `pg_depend` (`crates/jurisearch-storage/src/backend.rs:619`, `crates/jurisearch-storage/src/generations.rs:111`).
- The earlier migration ordering/atomicity fix still holds. `run_migrations_on` creates/reads `schema_migrations` and rejects `SchemaVersionAhead` before extension handling (`crates/jurisearch-storage/src/migrations.rs:1233`), and pending migration 1 creates required extensions inside the migration transaction before the migration SQL and version stamp (`crates/jurisearch-storage/src/migrations.rs:1278`).

I did not rerun the PG-gated integration tests in this environment; this review is source-based as requested by the r4 instructions.

VERDICT: GO
