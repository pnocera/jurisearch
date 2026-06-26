# Codex Review: create-jurisearch-lxc.sh

Scope: `/home/pierre/bear-storage/create-jurisearch-lxc.sh`

Review mode: static source review against `/home/pierre/bear-storage/codex-review-create-lxc-instructions.md`. I did not execute the script on `bear`.

## Findings

### WARN - Lines 107-108 - Gateway reachability failure still reports `CREATE-DONE`

The script verifies gateway reachability, but the check is explicitly non-fatal:

```bash
pct exec "$VMID" -- sh -c "ping -c1 -W2 ${GW} >/dev/null 2>&1 && echo 'gw reachable' || echo 'WARN: gw not reachable'"
```

Because the inner shell always exits successfully after printing the warning branch, the outer script still reaches `SENTINEL: CREATE-DONE rc=0`. For this container, bridge networking is the only intended network path and the later Postgres/build script will need working outbound connectivity. A bad gateway, bad bridge attachment, duplicate IP, or host firewall issue would therefore leave a materially broken container while the script reports success.

Concrete fix: make the gateway check fatal unless the operator intentionally opts out:

```bash
log "network (ping gw ${GW}):"
pct exec "$VMID" -- ping -c1 -W2 "$GW" >/dev/null || die "CT $VMID cannot reach gateway $GW"
log "gw reachable"
```

If ICMP is intentionally blocked on this LAN, replace it with another fatal connectivity check that matches the actual dependency, such as reaching the configured package mirror or resolving via the configured nameserver.

### WARN - Lines 33-35 and 44-51 - No pre-create check that `192.168.0.110` is unused

The script verifies VMID uniqueness for both CTs and VMs, but it does not check whether the chosen static address is already in use. The instructions establish that existing guests use static IPs on `vmbr1`; a VMID-free state does not prove that `192.168.0.110` is free. If another guest or physical host already owns the address, `pct create` and `pct start` can still succeed while creating an IP conflict on the LAN.

Concrete fix: derive the address from `IPCIDR` and abort before `pct create` if it appears in existing PVE guest configs or answers on the bridge. For example:

```bash
IP_ADDR="${IPCIDR%%/*}"
grep -R --include='*.conf' -F "ip=${IP_ADDR}/" /etc/pve/lxc /etc/pve/qemu-server >/dev/null 2>&1 \
  && die "IP $IP_ADDR is already present in a PVE guest config"
ping -c1 -W1 "$IP_ADDR" >/dev/null 2>&1 \
  && die "IP $IP_ADDR already responds on the network"
```

For stronger duplicate-address detection on the local L2 segment, use `arping -D -I "$BRIDGE" "$IP_ADDR"` when available, with a documented fallback if `arping` is not installed.

### WARN - Lines 50, 54-56, and 77-79 - Only the rootfs storage is validated before creating the CT

The script checks `ROOTFS_STORE`, and the current defaults set `TEMPLATE_STORE`, `ROOTFS_STORE`, and `DATA_STORE` to `local`. Under those defaults, the storage preflight covers the intended bear configuration. However, the script documents all three as environment-overridable parameters, and `DATA_STORE` is first exercised after `pct create` has already succeeded:

```bash
pct set "$VMID" --mp0 "${DATA_STORE}:${DATA_GB},mp=${DATA_MP}"
```

If `DATA_STORE` is invalid, disabled, lacks rootdir/container support, or lacks space, the script fails after leaving a partially created CT behind. That is inspectable, but it also blocks an immediate rerun because the VMID now exists.

Concrete fix: preflight every storage parameter before `pct create`, and ideally check that the data store has enough available space for `DATA_GB`:

```bash
pvesm status --storage "$TEMPLATE_STORE" >/dev/null 2>&1 || die "template storage '$TEMPLATE_STORE' not available"
pvesm status --storage "$ROOTFS_STORE" >/dev/null 2>&1 || die "rootfs storage '$ROOTFS_STORE' not available"
pvesm status --storage "$DATA_STORE" >/dev/null 2>&1 || die "data storage '$DATA_STORE' not available"
```

If the script should support non-`local` overrides, also validate content support with `pvesm status --content rootdir` or an equivalent storage-content check before allocating the mountpoint.

### NIT - Lines 90-93 - The `/etc/os-release` readiness loop has no terminal assertion

The first wait loop correctly asserts that the CT reaches `status: running`. The second loop waits for `pct exec "$VMID" -- test -e /etc/os-release`, but if all 30 attempts fail, the script simply falls through to `chpasswd`. In practice `chpasswd` should then fail and trigger the ERR trap, so this is not a separate safety break, but the failure message will be less direct.

Concrete fix: mirror the running-state loop with an explicit postcondition:

```bash
os_ready=0
for _ in $(seq 1 30); do
  if pct exec "$VMID" -- test -e /etc/os-release 2>/dev/null; then
    os_ready=1
    break
  fi
  sleep 1
done
[ "$os_ready" = 1 ] || die "CT $VMID did not become exec-ready"
```

## Checked Correct

- `pct create` usage is structurally correct for PVE LXC creation: `--rootfs local:20`, `--net0 name=eth0,bridge=vmbr1,ip=192.168.0.110/24,gw=192.168.0.3,type=veth`, `--features nesting=1`, `--unprivileged 1`, `--ostype debian`, `--swap`, `--memory` in MiB, and `--cores` are the right class of options for this CT.
- The `--start 0` followed by an explicit `pct start "$VMID"` is the right sequencing for adding the PostgreSQL data mountpoint before first boot.
- The template reference `local:vztmpl/debian-13-standard_13.1-2_amd64.tar.zst` matches the expected Proxmox template-volume form, and the script downloads the template if `pveam list local` does not show it.
- `pct set "$VMID" --mp0 local:1024,mp=/var/lib/postgresql` is the intended storage-backed mountpoint shape for a 1024 GiB data volume. Creating and mounting it before PostgreSQL is installed is the correct order because it avoids masking an already-initialized cluster.
- For an unprivileged container, a Proxmox-managed storage-backed mountpoint is the right approach; it avoids the common host bind-mount idmap problem. The fresh mountpoint may appear as root-owned inside the CT, but the later PostgreSQL package/cluster setup should be able to create and chown its cluster directories as root. If a later install path does hit ownership friction, the standard fix is inside the CT, for example `install -d -o postgres -g postgres -m 700 /var/lib/postgresql/17/main` or `chown postgres:postgres /var/lib/postgresql` before `initdb`/`pg_createcluster`, not host-side ownership edits.
- VMID safety is good: the script refuses both an existing CT via `pct status "$VMID"` and an existing VM via `qm status "$VMID"`.
- There are no host reboot commands, no commands targeting existing guests, and no `/dev/net/tun` or Tailscale passthrough. That matches the bridge-only decision.
- The bridge existence check is adequate for the stated `vmbr1` default.
- The root-password command shape is correct: `printf 'root:%s\n' "$ROOT_PW" | pct exec "$VMID" -- chpasswd` avoids shell interpolation of the password inside the container.
- Syntax checks passed locally: `bash -n /home/pierre/bear-storage/create-jurisearch-lxc.sh` and `shellcheck /home/pierre/bear-storage/create-jurisearch-lxc.sh` produced no diagnostics.

VERDICT: FIXES_REQUIRED
