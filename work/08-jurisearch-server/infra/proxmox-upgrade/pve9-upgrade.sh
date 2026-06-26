#!/usr/bin/env bash
#
# pve9-upgrade.sh — staged Proxmox VE 8.4 -> 9.x upgrade helper for host "bear".
#
# Context (verified 2026-06-26 on bear):
#   - Single node, cluster "universe" now has 1 node (quorate). No HA, no ceph in use.
#   - PVE 8.4.1 (candidate 8.4.19) on Debian 12 bookworm, kernel 6.8.12-11-pve.
#   - APT repos: pve-no-subscription (download.proxmox.com + a mirror.hetzner.com mirror),
#     Debian bookworm (base/updates/security), plus third-party caddy (EXPIRED key) and tailscale.
#   - pve-no-subscription is declared twice (proxmox.list + pve-install-repo.list) -> apt warning.
#   - Two LXC guests 101 (nats1) and 107 (postgresql), both onboot:1 -> auto-start after reboot.
#   - Access is SSH over Tailscale; Hetzner console is available as out-of-band fallback.
#
# Design:
#   - Runs exactly ONE phase per invocation and NEVER reboots. The operator reboots between phases
#     and validates the node came back before continuing. A clean `reboot` gracefully stops/starts
#     onboot guests via pve-guests, so guests are not managed here.
#   - Non-interactive, keeping existing config files on conflict (--force-confold) so the remote
#     network/ssh config that carries our access is preserved across the upgrade.
#   - Fails closed: any error aborts before a reboot is ever suggested. Never reboot after a failure.
#   - phase-b enforces a clean `pve8to9 --full` gate (abort on FAILURES; WARNINGS require an explicit
#     operator acknowledgement file).
#   - Each phase prints a final SENTINEL line the operator/automation polls for.
#
# Usage:
#   ./pve9-upgrade.sh phase-a    # fully patch to latest 8.4.x (still Debian bookworm); fix apt repos
#   <reboot, reconnect, verify>
#   ./pve9-upgrade.sh precheck   # run pve8to9 --full (read-only readiness report) + parse summary
#   <resolve FAILURES; to accept WARNINGS: touch /root/pve9-precheck.ack>
#   ./pve9-upgrade.sh phase-b    # enforced pve8to9 gate, switch repos bookworm->trixie, upgrade to 9.x
#   <reboot, reconnect, verify; re-run pve8to9>
#
set -Eeuo pipefail

export DEBIAN_FRONTEND=noninteractive
APT=(apt-get -y -o Dpkg::Options::=--force-confdef -o Dpkg::Options::=--force-confold)
ACK_FILE=/root/pve9-precheck.ack

ts()  { date '+%Y-%m-%d %H:%M:%S'; }
log() { echo "[$(ts)] $*"; }
die() { echo "[$(ts)] FATAL: $*" >&2; echo "SENTINEL: ${PHASE:-?}-FAILED rc=1"; exit 1; }

arm_trap()  { trap 'die "unexpected error on line $LINENO"' ERR; }
disarm_trap() { trap - ERR; }
arm_trap

require_pve() { command -v pveversion >/dev/null 2>&1 || die "pveversion not found — not a PVE host"; }

pve_manager_version() {
  # e.g. "pve-manager/8.4.19/abc (running kernel: ...)" -> "8.4.19"
  pveversion 2>/dev/null | sed -n 's#.*pve-manager/\([0-9][0-9.]*\).*#\1#p' | head -1
}

backup_apt() {
  local b
  b="/root/apt-backup-$(date +%Y%m%d-%H%M%S)"
  mkdir -p "$b"
  cp -a /etc/apt/sources.list      "$b"/ 2>/dev/null || true
  cp -a /etc/apt/sources.list.d    "$b"/ 2>/dev/null || true
  log "Backed up current APT config to $b"
}

disable_deb_lines() {
  # Comment out every active (uncommented) deb/deb-src line in $1.
  local f="$1"
  [ -f "$f" ] || return 0
  sed -i -E 's/^([[:space:]]*)(deb(-src)?[[:space:]])/\1#\2/' "$f"
}

# Run pve8to9 --full, tee to a log, and parse the summary. Returns via globals
# CHECK_RC / CHECK_FAILS / CHECK_WARNS. errexit + ERR trap are suspended around the checker so a
# non-zero pve8to9 exit (it returns non-zero on warnings/failures) does not abort the script here.
run_checklist() {
  local logf="$1"
  disarm_trap; set +e
  pve8to9 --full 2>&1 | tee "$logf"
  CHECK_RC=${PIPESTATUS[0]}
  set -e; arm_trap
  # pve8to9 emits ANSI colour codes even through a pipe, so summary lines begin with an escape
  # sequence (e.g. ESC[0mFAILURES: 0). Strip ANSI SGR codes before parsing the summary counts.
  local plain
  plain=$(sed -E 's/\x1b\[[0-9;]*m//g' "$logf")
  CHECK_FAILS=$(printf '%s\n' "$plain" | sed -n 's/^[[:space:]]*FAILURES:[[:space:]]*\([0-9]\+\).*/\1/p' | tail -1)
  CHECK_WARNS=$(printf '%s\n' "$plain" | sed -n 's/^[[:space:]]*WARNINGS:[[:space:]]*\([0-9]\+\).*/\1/p' | tail -1)
}

