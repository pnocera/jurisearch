# Adapting jurisearch to a hosted Postgres service

Date: 2026-06-24

Question: would it require a big effort if jurisearch later moves from embedded/managed Postgres (`pgembed`-style local index) to a service hosted with a PostgreSQL database?

## Short answer

Not a rewrite, but not a trivial switch either.

For a **single-tenant hosted service using one PostgreSQL database**, the effort looks **moderate**: roughly one focused architecture slice to split the current storage runtime into two modes:

- local embedded/managed Postgres, current behavior;
- external Postgres connection, no local PGDATA ownership.

The current SQL schema, migrations, retrieval functions, ingest accounting, projection logic, and evaluation logic are mostly reusable because they already operate on normal PostgreSQL tables with `pgvector` and `pg_search`. The main adaptation is around lifecycle and connection management.

For a **real multi-tenant SaaS** with public HTTP API, auth, per-tenant indexes, background workers, quotas, observability, backups, secrets, and concurrency isolation, the effort becomes **large**. That is a product/platform expansion, not just "swap embedded Postgres for hosted Postgres."

## Current state from source

The code currently models the database handle as `ManagedPostgres`.

Important current assumptions:

- `crates/jurisearch-storage/src/runtime.rs:86` defines `ManagedPostgres` with local `data_dir`, `socket_dir`, `log_path`, `port`, and `PgConfig`.
- `ManagedPostgres::start_durable_with_profile` at `crates/jurisearch-storage/src/runtime.rs:170` creates/owns `index_dir/pg/data`, writes runtime config, starts `pg_ctl`, creates the `jurisearch` database, applies runtime tuning, takes a data-dir advisory lock, runs migrations, and stops Postgres on `Drop`.
- `ManagedPostgres::connection_string` at `crates/jurisearch-storage/src/runtime.rs:233` returns a local loopback connection string.
- `ManagedPostgres::execute_sql` at `crates/jurisearch-storage/src/runtime.rs:237` shells out through local `psql`; this is convenient locally but is the wrong abstraction for hosted DB use.
- CLI helpers in `crates/jurisearch-cli/src/main.rs:5936` require an `index_dir` and check for `pg/data/PG_VERSION`.
- `open_index` and `open_index_for_bulk_ingest` at `crates/jurisearch-cli/src/main.rs:5966` and `5971` always discover local `pg_config` and start a durable local Postgres instance.
- `search_payload` at `crates/jurisearch-cli/src/main.rs:2489` resolves `--index-dir` / `JURISEARCH_INDEX_DIR`, then opens local Postgres before calling `search_with_postgres`.
- `search_with_postgres` at `crates/jurisearch-cli/src/main.rs:2662` already accepts an opened `&ManagedPostgres`, which is good: query behavior is separate from local startup.
- Bulk ingest paths at `crates/jurisearch-cli/src/main.rs:3815` and `4181` open local Postgres, then immediately create `postgres::Client` from `postgres.connection_string()` and perform most batch work through normal client/transaction APIs.
- Migrations in `crates/jurisearch-storage/src/migrations.rs:443` are attached to `ManagedPostgres` and use `execute_sql`, but the migration SQL itself is ordinary PostgreSQL extension/table/index setup.
- A rudimentary `serve` exists in `crates/jurisearch-cli/src/main.rs:6758`; it is a local JSONL socket server over the same session dispatcher, not a hosted HTTP service. It helps with process shape but does not remove the embedded database assumption.

## Why this is feasible

The core storage model is already PostgreSQL, not an embedded-only format. The project stores documents, chunks, embeddings, graph edges, ingest runs, manifests, and replay snapshots in regular tables. Retrieval uses `pg_search` and `pgvector`, both of which are PostgreSQL extension dependencies that can exist in a managed or self-hosted Postgres deployment if the provider supports them.

Several APIs already use `postgres::GenericClient` or explicit `postgres::Client`:

- projection insert helpers have `*_with_client` / `*_with_statements` forms;
- ingest accounting has many `*_with_client` forms;
- CLI batch ingest already keeps one `postgres::Client` open for large archive runs.

