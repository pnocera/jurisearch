# Code Review: bear-disk-merge.sh

Scope: `/home/pierre/bear-storage/bear-disk-merge.sh`

Review mode: static safety review only. I did not run the script or any mutating disk/PVE commands. I read the review brief, inspected the script with line numbers, ran `bash -n` against the script, and checked the `sgdisk -p` parsing logic against the ground-truth partition ordering from the brief.

## Findings

### WARN - `growpart` is installed after the destructive partition delete

Location: `bear-disk-merge.sh:99-103`

Problem: `growpart` is a non-destructive prerequisite, but the script installs it only after `/home` has been removed from fstab, unmounted, and p4 has been deleted from GPT. If `growpart` is missing and `apt-get -y install cloud-guest-utils` fails because of an apt lock, package issue, mirror problem, or network issue, the script aborts after the destructive partition-table edit. At that point a simple re-run will fail at `bear-disk-merge.sh:50` because `/dev/nvme0n1p4` no longer exists.

This is not a boot-brick condition by itself because the `/home` fstab entry has already been commented and p3/root is still intact, but it is not fail-closed ordering for a remote live-root disk operation.

Concrete fix: move the `growpart` availability/install block into the precondition phase before backups and before any fstab, unmount, or partition-table changes. After the install attempt, assert `command -v growpart >/dev/null || die "growpart still unavailable"`. I would also preflight `partx`, `partprobe`, and `resize2fs` there so all tooling failures happen before line 79.

### WARN - kernel partition-table update failures after deleting p4 are ignored

Location: `bear-disk-merge.sh:94-96`

Problem: after `sgdisk -d "$HOME_PART_NUM" "$DISK"`, the script runs:

```bash
partx -d --nr "$HOME_PART_NUM" "$DISK" 2>/dev/null || partprobe "$DISK" 2>/dev/null || true
```

The final `|| true` means the script proceeds even if the kernel refuses both update attempts. If the kernel still has p4 in its partition table, the following `growpart "$DISK" "$ROOT_PART_NUM"` will likely fail or fail to publish the grown p3 size, but the script will already be in a partially modified state with p4 deleted from on-disk GPT.

Concrete fix: make this a hard check. For example:

```bash
if ! partx -d --nr "$HOME_PART_NUM" "$DISK"; then
  partprobe "$DISK" || die "kernel still has p$HOME_PART_NUM after GPT delete; aborting before grow"
fi
```

Then add a verification step before `growpart`, such as confirming `lsblk` no longer reports p4 or that `blockdev --getsz "$HOME_PART"` fails. If that verification fails, abort before attempting to grow p3.

### WARN - fstab edit is not strictly field-aware

Location: `bear-disk-merge.sh:80-81`

Problem: the sed expression only checks for an active line containing whitespace, `/home`, and whitespace somewhere after the first non-comment character:

```bash
^[[:space:]]*[^#].*[[:space:]]\/home[[:space:]]
```

For a normal fstab line with mountpoint field `/home`, it works, and it will not match `/` or `/home2`. It also correctly skips full-line comments. However, it does not actually prove that field 2 is `/home`; it can match any active fstab line that contains ` /home ` later in the line.

Concrete fix: replace the sed with a field-aware rewrite. For example, use `awk` and match `$2 == "/home"` on non-comment lines, writing to a temp file and then atomically replacing `/etc/fstab`. That makes the safety claim "only the active `/home` mountpoint entry is commented" true by construction.

### WARN - printed GPT restore command is unsafe after filesystem growth

Location: `bear-disk-merge.sh:68-71`, `bear-disk-merge.sh:107-112`

Problem: the GPT backup is valuable, but the printed restore command is only safe before the root filesystem has been grown. If `resize2fs "$ROOT_PART"` succeeds and a later verification fails, restoring the old GPT from `/root/gpt-backup-${stamp}.bin` would shrink p3's partition boundary back under an enlarged ext4 filesystem and can corrupt root.

Concrete fix: print a stage-aware warning with the restore command, for example: "Only use this GPT restore before resize2fs has grown root." Also capture a text snapshot before destructive steps, such as `sgdisk -p "$DISK"`, `lsblk -f`, `blkid`, `findmnt`, and `pvesm status`, next to the binary GPT and fstab backups. That improves console recovery without implying the old GPT is always safe to reload.

### NIT - `/home` size guard allows exactly `HOME_MAX_MB`

Location: `bear-disk-merge.sh:62-63`

Problem: the review brief describes the guard as requiring `/home` to use `< HOME_MAX_MB`, but the script uses `-le`, so exactly 1024 MiB passes.

Concrete fix: use `-lt "$HOME_MAX_MB"` if the intended contract is strict, or update the comment/brief to say `<= HOME_MAX_MB`.

## Checked And Found Correct

- Geometry parsing: `bear-disk-merge.sh:57` sorts by start sector, not partition number. Against the ground-truth layout, this selects p4 as the last physical partition even though p5 has the highest partition number and the lowest start sector. If any other partition had the highest start sector, line 58 would abort.
- Targeted partition operations: the script deletes only p4 with `sgdisk -d "$HOME_PART_NUM"` and grows only p3 with `growpart "$DISK" "$ROOT_PART_NUM"`. There are no operations targeting p1 swap, p2 `/boot`, or p5 BIOS boot.
- Root-growth geometry: given the verified layout, p4 starts immediately after p3 and is the final partition. Deleting p4 leaves contiguous free tail space after p3, which is the correct condition for `growpart /dev/nvme0n1 3`.
- `/home` identity guards: before destructive steps, the script requires `/home` to be a mountpoint, mounted from `/dev/nvme0n1p4`, and ext4. It also requires `/` to be mounted from `/dev/nvme0n1p3`.
- `/home` data-loss guard: `du -sxm /home` is the right shape for this guard. `-x` keeps the measurement on the `/home` filesystem and avoids counting any submounts that would not be deleted with p4. A 1024 MiB threshold is reasonable for the stated ground truth of only `lost+found` plus an empty `/home/pve`, though it intentionally would still allow up to roughly 1 GiB of real files.
- fstab backup ordering: `/etc/fstab` is backed up at line 69 before the edit at lines 80-81. The fstab edit occurs before p4 is deleted, so a later boot is not left waiting on a missing `/home` device if the normal `/home` fstab line is present.
- Mounted-root resize sequence: `growpart /dev/nvme0n1 3` followed by `resize2fs /dev/nvme0n1p3` is the correct online-grow sequence for an ext4 root filesystem on a modern Linux kernel, assuming the kernel accepts the p3 partition-size update. If the kernel does not accept the live p3 update, the mitigation is manual console recovery or an explicit `partx -u --nr 3 "$DISK"`/verification step; the script correctly never reboots.
- Failure behavior: `set -Eeuo pipefail` plus the ERR trap catches unhandled command failures and emits the `SENTINEL: MERGE-FAILED rc=1` marker. The explicit `umount` failure path logs holders with `fuser` and aborts before p4 deletion.
- PVE storage handling: removing the `home` storage before `umount /home` is a reasonable way to stop PVE from intentionally tracking that mount. If pvestatd or another process still holds `/home`, the hard `umount` guard stops the destructive partition delete.
- Syntax: `bash -n /home/pierre/bear-storage/bear-disk-merge.sh` passed.

VERDICT: FIXES_REQUIRED
