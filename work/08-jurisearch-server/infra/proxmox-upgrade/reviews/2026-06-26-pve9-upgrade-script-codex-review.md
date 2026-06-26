# Code Review: `pve9-upgrade.sh`

Scope reviewed: `/home/pierre/proxmox-upgrade/pve9-upgrade.sh`

Reference material checked:

- Proxmox VE official "Upgrade from 8 to 9" wiki, retrieved 2026-06-26.
- Proxmox VE official "Package Repositories" wiki, retrieved 2026-06-26.
- Bear-specific ground truth from `codex-review-instructions.md`.
- Static script inspection with line numbers, `bash -n`, `shellcheck`, and a direct sed sample for the phase-b repo rewrite expression.

CodeGraph note: the project has CodeGraph instructions, but CodeGraph is not initialized in `/home/pierre/proxmox-upgrade`, so this review used direct source inspection.

## Findings

### BLOCKER: `phase-b` can run without an acknowledged clean `pve8to9 --full` result

Location: `precheck` lines 103-110 and `phase_b` lines 114-123.

Problem: the official procedure says to run `pve8to9 --full` before the upgrade and to re-run it after fixing issues. The script exposes a `precheck` phase, but `precheck` masks every non-zero exit from `pve8to9 --full` with `|| true` and always emits `SENTINEL: PRECHECK-DONE rc=0`. `phase_b` only checks that `pve8to9` exists and that `pve-manager` is `8.4.*`; it does not require that `precheck` was run, that its output was reviewed, or that any `FAIL` entries were resolved.

For this remote, destructive upgrade, that is not fail-closed enough: an operator or wrapper that keys off sentinels can proceed to the major `dist-upgrade` after a checklist failure or without having run the checklist at all. Some `pve8to9` findings are exactly the kind of pre-reboot or storage/bootloader issues that should stop this plan before package replacement begins.

Recommended fix: make the checklist an enforced gate before `phase-b`. A practical noninteractive pattern is:

- Have `precheck` tee output to a fixed log under `/root`, capture the exit code, and print the actual code in the sentinel.
- Abort `phase-b` unless an explicit operator-created acknowledgement exists for that exact precheck log, or rerun `pve8to9 --full` inside `phase-b` and abort on `FAIL` while requiring an explicit acknowledgement for accepted `WARN` entries.
- Do not print a successful `PRECHECK-DONE rc=0` when the checker returned non-zero.

### WARN: the phase-b repo rewrite keeps an unverified Hetzner Proxmox mirror active for Trixie

Location: `phase_a` lines 77-84 and `phase_b` lines 128-140.

Problem: phase-a disables `pve-install-repo.list` but intentionally keeps both active PVE no-subscription entries in `proxmox.list`: the canonical `download.proxmox.com` entry and `mirror.hetzner.com/debian/pve`. Phase-b then blindly rewrites active `bookworm` lines to `trixie`, producing a Hetzner PVE `trixie pve-no-subscription` source.

The official PVE 9 no-subscription repository is `http://download.proxmox.com/debian/pve` with suite `trixie` and component `pve-no-subscription`. I could verify the canonical `download.proxmox.com` Trixie Release file. I could not verify the Hetzner PVE mirror endpoint from this environment; both Bookworm and Trixie Release checks timed out. If that mirror does not publish PVE 9/Trixie metadata, phase-b fails at `apt update` after the script has already rewritten repo files and disabled the Tailscale repo. That is fail-closed before `dist-upgrade`, but it is avoidable operator work on a remote box.

Recommended fix: for phase-b, prefer one known-good PVE repository. Disable the Hetzner PVE mirror before switching suites unless it has been verified on bear immediately before the run, or create the official deb822 `/etc/apt/sources.list.d/proxmox.sources` entry for `download.proxmox.com` and disable old `.list` PVE entries.

### WARN: Debian `non-free-firmware` is not added for the PVE 9/Trixie base repos

Location: `phase_b` lines 128-140.

Problem: bear's active Debian lines are `main contrib`, and the script only rewrites `bookworm` to `trixie`; it does not add `non-free-firmware`. The official PVE 9 package repository page shows Trixie base repositories with `Components: main non-free-firmware` and separately states that PVE 9 enables the firmware component by default for new installations to provide early OS microcode updates.

