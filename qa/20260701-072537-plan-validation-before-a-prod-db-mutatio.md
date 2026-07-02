# Q&A — 20260701-072537

## Question

# Plan validation (BEFORE a prod DB mutation) — grant the producer CREATE on schema `public` on CT 110

Repo `/home/pierre/Work/jurisearch`. **Read the real source (esp. `crates/jurisearch-storage/src/backend.rs`
provisioning + `crates/jurisearch-storage/src/dense.rs` ivfflat rebuild + `crates/jurisearch-storage/src/provision.rs`)
to validate; don't trust my prose.** This is a pre-action gate on a production DB change (CT 110, PostgreSQL 18.4,
db `jurisearch`). I will NOT execute until you validate.

## Confirmed root cause (from the live CT 110 postgres log)
The producer's incremental `update --group legislation` fails at the EMBED step with (server log, verbatim):
```
jurisearch_write@jurisearch ERROR:  permission denied for schema public
STATEMENT:  DROP INDEX IF EXISTS chunk_embeddings_embedding_ivfflat_idx;
            CREATE INDEX chunk_embeddings_embedding_ivfflat_idx ON chunk_embeddings
            USING ivfflat (embedding vector_l2_ops) WITH (lists = 2189); ANALYZE chunks; ANALYZE chunk_embeddings;
```
Verified live DB facts:
- `pg_database.datdba` for `jurisearch` = **`postgres`** (hand-created; NOT `jurisearch_owner`).
- `public` schema ACL = `pg_database_owner=UC/pg_database_owner, jurisearch_write=U/…, jurisearch_read=U/…` — i.e.
  `pg_database_owner` has USAGE+CREATE; the producer roles have USAGE only.
- `has_schema_privilege('jurisearch_owner','public','CREATE')` = **false**; same for `jurisearch_write`.
- `jurisearch_write` is a member of `jurisearch_owner` (rolinherit=t); `jurisearch_owner` has no memberships.
- The corpus tables/indexes in `public` are now owned by `jurisearch_owner` (a prior ownership-reassignment fix);
  the `chunk_embeddings_embedding_ivfflat_idx` is currently valid + owned by `jurisearch_owner`.
- The corpus was HAND-LOADED into `public` by `postgres`, outside the normal producer build flow.

So `CREATE INDEX` (a new object in `public`) needs schema-level CREATE, which no producer role has, because the DB
owner (→ `pg_database_owner`, the role the ACL grants CREATE to) is `postgres`, not a jurisearch role.

## What the source shows (verify)
- `provision.rs::ensure_target_database` runs `CREATE DATABASE {db};` with NO `OWNER` clause (→ owned by the admin
  = postgres).
- `backend.rs` producer-role provisioning: `REVOKE CREATE ON SCHEMA public FROM PUBLIC`, then grants the writer
  USAGE on `public` + DML on ALL tables/sequences — but I can find NO `GRANT CREATE ON SCHEMA public` and NO
  `ALTER DATABASE ... OWNER` / `ALTER SCHEMA public OWNER`. Confirm this: does provision-db EVER give any jurisearch
  role CREATE on `public` (directly, via schema ownership, or via database ownership → pg_database_owner)? If not,
  how is the ivfflat `CREATE INDEX` in `dense.rs` intended to succeed in a canonical deployment — i.e. what is the
  DESIGNED authority path for producer DDL in `public`? (Is the canonical DB meant to be owned by `jurisearch_owner`,
  or is there a missing grant? This determines whether my fix is "repair a hand-load gap" or "also a provision-db bug".)

## Proposed fix (validate / correct)
**Primary:** `ALTER DATABASE jurisearch OWNER TO jurisearch_owner;` — makes `jurisearch_owner` the DB owner, so it
becomes `pg_database_owner` for this DB and thereby holds USAGE+CREATE on `public` (per the existing ACL), which
`jurisearch_write` inherits via membership → the ivfflat `CREATE INDEX` succeeds.

Questions:
1. Is `ALTER DATABASE ... OWNER TO jurisearch_owner` the correct, minimal, idiomatic fix here, versus
   `GRANT CREATE ON SCHEMA public TO jurisearch_owner;` (explicit, no DB-owner change)? Trade-offs?
