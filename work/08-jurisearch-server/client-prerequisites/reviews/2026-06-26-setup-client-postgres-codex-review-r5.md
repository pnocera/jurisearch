Summary:

The r4 blocker is fixed. `setup-client-postgres.sh:243-248` now runs the database-existence probe through stdin against the known `postgres` maintenance database, passes `-v ON_ERROR_STOP=1`, and quotes `APP_DB` via psql's `:'app_db'` interpolation in script input rather than in a `-c` SQL string. I verified the behavior with the installed `psql (PostgreSQL) 18.3`: the heredoc form returns `1` for an existing database name and parses quoted psql variables correctly, while the old `-c "… :'app_db' …"` form still reaches the server as literal colon syntax and fails with a syntax error. A source grep also found no remaining `psql -c` use with `:'var'`.

The destructive ordering remains sound: package resolution/install happens before `rm -rf "$PGDATA"`, the cluster purge is gated behind the package step, PostgreSQL is restarted after `shared_preload_libraries` and HBA changes, extension creation is checked under `ON_ERROR_STOP`, and the final TCP loopback password-auth smoke test is fail-closed. I re-ran `bash -n` and `shellcheck -S style`; both are clean.

## BLOCKER

None.

## WARN

1. `setup-client-postgres.sh:196-264` uses scripted `psql` invocations without `-X`, so a stale startup file for either the `postgres` system account or the invoking user can still perturb a noninteractive provisioning run.

   This is not a blocker for the stated fresh Fedora target because the `postgres` account normally has no `.psqlrc`, and the current failure-sensitive SQL paths otherwise use `ON_ERROR_STOP`. It is still a real scripting trap: the script purges `$PGDATA`, not `/var/lib/pgsql/.psqlrc`, and psql startup files can print output, override variables such as `ON_ERROR_STOP`, or prompt for input before the heredoc/`-c` command stream is processed.

   Fix: add `-X` to every noninteractive `psql` call, including the readiness probes, `ALTER SYSTEM` heredoc, database-existence heredoc, extension creation heredoc, extension-version reads, and final TCP-auth smoke test.

## NIT

None.

VERDICT: GO
