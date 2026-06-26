# Codex review — Proxmox VE 8.4 -> 9.x staged upgrade script for host "bear"

## Repo / scope
Review this single script:
`/home/pierre/proxmox-upgrade/pve9-upgrade.sh`

It will be copied to a remote Proxmox host ("bear") and run there, one phase per invocation, by an
operator who reboots and validates between phases. **This is a destructive, hard-to-reverse operation
on a remote box accessed only over SSH-via-Tailscale (Hetzner console is the fallback).** Review it as
a pre-execution gate: correctness and safety matter more than style.

## Ground truth about bear (verified 2026-06-26 — review the script against THIS reality)
- Single node; cluster "universe" now has 1 quorate node. No HA, **no ceph in use** (ceph.list lines
  are all commented).
- **PVE 8.4.1**, candidate **8.4.19**; Debian **12 bookworm**; running kernel 6.8.12-11-pve; uptime ~1yr.
- Disk: 1.9 TB free on `/`. `tmux`/`screen` are NOT installed (the operator will launch the script
  detached via `systemd-run`, not from inside this script).
- Active APT repos (from `/etc/apt/sources.list*`):
  - Debian: `deb http://deb.debian.org/debian bookworm main contrib`,
    `... bookworm-updates main contrib`,
    `deb http://security.debian.org/debian-security bookworm-security main contrib`.
  - Proxmox no-subscription: `deb http://download.proxmox.com/debian/pve bookworm pve-no-subscription`
    declared in BOTH `proxmox.list` (line 6) and `pve-install-repo.list` (line 1) -> "configured
    multiple times" apt warning.
  - A hetzner mirror in `proxmox.list`: `deb http://mirror.hetzner.com/debian/pve bookworm pve-no-subscription`
    and `deb http://mirror.hetzner.com/debian/security bookworm-security main contrib non-free`
    (hetzner-security-updates.list).
  - Third-party: `caddy-stable.list` (active `deb`+`deb-src`, **GPG key EXPIRED -> breaks apt update**)
    and `tailscale.list` (active `deb ... bookworm main`).
  - `pve-enterprise.list` and `pvetest-for-beta.list` are commented (inactive). `ceph.list` commented.
- Two LXC guests: 101 (nats1), 107 (postgresql), both `onboot:1`. A clean host `reboot` gracefully
  stops/starts them via `pve-guests`; the script intentionally does not manage guests.

## What to verify (against source + the official Proxmox "Upgrade from 8 to 9" procedure)
1. **Correctness of the repo edits.**
   - Does the phase-b `sed` (`/^[[:space:]]*deb(-src)?[[:space:]]/ s/bookworm/trixie/g`) correctly and
     ONLY rewrite active lines, leaving commented lines (e.g. ceph.list, pve-enterprise.list) untouched?
     Does it correctly produce `trixie`, `trixie-updates`, `trixie-security` from the bookworm suites?
   - Is skipping `caddy-stable.list` and `tailscale.list` in the loop correct given caddy is disabled
     in phase-a and tailscale is disabled separately in phase-b?
   - The phase-a de-dup disables `pve-install-repo.list` but leaves BOTH the proxmox.list download.proxmox
     line AND the hetzner mirror line active. Is that the right call, and will the hetzner mirror
     (`mirror.hetzner.com/debian/pve trixie pve-no-subscription`) plausibly exist for trixie — if not,
     phase-b `apt update` will fail closed; is that acceptable, or should the script prefer the
     canonical download.proxmox.com repo and/or warn?
2. **Adherence to the official 8->9 procedure.** Anything missing or out of order vs the Proxmox wiki:
   prerequisite of being on latest 8.4 first (script gates phase-b on `8.4.*` — correct?), running
   `pve8to9 --full`, the repo switch set (did it miss any repo that needs bookworm->trixie?), the
   `apt update && apt dist-upgrade` step, and rebooting. Should Debian's `non-free-firmware` component
   be added for microcode on trixie? Is `apt dist-upgrade` (vs `apt full-upgrade`/a minimal upgrade
   first) the right choice here?
3. **Bash safety.** `set -Eeuo pipefail` + the `ERR` trap + `die` interaction (does the trap fire
   correctly and not double-print? does `|| true` / `|| die` defeat the trap as intended?); the
   `pve_manager_version` sed extraction against real `pveversion` output; quoting; the `for f in ...*.list`
   glob when a glob has no match (`[ -e ] || continue` guard); whether `--force-confold` is the right
   conffile policy for preserving remote network/ssh access.
4. **Fail-closed guarantee.** Confirm there is no path where a dist-upgrade failure still leads the
   operator to reboot (the script never reboots, but check the messaging/sentinels make a failed phase
   unambiguous). Confirm a partial/interrupted `apt` won't be hidden.
5. **Anything that would brick remote access** (sshd/network config replacement, disabling a repo that
   removes the running tailscaled, etc.) — call it out as a BLOCKER.

## Output
For each finding: severity (BLOCKER / WARN / NIT), exact location (function/line), the problem, and a
concrete recommended fix — for every severity. Note what you checked and found correct. End with a
final line that is exactly `VERDICT: GO` or `VERDICT: FIXES_REQUIRED`.
