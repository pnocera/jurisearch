# Code Review: switch-to-pg18.sh r2

## R1 Findings

1. RESOLVED - The script no longer installs `postgresql-18-pgvector`; it clones pgvector tag `v0.8.0`, builds with `/usr/lib/postgresql/18/bin/pg_config`, installs into PG18's extension paths, and rejects any installed `vector.control` default version other than `0.8.0`.

2. RESOLVED - PGDG repo setup, `apt-get update`, and `apt-get install --simulate postgresql-18 postgresql-server-dev-18` now run before `pg_dropcluster 17 main --stop`; `apt-get --simulate` returns nonzero for unresolvable package sets, and dropping `17/main` immediately before the real PG18 install is the correct way to let the auto-created `18/main` cluster take port 5432.

## New Issues

No new issues found.

## Checked And Found Correct

- `/home/pierre/bear-storage/switch-to-pg18.sh:41-44` uses the PGDG key path `/usr/share/postgresql-common/pgdg/apt.postgresql.org.asc` and a `signed-by=` source line for `https://apt.postgresql.org/pub/repos/apt trixie-pgdg main`. The current PGDG apt documentation still uses that key URL/path and `Signed-By`; its preferred deb822 `.sources` format is equivalent to this one-line `.list` entry for this use.
- Current `trixie-pgdg/main` metadata has `postgresql-18` and `postgresql-server-dev-18` available for amd64, and `postgresql-18-pgvector` is `0.8.3-1.pgdg13+1` with no `0.8.0` candidate in the current package index. Avoiding the apt pgvector package is therefore still required for the stated physical-copy contract.
- Upstream `pgvector` tag `v0.8.0` has `EXTVERSION = 0.8.0` in the Makefile and `default_version = '0.8.0'` in `vector.control`. Building and installing from that single tag with `PG_CONFIG=/usr/lib/postgresql/18/bin/pg_config` makes the installed control file, SQL files, and shared library come from the matching 0.8.0 source tree.
- The pgvector 0.8.0 Makefile is a standard PGXS build and does not require external libraries beyond the C build toolchain and PG18 server development files. The script checks `curl`, `git`, `make`, and `gcc` up front, then preflights and installs `postgresql-server-dev-18`, whose package supplies the PGXS makefiles and PG18 headers.
- `/home/pierre/bear-storage/switch-to-pg18.sh:74` uses `sed -nE "s/^default_version = '([^']+)'.*/\1/p"`, which returns `0.8.0` for pgvector 0.8.0's `vector.control` line.
- `/home/pierre/bear-storage/switch-to-pg18.sh:16-28` keeps `set -Eeuo pipefail`, the `ERR` trap, and the `PG18-SWITCH-FAILED` sentinel. Failures in repo setup, apt update, simulation, install, clone, build, install, or version verification abort rather than emitting the success sentinel.
- `/home/pierre/bear-storage/switch-to-pg18.sh:38-57` performs only repo/key/package-list setup and the apt resolver preflight before the cluster drop. The first cluster-destructive step is the exact `17 main` guarded `pg_dropcluster`, after PG18 and server-dev have already been proven resolvable.
- `/home/pierre/bear-storage/switch-to-pg18.sh:54-57` can only drop the exact `17 main` tuple because it compares the first two `pg_lsclusters` columns against `17 main` with `grep -qx`.
- `bash -n /home/pierre/bear-storage/switch-to-pg18.sh` passes.
- `shellcheck /home/pierre/bear-storage/switch-to-pg18.sh` passes with no findings.

VERDICT: GO
