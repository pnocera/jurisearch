# Review: work/09 Phase 2B shared-server writer path re-review r3

## Findings

No findings.

## Re-verification Notes

The r2 blocker is addressed. `build_provision_sql` now converges the writer's existing `public` table surface before rebuilding its allowed access: it emits `REVOKE ALL ON ALL TABLES IN SCHEMA public FROM {writer}` at `crates/jurisearch-storage/src/backend.rs:482-484`, then grants `SELECT` only on the `REPLICATED_TABLES` templates at `crates/jurisearch-storage/src/backend.rs:485-490`, and then separately restores the named control/catalog DML grants at `crates/jurisearch-storage/src/backend.rs:491-496`. Under PostgreSQL's additive grant model, that ordering removes a stale broad `GRANT SELECT ON ALL TABLES IN SCHEMA public` from existing public tables before the intended narrow grants are re-applied.

The role regression now tests the previously missing drift case rather than only a fresh role. `writer_public_select_is_scoped_to_replicated_templates` creates `public.unrelated_secret`, creates the writer role, seeds the old broad public-table grant, runs provisioning, and then asserts the writer still has `SELECT` on `public.documents` but not on `public.unrelated_secret` at `crates/jurisearch-storage/tests/shared_server_roles.rs:433-470`. That directly covers the convergence failure from r2.

The writer baseline path is still covered by the shared-writer loopback: it provisions the client roles, obtains `backend.writer_handle()`, applies the baseline through `apply_baseline(&writer, ...)`, and verifies the read role sees the resulting cursor and document through the active `jurisearch_server` view. That complements the privilege probe by exercising the writer identity that still needs to clone/read the replicated templates.

I did not rerun the cargo validation commands because they would write build artifacts, and the task allows only this Markdown artifact to be modified. I did run `git diff --check` against the touched backend and role-test files; it passed.

VERDICT: GO
