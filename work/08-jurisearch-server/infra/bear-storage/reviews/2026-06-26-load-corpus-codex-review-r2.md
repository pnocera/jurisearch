# Code Review: load-corpus.sh r2

## R1 Findings

1. RESOLVED - The destructive copy is now gated by both `pg_ctlcluster status` returning nonzero and `$DATADIR/postmaster.pid` being absent before `rsync --delete` can run, so a normally detectable live cluster is not overwritten.
2. PARTIALLY - The rewritten `mp1` guard no longer clobbers a foreign `mp1` and verifies a PG18-looking mount, but the write-probe does not prove the mount is genuinely read-only under an unprivileged bind mount because `touch` can fail from ordinary DAC permissions on a writable mount.
3. RESOLVED - The script now checks `pg_search.so`, `vector.so`, `pg_search.control`, and `vector.control` before the copy, and the post-start `SELECT '[1,2,3]'::vector;` forces the vector C library to load instead of trusting `\dx` metadata.
4. RESOLVED - The staging guard now rejects non-empty `pg_tblspc` and symlinked `pg_wal`, so the script fails closed for the external-tablespace or external-WAL cases that `rsync -a` would not make self-contained.
5. RESOLVED - The pre-copy `systemctl is-active --quiet pgsearch-build` check prevents the CT reboot while the build unit is still active, and the artifact checks still catch missing installed outputs.

## New Issues

- BLOCKER: `/home/pierre/bear-storage/load-corpus.sh:35-78` dropped the previously-approved `chmod -R a+rX "$STAGING"` step from the ground-truth method. In an unprivileged LXC bind mount, container root is host-mapped and generally cannot read a source PGDATA that still has PostgreSQL's usual 0700 directories / 0600 files unless the host staging copy is made world-readable/traversable. The current checks only prove `$MNT/PG_VERSION` is readable; they do not prove `base/`, `global/`, relation files, or WAL files are readable before the script stops PostgreSQL and starts `rsync --delete`. This can fail after the destructive phase and leave CT 110's data dir empty or partially copied. Concrete fix: restore `chmod -R a+rX "$STAGING"` before the bind-mount/reboot block, then keep the mount read-only and perform the existing in-container `chown` only on `$DATADIR`.

- WARN: `/home/pierre/bear-storage/load-corpus.sh:62-77` treats a failed `touch "$MNT/.ro_probe"` as proof of a read-only mount. On this exact unprivileged-bind setup, the same failure can be caused by the staging directory being non-writable to the container's mapped root even when the LXC mount itself is writable. That means an existing `mp1: $STAGING,mp=$MNT` without `ro=1` can pass if permissions deny the probe. Concrete fix: require `ro=1` in the accepted `mp1` config line and/or verify the live mount options inside the CT with `findmnt`/`/proc/self/mountinfo` rather than relying only on write failure.

## Non-Regression Checks

- The physical-copy approach remains sound for a stopped same-major PG18 data directory, and copying the clean source `pg_wal/` is still correct for a physical cluster replacement.
- Debian's `/etc/postgresql/18/main/postgresql.conf` remains authoritative when the cluster is started through `pg_ctlcluster`, while the copied in-datadir `postgresql.conf` is not the active config file.
- Replacing `postgresql.auto.conf` with only `shared_preload_libraries = 'pg_search'` still removes the source pgrx runtime settings while preserving the required preload.
- Running `rsync` and `chown -R postgres:postgres` inside the container remains the right ownership boundary for an unprivileged CT.
- The source-on-fedora boundary is still preserved; the script only reads/modifies the bear staging copy and the CT target.
- `bash -n /home/pierre/bear-storage/load-corpus.sh` passed, and `shellcheck /home/pierre/bear-storage/load-corpus.sh` produced no findings in this environment.

VERDICT: FIXES_REQUIRED
