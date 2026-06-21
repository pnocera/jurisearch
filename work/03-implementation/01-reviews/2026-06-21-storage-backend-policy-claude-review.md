I've now verified the policy against the committed runtime code, setup scripts, migrations, and the implementation plan. Every concrete claim in the policy checks out. Here is my review.

VERDICT: GO

# Storage Backend Policy — Review

`work/03-implementation/00-setup/storage-backend-policy.md` (commit `dd71b19`)

## Summary

The policy accurately records the Phase 0 platform/Postgres/offline decision and is consistent with the committed `jurisearch-storage` implementation, the two setup scripts, and `IMPLEMENTATION_PLAN.md` / `PREREQUISITES.md`. Every factual assertion I checked maps to real code or a real script. No blocking issues.

## Verified claims

- **`pg_config` precedence** (policy §Decision, line 12): "`JURISEARCH_PG_CONFIG` wins, then `PG_CONFIG`, then the newest `~/.pgrx/*/pgrx-install/bin/pg_config`" — matches `PgConfig::discover()` (`runtime.rs:27-35`) and `discover_pgrx_pg_config()` (`runtime.rs:444-475`, exact `~/.pgrx/<ver>/pgrx-install/bin/pg_config` path, newest-by-version selection).
- **Same-prefix extension rule** (line 13): `start_temp`/`start_durable` call `require_extension_assets("pg_search")`/`("vector")` against the `pkglibdir`/`sharedir/extension` derived from the same `pg_config` whose `bindir` runs `initdb`/`pg_ctl`/`psql` (`runtime.rs:94-95,161-162,318-368`). Accurate.
- **PG major 18** (line 11): consistent with `build-pg-search.sh` (`PG_MAJOR=18`) and `smoke-pg-extensions.sh`.
- **"Current Validated Path"** (lines 20-22): `build-pg-search.sh` does install the pgrx version pinned from `paradedb/Cargo.toml`, `cargo pgrx init --pg18 download`, then `make package`; `smoke-pg-extensions.sh` does prove `CREATE EXTENSION vector; CREATE EXTENSION pg_search;` in a throwaway dir; and the durable/migration/BM25/vector smokes exist (`durable_lifecycle.rs`, `schema_migrations.rs`, `retrieval_smoke.rs`). Migration v1 creates both extensions, v2 adds the `bm25` index; `retrieval_smoke.rs` verifies `@@@` BM25 + `<->` vector retrieval (`migrations.rs:19-114`, `retrieval_smoke.rs:59-75`).
- **Fedora route rejection** (line 24): matches the comment block in `build-pg-search.sh:11-13`. The smoke script's `cp` of `/usr/lib64/pgsql/vector.so` into the pgrx prefix is consistent with line 33's "built or copied into the same runtime prefix."
- **Offline verification command** (lines 40-42): correct and notably precise. Setting `JURISEARCH_REQUIRE_PG_EXTENSIONS=1` is what flips the tests from silently skipping to hard-failing on a missing `pg_config`/extensions (`tests/common/mod.rs:7-23`, `runtime.rs:640-657`), and `JURISEARCH_PG_CONFIG` is the highest-priority override. The command does exercise extension creation → migrations → chunk insert → BM25+vector, exactly as described.
- **Offline artifact list** (lines 31-36): `pg_config`/`initdb`/`pg_ctl`/`psql`/`pkglibdir`/`sharedir` is exactly the set the runtime consumes. Complete.
- **Failure Policy** (lines 48-51): the loopback/Unix-socket-only rule is enforced in `write_runtime_conf` (`listen_addresses='127.0.0.1'` + `unix_socket_directories`, `runtime.rs:417-442`); missing/mismatched `pg_config` surfaces as a hard `StorageError`, not a fallback. The native-FTS → no-Postgres fallback ordering matches `IMPLEMENTATION_PLAN.md §1` precedence.

## Suggestions (non-blocking, may be applied without re-review)

1. **Offline `cargo test` needs a pre-staged crate cache.** The artifact list (lines 28-36) covers the Postgres/extension/model/corpus assets but omits the Rust dependency cache. On a true air-gapped host, `cargo test -p jurisearch-storage` will attempt to reach crates.io. Consider adding `--offline` to the command and listing a vendored/pre-fetched cargo registry as a pre-stage item, so the verification step itself doesn't require network.
2. **PG-major narrative drift.** This policy locks Phase 0 to PG 18, but `PREREQUISITES.md §2` still frames the `gciauto2` reuse as "pins PG 17" and the readiness matrix (§13) says "pick PG major (17/18)." `§15.1` and the plan already reflect 18; a one-line note in `PREREQUISITES.md §2` that 18 is the settled Phase 0 target would remove the lingering "still open" impression. (Cosmetic — the authoritative decision here is unambiguous.)
3. **Traceability to smoke files.** Line 22 refers to "durable lifecycle, migration, BM25, and vector smokes" generically. Naming the four test files (as `IMPLEMENTATION_PLAN.md`'s "Current status" section does) would make the policy self-auditing.
4. **Name the no-Postgres fallback.** Line 50's "no-Postgres fallback path" could cross-reference the locked precedence (Tantivy + local vector/metadata, then LanceDB) so the fallback target is unambiguous from the policy alone.
