# Codex re-review (r2) — load-corpus.sh

## Scope
`/home/pierre/bear-storage/load-corpus.sh` (physical copy of a 165 G PG18 corpus over CT 110's 18/main).
Ground truth unchanged from r1 instructions (`codex-review-loadcorpus-instructions.md`).

This is **r2**. Your r1 review returned FIXES_REQUIRED with 3 BLOCKERs + 2 WARNs. Verify each is resolved
and no new hazard was introduced. This script overwrites the cluster data dir, so it is high-stakes.

## Fixes applied — verify each
1. **BLOCKER (stop hidden by `|| true`).** After `pg_ctlcluster stop || true`, the script now ASSERTS the
   cluster is down before any destructive write: `! pg_ctlcluster status` (status returns 0 if running)
   AND `! test -f $DATADIR/postmaster.pid`, dying otherwise. Confirm no destructive rsync can run against
   a live cluster.
2. **BLOCKER (unsafe bind-mount detection).** mp1 handling rewritten: if an `mp1:` line already exists it
   must contain BOTH `$STAGING,` and `mp=$MNT` (else die — never clobber a foreign mp1); if absent it
   sets mp1 and reboots. After that it verifies `$MNT/PG_VERSION` exists, equals 18, and that `$MNT` is
   genuinely read-only via a write-probe (`touch` must fail). Confirm this can't copy from a stale/wrong
   or writable mount, and won't overwrite an unrelated mp1.
3. **BLOCKER (only pg_search gated; vector not load-verified).** Pre-copy it now checks all four
   artifacts exist in the CT: pg_search.so, vector.so, pg_search.control, vector.control. Post-start it
   runs `SELECT '[1,2,3]'::vector;` (forces vector.so to load) and checks pg_search is present in the
   corpus catalog (the server only starts if pg_search.so preloads). Confirm this catches a broken/missing
   vector library that `\dx` alone would miss.
4. **WARN (external tablespaces / symlinked WAL).** Pre-copy now dies if `pg_tblspc` is non-empty or
   `pg_wal` is a symlink in the staging. Confirm this fails closed on the non-self-contained case.
5. **WARN (build-not-finished guard).** Now also dies if `systemctl is-active --quiet pgsearch-build`
   (build still running) in addition to checking the artifacts. Confirm this prevents rebooting the CT
   mid-build.

## Also confirm
- The previously-approved correct parts still hold: physical-copy validity, copying pg_wal of a clean
  cluster, Debian /etc config authority, auto.conf neutralisation, in-container chown ownership boundary,
  fail-closed structure, source-on-fedora untouched.
- No new quoting/logic bug in the rewritten mp1 block or the verification.

## Output
For the 5 findings: RESOLVED / PARTIALLY / NOT RESOLVED + one-line justification. List any new issues
(severity + concrete fix). End with exactly `VERDICT: GO` or `VERDICT: FIXES_REQUIRED`.
