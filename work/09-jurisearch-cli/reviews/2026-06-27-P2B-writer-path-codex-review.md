# Review: work/09 Phase 2B shared-server writer path

## Findings

### WARN: `GRANT SELECT ON ALL TABLES IN SCHEMA public` is broader than the writer path needs

`crates/jurisearch-storage/src/backend.rs:472-478` grants the writer `SELECT` on every current table in `public` so `CREATE TABLE ... (LIKE public.<table> ...)` can clone replicated templates. That makes the writer sufficient, and it still does not give write access to arbitrary `public` tables. It is broader than least privilege, though: the role only needs the replicated template tables for `LIKE`/baseline reads plus the already-explicit control/catalog tables, but the grant also covers any other current table in `public` in the app database.

Actionable fix: replace the `ALL TABLES IN SCHEMA public` grant with an explicit generated list for `REPLICATED_TABLES` (and keep the existing explicit grants for `public.index_manifest`, `public.schema_migrations`, `public.package_change_log`, and `public.package_catalog`). Add a role test that a non-required `public` table is not selectable by the writer while a writer baseline still succeeds.

### WARN: The new negative test does not prove the failure came from the activation `SET ROLE` probe

`crates/jurisearch-package-build/tests/shared_writer_loopback.rs:256-278` revokes `jurisearch_read` from `jurisearch_write`, calls `apply_baseline`, and accepts any error as success. The cursor-unchanged assertion at `crates/jurisearch-package-build/tests/shared_writer_loopback.rs:280-288` proves the switch did not commit, but it does not distinguish the intended probe failure from an earlier writer privilege regression in schema creation, COPY, index build, or activation DML.

Actionable fix: match the returned error down to the PostgreSQL error from `probe_read_visibility`, preferably SQLSTATE `42501` with message text indicating the role cannot be set, and also assert the generation registry row remains `building`. That makes the test prove the `SET LOCAL ROLE <read>` postcondition is the failing edge.

### WARN: The membership grants are not fully convergent against pre-existing admin options

`crates/jurisearch-storage/src/backend.rs:370-377` grants owner/read membership to the writer without `WITH ADMIN OPTION`, and `crates/jurisearch-storage/src/backend.rs:544-547` asserts the generated SQL does not contain that phrase. That is enough for fresh roles, but it does not remove a pre-existing admin option on `jurisearch_owner -> jurisearch_write` or `jurisearch_read -> jurisearch_write`. If a deployment had drifted into `WITH ADMIN OPTION`, rerunning provisioning would not prove the writer cannot re-delegate those roles.

Actionable fix: explicitly revoke admin option, or revoke and re-grant the owner/read memberships, before the intended grants. Add a role test that seeds the writer with an admin-option membership, provisions, and verifies the admin option is gone while the writer can still apply and run the read-role probe.

### NIT: `WriterHandle::new` still allows shared writers with no activation visibility

`crates/jurisearch-storage/src/backend.rs:106-114` stores `visibility: Option<WriterVisibility>`, and `activation_read_visibility` returns `None` at `crates/jurisearch-storage/src/backend.rs:140-144` when it is absent. The current repo call sites are correct: the shared CLI passes `Some(visibility)` at `crates/jurisearch-syncd/src/main.rs:86-93`, and both backend adapters do the same for role-scoped handles. The public constructor still permits future shared-server code to create a writer handle that activates without stamping read-role visibility.

Actionable fix: make the shared writer constructor require `WriterVisibility`, and reserve `None` for the `ManagedPostgres` trait impl rather than for `WriterHandle`. If a no-visibility handle is genuinely needed for tests, give it an explicit test-only or clearly named constructor.

## Source Audit Notes

The central identity swap is sound in the reviewed source. `WriterConnection` is object-safe, `WriterHandle::writer_client()` opens the configured writer identity, and `ManagedPostgres::writer_client()` preserves the self-managed superuser adapter. `activate_generation_with_guard` now derives visibility from the connection provider and opens the switch transaction through `writer_client()`, so the apply-to-switch two-connection shape is preserved without the hardcoded `ManagedPostgres` connection string.

The syncd writer APIs no longer expose `&ManagedPostgres` outside the binary composition root. The compatibility gates that previously depended on `ManagedPostgres` helpers now use SQL through a writer client: schema version and bundle digest, extension checks, server major, cursor reads, trust/license writes, and status reads. The shared-server CLI path builds a `WriterHandle` and does not start `pg_ctl` or run migrations; the self-managed path still starts durable Postgres and runs migrations before returning the managed adapter.

The positive shared-writer loopback is meaningful for the fresh-site path: baseline apply uses a `WriterHandle`, read-role visibility is checked through stable views, incremental apply uses a fresh writer handle, and the read role observes the incrementally changed row. The remaining gaps above are about narrowing grants and making the negative/security tests prove exactly the claimed role semantics.

I did not rerun the cargo validation commands, because the review instruction required not modifying any file other than the requested Markdown artifact and cargo would create/update build artifacts.

VERDICT: GO
