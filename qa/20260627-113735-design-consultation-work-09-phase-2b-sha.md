# Phase 2B Design Consultation

**Verdict: GO-with-adjustments.**

The proposed direction is sound: Phase 2B should be the structural refactor that makes the existing syncd writer path run through a role-scoped writer connection provider, while `ManagedPostgres` remains the self-managed adapter. The adjustments are concrete, not architectural: the writer handle must carry activation visibility role metadata, provisioning must grant the read role to the writer for the existing `SET LOCAL ROLE <read>` probe, and the public activation surface should be changed in a way that preserves existing `&ManagedPostgres` call sites.

Source facts that drive the verdict:

- `WriterHandle` currently stores only a `ConnectionConfig`, so it cannot yet produce `ActivationReadVisibility { read_role, view_owner_role }`.
- `provision_roles` currently grants `owner` to `writer`, but not `read` to `writer`; the activation probe explicitly says `SET LOCAL ROLE` requires superuser or membership in the read role.
- `activate_generation_inner` still opens its own connection from `ManagedPostgres::connection_string()`, which is exactly the 2B back-edge to remove.
- `apply`, `planner`, `trust`, and `status` still expose `&ManagedPostgres` throughout the writer path.

1. **Q1: WriterConnection trait shape**

   Yes, use the narrow writer connection trait, and put it in `jurisearch-storage` rather than `jurisearch-syncd`, because `activate_generation_with_guard` lives in storage and must not depend upward on syncd.

   I would shape it roughly as:

   ```rust
   pub trait WriterConnection {
       fn writer_client(&self) -> Result<postgres::Client, StorageError>;
       fn activation_read_visibility(&self) -> Option<ActivationReadVisibility<'_>>;
   }
   ```

   Implement it for:

   - `ManagedPostgres`: `writer_client = self.client()`, `activation_read_visibility = None`.
   - `WriterHandle`: `writer_client = self.config.connect()`, `activation_read_visibility = Some(...)`.

   Prefer generic public functions such as `fn apply_baseline<C: WriterConnection + ?Sized>(client: &C, ...)` or `&dyn WriterConnection`; both are fine. The generic form preserves current `apply_baseline(&postgres, ...)` source shape after `ManagedPostgres` implements the trait.

   Do not make the apply path take a concrete `&WriterHandle` and synthesize a superuser `WriterHandle` for self-managed mode. That would add role/config ceremony to the compatibility path and make the self-managed adapter less honest.

   The adjustment: `WriterHandle` needs owned role metadata. Current `WriterHandle { config }` is not enough. Add something like an owned `ActivationReadVisibilityConfig { read_role: String, view_owner_role: String }` to `WriterHandle`, or to the backend and then into the handle. Do not derive the owner role from defaults; `RoleSpec` supports configured names and the P2A tests cover non-simple quoted names.

   Putting `activation_read_visibility()` on the trait is better than threading visibility as a parallel parameter through apply. It keeps the writer identity and the activation visibility contract bundled, avoiding a class of mismatches where a writer connection and read/owner roles come from different configurations.

2. **Q2: Replacing `execute_sql` / `server_version_major`**

   Yes. The apply path should stop using the `psql` shell path entirely.

   The current non-client uses are not inherently shell-only:

   - `check_schema_compatibility` can query `public.schema_migrations` through a writer client.
   - `schema_bundle_digest` already accepts a `GenericClient`; call it on the same writer client.
   - `check_extensions` already uses a client and reads `pg_extension`, which is catalog-readable.
   - `server_version_major` can become a small helper over `SELECT current_setting('server_version_num')`.

   Qualify `public.schema_migrations` in the new direct query. The default search path probably works, but a role-scoped shared-server path should not rely on it where a qualifier is cheap.

   Keep `run_migrations()` out of the writer abstraction. It remains a self-managed/provisioning concern. A shared site PG should be migrated and role-provisioned before the one-shot writer path attaches.

3. **Q3: `SET ROLE` probe and `GRANT read TO writer`**

   Add `GRANT <read_role> TO <writer_role>` in `provision_roles`. This is required for 2B if activation runs as the writer and continues using the current in-transaction `SET LOCAL ROLE <read>` probe.

   This does not create the privilege escalation problem you are avoiding:

   - It is `read` granted to `writer`, not `writer` granted to `read`.
   - Do not grant `WITH ADMIN OPTION`, so writer cannot delegate the role.
   - The read role remains SELECT-only and has its own memberships stripped by provisioning.
   - When the writer sets role to read for the probe, it is assuming a narrower identity.

   Prefer this over a `SECURITY DEFINER` helper. A definer function adds an owned executable object, search-path hardening requirements, and another grant surface. The existing probe is good; it just needs the role graph to permit it when the activation connection is no longer superuser.

   Add a test/assertion for the generated provisioning SQL and a real harness test where activation through a `WriterHandle` succeeds only when the writer can `SET LOCAL ROLE <read>`. A negative that revokes the read membership before activation should fail with cursor unchanged.

