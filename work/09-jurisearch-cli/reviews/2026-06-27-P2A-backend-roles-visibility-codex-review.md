# Review - P2A backend roles and activation read visibility

## Findings

### BLOCKER: Provisioning is additive, so the read role is not reliably SELECT-only

`crates/jurisearch-storage/src/backend.rs:250-321`

The provisioning SQL creates roles only when absent and then adds grants, but it never converges an existing deployment back to least privilege. If `jurisearch_read` already exists with `SUPERUSER`, `CREATEDB`, `CREATEROLE`, a membership grant, table DML, or schema `CREATE`, this provisioner leaves that privilege in place. The same issue applies to inherited `PUBLIC` privileges: on an existing shared server or older database template where `public` still grants `CREATE` to `PUBLIC`, the read role can create `public` objects even though this provisioner only grants `USAGE`.

That violates the 2A invariant that the read identity "cannot INSERT/UPDATE/DELETE" and the test's intended "cannot CREATE" proof. The current harness only proves the default privileges of the local freshly-created cluster, not that `provision_roles` enforces the invariant on the database it is provisioning.

Actionable fix: make provisioning fail-closed or convergent before granting the intended privileges. At minimum, explicitly normalize the role attributes (`ALTER ROLE ... NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS`), revoke inherited write/create surfaces that can affect the read role (`REVOKE CREATE ON SCHEMA public FROM PUBLIC` or fail provisioning if that grant is present and the deployment will not allow revocation), and revoke accidental read-role DML/schema privileges on the app schemas/tables before re-granting only `CONNECT`, schema `USAGE`, and `SELECT`. Add a harness regression that deliberately pre-grants `CREATE ON SCHEMA public TO PUBLIC` and/or DML to the read role before calling `provision_roles`, then proves provisioning removes or rejects it.

### BLOCKER: Custom owner role names are not identifier-quoted in the ownership-transfer DO block

`crates/jurisearch-storage/src/backend.rs:281-296`

Most role uses go through `sql_identifier`, but the dynamic ownership loop interpolates `owner_raw` directly into the command text:

```sql
EXECUTE format('ALTER TABLE %I.%I OWNER TO {owner_raw}', ...)
```

That is neither identifier-quoted nor `quote_ident`-escaped. The default `jurisearch_owner` happens to work, but `RoleSpec` is explicitly deployment-configurable. An owner role such as `jurisearch-owner`, a mixed-case role, or a role containing a double quote will make provisioning fail or target the wrong parsed identifier. This also contradicts the local comment that role names are identifier-quoted.

Actionable fix: pass the owner role as an identifier argument to `format`, for example `EXECUTE format('ALTER TABLE %I.%I OWNER TO %I', r.schemaname, r.tablename, owner_name)` with `owner_name` supplied as a safely string-literal-quoted text value. Apply the same pattern for views and sequences. Add a unit or harness test with a non-simple `owner_role` name so the dynamic ownership loop is covered, not just the generated top-level grants.

### WARN: The write-denial test can still pass when a physical generation table is writable

`crates/jurisearch-storage/tests/shared_server_roles.rs:109-124`

The denial test checks the physical generation with:

```sql
INSERT INTO <physical>.documents(document_id) VALUES('evil')
```

That statement is invalid even for a privileged writer because `documents.source`, `kind`, `source_uid`, `body`, and `source_payload_hash` are `NOT NULL`. So the test would still pass if the read role accidentally gained `INSERT` on the physical generation table. The assertions also only check `is_err()`, not that the error is `insufficient_privilege`, so constraint failures and permission failures are indistinguishable.

Actionable fix: use syntactically and semantically valid write statements for each denied surface, and assert PostgreSQL SQLSTATE `42501` where practical. For the physical documents table, reuse the fully-populated row shape from `seed_one_document` inside a rollback-only transaction or savepoint. Also add a denied write through `jurisearch_server.documents` so the stable-view surface is covered.

### WARN: The post-activation test only verifies one replicated relation after commit

`crates/jurisearch-storage/tests/shared_server_roles.rs:86-98`

The activation probe in production loops over `REPLICATED_TABLES`, but the post-commit harness only checks `jurisearch_server.documents` and `<physical>.documents`. If the probe later regresses to a partial relation set, or if a grant path silently narrows to only `documents`, this test will not catch the degraded "full active topology" postcondition the phase asks for.

Actionable fix: expose or duplicate the replicated table list in the test and assert `SELECT count(*)` succeeds for every stable view and every table in the activated physical generation schema. Keep the direct `documents` row assertion as the data-path proof, but make the topology proof table-complete.

### NIT: `RoleSpec` can leak configured passwords through derived `Debug`

`crates/jurisearch-storage/src/backend.rs:179-187`

`ConnectionConfig` has a custom redacted `Debug`, but `RoleSpec` derives `Debug` while carrying `read_password` and `writer_password`. A failed provision, panic, or debug log of a `RoleSpec` would expose the shared-server credentials.

Actionable fix: remove `Debug` from `RoleSpec` or implement the same redaction pattern used by `ConnectionConfig`, with a unit test mirroring `connection_config_debug_redacts_password`.

## Notes

The activation ordering itself is directionally sound: the generation grant happens inside the switch transaction before `rebuild_server_views`, and the read-role probe runs before commit, so failures after the cursor write still roll back the cursor and registry state. The `view_owner_role` refinement is also the right shape for PostgreSQL's view-owner privilege chain.

I did not rerun the full validation matrix; this review is based on the live working tree, the untracked files in scope, and the SQL semantics above.

VERDICT: FIXES_REQUIRED
