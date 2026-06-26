# P2 client storage topology review

## Summary

The storage-side shape is mostly in place: migration v20 creates the three long-lived namespaces and the control tables, and the new `generations` module can clone replicated tables, register generations, rebuild stable views, and resolve the active physical schema. The integration tests cover the happy path for one corpus and prove that a generated schema can serve BM25/vector reads.

P2 is not complete yet. The production read path still targets the old unqualified/public tables, and the activation/retire primitives do not yet enforce the atomicity and safety guarantees that the design assigns to the view switch and cleanup path.

## BLOCKER

### Production reads still bypass the generation topology

`crates/jurisearch-storage/src/retrieval/hybrid.rs:18` builds the current search CTEs without any active-generation context and `crates/jurisearch-storage/src/retrieval/hybrid.rs:132` runs them through `postgres.execute_sql(&sql)`. The generated SQL in `crates/jurisearch-storage/src/retrieval/sql.rs:105`, `crates/jurisearch-storage/src/retrieval/sql.rs:122`, `crates/jurisearch-storage/src/retrieval/sql.rs:133`, `crates/jurisearch-storage/src/retrieval/sql.rs:183`, `crates/jurisearch-storage/src/retrieval/sql.rs:220`, and `crates/jurisearch-storage/src/retrieval/sql.rs:231` still references unqualified `chunks`, `chunk_embeddings`, and `documents`. The same pattern remains in the non-indexed read modules (`fetch`, `context`, `citation`, `related`, `stats`).

That means a client database whose replicated rows live only in `jurisearch_server_<corpus>_gNNNN` will still search/fetch from `public` instead of from the activated generation or the `jurisearch_server` views. This fails the P2 deliverable that "the CLI read path is migrated to reference `jurisearch_server.<name>`" and the acceptance test that existing CLI reads return identical results through the seeded generation. Deferring all read-role wiring to P3 is not acceptable for P2 because P3's first baseline apply would otherwise produce an active generation that the existing CLI cannot read.

Recommended fix: introduce an explicit read-role/table-target abstraction now. Route non-indexed reads through `jurisearch_server.<table>` (or a per-invocation stable `search_path = jurisearch_server, public`), and route hot indexed reads through active physical schema targets resolved from `jurisearch_control.corpus_state` (`<schema>.chunks`, `<schema>.chunk_embeddings`, `<schema>.documents`, etc.; for multi-corpus search, build per-active-corpus indexed arms and `UNION ALL` above them). Add integration coverage that seeds an active generation while leaving `public` empty or stale, then runs the real CLI `search`/`fetch`/`context`/`cite`/`related` paths against the generation-backed data.

### `activate_generation` does not enforce an atomic view switch

`crates/jurisearch-storage/src/generations.rs:319` retires the old active row, `crates/jurisearch-storage/src/generations.rs:327` marks the new row active, `crates/jurisearch-storage/src/generations.rs:337` advances `corpus_state`, and only then does `crates/jurisearch-storage/src/generations.rs:367` rebuild the stable views. The function comment says the caller's transaction makes this atomic, but the public helper accepts any `GenericClient`; the new tests call it with a plain `postgres::Client`, so each statement auto-commits. If `rebuild_server_views` fails after `corpus_state` is advanced, the cursor and registry can point at the new generation while the stable views still point at the old one. That is exactly the half-state P2/§7.4 is supposed to prevent.

Recommended fix: make activation own the short-switch transaction (including the package-apply advisory lock and low `lock_timeout`) or narrow the API so activation can only be performed through a transaction-specific wrapper. In that transaction, validate the target registry row exists and is `building`, validate the expected previous cursor/generation, replace the views, update `corpus_state`, mark old/new registry states, and commit as one unit. Add a failure-injection test that proves a view rebuild error rolls back the cursor and registry changes.

## WARN

### `drop_retired_generation` can drop a live generation before checking state

`crates/jurisearch-storage/src/generations.rs:382` derives the schema from the caller-provided generation name and `crates/jurisearch-storage/src/generations.rs:383` drops it with `CASCADE`. The only `state = 'retired'` guard is in the later registry delete at `crates/jurisearch-storage/src/generations.rs:389`. Passing the currently active generation, a misspelled generation that maps to an existing schema, or racing with activation can therefore destroy the physical schema before the function discovers that the registry row was not retired.

Recommended fix: first select the registry row by `(corpus, generation)` with `state = 'retired'` (and preferably `FOR UPDATE`) and use its stored `physical_schema`; return an error if no retired row exists. Then drop that schema with a bounded lock timeout and delete or mark the registry row in the same controlled cleanup flow.

### Generation creation silently reuses existing schemas

`crates/jurisearch-storage/src/generations.rs:99` emits `CREATE SCHEMA IF NOT EXISTS`, `crates/jurisearch-storage/src/generations.rs:103` emits `CREATE TABLE IF NOT EXISTS`, and `crates/jurisearch-storage/src/generations.rs:114` upserts the registry row back to `building`. Retrying a failed build with the same `(corpus, counter)` can leave partially loaded rows and old indexes in place while the registry is reset as if this were a fresh generation.

Recommended fix: treat a generation name as single-use. Remove the `IF NOT EXISTS`/registry upsert behavior for the normal path and fail if the schema or registry row already exists. If retry support is needed, implement it as an explicit cleanup path that verifies the row is `building`/`failed`, drops or truncates the schema, and then recreates it before loading.

### Fresh v20 databases do not actually have stable views

Migration v20 creates `jurisearch_server`, `jurisearch_control`, and `jurisearch_app` at `crates/jurisearch-storage/src/migrations.rs:882`, then creates the control tables and indexes through `crates/jurisearch-storage/src/migrations.rs:920`. It does not create the `jurisearch_server.<relation>` views promised by the P2 deliverable. `rebuild_server_views` has the correct zero-active-corpus behavior (`SELECT * FROM public.<table> WHERE false` at `crates/jurisearch-storage/src/generations.rs:266`), but nothing invokes it during migration/startup, so a client read routed to `jurisearch_server.documents` before the first activation gets "relation does not exist" instead of an empty stable namespace.

Recommended fix: create empty compatibility views for every `REPLICATED_TABLES` relation as part of migration/startup, using the same `WHERE false` shape. Add a test for a freshly migrated DB with no active corpus that verifies every `jurisearch_server` relation exists and returns zero rows.

## NIT

### `execute_sql_with_search_path` trusts a raw search path string

`crates/jurisearch-storage/src/runtime.rs:245` accepts a raw `search_path` string and injects it directly into `SET search_path TO {search_path}` at `crates/jurisearch-storage/src/runtime.rs:250`. The comment says the value is already identifier-safe, but the API does not enforce that; the schema names are ultimately derived from corpus/generation labels stored in the database.

Recommended fix: change the helper to accept a list of schema names and quote each one with `sql_identifier`, or add a small `SearchPath` newtype constructed only from validated/quoted identifiers.

### One updated assertion message still says schema 19

`crates/jurisearch-cli/tests/cli_ingest_contract.rs:263` now checks `schema_version <> 20`, but the assertion message at `crates/jurisearch-cli/tests/cli_ingest_contract.rs:266` still says "schema 19".

Recommended fix: update the message to schema 20 or make it interpolate `CURRENT_SCHEMA_VERSION`.

VERDICT: FIXES_REQUIRED
