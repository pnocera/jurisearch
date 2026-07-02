# Bear Admin/Backups Setup Report

Date: 2026-07-02

## Scope

This report summarizes the work performed for the PostgreSQL backup/admin track on bear:

- WAL-RUS backup suitability analysis.
- New Proxmox CT preparation for pgAdmin and future backup services.
- pgAdmin installation and access setup.
- PostgreSQL access adjustment for the new admin CT.
- MTU diagnosis and correction.
- Persistent repo knowledge added to `AGENTS.md`.

## Files Added In This Repo

- `work/13-backups/wal-rus-postgres-backups-analysis.md`
  - Analysis of using ClickHouse WAL-RUS for PostgreSQL backups.
  - Recommendation: use WAL-RUS as an operator-managed PostgreSQL backup layer for external site/producer PostgreSQL, not inside application code or `ManagedPostgres`.
  - Includes pilot plan, operational policy, risks, and open questions.

- `AGENTS.md`
  - Added CodeGraph instructions plus bear-specific CT preparation guidance.
  - Captures Storebox bind-mount pattern, privileged CT requirement for writable CIFS mounts, Tailscale preparation via `/dev/net/tun`, MTU `1400` requirement, and the “reload only, do not restart production PostgreSQL” warning.

## Bear CT 112: `jurisearch-admin`

Created and prepared Proxmox LXC CT `112` on bear.

Current intended role:

- Host pgAdmin now.
- Host future PostgreSQL backup service later.
- Store backup output on Storebox.

Configuration:

- Hostname: `jurisearch-admin`
- IP: `192.168.0.112/24`
- Bridge: `vmbr1`
- Gateway: `192.168.0.3`
- MTU: `1400`
- Rootfs: `local:32`
- Cores: `4`
- Memory: `8192 MB`
- Swap: `4096 MB`
- Tags: `admin;backups;jurisearch`
- Storebox mount inside CT: `/srv/jurisearch/storebox`
- Host-side Storebox mount: `/mnt/jurisearch-admin-storebox`
- LXC Tailscale readiness options:

```text
lxc.cgroup2.devices.allow: c 10:200 rwm
lxc.mount.entry: /dev/net/tun dev/net/tun none bind,create=file
```

Important prepared paths:

- `/srv/jurisearch/storebox/backups/postgres`
- `/srv/jurisearch/storebox/backups/pgadmin`
- `/srv/jurisearch/storebox/backups/tmp`
- `/var/lib/jurisearch-backups`
- `/var/log/jurisearch-backups`
- `/etc/jurisearch/admin-paths.env`
- `/root/README-jurisearch-admin.txt`

Base packages installed:

- SSH and common tools.
- PostgreSQL client tools.
- Compression/network diagnostics.

Backup service packages were not installed:

- No WAL-RUS service was installed.
- No WAL-G service was installed.

## Tailscale

Initial instruction was to prepare Tailscale via CT options only, not install it. Later, the user authenticated `jurisearch-admin` onto Tailscale outside this setup flow.

Observed Tailscale address:

- `100.92.29.114`

pgAdmin is bound to that Tailscale IPv4 address only.

## pgAdmin

Installed `pgadmin4-web` from the official pgAdmin APT repository.

Current URL:

- `http://100.92.29.114/pgadmin4`

Configured Apache:

- Apache is enabled and active.
- Apache listens on `100.92.29.114:80`.
- It does not listen on wildcard `0.0.0.0:80`.

pgAdmin account:

- Working login: `admin@jurisearch.dev`
- Password: stored root-only on CT 112 in `/root/pgadmin-bootstrap-credentials.txt`; not recorded in git.
- The earlier account `admin@jurisearch.local` exists, but the pgAdmin web login rejected that identifier. A conventional email-shaped account was added.
- Root-only credential note on CT: `/root/pgadmin-bootstrap-credentials.txt` with mode `0600`.

Verified:

- pgAdmin redirects to login at `/pgadmin4/login`.
- Apache service active/enabled.
- `admin@jurisearch.dev` exists as an active pgAdmin Administrator account.

## PostgreSQL Access For pgAdmin

Problem observed:

- pgAdmin could reach `192.168.0.110:5432` but timed out/failed from the UI.
- Direct `psql` from CT 112 showed the actual PostgreSQL error:

```text
FATAL: no pg_hba.conf entry for host "192.168.0.112", user "postgres", database "postgres", no encryption
```

Cause:

- CT 110’s PostgreSQL `pg_hba.conf` allowed CT 111 only:

```text
host    all    all    192.168.0.111/32    scram-sha-256
```

Change made on CT 110:

```text
host    all    all    192.168.0.112/32    scram-sha-256
```

Important production note:

- PostgreSQL was reloaded for `pg_hba.conf`.
- PostgreSQL was not restarted.

Verified from CT 112:

```text
current_user = postgres
current_database = postgres
client = 192.168.0.112
server = 192.168.0.110
```

pgAdmin connection settings that should work:

- Host: `192.168.0.110`
- Port: `5432`
- Maintenance DB: `postgres`
- Username: `postgres`
- Password: use the operator-held PostgreSQL bootstrap/admin password; not recorded in git.
- SSL mode: disabled if pgAdmin exposes that option.

## MTU Diagnosis And Fix

Problem:

- pgAdmin did not work well even after PostgreSQL access was allowed.
- Bear private bridge `vmbr1` has MTU `1400`.
- CT 112 and CT 110 were showing `eth0` MTU `1500` inside the guests.

Observed before fix:

- `bear vmbr1`: MTU `1400`
- CT 112 `eth0`: MTU `1500`
- CT 110 `eth0`: MTU `1500`
- CT 112 `tailscale0`: MTU `1280`
- DF ping from CT 112 to CT 110 only succeeded at payload `1372` and below, matching MTU `1400`.

Fix applied:

- CT 112 Proxmox `net0`: added/confirmed `mtu=1400`.
- CT 112 `/etc/network/interfaces`: added `mtu 1400` under `eth0`.
- CT 110 Proxmox `net0`: added/confirmed `mtu=1400`.
- CT 110 `/etc/network/interfaces`: added `mtu 1400` under `eth0`.
- Applied live with `ip link set dev eth0 mtu 1400`.

Important production note:

- PostgreSQL was not restarted for the MTU change.
- Only the network interface MTU was changed live and persistently.

Verified after fix:

- CT 112 `eth0`: MTU `1400`
- CT 110 `eth0`: MTU `1400`
- DF ping expected behavior:
  - payload `1372`: OK
  - larger than MTU budget: fails as expected
- Large PostgreSQL response from CT 112 to CT 110 succeeded:

```sql
select length(repeat('x', 2000000));
```

Result:

```text
2000000
```

## Current Known State

- CT 110: production PostgreSQL host at `192.168.0.110`.
- CT 111: update-server at `192.168.0.111`.
- CT 112: admin/pgAdmin/future-backup host at `192.168.0.112`, Tailscale `100.92.29.114`.
- Storebox backup root for CT 112: `/srv/jurisearch/storebox/backups`.
- pgAdmin URL: `http://100.92.29.114/pgadmin4`.
- pgAdmin login: `admin@jurisearch.dev`; password is stored root-only on CT 112.

## Follow-Ups

- Confirm pgAdmin UI behavior after MTU fix from the browser.
- Register the PostgreSQL server in pgAdmin using the settings above.
- When ready for backups, install and pilot WAL-RUS on/near the PostgreSQL host according to `work/13-backups/wal-rus-postgres-backups-analysis.md`.
- Before production backup automation, define retention, encryption, restore drill cadence, and alerting.
