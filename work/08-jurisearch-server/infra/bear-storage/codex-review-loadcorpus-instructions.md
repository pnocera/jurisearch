# Codex review — load-corpus.sh (physical copy of a 165 G PG18 corpus into CT 110)

## Scope
`/home/pierre/bear-storage/load-corpus.sh`

Runs on the **bear host** (PVE). It physically loads a 165 G PostgreSQL 18 corpus into CT 110's
`18/main` cluster by overwriting the cluster data dir with a staged copy of the source PGDATA. This is
the highest-stakes script in this sequence (it destroys the empty `18/main` data and replaces it with
the corpus). Review it as a pre-execution gate.

## Ground truth
- The source corpus is a stopped PG18.4 PGDATA (PG_VERSION=18, base/, global/, at rest, no
  postmaster.pid), **already rsync'd from fedora to bear at `/root/jurisearch-staging`** (the `$STAGING`).
  The original is still on fedora — this is recoverable.
- CT 110 (unprivileged Debian 13 LXC) runs **PG18.4** + pgvector 0.8.3 + (being built) pg_search 0.24.1.
  Its empty `18/main` cluster is at `/var/lib/postgresql/18/main` (the `$DATADIR`).
- The corpus's catalog has `vector` (0.8.0) + `pg_search` extensions, so the cluster needs
  `shared_preload_libraries='pg_search'` to start, and pg_search.so/vector.so must be installed in the
  container's PG18 (the script checks pg_search.so presence as a gate).
- Debian starts the cluster with `-c config_file=/etc/postgresql/18/main/postgresql.conf` (which sets
  data_directory, hba_file, port 5432, socket dir), but `postgresql.auto.conf` is ALWAYS read from the
  data dir — so the script neutralises the source's pgrx-written auto.conf down to just
  `shared_preload_libraries='pg_search'`.

## The method (verify it is correct + safe)
1. Guards: root on bear; CT running; `$STAGING` is a stopped PG18 PGDATA; `$DATADIR/PG_VERSION` exists;
   pg_search.so installed in the CT.
2. `chmod -R a+rX $STAGING` then bind-mount it read-only into the CT at `/mnt/jurisearch-staging`
   (`pct set -mp1 ... ro=1`), reboot the CT to apply, wait for the mount.
3. Stop PG18; `rsync -a --delete $MNT/ $DATADIR/`; `chown -R postgres:postgres $DATADIR`; `chmod 700`;
   `rm -f postmaster.pid`; overwrite `postgresql.auto.conf` with just the pg_search preload line.
4. Start PG18; wait for connections; print DBs+sizes; find the largest non-postgres DB and run `\dx`.

## What to verify (correctness + safety, not style)
1. **Does this physical-copy method actually work on a Debian-managed cluster?** Is overwriting
   `/var/lib/postgresql/18/main` with a foreign (pgrx-created) PGDATA, while keeping Debian's `/etc`
   config, sound? Will Debian's `pg_ctlcluster ... start` use `/etc/.../postgresql.conf` (data_directory,
   hba_file, port, socket) over the copied data dir's own postgresql.conf? Is neutralising
   `postgresql.auto.conf` to only `shared_preload_libraries='pg_search'` the right move (vs leaving the
   source's pgrx port/socket/listen settings, which would conflict)? Any other in-datadir file that
   could fight Debian's config (e.g. the copied postgresql.conf is ignored — confirm)?
2. **Ownership for an unprivileged container.** The rsync + chown run INSIDE the container via
   `pct exec` (so `postgres:postgres` is the container's mapping, no host id-map math). Is
   `chmod -R a+rX $STAGING` on the host the right way to make 0600 PGDATA files readable through the
   unprivileged read-only bind mount? Any readability or id-map pitfall remaining?
3. **Destructive-rsync safety.** `rsync -a --delete $MNT/ $DATADIR/` makes `$DATADIR` exactly match the
   source. Confirm the guards make it impossible to run this against a wrong/empty `$DATADIR`, and that
   `--delete` here is intended (replace the empty cluster's files). Is copying the source `pg_wal/`
   (cleanly-stopped) correct, or should anything be excluded?
4. **The reboot.** Step 2 reboots the CT to apply the bind mount. The pg_search build runs as a
   separate unit in the CT; the pg_search.so gate ensures the build already finished before this runs —
   confirm that's a sufficient guard against rebooting mid-build, or recommend an explicit check.
5. **Fail-closed + recoverability.** `set -Eeuo pipefail` + ERR trap + LOADCORPUS-FAILED sentinel; the
   source on fedora is never touched. Any path that could corrupt the staging or leave the CT wedged?
6. Anything that would silently produce a broken/empty/half-copied cluster -> BLOCKER.

## Output
Severity-tagged findings (BLOCKER/WARN/NIT) with concrete fixes for every severity. Note what you
checked and found correct. End with a final line that is exactly `VERDICT: GO` or
`VERDICT: FIXES_REQUIRED`.
