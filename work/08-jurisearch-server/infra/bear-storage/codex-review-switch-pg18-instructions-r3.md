# Codex re-review (r3) — switch-to-pg18.sh

## Scope
`/home/pierre/bear-storage/switch-to-pg18.sh` (in CT 110, Debian 13: PG17 -> PG18 + pgvector for the
physical-copy of a PG18 corpus). Prior rounds: r1 + r2 (both addressed); r2 verdict was GO for a
build-pgvector-0.8.0-from-source approach.

## Why r3 (important new fact from running it)
Running r2's script **failed**: upstream pgvector **0.8.0 does not compile on PG18** —
`src/hnswvacuum.c: error: too few arguments to function 'vacuum_delay_point'`; PG18 changed the
signature to `vacuum_delay_point(bool is_analyze)`. So building pgvector 0.8.0 against PG18 is
impossible (which is why PGDG ships 0.8.3 for PG18). The script was changed to **install the PGDG apt
package (0.8.x, currently 0.8.3)** and accept any 0.8.x, on the rationale that pgvector keeps the
IVFFlat/HNSW on-disk index format stable across 0.8.x, so 0.8.3 reads the source's 0.8.0 indexes with
no reindex.

NOTE: the failed r2 run already dropped the empty PG17 cluster and installed postgresql-18 +
server-dev before failing at the pgvector build; so when this corrected script re-runs, the 17-drop
guard is a no-op and the PG18 install is a no-op — it effectively just adds pgvector. The script must
be safe under that partially-applied state.

## What to verify (this is the load-bearing part — the 165 G physical copy depends on it)
1. **Is the pgvector-version reasoning correct?** Does pgvector keep the **IVFFlat** on-disk index
   format stable across 0.8.0 -> 0.8.3 such that a 0.8.3 shared library reads a 0.8.0-created IVFFlat
   index WITHOUT reindexing? (The jurisearch dense index is IVFFlat.) If there is ANY 0.8.x patch that
   changed the IVFFlat/HNSW storage format or requires REINDEX, call it out as a BLOCKER. Also: is it
   true that the copied catalog `pg_extension.extversion = 0.8.0` works fine with a 0.8.3 vector.so
   (the .so provides the C symbols the 0.8.0 catalog objects reference), and that
   `ALTER EXTENSION vector UPDATE` is the correct optional way to later sync the catalog to 0.8.3?
2. **The 0.8.x gate.** Step 2 now preflights the pgvector **candidate** version (apt-cache policy) and
   dies if not `0.8.` BEFORE dropping PG17; step 5 re-checks the **installed** dpkg version. Confirm
   both gates are correct and that the destructive drop cannot happen if pgvector isn't a compatible
   0.8.x.
3. **Idempotency under the partially-applied state** (PG17 already dropped, PG18 already installed from
   the failed run): re-running must not error — confirm the 17-drop guard, the apt install (no-op for
   already-installed PG18, installs pgvector), and the verification all behave.
4. **No leftover references** to the removed source-build (PGVECTOR_DIR / git / make) and prereqs match
   what the script now uses (curl only).
5. Fail-closed (`set -Eeuo pipefail` + ERR trap + sentinel), PGDG repo/key correctness (unchanged).

## Output
Severity-tagged findings (BLOCKER/WARN/NIT) with concrete fixes; note what you verified correct
(especially the pgvector 0.8.0->0.8.3 index-format-compatibility claim). End with exactly
`VERDICT: GO` or `VERDICT: FIXES_REQUIRED`.
