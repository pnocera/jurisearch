# Codex re-review (r2) — tune-pg18.sh

## Scope
`work/08-jurisearch-server/infra/bear-storage/tune-pg18.sh` (server tuning + IVFFlat rebuild, in CT 110).
Ground truth unchanged from r1 instructions. This is **r2**; r1 returned FIXES_REQUIRED with 3 WARN + 1 NIT.

## Fixes applied — verify each
1. **WARN (persistent work_mem too aggressive + autovacuum inherits maintenance_work_mem).** Persistent
   `work_mem` is now `128MB` (env-overridable). Persistent `maintenance_work_mem` lowered to `2GB`, and
   `ALTER SYSTEM SET autovacuum_work_mem = '1GB'` added so autovacuum workers don't each reserve the big
   ceiling. The 16 GB is now ONLY a `SET LOCAL` inside the index-build transaction (`BUILD_MAINT_WORK_MEM`).
   Confirm this removes the OOM/autovacuum concern while still giving the IVFFlat build 16 GB.
2. **WARN (rebuild not atomic).** The DROP+CREATE is now wrapped in `BEGIN; SET LOCAL …; DROP INDEX IF
   EXISTS …; CREATE INDEX …; COMMIT;` so a failed CREATE rolls back the DROP and keeps the old index.
   ANALYZE moved to a separate post-commit statement. Confirm the transaction is correct (non-concurrent
   CREATE INDEX is allowed in a txn; SET LOCAL scopes to the txn) and that it now fails closed for index
   availability.
3. **WARN (verify bypassed ON_ERROR_STOP).** The verify now runs through `psqlf "$DB"` (which sets
   `-v ON_ERROR_STOP=1`), captured into `verify_out=$(…)`; a SQL error makes psqlf non-zero → the
   command-substitution assignment fails → `set -e`/ERR trap → die, BEFORE the success sentinel. The
   `grep` only formats the already-captured output. Confirm it's fail-closed now.
4. **NIT (su -c shell-string).** Helpers `psqlf`/`psqlc` now use `runuser -u postgres -- psql … "$1"
   -tAc "$2"` (argv-style, no shell parsing of the DB name or SQL; also drops the login-shell locale
   banner). Confirm argv-style is correct and that `runuser … -- psql -f -` still forwards the heredoc
   stdin.

## Also confirm
- The previously-approved-correct items still hold: shared_buffers=48GB, effective_cache_size=160GB,
  the lists heuristic (2154), vector_l2_ops, the restart ordering.
- No new quoting/logic regression from the rewrite. `bash -n` + `shellcheck` are clean locally.

## Output
For the 4 findings: RESOLVED / PARTIALLY / NOT RESOLVED + one-line justification. New issues with
severity + fix. End with exactly `VERDICT: GO` or `VERDICT: FIXES_REQUIRED`.
