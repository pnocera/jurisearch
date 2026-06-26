# Codex review — bear-disk-merge.sh (live root-disk partition merge)

## Scope
Review this single script: `/home/pierre/bear-storage/bear-disk-merge.sh`

It will run on a **remote Proxmox VE 9 host ("bear"), over SSH-via-Tailscale, on the LIVE boot disk**.
It deletes the empty `/home` partition and grows the **mounted root** partition into the freed space.
This is destructive and hard to reverse — review it as a pre-execution safety gate. A Hetzner console
is available as out-of-band fallback. The script must NEVER reboot.

## Ground truth about bear (verified 2026-06-26 — review the script against THIS)
- Disk `/dev/nvme0n1`, **GPT**, **legacy BIOS boot** (grub via a 1 MiB EF02 BIOS-boot partition).
- Partitions by **start sector** (sgdisk -p):
  - p5  start 2048        1 MiB   EF02 BIOS-boot   (FIRST on disk)
  - p1  start 4096        4 GiB   swap
  - p2  start 8392704     1 GiB   /boot (ext3)
  - p3  start 10489856    2 TiB   / (ext4)         <- to be grown
  - p4  start 4234153984  1.5 TiB /home (ext4)     <- LAST partition, to be deleted
- So p4 (/home) is physically the last partition, immediately after p3. p3 (/) is the live root fs.
- `/home` currently holds only `lost+found` + an empty `/home/pve` (a now-removed PVE `dir` storage
  scaffold) — i.e. no real data. The PVE `home` storage was created earlier and is empty.
- Other guests (CT 101 nats1, CT 107 postgresql) store their disks on `local` (=/var/lib/vz on p3),
  NOT on /home, so they are unaffected and are intentionally left running.
- Tools present: sgdisk (gdisk), partx, parted, resize2fs. `growpart` (cloud-guest-utils) may NOT be
  installed; the script apt-installs it. apt is healthy post-upgrade.

## What to verify (correctness + safety, not style)
1. **Geometry logic.** The "p4 is the last partition" check parses `sgdisk -p` and picks the partition
   with the max start sector. Confirm this correctly yields p4 here (note p5 has the LOWEST start sector
   despite the highest number) and would correctly ABORT if some other partition were last. Confirm the
   delete-p4 + growpart-p3 sequence actually grows p3 into the freed contiguous tail of the disk given
   this layout, and that swap (p1), /boot (p2), and BIOS-boot (p5) are never touched.
2. **fstab safety.** Confirm the script removes the active `/home` entry from /etc/fstab BEFORE deleting
   the partition (so a later boot cannot hang on a missing device), that the sed only matches an active
   (non-comment) line whose mountpoint field is `/home`, and that it won't accidentally match `/` or
   other paths. Confirm a backup of fstab is taken first.
3. **Data-loss guards.** Confirm it refuses unless /home is mounted from /dev/nvme0n1p4, is ext4, and
   uses < HOME_MAX_MB (1024 MiB). Is `du -sxm /home` the right measure (does `-x` correctly avoid
   crossing into submounts)? Is the threshold reasonable as a guard against nuking real data?
4. **The mounted-root grow.** Is `growpart /dev/nvme0n1 3` followed by `resize2fs /dev/nvme0n1p3` the
   correct and safe way to grow a MOUNTED root partition online on a modern kernel? Is deleting p4 with
   `sgdisk -d 4` then `partx -d --nr 4` the right way to free it so growpart sees the space? Any risk the
   kernel won't accept the grown p3 size while mounted, and if so what's the mitigation?
5. **Fail-closed / ordering.** With `set -Eeuo pipefail` + the ERR trap, confirm every failure aborts
   with the MERGE-FAILED sentinel and that no destructive step runs after a failed guard. Check the order:
   guards -> backups -> remove PVE `home` storage -> fstab edit -> umount -> delete p4 -> growpart ->
   resize2fs -> verify. Is removing the PVE `home` storage before unmount necessary/sufficient to let the
   umount succeed (pvestatd releasing /home)?
6. **Recoverability.** The script backs up the GPT via `sgdisk --backup` and prints the restore command.
   Is that an adequate recovery path if the partition edit goes wrong? Anything else worth capturing
   before the destructive steps?
7. **Anything that could brick boot or lose data** -> BLOCKER.

## Output
For each finding: severity (BLOCKER / WARN / NIT), exact location, the problem, and a concrete fix —
for every severity. Note what you checked and found correct. End with a final line that is exactly
`VERDICT: GO` or `VERDICT: FIXES_REQUIRED`.
