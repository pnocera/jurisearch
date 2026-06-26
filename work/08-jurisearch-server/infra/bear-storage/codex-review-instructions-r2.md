# Codex re-review (r2) — bear-disk-merge.sh

## Scope
`/home/pierre/bear-storage/bear-disk-merge.sh` (live root-disk partition merge on remote PVE host "bear").

This is **r2**. Your r1 review (`reviews/2026-06-26-bear-disk-merge-codex-review.md`) returned
FIXES_REQUIRED with 4 WARNs + 1 NIT. Confirm each is resolved and no regression/new hazard was
introduced. Ground truth about bear's geometry is unchanged from r1's instructions
(`codex-review-instructions.md`) — reuse it.

## Fixes applied — verify each
1. **WARN (growpart installed after destructive delete).** All tooling checks now run in a clearly
   marked "PHASE A — non-destructive" block BEFORE any backup/fstab/partition change: it verifies
   sgdisk/partx/resize2fs/blockdev, installs `cloud-guest-utils` if `growpart` is missing, and then
   `die`s if growpart is still absent. Confirm no destructive step can run if any tool/apt install
   fails, and that the PHASE A / PHASE B split is real (nothing in PHASE A mutates disk/fstab/storage).
2. **WARN (kernel partx/partprobe failure ignored).** After `sgdisk -d p4` the script now does
   `partx -d --nr 4 || partprobe || die`, then HARD-verifies the kernel forgot p4
   (`[ -b $HOME_PART ] && blockdev --getsz $HOME_PART` -> die) before `growpart`. Confirm this is now
   fail-closed and that the verification correctly detects a still-present p4.
3. **WARN (fstab edit not field-aware).** Replaced the sed with an awk that comments only the line
   whose mountpoint field `$2 == "/home"`, written to a temp file and atomically `mv`-ed over
   /etc/fstab (perms/owner preserved), with sanity asserts that the `/` entry survives and no active
   `/home` entry remains. Confirm correctness and atomicity, and that it cannot match `/`, `/home2`, or
   a path containing `/home`.
4. **WARN (GPT restore unsafe after resize).** The script now prints a stage-aware note ("restoring the
   GPT is ONLY safe BEFORE resize2fs grows root") and captures a text state snapshot (sgdisk -p, lsblk
   -f, blkid, findmnt, pvesm status) alongside the GPT + fstab backups. Confirm.
5. **NIT (`-le` allowed exactly HOME_MAX_MB).** Now uses `-lt` (strict `<`), with the comment updated.
   Confirm.

## Validation already done locally
`bash -n` passes; `shellcheck` is clean; a fixture test of the awk fstab edit confirmed it comments
ONLY the `/home` line and leaves `/`, `/boot`, `/home2`, and `/mnt/.../home/inside` untouched.

## Output
For each of the 5 findings: RESOLVED / PARTIALLY / NOT RESOLVED + one-line justification. List any new
issues (severity + concrete fix). End with exactly `VERDICT: GO` or `VERDICT: FIXES_REQUIRED`.
