# Review - P2A backend roles and activation read visibility rereview r3

## Findings

No findings.

## Notes

The r2 blocker is addressed in the live source. `build_provision_sql` now makes the read role `NOINHERIT` and, more importantly, runs a dynamic `pg_auth_members` loop that revokes every direct membership where the configured read login is the member before re-granting the intended read surface (`crates/jurisearch-storage/src/backend.rs:286`, `crates/jurisearch-storage/src/backend.rs:342`). That closes both PostgreSQL inherited privileges and the separate `SET ROLE` path called out in r2; `NOINHERIT` is correctly only defense in depth, not the primary fix.

The new harness regression is a meaningful proof rather than a false green. It creates a separate `rogue_writer` role with DML on `jurisearch_control.corpus_state`, grants it to the read login before provisioning, then verifies the membership is gone with `pg_has_role(..., 'MEMBER')`, verifies the inherited INSERT path is denied through `assert_denied` and SQLSTATE `42501`, and verifies `SET ROLE rogue_writer` is rejected (`crates/jurisearch-storage/tests/shared_server_roles.rs:301`). If the provisioner only set `NOINHERIT` while leaving the membership in place, the inherited INSERT assertion would pass but the `SET ROLE` assertion would fail, so the test covers the exact r2 hole.

I also re-checked the surrounding role and visibility paths: role/owner identifiers are quoted through `sql_identifier` or PostgreSQL `%I`, the ownership transfer DO block no longer interpolates raw owner names, the read role's direct object grants are revoked before the minimal read grants are added, and activation still grants and probes the new physical generation schema inside the switch transaction before commit. I did not find a remaining blocker in the reviewed Phase 2A surface.

Validation note: I did not rerun the full cargo validation matrix in this pass; this review is based on the r3 brief, the r2 review, the live working tree, and PostgreSQL privilege semantics.

VERDICT: GO
