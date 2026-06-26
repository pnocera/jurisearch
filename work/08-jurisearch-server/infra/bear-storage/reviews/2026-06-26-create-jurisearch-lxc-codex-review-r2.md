# Codex Re-Review: create-jurisearch-lxc.sh (r2)

Scope: `/home/pierre/bear-storage/create-jurisearch-lxc.sh`

Review mode: static source review against `/home/pierre/bear-storage/codex-review-create-lxc-instructions-r2.md` and the original ground truth in `/home/pierre/bear-storage/codex-review-create-lxc-instructions.md`. I did not execute the provisioning script on `bear`.

## r1 Finding Resolution

1. RESOLVED - Gateway check non-fatal: line 120 now runs `pct exec "$VMID" -- apt-get update` after exec-readiness and dies on failure, which tests DNS, NAT, gateway, and mirror reachability needed by the later PG17/pgvector/pg_search work.
2. RESOLVED - No IP-in-use check: lines 59-61 derive `IP_ADDR`, fail if `ip=${IP_ADDR}/` appears in PVE CT/VM configs, and fail if the address answers ping; the trailing `/` prevents substring matches such as `192.168.0.11` versus `192.168.0.110`.
3. RESOLVED - Only rootfs storage validated: lines 50-56 validate `TEMPLATE_STORE`, `ROOTFS_STORE`, and `DATA_STORE` before `pct create`, then compare `DATA_GB * 1024 * 1024` KiB against the `Available` column from `pvesm status`.
4. RESOLVED - os-release loop no terminal assertion: lines 100-105 set `os_ready=1` only after a successful `pct exec ... test -e /etc/os-release` and die explicitly if the CT never becomes exec-ready.

## New Issues

No new issues found.

## Checked Correct

- The `pct create` call remains correct for the stated PVE LXC target: unprivileged Debian CT, `local:20` rootfs, static `vmbr1` veth config, nesting, explicit `--start 0`, and later `pct start`.
- The dedicated PostgreSQL mountpoint remains correctly added before first boot with `pct set "$VMID" --mp0 "${DATA_STORE}:${DATA_GB},mp=${DATA_MP}"`, avoiding a later mount over an initialized PostgreSQL data directory.
- VMID safety remains intact for both CTs and VMs via `pct status "$VMID"` and `qm status "$VMID"` before creation.
- The template-download path still uses the expected `local:vztmpl/<template>` volume reference and downloads the Debian 13 template if absent.
- The new outbound connectivity check is correctly ordered after the CT is running and exec-ready, after basic `pct exec` verification succeeds, and before reporting `SENTINEL: CREATE-DONE`.
- The data-space parser is using the right column shape for `pvesm status`: `Name Type Status Total Used Available %`, so `$(NF-1)` is `Available` in KiB; the comparison against `DATA_GB * 1024 * 1024` is arithmetically correct for a GiB-sized Proxmox volume request.
- `bash -n /home/pierre/bear-storage/create-jurisearch-lxc.sh` passed.
- `shellcheck /home/pierre/bear-storage/create-jurisearch-lxc.sh` produced no diagnostics.

VERDICT: GO
