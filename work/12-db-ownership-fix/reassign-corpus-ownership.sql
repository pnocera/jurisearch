-- Remediation: reconcile hand-loaded corpus object ownership on CT110 `jurisearch` DB.
--
-- Problem: the hand-loaded core corpus tables/indexes in schema `public` are owned by the
-- superuser role `postgres`. The producer writes as `jurisearch_write` (a member of
-- `jurisearch_owner`, INHERIT), and its dense-finalize step rebuilds the ivfflat index via
-- `DROP INDEX ... ; CREATE INDEX ... USING ivfflat ...; ANALYZE` as the writer
-- (crates/jurisearch-storage/src/dense.rs:253-264, driven from embed.rs:211-224). Because the
-- writer is NOT a member of `postgres`, that DDL fails with:
--   ERROR: must be owner of index chunk_embeddings_embedding_ivfflat_idx
-- The producer's own provisioner (crates/jurisearch-storage/src/backend.rs:508-536) only ever
-- reassigns the four catalog tables (index_manifest, schema_migrations, package_change_log,
-- package_catalog) + the app schemas to `jurisearch_owner`; it never reassigns the corpus
-- working tables (documents, chunks, chunk_embeddings, zone_units, zone_unit_embeddings, ...),
-- which on a producer-built corpus would have been owner-owned from creation.
--
-- Fix: make `jurisearch_owner` the owner of every `public` table/sequence currently owned by
-- `postgres` (index owners follow their table in PostgreSQL). The writer then satisfies the
-- ownership check via its INHERIT membership. Convention matches the producer: owner owns,
-- writer acts via membership (read-role default-privilege grants are keyed on the owner —
-- backend.rs:579, 690-696), so we reassign TO the owner role, NOT to the writer.
--
-- Scope: schema `public`, current owner `postgres`, relkind in (r,p,S) only. This deliberately
-- excludes: objects already owned by `jurisearch_owner`; the `paradedb`/`pdb` and
-- `jurisearch_server`/`jurisearch_control`/`jurisearch_app` schemas; pg_catalog; and indexes
-- (relkind i) which follow their parent table. ALTER ... OWNER is metadata-only (no table
-- rewrite), so this is fast even on the 22M-row tables; nothing else is running (timers stopped).
--
-- Run as a superuser against DB `jurisearch`:
--   PGPASSWORD=postgres PGSSLMODE=disable psql -h 192.168.0.110 -U postgres -d jurisearch \
--     -v ON_ERROR_STOP=1 -f reassign-corpus-ownership.sql

\set ON_ERROR_STOP on

BEGIN;

-- Fail clean, never hang: ALTER TABLE ... OWNER takes ACCESS EXCLUSIVE. If any unexpected
-- reader / idle-in-transaction session holds a conflicting lock, abort atomically (ON_ERROR_STOP
-- + single txn => no half-reassigned ownership) rather than queue behind it indefinitely.
SET LOCAL lock_timeout = '5s';
SET LOCAL statement_timeout = '60s';

-- 0. CONVERGE the writer's membership in the owner role to the producer's intended model
--    (backend.rs:476-489: writer inherits the owner, admin option stripped). A bare
--    `GRANT owner TO writer` does NOT repair an EXISTING PG18 membership whose INHERIT option
--    drifted to FALSE (omitted options retain current values). The ownership check the finalize
--    relies on (has_privs_of_role / pg_has_role USAGE) needs INHERIT TRUE, so set it explicitly.
GRANT jurisearch_owner TO jurisearch_write WITH INHERIT TRUE, SET TRUE;
REVOKE ADMIN OPTION FOR jurisearch_owner FROM jurisearch_write;

-- 1. Pre-change visibility: how many public objects are postgres-owned, by kind.
\echo '== BEFORE: public objects owned by postgres, by relkind =='
SELECT c.relkind, count(*)
FROM pg_class c
JOIN pg_namespace n ON n.oid = c.relnamespace
JOIN pg_roles    o ON o.oid = c.relowner
WHERE n.nspname = 'public' AND o.rolname = 'postgres'
GROUP BY 1 ORDER BY 1;

