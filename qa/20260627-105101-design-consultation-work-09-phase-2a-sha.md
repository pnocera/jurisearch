# Phase 2A Design Consultation - Shared-Server Backend / Roles / Activation Visibility

## Verdict

**GO-with-adjustments.**

The proposed seam is directionally right, but I would narrow and harden it:

- Use 2A to introduce structured connection configs, role-scoped connection providers, role provisioning, activation grants, and the read-visibility postcondition.
- Do **not** build the real bounded pool yet. P4 is the first phase that needs pooling as a concurrency substrate.
- Do **not** refactor syncd/apply/activation fully to `StorageBackend` in 2A. The plan explicitly reserves that structural writer-path refactor for 2B.
- Do make role ownership/provisioning strong enough that the writer role can actually replace the existing stable views and create generation schemas later.
- Do not rely on `GRANT` success alone. Add an in-transaction read-role probe after the switch has rebuilt views and after all grants are in place.

Source basis: Phase 2A is scoped to backend/roles/activation visibility (`work/09-jurisearch-cli/04-implementation-plan.md:100-116`), while 2B is the `ManagedPostgres` writer-path refactor (`04-implementation-plan.md:118-140`). The design requires read/write role segregation and explicitly makes read-role visibility an activation postcondition (`work/09-jurisearch-cli/03-deployment-design.md:132-179`). Current code still connects as `user=postgres` (`crates/jurisearch-storage/src/runtime.rs:232-247`, `:852-854`), current `execute_sql` shells local `psql` as the superuser (`runtime.rs:237-239`, `:420-451`), and `activate_generation_with_guard` performs the short switch transaction then calls `rebuild_server_views` before commit (`crates/jurisearch-storage/src/generations.rs:959-1133`).

## 1. StorageBackend shape and provider-not-pool cut

Provider-not-pool is the right 2A cut.

The plan text says "read pool + writer pool", but the same plan also says 2B should accept a `StorageBackend`/writer handle as a **connection provider**, and P4 is where bounded worker/pool behavior becomes operationally relevant. Today every important path is synchronous `postgres::Client` or `psql`, and the query snapshot work is not until 3B/P4. Building a real pool now would create policy before the call patterns exist.

I would shape 2A like this:

```rust
pub trait StorageBackend {
    fn read_handle(&self) -> Result<ReadHandle, StorageError>;
    fn writer_handle(&self) -> Result<WriterHandle, StorageError>;
}

pub struct ReadHandle { config: ConnectionConfig }
pub struct WriterHandle { config: ConnectionConfig }

impl ReadHandle {
    pub fn client(&self) -> Result<postgres::Client, StorageError> { ... }
}
```

Keep the names honest: `read_handle` / `writer_handle` or `read_connector` / `writer_connector`, not `read_pool`, until there is a pool. P4 can wrap the same `ConnectionConfig` in a bounded pool without changing the role model.

For `ConnectionConfig`, prefer a structured `postgres::Config` builder or a carefully escaped libpq string builder. Do not let `Debug` print passwords. Include `application_name` if cheap; it makes role/connection mistakes visible in `pg_stat_activity`.

## 2. Writer-role scoping

Use schema/object ownership for the app-owned namespaces, not a hand-maintained privilege list, but do **not** make the writer superuser or owner of the whole database/public schema.

The writer must do dynamic DDL: create generation schemas, create generation tables/indexes, replace stable views, update `jurisearch_control`, and upsert `public.index_manifest`. An explicit grant list for every DDL target will be brittle. However, making the writer owner of `public` or the database is too broad.

Recommended shape:

1. Create a NOLOGIN owner role, for example `jurisearch_owner`.
2. Make the writer role a member of that owner role, or make the writer own the relevant objects directly.
3. Transfer ownership of `jurisearch_control`, `jurisearch_server`, `jurisearch_app`, their existing objects, and app-owned public objects such as `index_manifest`, `schema_migrations`, `package_change_log`, and package/trust tables as needed.
4. Grant the writer `CREATE ON DATABASE <db>` because generation schemas are created dynamically. Keep the role `NOSUPERUSER NOCREATEDB NOCREATEROLE`.
5. Do **not** transfer ownership of extensions, built-in/public extension objects, or the entire `public` schema.

The ownership point is important because migration 20 already creates the stable `jurisearch_server` views (`migrations.rs:903-958`). A non-owner writer will not be able to `CREATE OR REPLACE VIEW` those existing views during activation. Grants alone are not enough for that.

## 3. Read-role grant strategy

The proposed grants are mostly right, but add explicit grants for existing objects and be careful about who creates future objects.

Minimum read grants:

- `GRANT CONNECT ON DATABASE <db> TO <read>`;
- `GRANT USAGE ON SCHEMA public, jurisearch_control, jurisearch_server TO <read>`;
- `GRANT SELECT ON jurisearch_control.corpus_state, jurisearch_control.generation_registry TO <read>`;
- `GRANT SELECT ON public.index_manifest TO <read>`;
- `GRANT SELECT ON ALL TABLES IN SCHEMA jurisearch_server TO <read>` for the already-created stable views;
- at activation: `GRANT USAGE ON SCHEMA <new_gen_schema> TO <read>` and `GRANT SELECT ON ALL TABLES IN SCHEMA <new_gen_schema> TO <read>`.

Default privileges are useful but not sufficient:

- `ALTER DEFAULT PRIVILEGES FOR ROLE <writer_or_owner> IN SCHEMA jurisearch_server GRANT SELECT ON TABLES TO <read>` only affects future objects created by that role. It does not grant the views migration 20 already created as `postgres`.
- If future migrations add new stable views or tables, either rerun the provisioning grant step or put matching grants in the migration/provisioner.