That means the lower layers are partly prepared for a non-owning connection. The missing piece is a first-class abstraction for "a database session" that is not identical to "start a local Postgres child process."

## What would need to change

### 1. Introduce a non-owning database handle

Create a storage abstraction, for example:

```rust
enum StorageHandle {
    ManagedLocal(ManagedPostgres),
    External(ExternalPostgres),
}
```

or a trait/object split:

```rust
trait JurisearchDb {
    fn connection_string(&self) -> &str;
    fn execute_sql(&self, sql: &str) -> Result<String, StorageError>;
}
```

The external mode would not have `data_dir`, `socket_dir`, `pg_ctl`, `initdb`, `stop`, or `pg_config` ownership. It would just validate/connect to a configured DSN such as `JURISEARCH_DATABASE_URL`.

This is the main source-level seam.

### 2. Move `execute_sql` off local `psql`

Hosted mode should not require local `psql` or local `pg_config`. `execute_sql` should use the Rust PostgreSQL client and return the same `-qAt`-like text shape, or better, call typed query helpers where the current caller only needs JSON/count rows.

This is important because retrieval/eval/status functions call `ManagedPostgres::execute_sql` directly in many places.

Recommendation: first replace `execute_sql` internals with `postgres::Client` for local mode too. Then both local and hosted modes share the same execution path.

### 3. Split "open index" from "open database"

Today `require_existing_index_dir` checks `pg/data/PG_VERSION`, and `open_index` means "start local Postgres from this directory."

A hosted mode needs a source selector:

- `--index-dir` / `JURISEARCH_INDEX_DIR` for local mode;
- `--database-url` / `JURISEARCH_DATABASE_URL` for hosted mode.

The public command behavior should report `storage_mode: "local_index" | "external_postgres"` in `status`, `doctor`, and ingest outputs.

### 4. Rework locking semantics

Local mode currently uses:

- a filesystem startup lock under the index directory;
- a PostgreSQL advisory lock keyed by canonical local `data_dir`.

Hosted mode cannot lock a `data_dir`. It needs a logical advisory lock key, probably derived from:

- database name + schema;
- configured logical index name;
- optional tenant/index namespace.

This is manageable for single-tenant hosted mode. For multi-tenant SaaS, locking becomes a broader design problem.

### 5. Keep migrations, but add hosted preflight

Migrations are reusable, but hosted Postgres needs a preflight:

- PostgreSQL version;
- `vector` extension available;
- `pg_search` extension available and loadable;
- permissions to create extensions, tables, functions, and indexes;
- ownership/role model;
- schema version not ahead of binary.

Some managed Postgres providers may not support `pg_search` / ParadeDB. If the hosted target cannot install `pg_search`, this is not just an integration issue; it would trigger the existing fallback strategy around native Postgres FTS or a different deployment target.

### 6. Connection pooling

One-shot CLI can keep opening a single client. A hosted service should not.

A service mode needs:

- a connection pool;
- per-request timeouts;
- cancellation behavior;
- max concurrent searches;
- separate ingest/write worker limits;
- read/write transaction discipline.

This is not hard conceptually, but it is a real operational slice.

### 7. Revisit runtime tuning

Local mode writes `postgresql.conf` and applies settings like `shared_buffers`, `work_mem`, `synchronous_commit`, WAL/checkpoint tuning, and `ivfflat.probes`.

Hosted mode can only use:

- provider-level parameters;
- database/user settings where permitted;
- session-level `SET` for query knobs.

Bulk ingest currently uses local profile tuning plus `SET synchronous_commit TO off`. Hosted mode needs explicit safe policy: some providers may reject or ignore these settings.

### 8. Service/API layer is separate from DB hosting

The existing `serve` command is a local JSONL socket daemon. It is not enough for a hosted service because it lacks:

- HTTP API contract;
- authentication/authorization;
- TLS/reverse proxy story;
- request limits;
- tenant isolation;
- structured observability;
- deployment/health endpoints;
- backup/restore/runbook integration.

