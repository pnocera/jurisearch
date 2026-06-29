# Codex Review: M1-B Storage Client Source

## Findings

### BLOCKER: external `DbClientSource` clients do not guarantee the `public` search path

`ConnectionConfig::connect` constructs a plain libpq client and connects without setting `search_path` ([crates/jurisearch-storage/src/backend.rs:56](../../../../jurisearch-worktrees/m1b-external-pg/crates/jurisearch-storage/src/backend.rs#L56)). The new `DbClientSource` trait also only promises a fresh client, not a client pinned to the producer working schema ([crates/jurisearch-storage/src/backend.rs:170](../../../../jurisearch-worktrees/m1b-external-pg/crates/jurisearch-storage/src/backend.rs#L170)).

That means the new external-PG path relies on the caller role's default `"$user", public` resolving to `public`. This is not guaranteed on a shared server: a pre-existing role default (`ALTER ROLE ... SET search_path`) or a schema named after the writer role would make every unqualified helper read/write a different schema. The migrated psql helpers now run through this same unqualified client path, so the review instruction's "confirm both resolve to `public`" condition is not satisfied by source.

Actionable fix: make the producer client-source contract enforce `public`, not assume it. For example, set libpq `options = -csearch_path=public` in `ConnectionConfig::connect` (and keep ManagedPostgres equivalent), or run `SET search_path TO public` immediately after opening producer clients. Also add a regression test that creates a role/user schema or role-level non-public `search_path` and proves a translated helper still reads the public tables.

### WARN: `zone_retrieval_coverage_with_client` is only equivalent on the public producer topology

The existing shim opens a read snapshot and delegates through `zone_retrieval_coverage_in_snapshot` ([crates/jurisearch-storage/src/zone_units.rs:774](../../../../jurisearch-worktrees/m1b-external-pg/crates/jurisearch-storage/src/zone_units.rs#L774)), while the new direct-client variant runs the shared SQL with no snapshot and no search-path setup ([crates/jurisearch-storage/src/zone_units.rs:822](../../../../jurisearch-worktrees/m1b-external-pg/crates/jurisearch-storage/src/zone_units.rs#L822)). The shared SQL extraction itself is good, but the two variants only agree when the client is already on the public producer working set. They will not agree after an active corpus is installed, because the snapshot path routes replicated tables through the active generation.

Actionable fix: either rename/document the client variant as producer-public-only and make callers uphold that contract explicitly, or provide a snapshot/client-source equivalent that resolves active corpora and applies the same search path before running the coverage SQL.

### WARN: the parity test would still be weak when live PostgreSQL assets are present

`tests/client_source_parity.rs` compares the shim and client variants byte-for-byte, but it starts a freshly migrated database and never inserts fixture rows before the assertions ([crates/jurisearch-storage/tests/client_source_parity.rs:28](../../../../jurisearch-worktrees/m1b-external-pg/crates/jurisearch-storage/tests/client_source_parity.rs#L28)). This catches empty-result shape drift, basic connectivity, and the public-topology coverage case, but it would not catch row ordering, cursor, JSON value, NULL rendering inside returned data, or mixed-type formatting regressions.

Actionable fix: seed minimal non-empty rows for every translated helper before comparing, including cursor/since branches, JSON/JSONB fields, boolean/numeric counts, and at least one nullable value. Keep the current empty-case coverage as a separate case.

## Notes

I source-compared the psql-to-client SQL translations in `zone_units.rs` and `legislation_citations.rs`; the SQL text moved into `simple_query_text` appears unchanged. `simple_query_text` itself matches the relevant `psql -qAt` rendering semantics for these single-text-column JSON helpers: text output, `|` column joining, `\n` row joining, `NULL` as empty, and final trim.

I found no production caller that distinguishes `StorageError::Psql` from `StorageError::PostgresClient`; the source matches were the psql constructor itself and tests.

The transactional write helper shims I checked are thin delegations preserving the original transaction boundaries and parameter binding. The audited helper set matches the CLI ingest/enrich/embed payload call paths I spot-checked; helpers listed as already client-generic are indeed already generic or client-based.

Local validation run during review:

```text
cargo test -p jurisearch-storage --test client_source_parity -- --nocapture
```

Result: passed by skipping live PostgreSQL setup because `pg_config`/extension assets were unavailable, matching the expected gate.

VERDICT: FIXES_REQUIRED
