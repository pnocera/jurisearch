Summary:

The r2 package-safety restructure is correct for the reported blocker: the current script no longer purges `/var/lib/pgsql/data` until after `postgresql-contrib` has been removed, `dnf install --best` has settled the PostgreSQL stack/build dependencies, `/usr/bin/pg_config` exists, and the installed major has been asserted. On this fc45 host, `dnf install --best ...` fails while `postgresql-contrib-18.3-3` remains installed, for exactly the expected version-lock/Python conflict; the equivalent dry-run with an explicit `postgresql-contrib` remove action resolves, installs `postgresql-server-devel` plus `postgresql-private-devel`, upgrades the PG stack to `18.3-7`, and does not pull `postgresql-contrib` or `libpq-devel`. The service-stop, preflight major check, and `APP_DB` option-terminator fixes from r2 are also present. `bash -n` and `shellcheck -S style` are clean.

The remaining issue is independent of the r2 package transaction: `pg_search`'s extension SQL/control files are copied out of the user's pgrx staging tree with archive semantics, so the system install can preserve user ownership and SELinux `user_home_t` labels into `/usr/share/pgsql/extension`. On this SELinux-enforcing Fedora host, that is a real correctness risk for `CREATE EXTENSION pg_search` and a violation of the intended install-as-root split.

## BLOCKER

1. `sudo cp -a` can install `pg_search` extension files into `/usr/share/pgsql/extension` with the normal user's ownership and SELinux home labels.

   Evidence: `setup-client-postgres.sh:172-175` installs the `.so` with `sudo install -D -m 0755`, but then copies the staged extension directory with `sudo cp -a "$EXT_SRC_DIR/." "$SHARE/extension/"`. GNU `cp -a` is `--preserve=all`, which includes ownership and SELinux context when run as root. The local staged pgrx files are currently owned/labeled as user-home artifacts, for example `pierre pierre 644 .../pg_search.control` and `unconfined_u:object_r:user_home_t:s0 .../pg_search.control`; the target extension directory is labeled `system_u:object_r:usr_t:s0`, and `matchpathcon /usr/share/pgsql/extension/pg_search.control` also expects `usr_t`. The system PostgreSQL service runs under `postgresql_t` with SELinux enforcing.

   This can make the later `CREATE EXTENSION pg_search` fail even though the build and file copy succeeded, and it does so after the old cluster has already been purged. It also leaves user-writable files under a system extension directory, which is not the intended build-as-user/install-as-root boundary.

   Fix: install the control/SQL files with explicit root ownership and default system labels instead of preserving build-tree attributes. For example, replace the archive copy with an `install`-based copy and relabel the installed artifacts:

   ```bash
   sudo install -d -o root -g root -m 0755 "$SHARE/extension"
   find "$EXT_SRC_DIR" -maxdepth 1 -type f \( -name 'pg_search.control' -o -name 'pg_search--*.sql' \) \
     -exec sudo install -o root -g root -m 0644 -t "$SHARE/extension" {} +
   if command -v restorecon >/dev/null; then
     sudo restorecon "$PKGLIB/pg_search.so" "$SHARE/extension"/pg_search*
   fi
   ```

   Equivalent `cp` logic is fine if it explicitly avoids preserving ownership/context and then forces `root:root` plus the default SELinux context. Be careful with overwrites from a prior bad run: simply dropping `-a` is not enough if an existing destination file is already user-owned.

## WARN

None.

## NIT

None.

VERDICT: FIXES_REQUIRED