This is not likely to break the package upgrade itself, and `pve-firmware` still covers many runtime firmware files. It does leave the upgraded host without the PVE 9 default firmware component and can prevent CPU microcode packages from being installed or updated through the standard Debian component.

Recommended fix: before `apt update` in phase-b, add `non-free-firmware` to active `.debian.org` Debian base/security lines that do not already include it. Keep this scoped to Debian repositories, not Proxmox PVE repositories.

### WARN: `autoremove` failures are hidden behind a successful phase sentinel

Location: `phase_a` line 90 and `phase_b` line 155.

Problem: both phases run `apt-get --purge autoremove || true`. If this cleanup apt transaction fails, the script still prints the phase success sentinel and, in phase-b, proceeds to the reboot instruction. A failed autoremove after a successful `dist-upgrade` is much less serious than a failed `dist-upgrade`, but it can still indicate dpkg/apt state that the operator should inspect before rebooting a remote host.

Recommended fix: either make autoremove fatal like the other apt operations, or capture and report its failure explicitly in the final phase output, for example `WARN: autoremove failed; inspect apt/dpkg state before reboot`. For this host, making it fatal is the cleaner pre-execution-gate choice.

### NIT: the phase-b pre-sed grep guard misses deb-src-only files

Location: `phase_b` line 137.

Problem: the actual sed expression handles both `deb` and `deb-src`, but the guard that decides whether to run it only matches `^[[:space:]]*deb .*bookworm`. A `.list` file containing only active `deb-src ... bookworm` would be skipped.

This does not affect the stated bear repo set: the active Debian/PVE lines are `deb`, Caddy is intentionally skipped and disabled, and Tailscale is disabled separately.

Recommended fix: change the guard to match the sed predicate, for example `grep -qE '^[[:space:]]*deb(-src)?[[:space:]].*bookworm' "$f"`.

## Checked And Found Correct

- The phase-b sed expression itself only rewrites active `deb`/`deb-src` lines. A sample confirmed it leaves commented PVE/Ceph lines untouched and converts `bookworm`, `bookworm-updates`, and `bookworm-security` to `trixie`, `trixie-updates`, and `trixie-security`.
- Skipping `caddy-stable.list` and `tailscale.list` in the phase-b rewrite is correct for the intended phase order: Caddy is disabled in phase-a because its expired key breaks `apt update`, and Tailscale is disabled separately without removing the installed `tailscaled` package.
- The script uses the official `apt update` then `apt dist-upgrade` sequence for both the latest 8.4 patching step and the PVE 9 upgrade step. `apt-get dist-upgrade` is an appropriate noninteractive spelling of the official `apt dist-upgrade` action.
- The phase-b `8.4.*` gate matches the official requirement that the host be on the PVE 8.4 line before upgrading. Phase-a's `dist-upgrade` should get bear from 8.4.1 to the current 8.4 candidate first.
- The `pve_manager_version` extraction matches the documented `pveversion` shape such as `pve-manager/8.4.19/...`.
- `set -Eeuo pipefail`, the `ERR` trap, and explicit `|| die` around `apt update` / `dist-upgrade` do not double-print in the intentional failure paths. A failed phase-critical apt update or dist-upgrade emits a `FAILED` sentinel and does not reach the reboot instruction.
- The glob loop over `/etc/apt/sources.list.d/*.list` is guarded by `[ -e "$f" ] || continue`, so an unmatched glob is harmless.
- The script never calls `reboot`. On a phase-b `apt update` or `dist-upgrade` failure, the messages explicitly say not to reboot.
- `--force-confold` is a defensible policy for this specific remote-access context because it preserves existing SSH, network, and bootloader config files by default. The tradeoff is that the operator should review conffile deltas after the upgrade, especially `lvm.conf`, `sshd_config`, `chrony.conf`, and `grub`.
- Disabling the Tailscale repository does not remove the installed `tailscaled` package. This should not by itself drop the running remote access path; the remaining risk is ordinary service/package restart behavior during the major upgrade, which the systemd-run execution model is intended to tolerate.

VERDICT: FIXES_REQUIRED
