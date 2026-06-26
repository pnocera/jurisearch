#!/usr/bin/env bash
#
# bear-disk-merge.sh — reclaim the empty /home partition into / on host "bear".
#
# Goal: turn the awkward "2T / + 1.5T /home" split on the single 3.84 TB NVMe into ONE ~3.5 TB /,
# so the PVE `local` storage spans the whole disk (removing the 1.5 TB DB ceiling). The `home`
# PVE storage is removed as part of this.
#
# Geometry (verified 2026-06-26 on /dev/nvme0n1, GPT, legacy BIOS boot):
#   p5  1M    BIOS-boot (EF02)  start 2048         <- first on disk, UNTOUCHED
#   p1  4G    swap              start 4096         <- UNTOUCHED
#   p2  1G    /boot ext3        start 8392704      <- UNTOUCHED
#   p3  2T    / ext4            start 10489856     <- GROWN to fill the disk
#   p4  1.5T  /home ext4        start 4234153984   <- LAST partition, DELETED
# p4 is physically the last partition, immediately after p3, so deleting p4 and growing p3 into the
# freed contiguous space is a clean online grow. Swap, /boot and BIOS-boot are never touched.
#
# Safety posture:
#   - NEVER reboots.
#   - Fails closed (set -Eeuo pipefail + ERR trap). ALL tooling checks/installs and ALL guards run
#     BEFORE any destructive step, so a missing tool / apt failure aborts before fstab/partition edits.
#   - Backs up the GPT (sgdisk --backup), /etc/fstab, AND a text snapshot of disk/mount/storage state
#     before editing anything (for console recovery).
#   - Refuses unless: root on a PVE host; p4 is genuinely the LAST partition AND is the ext4 /home
#     mounted from /dev/nvme0n1p4; / is /dev/nvme0n1p3; and /home uses < HOME_MAX_MB (real-data guard).
#   - fstab edit is field-aware (only the line whose mountpoint is exactly /home) and atomic, and runs
#     BEFORE deleting the partition (so a later boot can't hang on a missing device).
#   - After deleting p4 it HARD-verifies the kernel forgot it before growing p3.
#   - Hetzner console is the out-of-band fallback if anything about the boot disk goes wrong.
#
# Usage: ./bear-disk-merge.sh         (does the merge; no arguments)
#
set -Eeuo pipefail

DISK=/dev/nvme0n1
ROOT_PART=/dev/nvme0n1p3
HOME_PART=/dev/nvme0n1p4
ROOT_PART_NUM=3
HOME_PART_NUM=4
HOME_MAX_MB=1024               # refuse if /home uses HOME_MAX_MB MiB or more (strict real-data guard)
MIN_EXPECTED_ROOT_MB=3000000   # after grow, / should be > ~3 TB

ts(){ date '+%Y-%m-%d %H:%M:%S'; }
log(){ echo "[$(ts)] $*"; }
die(){ echo "[$(ts)] FATAL: $*" >&2; echo "SENTINEL: MERGE-FAILED rc=1"; exit 1; }
trap 'die "unexpected error on line $LINENO"' ERR

# ==================================================================================================
# PHASE A — non-destructive: tooling, guards, backups. Anything here may abort with NOTHING changed.
# ==================================================================================================

# ---- 0. identity / host preconditions ----
[ "$(id -u)" = 0 ]            || die "must run as root"
command -v pvesm >/dev/null   || die "pvesm not found — not a PVE host"
[ -b "$HOME_PART" ]           || die "$HOME_PART is not a block device"
mountpoint -q /home           || die "/home is not a mountpoint (refusing to guess state)"
[ "$(findmnt -no SOURCE /home)" = "$HOME_PART" ] || die "/home is not mounted from $HOME_PART"
[ "$(findmnt -no FSTYPE  /home)" = "ext4" ]      || die "/home is not ext4 (unexpected layout)"
[ "$(findmnt -no SOURCE  /)"     = "$ROOT_PART" ] || die "/ is not $ROOT_PART (unexpected layout)"

# ---- 1. ensure ALL tools are present BEFORE any destructive step (fail-closed) ----
command -v sgdisk    >/dev/null || die "sgdisk (gdisk) not found"
command -v partx     >/dev/null || die "partx (util-linux) not found"
command -v resize2fs >/dev/null || die "resize2fs (e2fsprogs) not found"
command -v blockdev  >/dev/null || die "blockdev (util-linux) not found"
if ! command -v growpart >/dev/null 2>&1; then
  log "growpart missing — installing cloud-guest-utils now (before any destructive step)…"
  DEBIAN_FRONTEND=noninteractive apt-get -y install cloud-guest-utils
fi
command -v growpart >/dev/null || die "growpart still unavailable after install — aborting before any change"
command -v partprobe >/dev/null || log "note: partprobe (parted) absent — will rely on partx"
log "tooling present: sgdisk, partx, resize2fs, blockdev, growpart"

# ---- 2. verify p4 is the LAST partition on the disk (highest start sector) ----
last_partnum=$(sgdisk -p "$DISK" | awk '/^[[:space:]]*[0-9]+[[:space:]]/ {print $2, $1}' | sort -n | tail -1 | awk '{print $2}')
[ "$last_partnum" = "$HOME_PART_NUM" ] || die "partition $HOME_PART_NUM is not the last on disk (last is p$last_partnum) — aborting"
log "confirmed: p$HOME_PART_NUM (/home) is the last partition on $DISK"