2. **Side effects of changing the DB owner** to a NOLOGIN role: does making `jurisearch_owner` (NOLOGIN, no
   CREATEDB/superuser) the database owner break anything — connection, `provision-db` idempotency (does a later
   provision-db run assume/require a specific DB owner or reset it?), default privileges, DROP DATABASE, backups, or
   the site/client roles? Does `pg_database_owner` membership auto-flow to the DB owner (so `jurisearch_owner` and
   its member `jurisearch_write` genuinely gain CREATE on `public`)?
3. **Object ownership of the rebuilt index:** the producer runs DDL as `jurisearch_write` WITHOUT `SET ROLE`, so the
   newly-created `chunk_embeddings_embedding_ivfflat_idx` would be owned by `jurisearch_write`, not `jurisearch_owner`
   — diverging from the just-established owner-owned invariant. Does that matter (functionally / for future
   rebaselines / for a later ownership-converge)? Should the fix instead ensure new objects are owner-owned (e.g.
   grant CREATE to the owner AND have the producer `SET ROLE`, or `ALTER DEFAULT PRIVILEGES`), or is writer-owned
   indexes acceptable? Keep it minimal but call out the risk.
4. Should this be codified in `provision-db` (`backend.rs` producer profile and/or `ensure_target_database` adding
   `OWNER {owner}`) so a fresh/repaired deployment is correct and drift-proof — or is the manual one-off the right
   scope now, with the code fix as a follow-up? (I lean: apply the manual fix now to unblock, then a separate
   Codex-gated code patch.)
5. Any OTHER privilege the embed/rebaseline path needs in `public` that's also missing (so I don't fix CREATE only to
   hit the next wall) — e.g. does `dense.rs` / rebaseline DDL also need TEMP, or CREATE on other schemas, or
   ownership of specific objects? Scan the producer DDL paths.

## After the fix
I will re-run `update --group legislation` (incremental) monitored, and verify `core-1-2` publishes
(`published_head_sequence`→2). The prior run already ingested/embedded most of the delta; the embed step is
idempotent (pending-chunks query), so it should resume and reach the index rebuild quickly.

