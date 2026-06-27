# Review - P2A backend roles and activation read visibility rereview

## Findings

### BLOCKER: Read-role convergence still leaves inherited write roles in place

`crates/jurisearch-storage/src/backend.rs:283-352`

The follow-up fixes close the direct-grant and `PUBLIC CREATE` holes, but the provisioner still does not actually converge a pre-existing read role to SELECT-only when that role has arbitrary memberships. It normalizes `rolsuper` / `rolcreatedb` / `rolcreaterole` and revokes direct object grants, then only revokes the two configured app roles:

```sql
REVOKE "jurisearch_owner", "jurisearch_write" FROM "jurisearch_read";
```

Any other membership survives. With PostgreSQL's default inherited role privileges, a site where `jurisearch_read` was previously granted a write-capable role still lets the read login write after this provisioning runs. Even setting `NOINHERIT` would not fully close this by itself, because the read login can still `SET ROLE` into a granted role when that membership remains usable.

I verified the SQL behavior in a throwaway PostgreSQL cluster: after `REVOKE ALL ON ALL TABLES ... FROM read_role` and re-granting only `SELECT`, a `read_role` that remained a member of a `rogue_writer` role with `INSERT` on `jurisearch_control.corpus_state` could still insert successfully. The current convergence test misses this because it only pre-grants direct DML to `jurisearch_read` and does not add a helper write role membership.

This violates the P2A invariant that the read identity is SELECT-only and makes the strengthened test a false green for a real class of over-privileged existing deployments.

Concrete fix: during provisioning, dynamically revoke every existing membership where the read role is the member, or fail closed if any cannot be revoked. For example, iterate `pg_auth_members` joined to `pg_roles` for `member = read_role::regrole` and execute `REVOKE %I FROM %I` for each role before granting the intended read surface. Also make the read role `NOINHERIT` as defense in depth, but do not rely on that instead of revoking memberships. Add a harness regression that creates a helper role with `INSERT` and/or `CREATE` privileges, grants that role to `jurisearch_read`, runs `provision_roles`, then proves both the inherited write and `SET ROLE` elevation paths are gone.

## Notes

The prior owner-quoting blocker is fixed: the ownership DO block now binds `owner_name` as a string literal and passes it through `%I` for tables, views, and sequences. The prior direct/additive grant issue is partially fixed for role attributes, direct read-role grants, and `PUBLIC CREATE ON SCHEMA public`, but not for inherited memberships above. The denial tests now use valid statements and assert SQLSTATE `42501`, and the successful topology test now loops over every `REPLICATED_TABLES` relation through both `jurisearch_server` and the physical generation schema.

I ran:

- `cargo test -p jurisearch-storage --lib backend`
- `cargo test -p jurisearch-storage --test shared_server_roles`

Both passed locally.

VERDICT: FIXES_REQUIRED
