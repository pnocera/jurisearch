# Codex re-review (r2) — build-pg-search.sh

## Scope
`/home/pierre/bear-storage/build-pg-search.sh` (builds + installs ParadeDB pg_search for system PG17
inside CT 110). Ground truth unchanged from r1 instructions
(`codex-review-build-pgsearch-instructions.md`).

This is **r2**. Your r1 review (`reviews/2026-06-26-build-pg-search-codex-review.md`) returned
FIXES_REQUIRED with 2 BLOCKERs + 2 WARNs. Confirm each is resolved and no regression introduced. The
build is ~20-40 min, so this gate matters.

## Fixes applied — verify each
1. **BLOCKER (pgrx not initialized → install fails before the long build).** Added a step 2b that runs
   `cargo pgrx init "--pg${PGVER}=${PG_CONFIG}"` (i.e. `cargo pgrx init --pg17=/usr/lib/postgresql/17/bin/pg_config`)
   AFTER installing cargo-pgrx and BEFORE `cargo pgrx install`. Confirm this registers the *existing*
   system pg_config (no PG download/build) and satisfies `Pgrx::from_config()` so the subsequent
   `cargo pgrx install --pg-config ...` works, and that the flag form `--pg17=<path>` is correct for
   cargo-pgrx 0.18.1.
2. **BLOCKER (smoke not fail-closed).** The smoke now runs
   `psql -v ON_ERROR_STOP=1 -d ext_smoke -f /tmp/ext_smoke.sql`, so a failed `CREATE EXTENSION pg_search`
   aborts the script (ERR trap → PGSEARCH-FAILED) instead of reaching PGSEARCH-DONE. Confirm.
3. **WARN (shared_preload_libraries overwrite).** Now reads the current value, leaves it unchanged if
   `pg_search` is already present, sets `pg_search` if empty, else appends `,pg_search` — then
   `ALTER SYSTEM SET` + restart. (Locally tested: ``→`pg_search`; `pg_stat_statements`→
   `pg_stat_statements,pg_search`; idempotent when already present.) Confirm correctness and idempotency.
4. **WARN (prefix version match).** The cargo-pgrx check now uses `grep -qx "cargo-pgrx $PGRX_VERSION"`
   (exact line) so `0.18.10` no longer satisfies a `0.18.1` pin. Confirm.

## Validation already done locally
`bash -n` passes; `shellcheck` clean; the spl-merge branch logic was unit-tested for empty / other-lib /
already-present / both cases.

## Also confirm no regression in the parts you already approved
- feature set `--no-default-features --features pg17,deferred_wal`, install-into-system-PG mechanics,
  pgrx-version extraction, preload→restart→CREATE EXTENSION ordering, pgvector-no-preload.

## Output
For each of the 4 findings: RESOLVED / PARTIALLY / NOT RESOLVED + one-line justification. List any new
issues (severity + concrete fix). End with exactly `VERDICT: GO` or `VERDICT: FIXES_REQUIRED`.
