# Code Review: load-corpus.sh r3

## R2 Findings

1. RESOLVED - `/home/pierre/bear-storage/load-corpus.sh:63-86` restores `chmod -R a+rX "$STAGING"` before the bind mount is added or reused, then proves `$MNT/global/pg_control` is readable inside CT 110 before PostgreSQL is stopped and before `rsync --delete` can run. That closes the r2 failure mode where the destructive target delete could start before discovering that container-mapped root could not read the staged PGDATA tree.

2. RESOLVED - `/home/pierre/bear-storage/load-corpus.sh:70-84` now requires `ro=1` in a reused `mp1` config line and verifies the live mount options with `findmnt -rno OPTIONS "$MNT"`. Wrapping the options in commas before `grep -qE ',ro,'` requires `ro` as a standalone token, so strings such as `errors=remount-ro`, `relatime`, or other non-`ro` options do not satisfy the check.

## New Issues

None found.

## Non-Regression Checks

- Stop-before-rsync is still enforced at `/home/pierre/bear-storage/load-corpus.sh:92-101`: the script may ignore the stop command's return code, but it requires `pg_ctlcluster status` to be non-running and `$DATADIR/postmaster.pid` to be absent before the destructive copy.
- The four-artifact pre-copy gate is still present at `/home/pierre/bear-storage/load-corpus.sh:49-55` for `pg_search.so`, `vector.so`, `pg_search.control`, and `vector.control`.
- The post-start extension checks still force vector to load with `SELECT '[1,2,3]'::vector;` and confirm `pg_search` in the corpus catalog at `/home/pierre/bear-storage/load-corpus.sh:128-133`.
- External tablespaces and symlinked WAL are still rejected before copy at `/home/pierre/bear-storage/load-corpus.sh:40-42`.
- The build-active guard remains before any reboot or copy at `/home/pierre/bear-storage/load-corpus.sh:48`.
- `postgresql.auto.conf` is still replaced with only `shared_preload_libraries = 'pg_search'` at `/home/pierre/bear-storage/load-corpus.sh:99-107`, leaving Debian's `/etc/postgresql/18/main` config model authoritative.
- Ownership repair still happens inside the container with `chown -R postgres:postgres "$DATADIR"` at `/home/pierre/bear-storage/load-corpus.sh:99-107`, keeping the unprivileged-LXC id mapping boundary out of the host script.
- The source-on-fedora boundary remains intact: the script reads/modifies only the bear staging copy and the CT target, not the original corpus host.
- The fail-closed shape remains in place through `set -Eeuo pipefail`, `die`, the `ERR` trap, precondition checks before destructive work, and success/failure sentinels.
- No new quoting or control-flow issue was found in the r3 edits. `bash -n /home/pierre/bear-storage/load-corpus.sh` passed, and `shellcheck /home/pierre/bear-storage/load-corpus.sh` produced no findings.

VERDICT: GO
