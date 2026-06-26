# Code Review: switch-to-pg18.sh

## Findings

1. BLOCKER - `/home/pierre/bear-storage/switch-to-pg18.sh:52` and `/home/pierre/bear-storage/switch-to-pg18.sh:60`

   The script installs an unpinned `postgresql-18-pgvector` from PGDG and only checks the version after installation. Current `trixie-pgdg` metadata checked during this review shows the candidate package as `0.8.3-1.pgdg13+1`, with `0.8.2` and `0.8.1` also present, but no `0.8.0` for `postgresql-18-pgvector`. APT will therefore install `0.8.3`, then the default gate `REQUIRE_PGVECTOR=0.8.0` will reject it. That correctly prevents the unsafe physical copy, but it means the script does not complete the requested PG18 switch today; it also reaches that failure after the PG17 cluster has already been dropped and PG18 packages have been installed.

   Concrete fix: preflight the acceptable pgvector package before dropping `17/main`, then either install an exact matching package version or build/package pgvector `0.8.0` against `/usr/lib/postgresql/18/bin/pg_config`. If accepting `0.8.1+` is intended, change `REQUIRE_PGVECTOR` to an explicit `0.8.` policy only after proving pgvector's on-disk index/operator ABI and copied `pg_extension.extversion = 0.8.0` are safe with the newer shared library. For the stated "source corpus is pgvector 0.8.0; must match source" contract, the safer fix is to keep the exact `0.8.0` gate and supply/install a matching build.

2. WARN - `/home/pierre/bear-storage/switch-to-pg18.sh:36`

   The only destructive operation, `pg_dropcluster 17 main --stop`, happens before any PGDG package/version preflight. The instructions state the PG17 cluster is empty, so this is not a data-loss issue, and leaving PostgreSQL 17 packages installed is fine. It does mean a transient key/repo/network/install failure, or the current pgvector mismatch above, leaves the container with the old cluster removed before the script has established that the replacement stack can be installed with the required extension version.

   Concrete fix: reorder the script so repo setup, `apt-get update`, and a non-destructive candidate check or `apt-get install --download-only` for the exact package set happen first. Then drop `17/main` immediately before the final install, so PG18 still auto-creates `18/main` on the intended port without dropping the old cluster for a known-impossible run.

## Checked And Found Correct

- The PGDG source line on `/home/pierre/bear-storage/switch-to-pg18.sh:46` uses `${VERSION_CODENAME}-pgdg main`, which resolves to `trixie-pgdg main` after the Debian codename guard. PGDG's `trixie-pgdg` Release file exists and currently lists `amd64 arm64 loong64 ppc64el` architectures and `main` as a component.
- Fetching the PostgreSQL signing key from `https://www.postgresql.org/media/keys/ACCC4CF8.asc` and referencing it with `signed-by=` is the modern apt-key-free model. The script uses the expected PGDG key path under `/usr/share/postgresql-common/pgdg/`.
- `postgresql-18` and `postgresql-server-dev-18` are present in the current `trixie-pgdg/main` package index; current amd64 metadata shows `18.4-1.pgdg13+1`, matching the source corpus major/minor from the instructions.
- The `pg_lsclusters | awk 'NR>1{print $1, $2}' | grep -qx "17 main"` guard cannot match another cluster name such as `17 main2` or another major such as `117 main`; it only invokes `pg_dropcluster 17 main --stop` for the exact `17 main` tuple.
- Leaving PostgreSQL 17 packages installed after dropping only the `17/main` cluster is acceptable. PostgreSQL major-version packages are designed to coexist, and the cluster removal is what avoids the port/cluster collision for the auto-created PG18 `main` cluster.
- The pgvector gate on `/home/pierre/bear-storage/switch-to-pg18.sh:60` is stricter than "0.8.x": with the default setting it accepts Debian package versions beginning with `0.8.0`, such as `0.8.0-1.pgdg13+1`, and rejects `0.8.1+`. Given the physical-copy requirement and source extension version `0.8.0`, that strictness is the right safety posture unless compatibility is proven separately.
- `set -Eeuo pipefail`, the `ERR` trap, the trixie guard, and the apt install command make missing repo/package/install failures abort instead of continuing to the final success sentinel.
- `bash -n /home/pierre/bear-storage/switch-to-pg18.sh` passes.
- `shellcheck /home/pierre/bear-storage/switch-to-pg18.sh` produced no findings in this environment.

VERDICT: FIXES_REQUIRED