# Run `apt-get --purge autoremove` without aborting the phase on failure, but record a loud warning.
# Rationale: a failed cleanup *after* a successful dist-upgrade should be surfaced, but it should not by
# itself block rebooting an already-successfully-upgraded host. The result is reported in the sentinel.
AUTOREMOVE_STATUS=ok
autoremove_step() {
  disarm_trap; set +e
  "${APT[@]}" --purge autoremove
  local rc=$?
  set -e; arm_trap
  if [ "$rc" != "0" ]; then
    AUTOREMOVE_STATUS="failed(rc=$rc)"
    log "WARN: autoremove failed (rc=$rc) — run 'dpkg --audit' and 'apt-get -f install' and review apt/dpkg state BEFORE rebooting."
  fi
}

# ---------------------------------------------------------------------------
phase_a() {
  PHASE="PHASE-A"
  require_pve
  log "PHASE A — patch to the latest 8.4.x (Debian bookworm)."
  backup_apt

  # 1) The Caddy repo's signing key is EXPIRED (EXPKEYSIG) and currently breaks `apt update`.
  #    Disable it so the package set resolves cleanly; it is unrelated to the PVE upgrade.
  if [ -f /etc/apt/sources.list.d/caddy-stable.list ]; then
    disable_deb_lines /etc/apt/sources.list.d/caddy-stable.list
    log "Disabled caddy repo (expired GPG key)."
  fi

  # 2) De-duplicate pve-no-subscription: it is declared in BOTH proxmox.list and
  #    pve-install-repo.list. Keep proxmox.list (curated, also has the hetzner mirror); disable the
  #    duplicate in pve-install-repo.list.
  if grep -qsE '^[[:space:]]*deb .*/pve .*pve-no-subscription' /etc/apt/sources.list.d/proxmox.list \
     && grep -qsE '^[[:space:]]*deb .*/pve .*pve-no-subscription' /etc/apt/sources.list.d/pve-install-repo.list; then
    disable_deb_lines /etc/apt/sources.list.d/pve-install-repo.list
    log "De-duplicated pve-no-subscription (disabled pve-install-repo.list)."
  fi

  log "apt-get update…"
  "${APT[@]}" update || die "apt update failed"
  log "apt-get dist-upgrade (8.4.x)…"
  "${APT[@]}" dist-upgrade || die "dist-upgrade failed — fix before rebooting"
  autoremove_step

  log "PHASE A complete — now on: $(pveversion | head -1)"
  if command -v pve8to9 >/dev/null 2>&1; then
    log "pve8to9 is available."
  else
    log "WARN: pve8to9 not present after the update — verify the 8.4.x update really applied."
  fi
  log "ACTION REQUIRED: reboot into the new kernel, reconnect, then run: $0 precheck"
  echo "SENTINEL: PHASE-A-DONE rc=0 autoremove=$AUTOREMOVE_STATUS"
}

# ---------------------------------------------------------------------------
precheck() {
  PHASE="PRECHECK"
  command -v pve8to9 >/dev/null 2>&1 || die "pve8to9 not found — run phase-a and reboot first"
  local logf=/root/pve8to9-precheck.log
  log "Running pve8to9 --full (read-only); logging to $logf …"
  run_checklist "$logf"
  log "pve8to9 exit=$CHECK_RC  FAILURES=${CHECK_FAILS:-unparsed}  WARNINGS=${CHECK_WARNS:-unparsed}"
  if [ "${CHECK_FAILS:-1}" != "0" ]; then
    log "Resolve all FAILURES (or fix summary parsing) before phase-b. Re-run precheck afterwards."
  elif [ "${CHECK_WARNS:-0}" != "0" ]; then
    log "No FAILURES, but ${CHECK_WARNS} WARNING(s). Review $logf; to accept them run: touch $ACK_FILE"
  else
    log "No FAILURES and no WARNINGS — phase-b may proceed."
  fi
  echo "SENTINEL: PRECHECK-DONE rc=$CHECK_RC fails=${CHECK_FAILS:-NA} warns=${CHECK_WARNS:-NA}"
}

