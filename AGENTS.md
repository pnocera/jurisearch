<!-- CODEGRAPH_START -->
## CodeGraph

In repositories indexed by CodeGraph (a `.codegraph/` directory exists at the repo root), reach for it BEFORE grep/find or reading files when you need to understand or locate code:

- **MCP tool** (when available): `codegraph_explore` answers most code questions in one call -- the relevant symbols' verbatim source plus the call paths between them, including dynamic-dispatch hops grep can't follow. Name a file or symbol in the query to read its current line-numbered source. If it's listed but deferred, load it by name via tool search.
- **Shell** (always works): `codegraph explore "<symbol names or question>"` prints the same output.

If there is no `.codegraph/` directory, skip CodeGraph entirely -- indexing is the user's decision.
<!-- CODEGRAPH_END -->

## Bear Proxmox CT Preparation

Use this when preparing a new LXC container on the Proxmox host `bear`.

- Access: `root@bear.tail0cb6c3.ts.net`; use the operator-provided bootstrap password. Do not commit live passwords to this repo.
- Existing bear private bridge: `vmbr1`, gateway `192.168.0.3/24`, MTU `1400`.
- Storebox is mounted on bear at `/mnt/pve/storebox`; for writable CT bind mounts, follow the CT 111 pattern: create a dedicated host CIFS mount such as `/mnt/jurisearch-<role>-storebox`, then bind it into the CT as `/srv/jurisearch/storebox` with `backup=0`.
- Use privileged CTs when the CT must write to the CIFS-backed Storebox bind mount. An unprivileged CT may show Storebox files as `65534:65534` and fail on ownership/mode changes.
- Prepare Tailscale by adding only the LXC options below; do not install or authenticate Tailscale unless explicitly asked:

```text
lxc.cgroup2.devices.allow: c 10:200 rwm
lxc.mount.entry: /dev/net/tun dev/net/tun none bind,create=file
```

- Set MTU `1400` in both places:
  - Proxmox CT config `net0`: include `mtu=1400`.
  - Inside the CT, `/etc/network/interfaces`: add `mtu 1400` under `iface eth0 inet static`.
- Verify MTU after start with `ip -d link show eth0`; it must report `mtu 1400`. A CT can have `mtu=1400` in Proxmox while still showing `1500` inside if `/etc/network/interfaces` is missing the line.
- Do not restart the production PostgreSQL service on CT 110. If `pg_hba.conf` changes are required, use a config check and reload only.
- CT 110 is the production PostgreSQL host at `192.168.0.110`; CT 111 is the update-server at `192.168.0.111`; CT 112 is `jurisearch-admin` at `192.168.0.112`.

Minimal validation for a new admin/backup CT:

```sh
pct status <vmid>
pct exec <vmid> -- ip -d link show eth0
pct exec <vmid> -- test -c /dev/net/tun
pct exec <vmid> -- df -h /srv/jurisearch/storebox
pct exec <vmid> -- sh -c 'touch /srv/jurisearch/storebox/.write-test && rm /srv/jurisearch/storebox/.write-test'
pct exec <vmid> -- nc -zvw3 192.168.0.110 5432
```