## Deliver
GO / GO-with-adjustments / NO-GO on the exact DB statement to run, the corrected statement if mine is wrong, the
side-effect assessment (esp. #2 and #3), and whether a `provision-db` code patch should follow. Ground it in the source.

## Answer

# Plan Validation: producer `CREATE` on `public` for CT 110

## Verdict

**GO-with-adjustments.**

The root cause is confirmed by source: the embed finalize path does runtime DDL in `public` as `jurisearch_write`, and provisioning does not currently grant any JuriSearch role `CREATE ON SCHEMA public` nor make `jurisearch_owner` the database owner. The failure is therefore expected on a database whose `public` schema grants `CREATE` only to `pg_database_owner` while the database owner is `postgres`.

For the immediate production unblock, I recommend the more minimal statement:

```sql
GRANT CREATE ON SCHEMA public TO jurisearch_owner;
```

Then verify:

```sql
SELECT
  has_schema_privilege('jurisearch_owner', 'public', 'CREATE') AS owner_can_create_public,
  has_schema_privilege('jurisearch_write', 'public', 'CREATE') AS writer_can_create_public,
  pg_has_role('jurisearch_write', 'jurisearch_owner', 'USAGE') AS writer_inherits_owner;
```

Expected: all `true`.

Your proposed:

```sql
ALTER DATABASE jurisearch OWNER TO jurisearch_owner;
```

is also a valid way to make the existing `pg_database_owner=UC` ACL on `public` flow to the JuriSearch owner role, and it is idiomatic if the database is meant to be app-owned. But it is broader than needed for the failing DDL because it changes database ownership, not just the missing schema-create privilege. I would use the explicit schema grant now, then codify the intended model in `provision-db` as a separate reviewed patch.

## Source Findings

### The failing DDL is real producer runtime DDL

`finalize_dense_rebuild_with_client` runs the exact shape from the live log:

- `DROP INDEX IF EXISTS {index_name}`
- `CREATE INDEX {index_name} ON chunk_embeddings USING ivfflat ...`
- `ANALYZE chunks`
- `ANALYZE chunk_embeddings`

Source: `crates/jurisearch-storage/src/dense.rs:148-264`.

The zone-unit path has the same requirement for `public`:

- `DROP INDEX IF EXISTS {zone_index}`
- `CREATE INDEX {zone_index} ON zone_unit_embeddings USING ivfflat ...`
- `ANALYZE zone_units`
- `ANALYZE zone_unit_embeddings`

Source: `crates/jurisearch-storage/src/zone_units.rs:588-698`.

The producer connects as the configured writer role via `ProducerConfig::writer_handle`, which uses `database.writer_user` and pins `search_path=public`. There is no `SET ROLE` around the dense rebuild. Sources: `crates/jurisearch-producer/src/config.rs:525-540`, `crates/jurisearch-storage/src/backend.rs:68-91`, `:121-141`.

### Provisioning does not currently grant `CREATE` on schema `public`

`ensure_target_database` creates the database with no owner clause:

```rust
CREATE DATABASE {target_db};
```

Source: `crates/jurisearch-storage/src/provision.rs:137-155`.

`build_provision_sql`:

- revokes legacy `PUBLIC` create on `public`
- grants `CONNECT, CREATE ON DATABASE` to the writer
- transfers `jurisearch_control`, `jurisearch_server`, and `jurisearch_app` schema ownership to the owner
- grants `USAGE ON SCHEMA public` to read/writer
- grants producer writer DML on all current/future public tables and sequences

It does **not**:

- `GRANT CREATE ON SCHEMA public TO jurisearch_owner`
- `GRANT CREATE ON SCHEMA public TO jurisearch_write`
- `ALTER SCHEMA public OWNER TO jurisearch_owner`
- `ALTER DATABASE ... OWNER TO jurisearch_owner`

Sources: `crates/jurisearch-storage/src/backend.rs:493-501`, `:503-536`, `:582-697`.

So this is not only a hand-load artifact. The current `provision-db` role model has a gap for producer runtime DDL in `public`. A hand-created DB owned by `postgres` exposes the gap immediately because `pg_database_owner` maps to `postgres`.

## Correct Production Fix

Run as a role that can grant on `public`, likely `postgres`:

```sql
GRANT CREATE ON SCHEMA public TO jurisearch_owner;
```

Why this is the best immediate mutation:

- It grants the exact missing privilege: the ability for the owner role to create objects in `public`.
- `jurisearch_write` already inherits `jurisearch_owner`, according to your live facts and the provisioning design (`GRANT {owner} TO {writer}` in `backend.rs:481-490`).
- It does not change database ownership or broaden database-owner powers.
- A later `provision-db` run does not revoke schema privileges from `jurisearch_owner`; it only revokes `CREATE` from `PUBLIC`, strips the read role, and grants writer `USAGE`.

After the grant, re-run the privilege check above. Do not proceed until `writer_can_create_public = true`.

## `ALTER DATABASE ... OWNER` Assessment

`ALTER DATABASE jurisearch OWNER TO jurisearch_owner;` is valid, but broader.

Pros:

- Aligns with the PostgreSQL 15+ default `public` ACL pattern where `pg_database_owner` has `USAGE,CREATE` on `public`.
- Makes `jurisearch_owner` the app database owner, which is conceptually clean for an app-dedicated database.
- If `public` remains owned/granted via `pg_database_owner`, the owner role gains `CREATE` on `public`, and the writer should inherit it through membership.

Cons:

- It changes database ownership, which is larger than the immediate privilege fix.
- The owner role is `NOLOGIN`; that is allowed, but admin operations that expect the owner to log in directly will not use that role. Superuser `postgres` remains able to administer, backup, and drop.
- Members of the owner role may now be able to exercise database-owner capabilities through role membership, depending on connection/database privileges. This is broader than schema create.
- Existing provisioning does not explicitly manage database ownership, so this is not currently enforced/drift-corrected by code.

If you choose `ALTER DATABASE`, verify with:

```sql
SELECT datdba::regrole
FROM pg_database
WHERE datname = 'jurisearch';

SELECT
  has_schema_privilege('jurisearch_owner', 'public', 'CREATE') AS owner_can_create_public,
  has_schema_privilege('jurisearch_write', 'public', 'CREATE') AS writer_can_create_public;
```

Expected: owner is `jurisearch_owner`, both privilege checks `true`.

## Object Ownership Of Rebuilt Indexes

This is the main caveat.

The DDL is executed as `jurisearch_write` without `SET ROLE`. The table owner/member relationship is enough to permit `DROP INDEX`/`CREATE INDEX` once schema `CREATE` exists, but new index ownership may end up as the executing role or otherwise drift from the owner-owned invariant. The exact result should be checked after the rebuild:

```sql
SELECT c.relname, c.relowner::regrole
FROM pg_class c
JOIN pg_namespace n ON n.oid = c.relnamespace
WHERE n.nspname = 'public'
  AND c.relname IN (
    'chunk_embeddings_embedding_ivfflat_idx',
    'zone_unit_embeddings_embedding_ivfflat_idx'
  )
ORDER BY c.relname;
```

Functional impact if an ivfflat index is `jurisearch_write`-owned:

- Querying does not care.
- Future rebuilds by `jurisearch_write` should still work.
- It does weaken the clean owner-owned invariant you just repaired.

Minimal production stance: acceptable to unblock `core-1-2`, but record it as drift to converge later.

If you want to re-converge immediately after successful publish:

```sql
ALTER INDEX public.chunk_embeddings_embedding_ivfflat_idx OWNER TO jurisearch_owner;
ALTER INDEX public.zone_unit_embeddings_embedding_ivfflat_idx OWNER TO jurisearch_owner;
```

Only run the zone index statement if that index exists.

Longer-term, the cleaner fix is source-level: run producer-owned public DDL under `SET LOCAL ROLE jurisearch_owner` or make provisioning explicitly grant the intended schema create path and add a privilege probe for `CREATE INDEX` in `public`.

## Other Privileges Scanned

I scanned producer/runtime DDL paths:

- Chunk embedding insertion uses `CREATE TEMP TABLE stage_chunk_embeddings ... ON COMMIT DROP` (`crates/jurisearch-storage/src/projection/embeddings.rs:62-74`).
- Zone embedding insertion uses `CREATE TEMP TABLE stage_zone_unit_embeddings ... ON COMMIT DROP` (`crates/jurisearch-storage/src/zone_units.rs:451-462`).
- Those already succeeded far enough to reach the dense finalize failure, so `TEMP` is not the current blocker.
- The only `public` runtime `CREATE INDEX` paths in the producer embed/rebuild flow are the chunk and zone ivfflat rebuilds (`dense.rs`, `zone_units.rs`).
- Migration DDL and extension DDL run under the admin/provisioning connection, not the producer writer.
- Generation schema/table/index DDL is the sync/apply generation path, not this producer incremental path, and uses database-level `CREATE` for new schemas (`crates/jurisearch-storage/src/generations.rs:169-203`, `:217-251`, `:419-445`).

No additional `public` schema privilege appears necessary for the immediate resumed legislation update beyond `CREATE ON SCHEMA public`. If the DB has been unusually hardened, keep `TEMP` in mind, but the live run already passed the temp-table embedding stage.

## Provisioning Follow-Up

Yes, codify this. The current producer provisioning is missing a privilege/postcondition for runtime producer DDL in `public`.

Recommended follow-up patch:

1. In the producer role profile, add either:

```sql
GRANT CREATE ON SCHEMA public TO {owner};
```

or, if you decide the canonical model is app-owned databases:

```sql
ALTER DATABASE {db} OWNER TO {owner};
```

2. Add a provisioning postcondition/probe that actually exercises a harmless rolled-back `CREATE INDEX` or equivalent schema-create operation in `public` as the writer. The current write probe only tests DML and sequence usage; it would not catch this failure (`crates/jurisearch-storage/src/provision.rs:201-260`).

3. Consider wrapping dense/zone ivfflat rebuild DDL in `SET LOCAL ROLE {owner}` if owner-owned indexes are a hard invariant.

For production now, do the one-line grant and rerun the monitored update. Treat the code patch as a separate Codex-gated change after `core-1-2` is published.

## Final Command

Run this now:

```sql
GRANT CREATE ON SCHEMA public TO jurisearch_owner;
```

Then verify:

```sql
SELECT
  has_schema_privilege('jurisearch_owner', 'public', 'CREATE') AS owner_can_create_public,
  has_schema_privilege('jurisearch_write', 'public', 'CREATE') AS writer_can_create_public,
  pg_has_role('jurisearch_write', 'jurisearch_owner', 'USAGE') AS writer_inherits_owner;
```

Proceed with the monitored `update --group legislation` only after all three are `true`.