-- 2. Reassign every public TABLE / PARTITIONED TABLE owned by postgres to the owner role, then
--    any remaining STANDALONE sequences. Two passes, in this order, because:
--      * `ALTER TABLE ... OWNER` automatically moves the table's indexes AND its owned
--        (serial/identity) sequences to the new owner — so dependent sequences need no direct ALTER.
--      * `ALTER SEQUENCE ... OWNER` on an auto-owned (bigserial/identity) sequence FAILS while it is
--        still linked to a differently-owned table ("cannot change owner of sequence ... auto-owned").
--    So we must do tables first, and only then touch sequences that are NOT auto-dependent
--    (pg_depend.deptype='a') on a table column — i.e. genuinely standalone sequences.
DO $$
DECLARE r record;
BEGIN
  -- Pass 1: tables / partitioned tables (cascades to owned indexes + serial/identity sequences).
  FOR r IN
    SELECT c.relname
    FROM pg_class c
    JOIN pg_namespace n ON n.oid = c.relnamespace
    JOIN pg_roles    o ON o.oid = c.relowner
    WHERE n.nspname = 'public' AND o.rolname = 'postgres' AND c.relkind IN ('r','p')
    ORDER BY c.relname
  LOOP
    EXECUTE format('ALTER TABLE public.%I OWNER TO jurisearch_owner', r.relname);
  END LOOP;

  -- Pass 2: only sequences STILL owned by postgres that are NOT auto-owned by a table column
  -- (auto-owned ones already moved with their table in pass 1; altering them directly would error).
  FOR r IN
    SELECT c.relname
    FROM pg_class c
    JOIN pg_namespace n ON n.oid = c.relnamespace
    JOIN pg_roles    o ON o.oid = c.relowner
    WHERE n.nspname = 'public' AND o.rolname = 'postgres' AND c.relkind = 'S'
      AND NOT EXISTS (
        SELECT 1 FROM pg_depend d
        WHERE d.classid = 'pg_class'::regclass AND d.objid = c.oid AND d.deptype = 'a'
      )
    ORDER BY c.relname
  LOOP
    EXECUTE format('ALTER SEQUENCE public.%I OWNER TO jurisearch_owner', r.relname);
  END LOOP;
END $$;

-- 3. Post-change verification: nothing in public should remain postgres-owned (tables/seqs);
--    and the specific ivfflat index + its table must now resolve to jurisearch_owner.
\echo '== AFTER: public objects still owned by postgres (expect zero rows for r/p/S) =='
SELECT c.relkind, count(*)
FROM pg_class c
JOIN pg_namespace n ON n.oid = c.relnamespace
JOIN pg_roles    o ON o.oid = c.relowner
WHERE n.nspname = 'public' AND o.rolname = 'postgres' AND c.relkind IN ('r','p','S')
GROUP BY 1 ORDER BY 1;

\echo '== AFTER: ownership of the failing index + its table =='
SELECT n.nspname, c.relname, c.relkind, pg_get_userbyid(c.relowner) AS owner
FROM pg_class c
JOIN pg_namespace n ON n.oid = c.relnamespace
WHERE n.nspname = 'public'
  AND c.relname IN ('chunk_embeddings', 'chunk_embeddings_embedding_ivfflat_idx',
                    'chunks', 'documents', 'zone_units', 'zone_unit_embeddings')
ORDER BY c.relname;

-- 4. Read-only ownership assertion (no SET ROLE / no LOCK, so it can never roll back the fix):
--    CREATE INDEX / DROP INDEX / ANALYZE perform an ownership check = has_privs_of_role(current,
--    relowner). For the writer to pass on chunk_embeddings, two facts must hold post-reassign:
--    (a) the table's owner is jurisearch_owner, and (b) jurisearch_write is a member of (has the
--    privileges of) jurisearch_owner. Assert both straight from the catalog.
-- Cover BOTH ivfflat finalize paths the producer runs as the writer: chunk_embeddings
-- (dense.rs:253-264) and zone_unit_embeddings (zone_units.rs:687-698). Every row must show
-- ownership_check_will_pass = t.
\echo '== ASSERT: writer will pass the CREATE/DROP INDEX ownership check on both embedding tables =='
SELECT
  c.relname,
  pg_get_userbyid(c.relowner)                              AS table_owner,
  pg_has_role('jurisearch_write', c.relowner, 'USAGE')     AS writer_has_owner_privs,
  (pg_get_userbyid(c.relowner) = 'jurisearch_owner'
   AND pg_has_role('jurisearch_write', c.relowner, 'USAGE')) AS ownership_check_will_pass
FROM pg_class c
JOIN pg_namespace n ON n.oid = c.relnamespace
WHERE n.nspname = 'public' AND c.relname IN ('chunk_embeddings', 'zone_unit_embeddings')
ORDER BY c.relname;

COMMIT;

\echo '== DONE: corpus ownership reconciled to jurisearch_owner; writer is a member. =='
