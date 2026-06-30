# Review: reassign-corpus-ownership.sql (r3)

## Findings

No BLOCKER/WARN/NIT findings.

## Verification Notes

- The r2 blocker is resolved in [work/12-db-ownership-fix/reassign-corpus-ownership.sql](/home/pierre/Work/jurisearch/work/12-db-ownership-fix/reassign-corpus-ownership.sql:67). The `DO` block now runs the ownership change in two separate passes: pass 1 changes only `relkind IN ('r','p')` tables/partitioned tables at lines 71-80, then pass 2 changes only remaining `postgres`-owned sequences at lines 84-97. This avoids issuing `ALTER SEQUENCE ... OWNER` against serial/identity sequences before their parent tables have moved.

- The pass-2 dependency filter at [work/12-db-ownership-fix/reassign-corpus-ownership.sql](/home/pierre/Work/jurisearch/work/12-db-ownership-fix/reassign-corpus-ownership.sql:90) is the right shape for skipping auto-owned sequences: for serial/identity/`OWNED BY` sequences, the dependent object is the sequence's `pg_class` row (`classid = 'pg_class'::regclass`, `objid = c.oid`) and the dependency type is auto (`deptype = 'a'`). A genuinely standalone sequence has no such auto dependency, so it is not wrongly skipped and remains eligible for reassignment if still owned by `postgres`.

- If there are no standalone public sequences after pass 1, pass 2 is correctly a no-op: the `FOR r IN SELECT ... LOOP` simply has no iterations. If standalone sequences do exist, they are still selected because they remain `relkind = 'S'`, `owner = postgres`, and lack the auto-dependency row that pass 2 excludes.

- The after-verification count at [work/12-db-ownership-fix/reassign-corpus-ownership.sql](/home/pierre/Work/jurisearch/work/12-db-ownership-fix/reassign-corpus-ownership.sql:102) still includes `relkind IN ('r','p','S')`, so it will show any remaining `postgres`-owned table, partitioned table, or sequence in `public`. With the two-pass logic, zero rows there is the expected confirmation that table-owned sequences cascaded during pass 1 and standalone sequences, if any, moved during pass 2.

- The r1/r2 fixes still hold. Lock safety is present via transaction-local `lock_timeout` and `statement_timeout` at [work/12-db-ownership-fix/reassign-corpus-ownership.sql](/home/pierre/Work/jurisearch/work/12-db-ownership-fix/reassign-corpus-ownership.sql:39). Membership convergence uses explicit PG18 role options plus admin-option removal at lines 47-48. Scope remains limited to schema `public`, current owner `postgres`, and the intended relkinds in the reassign queries. The final ownership check at lines 127-137 is read-only catalog verification and cannot roll back the fix because it performs no DDL, `SET ROLE`, or locking operation.

VERDICT: GO
