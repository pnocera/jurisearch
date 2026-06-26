Summary:

The r2 script correctly addresses most of the r1 items: `libpq-devel` is gone, the current fc45 metadata confirms `postgresql-server-devel` pulls `postgresql-private-devel` while `libpq-devel` conflicts with that stack, `/usr/bin/pg_config` is now hard-pinned, `cargo pgrx package` is pointed at that pg_config without `cargo pgrx init`, package removal no longer suppresses `dnf remove` failures, `PGDATA` is fixed to Fedora's stock service path, and the reviewed SQL literals now use psql variables. `bash -n` and `shellcheck -S style` are still clean.

The remaining issue is in the safety contract around the DNF preflight. `--downloadonly` is a real DNF5 install option and it does run dependency solving, but the current preflight solves against the machine's current installed package set, not against the post-removal state the script creates immediately afterward. That leaves a destructive failure path.

## BLOCKER

1. The DNF preflight can pass while relying on PostgreSQL packages that the script later removes.

   Evidence: `setup-client-postgres.sh:111-113` runs `sudo dnf install -y --downloadonly "${PKGS[@]}"` before destruction, then `setup-client-postgres.sh:120-130` removes `postgresql-server`, `postgresql-contrib`, and `postgresql-server-devel` and purges `/var/lib/pgsql/data`, and only then `setup-client-postgres.sh:133-134` runs the real install. On this host, `dnf install --assumeno --downloadonly ...` reports `postgresql-server`, `postgresql`, and `postgresql-contrib` as already installed, then skips the currently available `postgresql-contrib-18.3-7.fc45` candidate because of a Python 3.15 dependency conflict. With `--best`, the same resolver check fails closed. Without `--best`, the preflight is not proving that the package set can be installed after `postgresql-contrib` has been removed; it can be satisfied by the already-installed package that line 125 later deletes.

   This is exactly the class of failure the fail-closed preflight is supposed to prevent: the script can reach the purge with a preflight that was only valid for the pre-purge installed state, then fail during the real install because the requested PostgreSQL packages now need to come from the repo.

   Fix: make the preflight validate the same transaction shape as the destructive path. At minimum, use `--best` for both the preflight and the real install so skipped/broken candidates fail before removal, and add an explicit availability check for every package that may be removed before reinstall (`dnf repoquery --available --latest-limit=1 ...`, or an equivalent NEVRA-pinned download/reinstall preflight). A stronger fix is to avoid deleting packages between the preflight and install: install/upgrade the target stack first, assert `/usr/bin/pg_config` and the PG major, then stop PostgreSQL and purge/initdb only after the required system packages are already present.

## WARN

1. The service stop/disable step still suppresses real failures before removing packages and deleting the cluster.

   Evidence: `setup-client-postgres.sh:117-119` ignores all failures from `systemctl stop postgresql.service` and `systemctl disable postgresql.service`. Ignoring "unit does not exist" is fine, but ignoring a real stop failure means the script can proceed to `dnf remove` and `rm -rf "$PGDATA"` while the existing server is still active or in a failed stop state.

   Fix: distinguish an absent unit from a failed stop. If `postgresql.service` exists or is active, stop it, then verify `systemctl is-active postgresql.service` is not `active` before package removal and purge. Keep the "unit missing" case non-fatal.

2. The PostgreSQL major assertion is still after the destructive step.

   Evidence: `setup-client-postgres.sh:141-143` checks `PG_MAJOR_EXPECTED` only after lines 120-130 have removed packages and purged the data directory. The current fc45 metadata does provide PostgreSQL 18 packages, so the default target is correct today, but a repo mismatch, release mismatch, or bad `PG_MAJOR_EXPECTED` override would still be discovered too late.

   Fix: fold the major-version check into the preflight. Query the available `postgresql-server`/`postgresql` candidate version before removal, require it to match `PG_MAJOR_EXPECTED`, and then repeat the `/usr/bin/pg_config --version` assertion after install as a defense-in-depth check.

## NIT

1. `createdb` should use an option terminator for the env-derived database name.

   Evidence: `setup-client-postgres.sh:226-229` now quotes the `APP_DB` SQL lookup correctly, but `createdb "${APP_DB}"` still lets an env override beginning with `-` be parsed as a `createdb` option.

   Fix: use `sudo -u postgres createdb -- "${APP_DB}"`, and consider rejecting empty or option-looking database names up front.

VERDICT: FIXES_REQUIRED
