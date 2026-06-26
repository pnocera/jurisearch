Summary:

The script is directionally aligned with the requested client provisioning flow: it refuses root execution, builds extensions as the normal user, stages pg_search with `cargo pgrx package` instead of running a root build, installs extension artifacts with sudo, builds pgvector with `PG_CONFIG`, restarts after `shared_preload_libraries`, prepends loopback scram HBA rules, and verifies TCP password auth. The blocking issues are in the Fedora package transaction and destructive ordering: on this machine the declared package set does not resolve, and the script reaches that failure only after stopping/removing PostgreSQL and deleting the data directory.

## BLOCKER

1. `libpq-devel` makes the Fedora dependency transaction fail, after the script has already removed PostgreSQL and purged the cluster.

   Evidence: `setup-client-postgres.sh:86-95` removes the existing server packages, deletes `$PGDATA`, then runs `dnf install` with both `postgresql-server-devel` and `libpq-devel`. A local resolver check for that pair reports that `postgresql-server-devel` requires `postgresql-private-devel`, while `postgresql-private-devel-18.3-7.fc45` conflicts with `libpq-devel-18.4-3.fc45`. Fedora's server-extension build path is `postgresql-server-devel` plus its `postgresql-private-devel` dependency; that stack provides the server headers, PGXS, and `/usr/bin/pg_config`. `libpq-devel`/`postgresql-devel` is the client-library devel package and is the wrong thing to install alongside the private server devel package here.

   Fix: remove `libpq-devel` from the install list. If an old client devel package may already be present, explicitly remove or swap out `libpq-devel`/`postgresql-devel` before installing `postgresql-server-devel`, while keeping the runtime `libpq` package alone. After install, assert that `/usr/bin/pg_config` exists and is owned by the Fedora PostgreSQL private/server-devel stack.

2. Package-resolution and source/build preflights happen after destructive actions.

   Evidence: `setup-client-postgres.sh:81-95` stops/disables `postgresql.service`, runs `dnf remove`, deletes `$PGDATA`, and only then tests whether the Fedora package set is installable. This is not fail-closed: the current `libpq-devel` conflict leaves the workstation with no previous cluster before the script ever reaches `pg_search` or `pgvector`. The bear `switch-to-pg18.sh` reference does the opposite for its package manager: it simulates the target install and validates pgvector compatibility before dropping the old cluster.

   Fix: add a non-destructive preflight before line 81 that resolves the exact target package set, verifies `postgresql-server-devel`/`postgresql-private-devel` and the clang/LLVM build dependencies are available, checks the paradedb fork and `cargo-pgrx` version, and optionally verifies the pgvector tag can be fetched. Only stop/remove PostgreSQL and purge `$PGDATA` after those checks have passed.

## WARN

1. `pg_config` selection is PATH-dependent and can target the user's pgrx-managed PostgreSQL instead of the system service.

   Evidence: `setup-client-postgres.sh:97-105` prefers `command -v pg_config` before falling back to `/usr/bin/pg_config`. The review context explicitly says this user has a separate `~/.pgrx` PG 18.4 for tests. If that binary appears earlier in PATH, the major-version check still passes, but `PKGLIB`/`SHARE` point at the pgrx-managed tree, so pg_search and pgvector are built and installed for the wrong PostgreSQL while `postgresql.service` later starts the Fedora system server.

   Fix: hard-pin `PGC=/usr/bin/pg_config` for this Fedora system-service script, then reject any `pg_config` under `$HOME` and log/assert system-looking `--pkglibdir` and `--sharedir` paths. Also invoke `cargo pgrx package` with `PGRX_PG_CONFIG_PATH="$PGC"` so cargo-pgrx reads the intended system pg_config without modifying `~/.pgrx/config.toml`.

2. The clean-uninstall step suppresses all `dnf remove` failures.

   Evidence: `setup-client-postgres.sh:85-88` redirects `dnf remove` stderr to `/dev/null` and then `|| true`s the command. That is fine for "package not installed", but it also hides rpmdb, lock, scriptlet, dependency, or permission failures and then proceeds to `rm -rf "$PGDATA"` and a fresh install attempt.

   Fix: query the installed package set first and only call `dnf remove` when there is something to remove, or handle the specific "not installed/no match" case. Let real `dnf remove` failures abort before the cluster directory is purged.

## NIT

1. `PGDATA` is advertised as a tunable, but Fedora `postgresql-setup --initdb` and `postgresql.service` are still the stock system defaults.

   Evidence: `setup-client-postgres.sh:51` allows `PGDATA` override; `setup-client-postgres.sh:87-88`, `138-140`, and `176-185` then purge, initialize-check, and edit HBA at that path. The actual `postgresql-setup --initdb` call and the enabled `postgresql.service` are not given a matching systemd environment/drop-in, so a non-default `PGDATA` can make the script touch one directory while the service uses another.

   Fix: either remove `PGDATA` as an env override and assert `/var/lib/pgsql/data`, or create the matching Fedora systemd/service configuration before initdb and use the same path consistently for service startup and HBA edits.

2. SQL values from env tunables are interpolated directly into SQL string literals.

   Evidence: `setup-client-postgres.sh:159-172` interpolates `PG_SUPERUSER_PASSWORD` into `ALTER USER ... PASSWORD '...'`, and `setup-client-postgres.sh:195` interpolates `APP_DB` into a SQL literal. The defaults are safe, but a quote in an env override breaks the script or changes the SQL.

   Fix: use psql variables with `:'var'` quoting for the password and database lookup, or avoid SQL interpolation for the database existence check by using `psql -v app_db="$APP_DB"` plus `WHERE datname = :'app_db'`.

VERDICT: FIXES_REQUIRED
