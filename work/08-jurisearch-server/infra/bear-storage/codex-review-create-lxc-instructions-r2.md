# Codex re-review (r2) — create-jurisearch-lxc.sh

## Scope
`/home/pierre/bear-storage/create-jurisearch-lxc.sh` (creates an unprivileged Debian 13 LXC, CT 110,
on remote PVE host "bear"; bridge-only, no tailscale). Ground truth unchanged from r1 instructions
(`codex-review-create-lxc-instructions.md`).

This is **r2**. Your r1 review (`reviews/2026-06-26-create-jurisearch-lxc-codex-review.md`) returned
FIXES_REQUIRED with 3 WARNs + 1 NIT. Confirm each is resolved and no regression was introduced.

## Fixes applied — verify each
1. **WARN (gateway check non-fatal).** Replaced the non-fatal `ping gw` with a FATAL outbound-connectivity
   gate: `pct exec 110 -- apt-get update || die`. This tests the real dependency (DNS + NAT + mirror)
   the later PG17/pgvector/pg_search-build steps need, and avoids relying on ICMP working from an
   unprivileged container. Confirm this is the right fatal check and aborts on failure.
2. **WARN (no IP-in-use check).** Added, before `pct create`: derive `IP_ADDR` from `IPCIDR`, `die` if
   `ip=${IP_ADDR}/` appears in any /etc/pve/lxc or /etc/pve/qemu-server config, and `die` if the host
   can `ping` the address (already live). Confirm correctness and that it cannot false-positive on a
   substring (e.g. 192.168.0.11 vs 192.168.0.110 — note the trailing `/` anchors the match).
3. **WARN (only rootfs storage validated).** Now loops over TEMPLATE_STORE, ROOTFS_STORE, DATA_STORE
   (all validated before `pct create`), plus a data-space check: parses Available KiB from
   `pvesm status --storage DATA_STORE` (`$(NF-1)`) and `die`s if it's below `DATA_GB*1024*1024`. Confirm
   the `$(NF-1)` column is Available (verified locally against bear's real output → 3501012168 KiB) and
   the arithmetic is right.
4. **NIT (os-release loop no terminal assertion).** Added an `os_ready` flag with
   `[ "$os_ready" = 1 ] || die "CT did not become exec-ready"`. Confirm.

## Validation already done locally
`bash -n` passes; `shellcheck` is clean; the Available-KiB parser was tested against bear's real
`pvesm status` line and returned 3501012168.

## Also check
- No regression in the (already-correct) `pct create`, mountpoint, VMID-safety, password, or
  template-download logic.
- The new fatal `apt-get update` runs after the container is exec-ready and networking is up — correct
  ordering?

## Output
For each of the 4 findings: RESOLVED / PARTIALLY / NOT RESOLVED + one-line justification. List any new
issues (severity + concrete fix). End with exactly `VERDICT: GO` or `VERDICT: FIXES_REQUIRED`.