# ---------------------------------------------------------------------------
phase_b() {
  PHASE="PHASE-B"
  require_pve
  command -v pve8to9 >/dev/null 2>&1 || die "pve8to9 missing — complete phase-a + reboot first"

  local ver; ver="$(pve_manager_version)"
  case "$ver" in
    8.4.*) log "On pve-manager $ver — proceeding to the major upgrade." ;;
    *)     die "expected pve-manager 8.4.x, found '$ver' — run phase-a first" ;;
  esac

  # ---- Enforced fail-closed checklist gate (official procedure requires a clean pve8to9 first) ----
  local logf=/root/pve8to9-phaseb.log
  log "Re-running pve8to9 --full as an enforced gate; logging to $logf …"
  run_checklist "$logf"
  [ -n "${CHECK_FAILS:-}" ] || die "could not parse pve8to9 FAILURES count from $logf — aborting (fail-closed)"
  [ "$CHECK_FAILS" = "0" ] || die "pve8to9 reports $CHECK_FAILS FAILURE(s) — resolve them before phase-b (see $logf)"
  if [ "${CHECK_WARNS:-0}" != "0" ]; then
    [ -f "$ACK_FILE" ] || die "pve8to9 reports $CHECK_WARNS WARNING(s) and no acknowledgement at $ACK_FILE — review $logf, then: touch $ACK_FILE"
    log "Proceeding with $CHECK_WARNS acknowledged WARNING(s) ($ACK_FILE present)."
  fi

  backup_apt
  log "PHASE B — switching repositories bookworm -> trixie and upgrading to PVE 9.x."

  # Prefer the canonical download.proxmox.com PVE repo. Disable the hetzner PVE mirror: its PVE-9 /
  # trixie availability is unverified, and depending on it would risk an apt-update failure after the
  # repo files are already rewritten. (The canonical download.proxmox.com PVE line stays active.)
  if grep -qsE '^[[:space:]]*deb .*mirror\.hetzner\.com/debian/pve ' /etc/apt/sources.list.d/proxmox.list; then
    sed -i -E '/^[[:space:]]*deb .*mirror\.hetzner\.com\/debian\/pve / s/^([[:space:]]*)/\1#/' /etc/apt/sources.list.d/proxmox.list
    log "Disabled hetzner PVE mirror (unverified for trixie); using canonical download.proxmox.com."
  fi

  # Tailscale: disable the repo for the duration of the upgrade. The installed tailscaled package
  # (which carries our SSH access) is NOT removed by disabling the repo; re-add a trixie repo after.
  if [ -f /etc/apt/sources.list.d/tailscale.list ]; then
    disable_deb_lines /etc/apt/sources.list.d/tailscale.list
    log "Disabled tailscale repo for the upgrade (tailscaled stays installed)."
  fi

  # Rewrite ACTIVE deb/deb-src lines mentioning 'bookworm' to 'trixie' across Debian base + Proxmox +
  # remaining hetzner repos (covers bookworm, -updates and -security suites). Then ensure the Debian
  # base/security repos carry the 'non-free-firmware' component (PVE 9 default for microcode). Caddy
  # (disabled in phase-a) and tailscale (disabled above) are skipped.
  local f
  for f in /etc/apt/sources.list /etc/apt/sources.list.d/*.list; do
    [ -e "$f" ] || continue
    case "$f" in
      */caddy-stable.list|*/tailscale.list) continue ;;
    esac
    if grep -qE '^[[:space:]]*deb(-src)?[[:space:]].*bookworm' "$f"; then
      sed -i -E '/^[[:space:]]*deb(-src)?[[:space:]]/ s/bookworm/trixie/g' "$f"
      log "Rewrote $f -> trixie"
    fi
    # Add non-free-firmware to active Debian (.debian.org) `deb` lines that lack it (scoped to Debian,
    # never to Proxmox repos).
    if grep -qE '^[[:space:]]*deb[[:space:]].*\.debian\.org' "$f"; then
      sed -i -E '/^[[:space:]]*deb[[:space:]].*\.debian\.org/ { /non-free-firmware/! s/[[:space:]]*$/ non-free-firmware/ }' "$f"
    fi
  done

  log "apt-get update (trixie)…"
  "${APT[@]}" update || die "apt update against trixie failed — inspect repositories, do NOT reboot"

  log "Major dist-upgrade to PVE 9 — this is the big one…"
  "${APT[@]}" dist-upgrade || die "major dist-upgrade failed — run 'apt-get -f install', do NOT reboot"
  autoremove_step

  log "PHASE B package upgrade complete — now reports: $(pveversion | head -1)"
  log "ACTION REQUIRED: reboot into the PVE 9 kernel, reconnect, then run: $0 precheck"
  log "After reboot: re-add a trixie tailscale repo if you want tailscale package updates."
  echo "SENTINEL: PHASE-B-DONE rc=0 autoremove=$AUTOREMOVE_STATUS"
}

# ---------------------------------------------------------------------------
main() {
  case "${1:-}" in
    phase-a)  phase_a ;;
    precheck) precheck ;;
    phase-b)  phase_b ;;
    *) echo "usage: $0 {phase-a|precheck|phase-b}" >&2; exit 2 ;;
  esac
}
main "$@"
