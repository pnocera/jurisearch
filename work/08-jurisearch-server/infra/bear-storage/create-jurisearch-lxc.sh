#!/usr/bin/env bash
#
# create-jurisearch-lxc.sh — create the Debian 13 LXC that will host standalone PostgreSQL 17
# (+ pgvector + a source-built pg_search) and the jurisearch service, on host "bear".
#
# This script ONLY creates and configures the container shell (template, resources, network, a
# dedicated PostgreSQL data mountpoint). Installing PG17 / pgvector / building pg_search happen in a
# SEPARATE, later script run inside the container.
#
# Context (verified 2026-06-26 on bear, PVE 9.2.3):
#   - VMID 110 is free; bridge vmbr1 (192.168.0.0/24, gw = bear 192.168.0.3); other guests use static IPs.
#   - storage `local` is one ~3.5 TB dir pool (post disk-merge); `storebox` (CIFS) is for datasources/backups.
#   - Debian 13 template available as debian-13-standard_13.1-2_amd64 (downloaded by this script if absent).
#   - bridge-only networking (no tailscale in the container): reachable on vmbr1 and via `pct exec` from bear.
#
# Safety: never reboots the host; fails closed; refuses if VMID already exists.
#
set -Eeuo pipefail

# ---- parameters (override via env) ----
VMID="${VMID:-110}"
HOSTNAME_="${HOSTNAME_:-jurisearch}"
TEMPLATE_NAME="${TEMPLATE_NAME:-debian-13-standard_13.1-2_amd64.tar.zst}"
TEMPLATE_STORE="${TEMPLATE_STORE:-local}"
ROOTFS_STORE="${ROOTFS_STORE:-local}"
ROOTFS_GB="${ROOTFS_GB:-20}"
DATA_STORE="${DATA_STORE:-local}"
DATA_GB="${DATA_GB:-1024}"                 # start at 1 TB; local has ~3.5 TB so `pct resize` can grow it later
DATA_MP="${DATA_MP:-/var/lib/postgresql}"  # PostgreSQL data lives on the dedicated mountpoint, off the rootfs
CORES="${CORES:-16}"
MEM_MB="${MEM_MB:-65536}"
SWAP_MB="${SWAP_MB:-8192}"
BRIDGE="${BRIDGE:-vmbr1}"
IPCIDR="${IPCIDR:-192.168.0.110/24}"
GW="${GW:-192.168.0.3}"
NAMESERVER="${NAMESERVER:-8.8.8.8}"
ROOT_PW="${ROOT_PW:-20Sense20}"            # tailnet-only host; per the operator's stated security posture

ts(){ date '+%Y-%m-%d %H:%M:%S'; }
log(){ echo "[$(ts)] $*"; }
die(){ echo "[$(ts)] FATAL: $*" >&2; echo "SENTINEL: CREATE-FAILED rc=1"; exit 1; }
trap 'die "unexpected error on line $LINENO"' ERR

# ---- 0. preconditions ----
[ "$(id -u)" = 0 ] || die "must run as root"
command -v pct >/dev/null || die "pct not found — not a PVE host"
command -v pveam >/dev/null || die "pveam not found"
pct status "$VMID" >/dev/null 2>&1 && die "VMID $VMID already exists — refusing to clobber"
qm status "$VMID" >/dev/null 2>&1 && die "VMID $VMID already exists as a VM — refusing to clobber"
for st in "$TEMPLATE_STORE" "$ROOTFS_STORE" "$DATA_STORE"; do
  pvesm status --storage "$st" >/dev/null 2>&1 || die "storage '$st' not available"
