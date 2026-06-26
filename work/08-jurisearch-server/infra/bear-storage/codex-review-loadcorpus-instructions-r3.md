# Codex re-review (r3) — load-corpus.sh

## Scope
`/home/pierre/bear-storage/load-corpus.sh` (physical copy of a 165 G PG18 corpus over CT 110's 18/main).
Ground truth unchanged. r1: 3 BLOCKER + 2 WARN (fixed). r2: those resolved but found 1 new BLOCKER + 1
WARN about reading the staging through the unprivileged bind mount. This is **r3** — confirm those two.

## r2 fixes applied — verify each
1. **BLOCKER (dropped `chmod -R a+rX "$STAGING"` regression).** Restored: step 1 now runs
   `chmod -R a+rX "$STAGING"` on the host BEFORE adding the bind mount, so the container's mapped root
   can read PostgreSQL's 0700 dirs / 0600 files through the read-only mount. Additionally, after the
   mount is verified, the script now proves a DEEP 0600 file is readable inside the CT:
   `ctexec test -r "$MNT/global/pg_control"` (dies otherwise) — so an unreadable tree is caught BEFORE
   the destructive stop+rsync, not after. Confirm this closes the "rsync fails after delete -> empty
   data dir" hole.
2. **WARN (touch write-probe not a reliable read-only check).** Replaced: the read-only check now uses
   `findmnt -rno OPTIONS "$MNT"` and requires a comma-delimited `ro` token (`grep -qE ',ro,'` after
   wrapping in commas), and the reuse-existing-mp1 path now additionally requires `ro=1` in the config
   line. Confirm this reliably proves read-only and that the `,ro,` match can't false-positive on
   another option (e.g. `errors=remount-ro` is not present on a bind mount; `relatime` etc. don't
   contain a standalone `ro` token).

## Also confirm (no regression in previously-approved logic)
- stop-asserted-before-rsync (status nonzero + no postmaster.pid), the four-artifact pre-copy gate,
  the `'[1,2,3]'::vector` functional load + pg_search catalog check, external-tablespace/symlinked-WAL
  rejection, the build-active guard, auto.conf neutralisation, in-container chown boundary, source-on-
  fedora untouched, and overall fail-closed structure.
- No new quoting/logic issues from the edits.

## Output
For the 2 r2 findings: RESOLVED / PARTIALLY / NOT RESOLVED + one-line justification. List any new issues
(severity + concrete fix). End with exactly `VERDICT: GO` or `VERDICT: FIXES_REQUIRED`.
