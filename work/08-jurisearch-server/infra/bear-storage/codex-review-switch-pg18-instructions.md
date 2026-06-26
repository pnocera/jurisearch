# Codex review — switch-to-pg18.sh

## Scope
`/home/pierre/bear-storage/switch-to-pg18.sh`

Runs INSIDE CT 110 (Debian 13 trixie) as root. It replaces the PG17 stack with **PG18 from PGDG**
(apt.postgresql.org) + pgvector, because the corpus to import is a physical **PostgreSQL 18** data dir
(pgvector 0.8.0, pg_search 0.24.1). It drops the empty PG17 cluster (no real data yet), adds the PGDG
repo, installs postgresql-18 + postgresql-18-pgvector + postgresql-server-dev-18, and **refuses unless
the installed pgvector is 0.8.x** (must match the source for a clean physical copy).

After this script: rebuild pg_search for PG18, then physically copy the source data over the empty
18/main cluster.

## Ground truth (verified 2026-06-26)
- CT 110 = Debian 13 trixie; trixie ships postgresql-17 only (so PG18 must come from PGDG).
- An empty PG17 'main' cluster currently exists (UTF8/C.UTF-8, no real data); safe to drop.
- The source corpus (to be copied later) is PG **18.4** with **pgvector 0.8.0** and **pg_search 0.24.1**.
- Data mountpoint /var/lib/postgresql is a 1 TB volume; the PG18 install will auto-create 18/main there.
- Outbound connectivity works (verified earlier via apt-get update).

## What to verify (correctness + safety)
1. **PGDG repo setup for trixie.** Is the apt source line correct —
   `deb [signed-by=/usr/share/postgresql-common/pgdg/apt.postgresql.org.asc]
    https://apt.postgresql.org/pub/repos/apt trixie-pgdg main` (codename from VERSION_CODENAME)?
   Is fetching the ASCII key to `/usr/share/postgresql-common/pgdg/apt.postgresql.org.asc` and
   referencing it via `signed-by` the correct modern approach (vs apt-key)? Will
   `postgresql-18` + `postgresql-18-pgvector` + `postgresql-server-dev-18` resolve from PGDG for trixie?
   (If PG18 is not yet published for trixie-pgdg, the apt-get install fails closed — is that acceptable,
   and is the failure surfaced clearly?)
2. **pgvector version gate.** `dpkg-query -W -f='${Version}' postgresql-18-pgvector` yields something like
   `0.8.0-1.pgdg13+1`; the `case "$pv" in "${REQUIRE_PGVECTOR}"*)` match accepts `0.8.0*`. Confirm this
   correctly enforces "pgvector 0.8.x" and dies otherwise. Is matching on a version *prefix* the right
   call here (0.8.0 vs 0.8.1 — would 0.8.1 be index-compatible with a 0.8.0 source for a physical copy)?
   Flag if the gate should be stricter/looser.
3. **Dropping PG17.** `pg_dropcluster 17 main --stop` only runs if `17 main` exists (awk/grep guard).
   Confirm it cannot drop the wrong cluster and that leaving the postgresql-17 *packages* installed
   (only the cluster dropped) is fine / won't conflict with PG18.
4. **Fail-closed.** `set -Eeuo pipefail` + ERR trap + PG18-SWITCH-FAILED sentinel; the codename guard;
   `apt-get update`/`install` failures abort. Any path that leaves apt or the container half-configured?
5. **Anything that would break apt, pull the wrong major, or make the later physical copy unsafe** ->
   BLOCKER.

## Output
For each finding: severity (BLOCKER / WARN / NIT), exact location, the problem, and a concrete fix —
for every severity. Note what you checked and found correct. End with a final line that is exactly
`VERDICT: GO` or `VERDICT: FIXES_REQUIRED`.
