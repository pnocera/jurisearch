# Code Review: load-corpus.sh

## Findings

- BLOCKER: `/home/pierre/bear-storage/load-corpus.sh:62` hides every `pg_ctlcluster stop` failure with `|| true`, then immediately runs destructive `rsync --delete` into the live data directory. If the stop times out, fails through systemd, or leaves the server running, the script can delete/rewrite files under an active PostgreSQL cluster before failing later. Fix: only tolerate the already-stopped case explicitly, and require a down cluster before rsync. For example, run `pg_ctlcluster "$PGVER" main stop`, or if it returns nonzero verify `pg_lsclusters` reports `18 main down`; then assert `pg_ctlcluster "$PGVER" main status` is nonzero and `$DATADIR/postmaster.pid` is absent before the copy.

- BLOCKER: `/home/pierre/bear-storage/load-corpus.sh:52-58` treats any existing LXC config line containing `mp=$MNT` as good enough. A stale mount at `/mnt/jurisearch-staging` from a different host path, a writable mount, or a partially-applied config can pass or fail in the wrong direction; if the stale mount contains any PG18 `PG_VERSION`, the script will copy the wrong corpus and still emit success. The same block also blindly writes `-mp1` when no matching mountpoint is found, which can overwrite an unrelated existing `mp1`. Fix: allocate or reserve a dedicated mount key safely, fail if that key is already used for another mount, and verify the exact config tuple before proceeding, e.g. `mpN: $STAGING,mp=$MNT,ro=1`. After reboot, verify inside the container that `$MNT` is mounted read-only and still exposes the expected source, not merely any PGDATA-looking directory.

- BLOCKER: `/home/pierre/bear-storage/load-corpus.sh:46-47` gates only `pg_search.so`, and `/home/pierre/bear-storage/load-corpus.sh:87-93` verifies only database metadata plus `\dx`. A copied catalog can list the `vector` extension even when PG18's `vector.so` or extension files are missing, because `\dx` does not force the vector C library to load. That would produce a successful sentinel followed by failing vector queries/index access. Fix: before the destructive copy, check PG18's `pkglibdir` and `sharedir` for both `pg_search` and `vector` artifacts, and after start run a query in the corpus DB that actually loads vector, such as `SELECT '[1,2,3]'::vector;`, plus a minimal pg_search-dependent check if a cheap one is available.

- WARN: `/home/pierre/bear-storage/load-corpus.sh:38-42` accepts any stopped PG18-looking data directory with `base/` and `global/`, and `/home/pierre/bear-storage/load-corpus.sh:63-65` copies only the mounted PGDATA tree. If the source uses external tablespaces under `pg_tblspc` or a symlinked `pg_wal`, `rsync -a` will preserve symlinks whose targets do not exist in CT 110, producing an incomplete physical copy. The stated source is probably the default-layout corpus, but the script should fail closed on this class. Fix: reject non-empty `pg_tblspc` symlinks and a symlinked `pg_wal`, or explicitly copy and remap those external paths.

- WARN: `/home/pierre/bear-storage/load-corpus.sh:46-47` is not a complete guard that the separate pg_search build unit has finished. `pg_search.so` can exist after `cargo pgrx install` starts installing artifacts but before the build script has completed its restart/smoke/sentinel path. Rebooting CT 110 in that window can interrupt the build workflow. Fix: also require the build unit to be inactive/succeeded, require its success sentinel/artifact, or check the installed `pg_search.control` and SQL files along with the shared object.

## Checked And Found Correct

- The physical-copy approach is valid for the stated shape of the source: a same-major PostgreSQL 18 data directory, server stopped, copied as a complete cluster. PostgreSQL's file-system backup documentation requires the server to be shut down for a plain file copy and says complete cluster restoration needs the full data directory state, including transaction/WAL state; therefore copying the clean source `pg_wal/` is correct and excluding it would be wrong.

- The Debian-managed configuration model is compatible with this swap. `pg_ctlcluster` wraps `pg_ctl`, determines the cluster version/data path, and calls the right server with the appropriate configuration parameters and paths. PostgreSQL also supports moving configuration out of the data directory via command-line `config_file` plus `data_directory` in the main config. Therefore the copied source `postgresql.conf` in `$DATADIR` should not be the active main config when the Debian `18/main` cluster starts through `/etc/postgresql/18/main/postgresql.conf`.

- Neutralising `postgresql.auto.conf` is the right move. PostgreSQL reads `postgresql.auto.conf` in addition to `postgresql.conf`, and `ALTER SYSTEM` writes settings there. Leaving the pgrx source's auto.conf could override Debian's intended port/socket/listen/preload state. Replacing it with only `shared_preload_libraries = 'pg_search'` matches the stated requirement that pg_search must be preloaded while Debian's `/etc` config remains authoritative for cluster paths and connection settings.

- Running `rsync` and `chown -R postgres:postgres` inside the unprivileged container is the right ownership boundary. It avoids host id-map arithmetic and leaves the destination owned by the container's `postgres` user. Making the staging copy world-readable/executable on the host is a reasonable way to let container root read 0600 PGDATA files through a read-only bind mount; the remaining caveat is the external-symlink case called out above.

- `rsync -a --delete "$MNT/" "$DATADIR/"` is the intended destructive operation for replacing an empty target cluster with the corpus. Once the target server is proven stopped and the source mount is proven exact, `--delete` is appropriate because the target must become the source PGDATA, not a merge of the empty Debian cluster and the copied corpus.

- The script's broad fail-closed structure is good: `set -Eeuo pipefail`, `die`, the `ERR` trap, and failure/success sentinels are present; the original corpus on fedora is never touched; the copy source is mounted read-only in the container.

- Local static checks passed: `bash -n /home/pierre/bear-storage/load-corpus.sh` and `shellcheck /home/pierre/bear-storage/load-corpus.sh` both completed with no findings.

## Sources Consulted

- PostgreSQL 18 file-system backup documentation: https://www.postgresql.org/docs/current/backup-file.html
- PostgreSQL 18 file-location/configuration documentation: https://www.postgresql.org/docs/current/runtime-config-file-locations.html
- PostgreSQL 18 `ALTER SYSTEM` / `postgresql.auto.conf` documentation: https://www.postgresql.org/docs/current/sql-altersystem.html
- Debian `pg_ctlcluster(1)` man page: https://manpages.debian.org/unstable/postgresql-common/pg_ctlcluster.1.en.html
- PostgreSQL 18 `pg_ctl` stop/status documentation: https://www.postgresql.org/docs/current/app-pg-ctl.html

VERDICT: FIXES_REQUIRED