If the desired target is "a private single-user service on a VM", the existing JSONL serve surface could be evolved. If the target is "public hosted jurisearch", design it as a new adapter over the same core command functions.

## Effort estimate by target

### Target A: local-first CLI plus optional external Postgres DSN

Effort: medium-small to medium.

What changes:

- add `JURISEARCH_DATABASE_URL`;
- introduce non-owning DB handle;
- make `execute_sql` client-backed;
- make `open_index` choose local vs external;
- update `status`, `doctor`, `stats`, `search`, `fetch`, `cite`, `context`, `related`, `ingest`, and eval paths to report/accept the external mode;
- add integration tests against a normal Postgres instance.

This is the lowest-risk path and preserves the current product contract.

### Target B: private long-running service using external Postgres

Effort: medium.

Add to Target A:

- a real process lifecycle;
- connection pool;
- health/readiness checks;
- service config file/env;
- request concurrency controls;
- graceful shutdown;
- basic auth or loopback-only reverse-proxy deployment;
- separate ingest worker policy.

The current `serve` implementation gives a protocol reuse point, but it should not be mistaken for production service infrastructure.

### Target C: multi-tenant hosted SaaS

Effort: large.

Add to Target B:

- tenant/index namespace model;
- authn/authz;
- per-tenant quota/rate limits;
- background ingest orchestration;
- migrations across tenant schemas/databases;
- corpus update scheduling;
- billing/usage accounting if relevant;
- legal attribution and source-availability release posture for hosted distribution;
- monitoring, backup/restore, disaster recovery, incident runbooks;
- data-retention and pseudonymisation policy review.

This is a new product shape.

## Main risks

1. **`pg_search` availability.** The hosted provider must support the exact extension stack. This is the biggest deployment gating risk.
2. **Lifecycle assumptions.** Current code owns PGDATA and can tune PostgreSQL. Hosted mode cannot.
3. **`execute_sql` text API.** Many query helpers depend on a simple text-returning SQL runner. Replace it carefully so behavior stays stable.
4. **Read/write concurrency.** The local model is effectively single-writer and often one process owns the database lifecycle. Hosted service mode needs explicit concurrency policy.
5. **Index identity.** `index_dir` currently gives identity, manifest location, and lock key. Hosted mode needs an equivalent logical identity.
6. **Product scope drift.** The locked conception says CLI-only/local-first. A hosted service should be framed as an adapter/deployment mode, not a silent redefinition of jurisearch's core contract.

## Recommended implementation path

1. Do not start with HTTP. Start by making the database runtime dual-mode behind the existing CLI.
2. Add `ExternalPostgres` with `JURISEARCH_DATABASE_URL`, but keep `--index-dir` as default/local mode.
3. Refactor `ManagedPostgres::execute_sql` to use `postgres::Client`, then generalize it behind a small `DbSession`/`StorageHandle` interface.
4. Move migration execution onto that interface and add extension/permission preflight.
5. Replace `require_existing_index_dir` at command boundaries with `resolve_storage_target`, returning either local index or external database.
6. Add `status`/`doctor` output for storage mode, extension availability, migration version, corpus freshness, query readiness, and lock identity.
7. Only after CLI parity is green, decide whether the service surface is:
   - local JSONL daemon,
   - private HTTP API,
   - or multi-tenant hosted product.

## Bottom line

Adapting jurisearch to use a hosted PostgreSQL database is **not a big rewrite** if the target is a single-tenant service or CLI mode backed by one external Postgres database. The current schema and most retrieval/ingest logic are reusable.

The required work is mainly an ownership/lifecycle refactor: separate "how to connect to the database" from "how to start and stop a local embedded Postgres." That is a moderate, worthwhile seam to introduce even if the project remains local-first.

It becomes a big effort only if "hosted service" means SaaS-grade HTTP service with tenants, auth, operations, quotas, and background corpus management. That should be treated as a separate product phase.
