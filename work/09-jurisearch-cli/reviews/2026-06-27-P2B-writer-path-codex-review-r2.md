# Review: work/09 Phase 2B shared-server writer path re-review

## Findings

### BLOCKER: Writer `public` SELECT narrowing is not convergent for databases that already received the broad grant

`crates/jurisearch-storage/src/backend.rs:481-486` now emits the intended narrow grant, `GRANT SELECT ON {replicated} TO {writer}`, and the SQL unit test rejects the old `GRANT SELECT ON ALL TABLES IN SCHEMA public` string at `crates/jurisearch-storage/src/backend.rs:568-573`. But the provisioning SQL never first revokes existing writer privileges on `public` tables. PostgreSQL grants are additive, so a writer role that already has `SELECT` on `public.unrelated_secret` from the previously reviewed broad grant, or from equivalent drift, keeps that access after the new provisioning run.

That means the least-privilege fix holds only for fresh role state, not for the idempotent/convergent provisioning contract described at `crates/jurisearch-storage/src/backend.rs:323-327`. The new regression test is false-green for this case: `writer_public_select_is_scoped_to_replicated_templates` creates `public.unrelated_secret` at `crates/jurisearch-storage/tests/shared_server_roles.rs:433-440`, then runs the current provisioner once and asserts the writer cannot read it at `crates/jurisearch-storage/tests/shared_server_roles.rs:442-466`. Since the test never seeds the old broad writer grant, the denial proves only the fresh default state, not cleanup of the exposure this follow-up was meant to close.

Fix: before the explicit writer grants in step 6, revoke the old table surface from the writer, for example `REVOKE ALL ON ALL TABLES IN SCHEMA public FROM {writer}` or at least `REVOKE SELECT ON ALL TABLES IN SCHEMA public FROM {writer}`, then regrant exactly `REPLICATED_TABLES` plus the named catalog/control tables. Add a regression that grants the writer broad `public` SELECT on an unrelated table, reruns `provision_roles`, asserts `has_table_privilege(writer, 'public.unrelated_secret', 'SELECT') = false`, and still proves the writer baseline path can clone/read the replicated templates.

## Re-verification Notes

The admin-option follow-up otherwise matches the requested semantics. `build_provision_sql` grants `owner -> writer` and `read -> writer`, then strips the admin option for both memberships at `crates/jurisearch-storage/src/backend.rs:378-381`; `provisioning_strips_a_preexisting_admin_option_on_writer_memberships` seeds both memberships with `WITH ADMIN OPTION`, verifies `pg_auth_members.admin_option` is false, keeps the memberships, and confirms the writer can still `SET ROLE` read at `crates/jurisearch-storage/tests/shared_server_roles.rs:471-531`.

The activation negative test now proves the intended failing edge. `activation_through_writer_fails_without_read_membership` revokes the writer's read-role membership, requires SQLSTATE `42501`, checks the message is from the `SET ROLE` probe, and confirms the generation registry row remains `building` with no `corpus_state` cursor at `crates/jurisearch-package-build/tests/shared_writer_loopback.rs:254-310`.

The `WriterHandle` constructor no longer permits a shared writer without visibility: `WriterHandle::new` requires `WriterVisibility`, the `WriterConnection` impl for `WriterHandle` always returns `Some(ActivationReadVisibility)`, and only the self-managed `ManagedPostgres` impl returns `None` at `crates/jurisearch-storage/src/backend.rs:112-156`. The shared-server CLI path constructs that visibility from the configured read/owner roles at `crates/jurisearch-syncd/src/main.rs:76-90`.

I did not rerun cargo validation commands because `cargo check` or the managed-PG tests would write build artifacts, and the review instruction only permits writing this Markdown artifact. I did run `git diff --check`, which passed.

VERDICT: FIXES_REQUIRED
