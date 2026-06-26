# Codex Re-review r2: bear-disk-merge.sh

Scope: `/home/pierre/bear-storage/bear-disk-merge.sh`

Review mode: static safety review only. I did not run the script or any mutating disk/PVE commands. I read the r2 brief and the r1 ground-truth brief, inspected the script with line numbers, ran `bash -n`, ran `shellcheck`, and checked the partition-order and fstab-rewrite logic against small non-mutating fixtures.

## r1 Findings

1. RESOLVED - `growpart` installed after destructive delete: lines 61-72 now perform the tool preflight before backups, fstab edits, PVE storage edits, unmount, or partition changes; an `apt-get` failure aborts via `set -e`/`ERR`, and line 70 dies if `growpart` is still unavailable.
2. RESOLVED - kernel `partx`/`partprobe` failure ignored: lines 129-138 now fail closed after `sgdisk -d p4`; `partx` failure requires `partprobe` success, and the `blockdev --getsz /dev/nvme0n1p4` check correctly detects a still-present kernel p4 before `growpart`.
3. RESOLVED - fstab edit not field-aware: lines 107-119 use `awk` with `$2 == "/home"` on non-comment lines, write a temp file under `/etc`, assert `/` remains active and `/home` does not, preserve owner/perms, and atomically rename over `/etc/fstab`; it cannot match `/`, `/home2`, or a later path containing `/home`.
4. RESOLVED - GPT restore unsafe after resize: lines 84-95 now create the GPT backup, fstab backup, and text state snapshot (`sgdisk -p`, `lsblk -f`, `blkid`, `findmnt`, `pvesm status`) before destructive work, and the recovery note explicitly says GPT restore is only safe before `resize2fs` grows root.
5. RESOLVED - `-le` allowed exactly `HOME_MAX_MB`: lines 40 and 80-82 now document and enforce a strict `< HOME_MAX_MB` guard with `-lt`.

## New Issues

None found.

## Checked Correct

- Phase split: PHASE A does not mutate the partition table, `/etc/fstab`, or PVE storage. It may install `cloud-guest-utils` and create backup/snapshot files, but all destructive disk/fstab/storage actions start in PHASE B at line 101.
- Geometry logic: line 75 sorts `sgdisk -p` rows by start sector, so the known bear layout selects p4 as the last physical partition even though p5 has the highest partition number. The delete/grow sequence targets only p4 and p3; p1 swap, p2 `/boot`, and p5 BIOS-boot are untouched.
- Data-loss guards: the script refuses unless `/home` is mounted from `/dev/nvme0n1p4`, is ext4, `/` is `/dev/nvme0n1p3`, and `/home` usage is under 1024 MiB. `du -sxm /home` is appropriate here because `-x` avoids crossing into submounts that would not be deleted with p4.
- Mounted-root grow: for the verified layout, deleting the final p4 leaves contiguous tail space after mounted ext4 root p3. `growpart /dev/nvme0n1 3` followed by `resize2fs /dev/nvme0n1p3` is the correct online-grow sequence on a modern kernel; failures abort before the next step.
- Failure behavior: `set -Eeuo pipefail` plus the `ERR` trap gives the `MERGE-FAILED` sentinel for unhandled command failures, and the explicit `umount` path reports holders then aborts before p4 deletion.
- Validation commands: `bash -n bear-disk-merge.sh` passed; `shellcheck bear-disk-merge.sh` produced no findings. Fixture checks confirmed the partition parser returns `4` for the supplied geometry and the fstab `awk` comments only an active `/home` mountpoint line.

VERDICT: GO
