# Storage Backend Policy

Date: 2026-06-21

This records the Phase 0 platform, Postgres-binary, and offline-install policy for task 0.3.

## Decision

- Phase 0 targets Linux x86_64 only.
- The storage backend is a managed local PostgreSQL child process, not an external service and not an in-process database.
- The Phase 0 PostgreSQL major is 18.
- The runtime must be addressed through `pg_config`; `JURISEARCH_PG_CONFIG` wins, then `PG_CONFIG`, then the newest `~/.pgrx/*/pgrx-install/bin/pg_config`.
- `pg_search` and `pgvector` must be installed into the same `pg_config` prefix used to start `initdb`, `pg_ctl`, and `psql`.
- macOS and Windows are unsupported for Phase 0. They require a separate extension-packaging proof before being claimed.

## Current Validated Path

The current developer/test path is the pgrx-managed PostgreSQL 18 prefix under `~/.pgrx`.

- `work/03-implementation/00-setup/build-pg-search.sh` installs the `cargo-pgrx` version pinned by `/home/pierre/Work/paradedb`, initializes/downloads PG18 through `cargo pgrx init`, and packages the local `pg_search` fork against that prefix.
- `work/03-implementation/00-setup/smoke-pg-extensions.sh` proves `CREATE EXTENSION vector; CREATE EXTENSION pg_search;` in a throwaway data dir.
- `jurisearch-storage` starts `initdb`/`pg_ctl` from the selected `pg_config --bindir`, writes local-only runtime config, and runs durable lifecycle, migration, BM25, and vector smokes against that child process through `extension_smoke.rs`, `durable_lifecycle.rs`, `schema_migrations.rs`, and `retrieval_smoke.rs`.

The Fedora system `postgresql-server-devel` route is not the accepted path on this workstation because PGDG `libpq5` from the pgAdmin repo conflicts with Fedora `postgresql-private-devel`. Use the pgrx-managed prefix instead.

## Offline Install Story

Online acquisition is acceptable during Phase 0 development. An offline/air-gapped install must pre-stage these artifacts:

- the built `jurisearch` binaries;
- a vendored or pre-fetched Cargo registry/cache for any source-build verification;
- a PostgreSQL 18 runtime directory with `pg_config`, `initdb`, `pg_ctl`, `psql`, `pkglibdir`, and `sharedir`;
- `pg_search` artifacts built against that exact runtime/major/platform: `.so`, `.control`, and SQL files;
- `pgvector` artifacts built or copied into the same runtime prefix;
- local embedding/reranker model files needed by the selected model configuration;
- official corpus archives and DTDs used by the ingest run;
- a config/env file pointing `JURISEARCH_PG_CONFIG` at the staged runtime.

The offline verification command is:

```bash
JURISEARCH_REQUIRE_PG_EXTENSIONS=1 JURISEARCH_PG_CONFIG=/path/to/pg_config cargo test --offline -p jurisearch-storage
```

For a packaged binary, the equivalent installer check must start a throwaway data dir, create both extensions, run migrations, insert a tiny chunk corpus, and verify BM25 plus vector retrieval without network access.

## Failure Policy

- A missing or mismatched `pg_config` is a dependency failure, not a fallback trigger by itself.
- A `pg_search` build or ABI failure triggers the locked first fallback: native PostgreSQL FTS.
- A managed-child-Postgres lifecycle failure triggers the no-Postgres fallback path only after the lifecycle failure is recorded with logs and reproduction steps: standalone Tantivy plus local vector/metadata storage first, with LanceDB only if the Postgres route fails both packaging and quality gates.
- No Phase 0 runtime may bind Postgres publicly; loopback or Unix socket only.