done
# data store must have room for the mountpoint volume
avail_kib=$(pvesm status --storage "$DATA_STORE" 2>/dev/null | awk 'NR==2{print $(NF-1)}')
need_kib=$(( DATA_GB * 1024 * 1024 ))
[ "${avail_kib:-0}" -ge "$need_kib" ] || die "data storage '$DATA_STORE' has ${avail_kib:-0} KiB free, need ${need_kib} for a ${DATA_GB}G mountpoint"
grep -qE "^(auto |iface )$BRIDGE" /etc/network/interfaces 2>/dev/null || ip link show "$BRIDGE" >/dev/null 2>&1 || die "bridge '$BRIDGE' not found"
# the chosen static IP must not already be claimed by a guest config or be live on the LAN
IP_ADDR="${IPCIDR%%/*}"
grep -rsoE "ip=${IP_ADDR}/" /etc/pve/lxc /etc/pve/qemu-server 2>/dev/null | grep -q . && die "IP $IP_ADDR is already present in a PVE guest config"
ping -c1 -W1 "$IP_ADDR" >/dev/null 2>&1 && die "IP $IP_ADDR already answers on the network — pick another"

# ---- 1. ensure the Debian 13 template is present ----
if ! pveam list "$TEMPLATE_STORE" 2>/dev/null | grep -q "$TEMPLATE_NAME"; then
  log "downloading template $TEMPLATE_NAME into $TEMPLATE_STORE …"
  pveam download "$TEMPLATE_STORE" "$TEMPLATE_NAME"
fi
template_vol="${TEMPLATE_STORE}:vztmpl/${TEMPLATE_NAME}"
log "using template $template_vol"

# ---- 2. create the container (unprivileged, nesting for builds) ----
log "creating CT $VMID ($HOSTNAME_): ${CORES} cores, ${MEM_MB} MiB RAM, ${ROOTFS_GB}G rootfs on $ROOTFS_STORE"
pct create "$VMID" "$template_vol" \
  --hostname "$HOSTNAME_" \
  --cores "$CORES" \
  --memory "$MEM_MB" \
  --swap "$SWAP_MB" \
  --rootfs "${ROOTFS_STORE}:${ROOTFS_GB}" \
  --net0 "name=eth0,bridge=${BRIDGE},ip=${IPCIDR},gw=${GW},type=veth" \
  --nameserver "$NAMESERVER" \
  --features nesting=1 \
  --ostype debian \
  --unprivileged 1 \
  --onboot 1 \
  --start 0

# ---- 3. add the dedicated PostgreSQL data mountpoint on `local` ----
log "adding data mountpoint mp0 = ${DATA_STORE}:${DATA_GB}G at ${DATA_MP}"
pct set "$VMID" --mp0 "${DATA_STORE}:${DATA_GB},mp=${DATA_MP}"

# ---- 4. start + wait for it to be running ----
log "starting CT $VMID …"
pct start "$VMID"
for _ in $(seq 1 30); do
  [ "$(pct status "$VMID" 2>/dev/null)" = "status: running" ] && break
  sleep 1
done
[ "$(pct status "$VMID")" = "status: running" ] || die "CT $VMID did not reach running state"
# give systemd inside a moment to bring up; require exec-readiness
os_ready=0
for _ in $(seq 1 30); do
  if pct exec "$VMID" -- test -e /etc/os-release 2>/dev/null; then os_ready=1; break; fi
  sleep 1
done
[ "$os_ready" = 1 ] || die "CT $VMID did not become exec-ready"

# ---- 5. set root password (bridge-only; per the operator's stated security posture) ----
printf 'root:%s\n' "$ROOT_PW" | pct exec "$VMID" -- chpasswd
log "root password set"

# ---- 6. verify ----
conf="/etc/pve/lxc/${VMID}.conf"
log "=== verification ==="
pct status "$VMID"
# shellcheck disable=SC2016  # $(nproc) is intentionally expanded INSIDE the container, not here
pct exec "$VMID" -- sh -c 'grep PRETTY_NAME /etc/os-release; echo "cpus: $(nproc)"; free -h | grep Mem'
log "data mountpoint ${DATA_MP}:"
pct exec "$VMID" -- sh -c "df -h ${DATA_MP} | tail -1"
log "verifying outbound connectivity (required for the later PG17 / pgvector / pg_search build)…"
pct exec "$VMID" -- apt-get update >/dev/null 2>&1 || die "CT $VMID cannot apt-get update — no working outbound connectivity (gw/NAT/DNS/mirror); fix before the PG/build step"
log "outbound connectivity OK (apt-get update succeeded)"
log "container config:"; grep -vE '^$' "$conf"
echo "SENTINEL: CREATE-DONE rc=0 vmid=$VMID"
