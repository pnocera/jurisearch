# Codex review — create-jurisearch-lxc.sh

## Scope
`/home/pierre/bear-storage/create-jurisearch-lxc.sh`

Runs on remote Proxmox VE 9.2.3 host "bear" (over SSH-via-Tailscale; Hetzner console fallback). It
creates ONE new unprivileged Debian 13 LXC (the future home of standalone PostgreSQL 17 + pgvector +
a source-built pg_search + the jurisearch service). It does NOT install Postgres or build anything —
that's a separate later script. It must NEVER reboot the host and must not touch existing guests.

## Ground truth about bear (verified 2026-06-26)
- PVE 9.2.3, Debian 13 host. VMID 110 is FREE (101 nats1, 107 postgresql exist and must be untouched).
- Storage `local` = one ~3.5 TB `dir` pool (post disk-merge), holds rootdir+images. `storebox` = CIFS.
- Bridge `vmbr1`, subnet 192.168.0.0/24, gateway = bear 192.168.0.3; existing guests use static IPs.
- LXC template `debian-13-standard_13.1-2_amd64.tar.zst` is AVAILABLE in pveam but not yet downloaded
  to `local` (the script downloads it if absent).
- Decision: **bridge-only networking, NO tailscale in this container** (intentional; reachable on
  vmbr1 and via `pct exec` from bear). So there is deliberately no /dev/net/tun passthrough.

## What to verify (correctness + safety, not style)
1. **`pct create` correctness.** Are the flags/values valid for PVE 9 (`--rootfs local:20`, `--net0
   name=eth0,bridge=vmbr1,ip=...,gw=...,type=veth`, `--features nesting=1`, `--unprivileged 1`,
   `--ostype debian`, `--swap`, `--memory` in MiB, `--cores`)? Is `--start 0` then a later `pct start`
   the right sequence? Is the template volume reference `local:vztmpl/<name>` correct?
2. **Data mountpoint.** `pct set 110 --mp0 local:1024,mp=/var/lib/postgresql` allocates a 1 TB volume
   and mounts it at /var/lib/postgresql. Is the size syntax (`local:1024` = 1024 GiB) correct? Mounting
   a FRESH empty volume at /var/lib/postgresql BEFORE Postgres is installed (later) — is that the right
   approach, and will an UNPRIVILEGED container's id-mapped mountpoint let the later `postgresql-17`
   package initdb into it (ownership/permissions)? Flag any pitfall (e.g. the mountpoint root owned by
   mapped-root vs the postgres user) and the standard fix, even though PG install is a later step.
3. **Idempotency / safety.** Confirm it refuses if VMID 110 already exists (as CT or VM), validates the
   bridge and storage, and that a failure aborts via the ERR trap + CREATE-FAILED sentinel without
   leaving the host rebooted or other guests touched. Is there any partial-create state worth cleaning
   up on failure (e.g. a half-created CT), or is leaving it for inspection acceptable?
4. **Start/readiness waits.** The two `for _ in $(seq 1 30)` loops wait for running state and for
   /etc/os-release. Are these adequate, and is the password set via `printf 'root:%s' | pct exec ...
   chpasswd` correct?
5. **Network.** Static IP 192.168.0.110/24 gw 192.168.0.3 on vmbr1 — consistent with the existing
   guests; no DHCP. Any conflict risk (does the script need to check the IP isn't already used)?
6. **Anything that would create a broken/unusable container, clobber an existing guest, or wedge the
   host** -> BLOCKER.

## Output
For each finding: severity (BLOCKER / WARN / NIT), exact location, the problem, and a concrete fix —
for every severity. Note what you checked and found correct. End with a final line that is exactly
`VERDICT: GO` or `VERDICT: FIXES_REQUIRED`.
