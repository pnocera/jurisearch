# Spike C — on-host execution under the dashboard identity (CT 111)

De-risking spike: prove the dashboard's runtime operations work **as the eventual
dashboard identity** (`User=jurisearch`, uid 999 / gid 991) on CT 111, so M2–M5 can't
go green over fixtures while the real box fails.

- **Date:** 2026-06-30
- **Target:** CT 111 — `100.71.35.39`, hostname `jurisearch-update`, x86_64, glibc 2.41
- **Identity today:** `uid=999(jurisearch) gid=991(jurisearch) groups=991(jurisearch)` — NOT in `systemd-journal`.
- **`systemd-journal` group:** exists (`systemd-journal:x:999:`), jurisearch not a member.
- **Tailnet addr:** `tailscale0` → `100.71.35.39/32`.
- Raw transcript: `probes.txt` (same directory).

All probes were **transient** — no permanent group-add, no installed unit; `/tmp/spikeC` removed afterward.

## Probe results

| # | Probe | Command | Exit | Outcome |
|---|-------|---------|------|---------|
| 1 | journalctl as jurisearch, **no** journal group | `runuser -u jurisearch -- journalctl -u jurisearch-producer-legislation.service -o json -n 1` | **1** | **FAIL** — `No journal files were opened due to insufficient permissions.` |
| 2 | journalctl as jurisearch **with** systemd-journal group | `runuser -u jurisearch -g systemd-journal -- journalctl -u jurisearch-producer-legislation.service -o json -n 1` | **0** | **SUCCESS** — returns a JSON journal line. Group fixes it. |
| 3 | systemctl list-timers as jurisearch (plain) | `runuser -u jurisearch -- systemctl list-timers 'jurisearch-producer-*' -o json` | **0** | **SUCCESS plain** — JSON array of both producer timers, no extra group needed. |
| 4 | Bun.serve tailnet bind as jurisearch | `runuser -u jurisearch -- /tmp/spikeC/jurisearch-spikeC` then `curl http://100.71.35.39:18082/` | **0** | **SUCCESS** — `HTTP 200`, body `spikeC ok`. Socket bound to `100.71.35.39:18082` only; loopback refused (curl exit 7). |

Probe 4 binary built locally: `bun build server.ts --compile --target=bun-linux-x64 --outfile jurisearch-spikeC`
(the box has no Bun); `server.ts` binds `hostname: "100.71.35.39", port: 18082`.

## Conclusions

**(a) Journal access REQUIRES `SupplementaryGroups=systemd-journal` — LOCK INTO M6b.**
Probe 1 (no group) fails with insufficient permissions; probe 2 (with `systemd-journal`)
succeeds and returns journal JSON. The dashboard unit MUST set
`SupplementaryGroups=systemd-journal` (and/or `deploy.sh` must add the group). Producer
units today render `User/Group=jurisearch` with no journal access (`render.rs:55-56`) and
`deploy.sh` does not add the group (`deploy.sh:294-300`) — so without this the dashboard's
log-tail feature will silently see an empty journal on the real box. This is now a verified
deploy requirement, not a deploy-time discovery.

**(b) `systemctl list-timers` does NOT need the group.**
Probe 3 works as plain unprivileged jurisearch (read-only D-Bus query to the systemd
manager is unrestricted). No extra privilege required for timer listing. (Note: this only
covers read-only listing; start/stop/trigger of units would need polkit/privilege and is
out of scope for this spike.)

**(c) Tailnet bind as jurisearch works → M6b bind verify is feasible.**
Probe 4 proves an unprivileged-identity `Bun.serve` can bind the real `tailscale0` address
`100.71.35.39:18082` and serve 200 on the live box. Port 18082 (>1024) needs no extra
capability. The socket listened on `100.71.35.39:18082` ONLY — loopback `127.0.0.1:18082`
was refused — confirming an explicit-hostname bind is exact. The dashboard's **fail-closed
guard must REFUSE a `0.0.0.0`/wildcard bind config** and require an explicit tailnet
hostname; `0.0.0.0` was deliberately NOT exposed in this spike.

## Net requirements locked for M6b

1. Dashboard systemd unit: `User=jurisearch` + **`SupplementaryGroups=systemd-journal`** (required for log tail). `deploy.sh` must ensure the group membership.
2. No extra privilege needed for `systemctl list-timers` read-only listing.
3. Bind config must be an explicit tailnet address (e.g. `100.71.35.39`); the runtime guard must reject `0.0.0.0`/wildcard (fail-closed). Tailnet bind on the real interface is proven working as the unprivileged identity.
