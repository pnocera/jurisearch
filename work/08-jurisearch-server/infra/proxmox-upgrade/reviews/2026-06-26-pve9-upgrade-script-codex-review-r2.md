# Code Review: `pve9-upgrade.sh` r2

Scope reviewed: `/home/pierre/proxmox-upgrade/pve9-upgrade.sh`

Review basis:

- R2 instructions: `/home/pierre/proxmox-upgrade/codex-review-instructions-r2.md`
- R1 instructions and bear ground truth: `/home/pierre/proxmox-upgrade/codex-review-instructions.md`
- R1 review: `/home/pierre/proxmox-upgrade/reviews/2026-06-26-pve9-upgrade-script-codex-review.md`
- Official Proxmox VE "Upgrade from 8 to 9" wiki: https://pve.proxmox.com/wiki/Upgrade_from_8_to_9
- Official Proxmox VE "Package Repositories" wiki: https://pve.proxmox.com/wiki/Package_Repositories

CodeGraph note: CodeGraph was requested by project instructions, but the MCP server reported that `/home/pierre/proxmox-upgrade` is not initialized. This review used direct source inspection of the single in-scope shell script.

Validation performed:

- `bash -n /home/pierre/proxmox-upgrade/pve9-upgrade.sh` passed.
- `shellcheck /home/pierre/proxmox-upgrade/pve9-upgrade.sh` passed.
- Read-only pipeline checks confirmed the Hetzner PVE disable expression comments only the active `mirror.hetzner.com/debian/pve` line, leaves the canonical `download.proxmox.com/debian/pve` line active, and allows the active canonical line to be rewritten from `bookworm` to `trixie`.
- Read-only pipeline checks confirmed the `non-free-firmware` addition affects active `.debian.org` `deb` lines, does not double-add when already present, does not touch `deb-src`, and does not touch the Proxmox PVE repository line.
- Read-only pipeline check confirmed the suite-rewrite guard now matches an active `deb-src ... bookworm` line.

## R1 Findings Rechecked

1. RESOLVED - BLOCKER: `pve8to9` is now an enforced phase-b gate. `phase_b` reruns `pve8to9 --full`, fails closed when `FAILURES` cannot be parsed, aborts on any parsed failure count above zero, and requires `/root/pve9-precheck.ack` when parsed warnings are nonzero; the checker runs with `errexit` and the `ERR` trap suspended only around the nonfatal checklist pipeline, then restores both before parsing and enforcing.

2. RESOLVED - WARN: the unverified Hetzner PVE mirror is disabled before the suite rewrite. The regex is scoped to active `deb` lines containing `mirror.hetzner.com/debian/pve`, so it does not touch the canonical `download.proxmox.com/debian/pve` entry or the separate Hetzner Debian security mirror; the canonical active PVE line remains eligible for the `bookworm` to `trixie` rewrite.

3. RESOLVED - WARN: active Debian `.debian.org` `deb` lines now get `non-free-firmware` idempotently. The sed block is scoped to active `deb` lines containing `.debian.org`, skips lines that already contain `non-free-firmware`, and does not touch `deb-src` or Proxmox repository lines.

4. RESOLVED - WARN: autoremove failures are no longer hidden. `autoremove_step` captures the cleanup exit code without aborting, logs a loud warning with explicit pre-reboot inspection commands, and reports `autoremove=ok` or `autoremove=failed(rc=N)` in the final phase sentinel. I accept the nonfatal handling here because it only runs after the critical update/dist-upgrade step has succeeded, and the sentinel gives automation a concrete failure field to inspect.

5. RESOLVED - NIT: the suite-rewrite grep guard now matches the sed predicate. It uses `deb(-src)?`, so a file containing only active `deb-src ... bookworm` lines will no longer be skipped.

## New Issues

None found.

## Additional Checks Found Correct

- The phase-b repo switch still leaves commented lines untouched because the rewrite sed only applies to active `deb` or `deb-src` lines.
- The rewritten bear Debian suites are the expected `trixie`, `trixie-updates`, and `trixie-security`.
- Skipping `caddy-stable.list` and `tailscale.list` in the suite rewrite remains correct for this staged script: phase-a disables Caddy because its expired key breaks `apt update`, and phase-b disables the Tailscale repository without removing the installed `tailscaled` package.
- The official order is preserved: update to the latest PVE 8.4 package set, run `pve8to9 --full`, switch repositories to Trixie/PVE 9, run `apt update`, run `apt dist-upgrade`, then reboot manually only after success.
- The fail-closed upgrade path is intact: a failed `apt update` or `apt dist-upgrade` calls `die`, emits a failed sentinel, and does not reach the reboot instruction.
- `--force-confold` remains a defensible noninteractive policy for this host because preserving existing SSH, network, and bootloader config is the safer remote-access default; the operator should still review `.dpkg-dist` files after the upgrade.

VERDICT: GO