4. **Q4: Activation signature change**

   Change the activation signature now; this is exactly the structural refactor Phase 2B exists to perform.

   I would make `activate_generation_with_guard` take the writer abstraction and internally call `writer.writer_client()`, replacing the current `Client::connect(&postgres.connection_string(), ...)`. Because `ManagedPostgres` implements the trait, existing storage tests and self-managed call sites can still pass `&postgres`.

   Keep compatibility at the call-site level, not by preserving the old implementation path. In other words, `activate_generation_with_guard(&postgres, ...)` can still compile, but it should go through the same writer abstraction as `WriterHandle`.

   If you keep `activate_generation_with_guard_and_visibility`, treat it as a test/explicit override helper, not the syncd path. The syncd path should call the single activation API and let `activation_read_visibility()` decide whether the switch is self-managed (`None`) or shared-server (`Some`).

5. **Q5: Incremental path privileges**

   For generations created by the writer role, no extra grant should be needed. `create_generation_load_tables` creates the physical schema and tables on the same writer connection used by baseline/re-baseline apply; those objects are owned by the writer role, so a later fresh writer connection with the same role can DML the active physical generation.

   The caveat is existing generations created before this refactor by superuser/managed paths. P2A activation grants the read role and view-owner role SELECT on the generation schema; it does not grant writer DML on those generation tables. If 2B needs to support attaching a writer role to an already-populated DB, provisioning must backfill writer ownership or DML on existing `jurisearch_server_<corpus>_gNNNN` schemas. If the supported path is "fresh site PG, baseline/rebaseline applied through writer first", then this is not a Phase 2B blocker, but the tests must reflect that.

   The incremental proof should be: baseline via `WriterHandle`, then incremental via a fresh `WriterHandle`, then read role observes the changed active topology and the cursor advanced. Also add a negative where writer DML on the active generation is missing or revoked and the incremental fails with cursor unchanged.

6. **Q6: CLI scope**

   Add minimal one-shot shared-server attach wiring in 2B. Do not defer all CLI wiring to P5.

   The plan explicitly lists syncd `trust` / `subscribe` / `status` / `update` and calls the done condition "one-shot baseline/rebaseline/incremental apply proven against a standalone PG with the writer role." If the only consumer is a test-only library path, P4 still lacks the operational way to populate the site PG by one-shot `update`.

   Keep the CLI cut narrow:

   - self-managed mode remains the current `--index-dir` path: start managed PG, run migrations, use `ManagedPostgres`;
   - shared-server mode attaches to an existing migrated/provisioned PG and builds a `SharedServerBackend` / `WriterHandle`;
   - shared-server mode must not run migrations or `pg_ctl`;
   - no daemon loop, no real pool, no read query path in 2B.

   If CLI option design threatens to sprawl, land the smallest composition root that lets `update`, `status`, trust anchor install, and license install run with a writer handle. The daemon policy remains P5.

7. **Q7: Invariants, negatives, ordering/locking**

   The existing locking shape can survive the identity swap. Baseline/re-baseline currently hold a per-corpus session advisory lock on the long apply connection, then activation opens a second connection and takes the short transaction-level switch lock. Running both connections as the writer role preserves that ordering. Do not collapse the whole load/build/switch into one transaction as part of 2B.

   Add or preserve these checks:

   - Existing self-managed work/08 loopbacks still call the same public functions with `&ManagedPostgres`.
   - The new shared-writer tests must pass only a `WriterHandle`/writer abstraction into apply, using `ManagedPostgres` only as the local server harness/provisioner.
   - Baseline, re-baseline, and incremental each succeed through the writer role and leave the read role able to read the active topology.
   - Insufficient writer privilege fails cleanly: missing database `CREATE`, missing control-table DML, missing active-generation DML for incremental, and missing read-role membership for the activation probe should each leave cursor state unchanged.
   - A code search gate after refactor should show no `&ManagedPostgres` in the syncd writer APIs except the binary's self-managed composition root and the trait impl.

**Additional Phase 2B risks**

- `WriterHandle` currently lacks `read_role` and `view_owner_role`; adding `activation_read_visibility()` without extending the handle/backend constructors will either hardcode defaults or silently skip the P2A postcondition.
- `SharedServerBackend::new(read, writer)` also lacks owner-role metadata. Either extend it or add a constructor/config that carries the `RoleSpec`-derived visibility config.
- The provisioning SQL currently normalizes the read role by revoking memberships where read is the member. That should stay; adding writer membership in read is the opposite direction and should not be removed by that loop.
- `schema_bundle_digest` and direct schema-version checks should run through the writer client with qualified tables where possible; do not leave a hidden `execute_sql`/superuser psql call in an early gate.
- If existing physical generation schemas from a pre-2B superuser apply are in scope, add an explicit privilege backfill. If they are out of scope, document that shared-server attach requires applying the first site baseline through the writer role.
