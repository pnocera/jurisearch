Summary:

The r3 pg_search install blocker is fixed correctly. `setup-client-postgres.sh:177-183` no longer uses `cp -a`: the `.so` is installed with `sudo install -D -o root -g root -m 0755`, the extension directory is created as `root:root`, and every regular file in the staged pg_search extension directory is installed with `sudo install -o root -g root -m 0644 -t "$SHARE/extension"`. On the local staged pgrx output, that directory contains `pg_search.control` and the full set of `pg_search--*.sql` files, so the current `find "$EXT_SRC_DIR" -maxdepth 1 -type f ...` form copies the control file plus all staged upgrade/default SQL files. GNU `install` sets owner/group/mode instead of preserving the source attributes, and the guarded `restorecon` is the right Fedora SELinux repair step: this host is enforcing, `restorecon` is present, and `matchpathcon` expects `lib_t` for `/usr/lib64/pgsql/pg_search.so` and `usr_t` for `/usr/share/pgsql/extension/pg_search*`.

The package-ordering, cluster-purge ordering, sudo keepalive, pg_search shared-preload restart, HBA prepend, TCP auth smoke check, `bash -n`, and `shellcheck -S style` checks are still in good shape. I did not find a remaining pg_search SELinux/ownership trap, and the pgvector path uses the upstream PGXS `make install` flow under `sudo`, not an archive copy from a user-owned staging tree.

However, the script still has one runtime blocker independent of the r3 SELinux fix.

## BLOCKER

1. `setup-client-postgres.sh:243` uses psql variable interpolation inside a `-c` SQL command, so the database-existence probe is not valid psql usage.

   Evidence: the line is:

   ```bash
   db_exists="$(sudo -u postgres psql -tA -v app_db="$APP_DB" -c "SELECT 1 FROM pg_database WHERE datname = :'app_db'")"
   ```

   The `:'app_db'` syntax is a psql variable-quoting feature, but psql's `-c/--command` SQL string must be parseable by the server; PostgreSQL documents `-c` as running a single SQL command or internal command, not a mixed psql-script input stream. In this form the server receives `:'app_db'` as SQL text rather than a quoted database name. That can fail at the final database creation step after packages have been changed, the cluster has been purged/recreated, pg_search and pgvector have been installed, and the service has already been restarted.

   Fix: run this probe through stdin/a heredoc, as the script already does for the later `ALTER SYSTEM` block, and make the query fail closed:

   ```bash
   db_exists="$(
     sudo -u postgres psql -d postgres -tA -v ON_ERROR_STOP=1 -v app_db="$APP_DB" <<'SQL'
   SELECT 1 FROM pg_database WHERE datname = :'app_db';
   SQL
   )"
   ```

   Alternatively, keep `-c` only if the database name is quoted without psql colon syntax, but the heredoc version is the lowest-risk fix because it preserves the current SQL-safe `:'app_db'` quoting behavior for arbitrary `JURISEARCH_APP_DB` values.

## WARN

None.

## NIT

None.

VERDICT: FIXES_REQUIRED