Sequences are not needed for read-only `SELECT` from tables/views, but granting the writer `USAGE, SELECT ON ALL SEQUENCES` in app-owned schemas may be needed if 2B later writes rows with sequence defaults. The read role should not get sequence write-like privileges.

Also consider `schema_migrations` SELECT for health/status. It is not part of the strict 2A invariant you listed, but the current status/gating paths often read schema state, and it is cheap to grant read-only visibility.

## 4. Fail-closed postcondition

Use the in-transaction probe. Do not rely on `GRANT` failure.

`GRANT` can succeed while the topology is still unreadable: the role may lack schema usage, an existing view may not have SELECT granted, the stable view may not have been rebuilt yet, or a new table/view could be missing from the grant set. The invariant is not "the GRANT statement ran"; it is "the read identity can read the active topology at commit time."

Run the probe after:

1. the registry and `corpus_state` updates,
2. dense `index_manifest` rows,
3. physical generation grants,
4. `rebuild_server_views`,
5. explicit stable-view grants if you choose to re-grant defensively.

Then, before commit:

```sql
SET LOCAL ROLE <read_role>;
SELECT 1 FROM jurisearch_control.corpus_state WHERE corpus = $1 LIMIT 0;
SELECT 1 FROM jurisearch_control.generation_registry LIMIT 0;
SELECT 1 FROM public.index_manifest LIMIT 0;
SELECT 1 FROM jurisearch_server.documents LIMIT 0;
SELECT 1 FROM <new_gen_schema>.documents LIMIT 0;
RESET ROLE;
```

In production code, probe every relation in `REPLICATED_TABLES`, both through `jurisearch_server.<table>` and `<new_gen_schema>.<table>`, not just `documents`. `LIMIT 0` is fine; PostgreSQL still checks relation privileges.

One caveat: `SET ROLE <read>` only works if the current activation user is superuser or is a member of the read role. In 2A, activation still uses `ManagedPostgres.connection_string()` and therefore superuser, so the probe works. For 2B, either make the writer role a `NOINHERIT` member of the read role solely to allow `SET ROLE` probes, or introduce a controlled SECURITY DEFINER probe helper. Do not switch to a separate read connection for this postcondition; it cannot see the uncommitted activation transaction.

## 5. Threading read-role into activation

Use a small optional visibility config in 2A; do not do the backend refactor early.

`Option<&str>` is workable, but I would make it a typed config so the call site says what invariant is being requested:

```rust
pub struct ActivationReadVisibility<'a> {
    pub read_role: &'a str,
}
```

Keep the existing public functions preserving old behavior, and delegate to an internal or new explicit function:

```rust
activate_generation_with_guard(...); // existing, no role grants
activate_generation_with_guard_and_visibility(..., &ActivationReadVisibility { ... });
```

That avoids churn across existing tests and syncd while giving 2A a real visibility-tested activation path. In 2B, replace this config threading with the writer handle/backend abstraction when `apply_baseline` / `apply_rebaseline` / `apply_incremental` stop taking `&ManagedPostgres`.

## 6. Invariants, negatives, and ordering hazards

Ordering matters. The safe switch order is:

1. begin transaction;
2. set low `lock_timeout`;
3. acquire the existing advisory xact lock;
4. validate target generation is `building`;
5. lock and validate `corpus_state` cursor guard;
6. retire old active generation and mark the target active;
7. write `corpus_state`;
8. write dense `index_manifest` rows;
9. grant read role on the new physical generation schema/tables;
10. rebuild stable views;
11. defensively grant SELECT on stable views if needed;
12. run the `SET LOCAL ROLE` postcondition probes;
13. commit.

That preserves the existing rollback property: any failure after cursor update still rolls back the cursor, registry, dense metadata, view replacement, and grants.

For negatives, test all of these on the managed harness with real roles:

1. read role cannot `INSERT`, `UPDATE`, or `DELETE` `corpus_state`, `index_manifest`, stable views, or generation tables;
2. read role cannot `CREATE` in `public`, `jurisearch_control`, `jurisearch_server`, or generation schemas;
3. activation with an invalid/unusable read-role visibility config fails and leaves `corpus_state` unchanged;
4. after that failed activation, the target generation is not left active;
5. after successful activation, read role can select from `corpus_state`, `generation_registry`, `public.index_manifest`, every stable view, and every table in the new physical generation schema.

## Additional 2A risks

1. **Existing views are already present.** The prose assumption that stable views "don't exist until first activation" is stale. Migration 20 creates empty views immediately, so provisioning must handle existing objects.

2. **View owner is a hidden blocker.** `CREATE OR REPLACE VIEW` is an ownership operation. If provisioning only grants writer DML/CREATE but leaves migrated views owned by `postgres`, 2B will fail when activation runs as writer.

3. **`execute_sql` is still local-superuser-only.** Do not try to make `execute_sql` the shared-server API in 2A. It shells local `psql` as `postgres`. Shared-server code should use role-scoped `postgres::Client` from `ConnectionConfig`.

4. **Identifier safety matters for roles too.** Role names and schema names must be identifier-quoted, not string-literal-quoted. Treat role provisioning as SQL construction with the same care as `sql_identifier`.

5. **Default privileges are creator-specific.** If migrations/provisioning create objects as superuser but activation creates/replaces as writer, default privileges must be set for the actual object-creating role(s), or backed up by explicit grants.

6. **Do not let 2A absorb 3A readiness.** 2A proves read visibility, not writer-owned readiness coverage. The readiness stamp/cache behavior is still a separate 3A gate, especially because incremental apply advances the cursor without activation.
