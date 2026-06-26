# Code Review: switch-to-pg18.sh r3

## Findings

- NIT: `/home/pierre/bear-storage/switch-to-pg18.sh:3` still describes the script as installing "PG18 (PGDG) + source-built pgvector 0.8.0". The executable path now installs the PGDG `postgresql-18-pgvector` package and explicitly accepts any `0.8.x`, so this is a stale source-build reference. Fix: change the header comment to say "PG18 (PGDG) + PGDG pgvector 0.8.x" or equivalent.

No BLOCKER or WARN findings.

## Checked And Found Correct

- pgvector 0.8.0 -> 0.8.3 index compatibility: I did not find an explicit upstream prose guarantee that says "all 0.8.x index formats are stable", but the source evidence supports the script's claim for this migration. In upstream `pgvector` tags `v0.8.0` and `v0.8.3`, the IVFFlat and HNSW index magic numbers and on-disk version constants are unchanged (`IVFFLAT_VERSION 1`, `IVFFLAT_MAGIC_NUMBER 0x14FF1A7`, `HNSW_VERSION 1`, `HNSW_MAGIC_NUMBER 0xA953A953`). The header diff does not change the on-disk metapage/list/page/tuple structs (`IvfflatMetaPageData`, `IvfflatPageOpaqueData`, `IvfflatListData`, `HnswMetaPageData`, `HnswPageOpaqueData`, `HnswElementTupleData`, `HnswNeighborTupleData`). The 0.8.1, 0.8.2, and 0.8.3 changelog entries also do not announce any REINDEX requirement or IVFFlat/HNSW storage-format change. The 0.8.3 HNSW vacuum corruption fix is not an IVFFlat format change; the stated jurisearch dense index is IVFFlat.

- pgvector 0.8.0 catalog with a 0.8.3 shared library: upstream `sql/vector.sql` is unchanged between `v0.8.0` and `v0.8.3`, and `vector.control` keeps `module_pathname = '$libdir/vector'`. The only SQL additions between those tags are `vector--0.8.0--0.8.1.sql`, `vector--0.8.1--0.8.2.sql`, and `vector--0.8.2--0.8.3.sql`, each containing only the standard psql guard line. That means a physically copied catalog at `pg_extension.extversion = 0.8.0` still references the same `$libdir/vector` C library path and object definitions, and `ALTER EXTENSION vector UPDATE` is the correct optional way to advance the catalog metadata to the package default version later.

- PG18 support and pgvector 0.8.0 build failure rationale: upstream `pgvector` 0.8.1 adds PostgreSQL 18 rc1 support, and the diff after 0.8.0 adds the PG18 compatibility wrapper for `vacuum_delay_point(false)` in `src/hnswvacuum.c` and `src/ivfvacuum.c`. That matches the reported failure mode for unpatched 0.8.0 on PostgreSQL 18.

- 0.8.x pre-drop gate: `/home/pierre/bear-storage/switch-to-pg18.sh:53-64` runs `apt-get install -y --simulate postgresql-18 postgresql-server-dev-18 postgresql-18-pgvector` and then checks the apt candidate version with `apt-cache policy` before `pg_dropcluster` can run. A missing package, resolver failure, `(none)` candidate, empty candidate, or non-`0.8.` candidate all abort before the destructive step. The live PGDG trixie package index currently lists `postgresql-18-pgvector` as `0.8.3-1.pgdg13+1`, so the current intended package passes this gate.

- Installed package gate: `/home/pierre/bear-storage/switch-to-pg18.sh:79-88` verifies `vector.control` exists under PG18's `pg_config --sharedir`, logs its `default_version`, and rejects the installed dpkg package version unless it begins with `0.8.`. This correctly catches package drift after install. Checking `ctrl_ver` against `0.8.` too would be a small hardening improvement, but the current dpkg gate is sufficient for the stated PGDG package path.

- Idempotency after the failed r2 run: if `17/main` is already gone, `/home/pierre/bear-storage/switch-to-pg18.sh:68-71` simply skips `pg_dropcluster`. If PostgreSQL 18 and server-dev are already installed, the install command at lines 74-76 is an apt no-op for those packages and installs only missing `postgresql-18-pgvector`. The subsequent `pg_config`, `dpkg-query`, and `vector.control` checks still run, so the partially-applied state described in the instructions should converge instead of erroring.

- Removed source-build path: the executable logic no longer references `PGVECTOR_DIR`, `git`, `make`, `gcc`, a pgvector clone, or a source install. The only remaining source-build text is the stale header comment called out as a NIT above. Runtime prerequisites now match the package-based script path: `postgresql-common`/`pg_lsclusters` must already exist, and the only fetched external tool checked by the script is `curl`.

- Fail-closed behavior remains intact: `/home/pierre/bear-storage/switch-to-pg18.sh:22-34` keeps `set -Eeuo pipefail`, the `ERR` trap, `die`, and the failure sentinel. Repo setup, `apt-get update`, simulation, candidate check, install, `pg_config`, `vector.control`, and installed-version verification all abort before the success sentinel on failure.

- PGDG repo/key setup is correct for the current PostgreSQL APT instructions: the script downloads `https://www.postgresql.org/media/keys/ACCC4CF8.asc` to `/usr/share/postgresql-common/pgdg/apt.postgresql.org.asc` and uses a `signed-by=` source line for `https://apt.postgresql.org/pub/repos/apt ${VERSION_CODENAME}-pgdg main`. The official wiki now shows the same key path and repository URI in deb822 `.sources` form; the script's `.list` form is equivalent for this single repository.

- Local static checks passed: `bash -n /home/pierre/bear-storage/switch-to-pg18.sh` and `shellcheck /home/pierre/bear-storage/switch-to-pg18.sh` both completed with no findings.

## Sources Consulted

- Upstream pgvector comparison: https://github.com/pgvector/pgvector/compare/v0.8.0...v0.8.3
- Upstream pgvector changelog: https://github.com/pgvector/pgvector/blob/v0.8.3/CHANGELOG.md
- PGDG trixie package index: https://apt.postgresql.org/pub/repos/apt/dists/trixie-pgdg/main/binary-amd64/Packages
- PostgreSQL APT repository instructions: https://wiki.postgresql.org/wiki/Apt
- PostgreSQL `ALTER EXTENSION` documentation: https://www.postgresql.org/docs/current/sql-alterextension.html
- PostgreSQL extension packaging documentation: https://www.postgresql.org/docs/current/extend-extensions.html

VERDICT: GO