# ---- 3. verify /home is essentially empty (strict guard against destroying real data) ----
home_mb=$(du -sxm /home 2>/dev/null | awk '{print $1}'); home_mb=${home_mb:-999999}
[ "$home_mb" -lt "$HOME_MAX_MB" ] || die "/home uses ${home_mb} MiB (>= ${HOME_MAX_MB}) — refusing to delete; inspect first"
log "/home uses ${home_mb} MiB (< ${HOME_MAX_MB}) — safe to reclaim"

# ---- 4. backups + recovery snapshot (BEFORE any change) ----
stamp=$(date +%Y%m%d-%H%M%S)
gpt_backup="/root/gpt-backup-${stamp}.bin"
sgdisk --backup="$gpt_backup" "$DISK"
cp -a /etc/fstab "/root/fstab-backup-${stamp}"
{ echo "### date"; date; echo; echo "### sgdisk -p $DISK"; sgdisk -p "$DISK";
  echo; echo "### lsblk -f"; lsblk -f "$DISK"; echo; echo "### blkid"; blkid;
  echo; echo "### findmnt"; findmnt; echo; echo "### pvesm status"; pvesm status;
} > "/root/bear-disk-merge-state-${stamp}.txt" 2>&1
log "backed up: GPT -> $gpt_backup ; fstab -> /root/fstab-backup-${stamp} ; state -> /root/bear-disk-merge-state-${stamp}.txt"
log "RECOVERY NOTE: 'sgdisk --load-backup=$gpt_backup $DISK' is ONLY safe BEFORE resize2fs grows root."
log "              After the filesystem is grown, restoring the smaller GPT can corrupt root — use console + a fresh plan instead."

# ==================================================================================================
# PHASE B — destructive. From here, changes are made; every step still fails closed before the next.
# ==================================================================================================

# ---- 5. remove the PVE `home` storage (if present) so pvestatd stops touching /home ----
if pvesm status --storage home >/dev/null 2>&1; then
  pvesm remove home
  log "removed PVE storage 'home'"
fi

# ---- 6. drop the active /home entry from fstab (field-aware, atomic) BEFORE deleting the partition --
fstab_tmp=$(mktemp /etc/fstab.bear-merge.XXXXXX)
awk '
  /^[[:space:]]*#/ { print; next }                                   # keep full-line comments
  ($2 == "/home")  { print "#bear-disk-merge: reclaimed /home -> " $0; next }   # comment the /home mount
  { print }
' /etc/fstab > "$fstab_tmp"
# sanity: the root entry must still be present, and no active /home entry may remain
awk '!/^[[:space:]]*#/ && $2=="/"      {f=1} END{exit f?0:1}' "$fstab_tmp" || { rm -f "$fstab_tmp"; die "fstab rewrite lost the / entry — aborting (original untouched)"; }
awk '!/^[[:space:]]*#/ && $2=="/home"  {f=1} END{exit f?1:0}' "$fstab_tmp" || { rm -f "$fstab_tmp"; die "fstab rewrite still has an active /home entry — aborting"; }
chmod --reference=/etc/fstab "$fstab_tmp"; chown --reference=/etc/fstab "$fstab_tmp"
mv -f "$fstab_tmp" /etc/fstab
log "commented the active /home entry in /etc/fstab (field-aware, atomic; / entry preserved)"

# ---- 7. unmount /home (fail loudly if busy) ----
sync
if ! umount /home; then
  log "umount /home failed; processes holding it:"; fuser -vm /home 2>&1 || true
  die "could not unmount /home — resolve holders and re-run"
fi
log "/home unmounted"

# ---- 8. delete p4, then HARD-verify the kernel forgot it before growing ----
sgdisk -d "$HOME_PART_NUM" "$DISK"
log "deleted p$HOME_PART_NUM from on-disk GPT"
if ! partx -d --nr "$HOME_PART_NUM" "$DISK" 2>/dev/null; then
  partprobe "$DISK" 2>/dev/null || die "kernel still holds p$HOME_PART_NUM after GPT delete (partx and partprobe both failed) — aborting before grow"
fi
if [ -b "$HOME_PART" ] && blockdev --getsz "$HOME_PART" >/dev/null 2>&1; then
  die "kernel still reports $HOME_PART after delete — aborting before grow (resolve via console)"
fi
log "kernel no longer reports p$HOME_PART_NUM — free tail space confirmed"

# ---- 9. grow p3 into the freed space (growpart handles the mounted root partition) ----
growpart "$DISK" "$ROOT_PART_NUM"
log "grew partition p$ROOT_PART_NUM"

# ---- 10. online-resize the root filesystem ----
resize2fs "$ROOT_PART"
log "resized root ext4 filesystem"

# ---- 11. verify ----
root_mb=$(df -BM --output=size / | awk 'NR==2{gsub(/M/,"",$1); print $1}')
[ "${root_mb:-0}" -ge "$MIN_EXPECTED_ROOT_MB" ] || die "root grew to ${root_mb} MiB but expected >= ${MIN_EXPECTED_ROOT_MB} — verify manually"
root_summary=$(df -h / | awk 'NR==2{print $2" total, "$4" free"}')
log "root filesystem now: $root_summary"
log "PVE storage 'local' now:"
pvesm status --storage local || true
echo "SENTINEL: MERGE-DONE rc=0"
