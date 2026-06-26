# Codex re-review (r2) — pve9-upgrade.sh

## Scope
`/home/pierre/proxmox-upgrade/pve9-upgrade.sh`

This is **r2**. Your r1 review (`reviews/2026-06-26-pve9-upgrade-script-codex-review.md`) returned
FIXES_REQUIRED with 1 BLOCKER, 3 WARNs, 1 NIT. Confirm each is resolved and no regression was
introduced. Ground truth about bear is unchanged from the r1 instructions
(`codex-review-instructions.md`) — reuse it.

## Fixes applied — verify each
1. **BLOCKER (pve8to9 not an enforced gate).** Added `run_checklist()` which tees `pve8to9 --full` to a
   log and parses the `FAILURES:`/`WARNINGS:` summary into globals. `precheck` now prints the REAL exit
   code and parsed counts in its sentinel (no more forced `rc=0`). `phase_b` now re-runs the checklist
   as an **enforced gate**: aborts if the FAILURES count can't be parsed (fail-closed), aborts on any
   FAILURE, and requires an explicit `/root/pve9-precheck.ack` file when WARNINGS>0. Verify this is
   genuinely fail-closed and that the errexit/ERR-trap suspension around the checker is correct.
2. **WARN (unverified hetzner PVE mirror on trixie).** `phase_b` now disables the
   `mirror.hetzner.com/debian/pve` line before the suite rewrite, keeping the canonical
   `download.proxmox.com` PVE repo. Confirm the disable regex only targets that line and that the
   canonical PVE repo remains active and gets rewritten to trixie.
3. **WARN (missing non-free-firmware).** `phase_b` now adds `non-free-firmware` to active `*.debian.org`
   `deb` lines lacking it, scoped to Debian (never Proxmox). Confirm scoping and idempotence (won't
   double-add, won't touch deb-src or Proxmox lines).
4. **WARN (autoremove failures hidden).** Added `autoremove_step()` that captures the rc, logs a loud
   WARN on failure, and reports `autoremove=ok|failed(rc=N)` in the phase sentinel. (I chose
   surface-explicitly over fatal, deliberately: a cleanup hiccup after a *successful* dist-upgrade
   should not block rebooting an already-upgraded host. Confirm this is an acceptable realization of
   the finding, or argue why fatal is required.)
5. **NIT (grep guard missed deb-src-only).** The suite-rewrite guard is now
   `grep -qE '^[[:space:]]*deb(-src)?[[:space:]].*bookworm'`. Confirm it matches the sed predicate.

## Regression / validation already done locally
`bash -n` passes; `shellcheck` is clean; a fixture test confirmed the rewrite yields
`trixie`/`trixie-updates`/`trixie-security` + `non-free-firmware` on Debian lines, rewrites the
canonical Proxmox PVE line to trixie, comments the hetzner PVE mirror, and leaves commented lines alone.

## Output
For each of the 5 findings: RESOLVED / PARTIALLY / NOT RESOLVED + one-line justification. List any new
issues (severity + concrete fix). End with exactly `VERDICT: GO` or `VERDICT: FIXES_REQUIRED`.
