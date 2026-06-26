# Infra — provisioning `bear` as the jurisearch PG18 server

Date: 2026-06-26
Host: **bear** (Hetzner, AMD EPYC 7502P 32c/64t, 251 GB RAM, single 3.84 TB Intel NVMe), on the
`tail0cb6c3` tailnet (`100.102.96.111`). Proxmox VE host.

This folder is a **snapshot** of the infra scripts + their Codex reviews used to (a) upgrade bear's
Proxmox and (b) stand up a standalone PostgreSQL 18 server in an LXC and load the existing jurisearch
corpus into it. Working copies live at `~/proxmox-upgrade` and `~/bear-storage` on the workstation;
this is the tracked record. Every script was driven script→Codex-review→GO→run; all run detached via
`systemd-run` so SSH/tailscale drops can't interrupt them, and all are fail-closed with backups.

## What was done, in order

### `proxmox-upgrade/pve9-upgrade.sh` — Proxmox 8.4 → 9.2 (DONE)
Staged upgrade of bear: `phase-a` (patch to 8.4.19) → reboot → `precheck` (`pve8to9 --full`, 0 failures)
→ `phase-b` (Debian 12 bookworm → 13 trixie, repos switched, dist-upgrade) → reboot.
Result: **PVE 9.2.3, kernel 7.0.12-1-pve, Debian 13**. Reviews: r1 (FIXES_REQUIRED) → r2 (GO).
Also disabled the post-upgrade `pve-enterprise.sources` (401) and re-enabled the Tailscale repo on trixie.

### `bear-storage/` — storage + the PG18 server

1. **`bear-disk-merge.sh`** (DONE) — reclaimed the empty 1.5 TB `/home` partition into `/`, giving one
   **~3.5 TB `local`** storage (was a 2 TB `/` + 1.5 TB `/home` split that capped the DB). Also removed
   the orphaned `home` PVE storage. (Earlier, the dead `quasar` Proxmox node + 8 orphaned guest disks
   were cleaned up, restoring cluster quorum.) Reviews: r1 → r2 (GO).

2. **`create-jurisearch-lxc.sh`** (DONE) — created **CT 110 `jurisearch`**: Debian 13, 16c/64 GB,
   20 GB rootfs (later grown to 50 G for the build) + a **1 TB PG data mountpoint** at
   `/var/lib/postgresql`, static `192.168.0.110` on `vmbr1`, **bridge-only (no tailscale)**, unprivileged.
   Reviews: r1 → r2 (GO).

3. **`switch-to-pg18.sh`** (DONE) — the corpus is a **PG18** data dir, so the LXC targets **PG18**
   (Debian 13 ships PG17 only → PGDG repo). Drops the empty PG17 cluster, installs **PostgreSQL 18.4**
   + **pgvector 0.8.3** (PGDG). Note: upstream pgvector 0.8.0 can't compile on PG18; 0.8.x shares the
   IVFFlat/HNSW on-disk format, so 0.8.3 reads the 0.8.0 source indexes with no reindex. Reviews:
   r1 → r2 → r3 (GO). (parameterised; `build-pg-search.sh` is run for PG18 next.)

4. **`build-pg-search.sh`** (DONE) — builds **ParadeDB `pg_search` 0.24.1** from the
   `pnocera/paradedb` fork via `cargo-pgrx 0.18.1` (Rust 1.96.0) against the system PG — no Debian
   package exists. Parameterised by `PGVER` / `EXT_FEATURES` (run with `PGVER=18 EXT_FEATURES=pg18,deferred_wal`).
   Sets `shared_preload_libraries=pg_search`, verifies `CREATE EXTENSION vector + pg_search`. Reviews:
   r1 (2 BLOCKERs incl. needing `cargo pgrx init`) → r2 (GO). Verified: **pg_search 0.24.1, vector 0.8.3**.

5. **`load-corpus.sh`** (DONE — run on the bear host) — physically copied the **165 GB PG18 corpus**
   (rsync'd from the workstation's `/mnt/models/jurisearch-index/phase2-full-juridic/pg/data` to a bear
   staging dir) over CT 110's empty `18/main`: read-only bind-mount of the staging → assert PG18 down →
   `rsync --delete` → generate the corpus's libc locale → neutralise `postgresql.auto.conf` to just the
   pg_search preload → start PG18 → verify (`'[1,2,3]'::vector` + `pg_search`) → `REFRESH COLLATION
   VERSION`. The fedora source is never touched. Reviews: r1 (3 BLOCKERs) → r2 (1 BLOCKER regression) →
   r3 (GO).
   **Result: `jurisearch` DB = 163 GB, all indexes intact (232 btree, 2 ivfflat, 2 bm25, 1 gin) — no
   rebuild.** Tables: chunk_embeddings 61 GB/4.6 M, chunks 31 GB/4.7 M, documents 31 GB/2.5 M,
   graph_edges 26 GB/18.2 M, + zone_units/embeddings, official_api_responses, citations.
   **Two lessons baked into the script afterward** (the first two runs failed *before* any destructive
   step and were trivially recovered): (a) the minimal Debian LXC lacks `rsync` (now a precondition);
   (b) PG18 refuses to start unless the corpus's libc locale (here `en_US.UTF-8`) is generated in the
   container — the script now reads `LC_COLLATE`/`LC_CTYPE` from `pg_controldata` and `locale-gen`s them
   before starting.

## Final state
- **bear**: PVE 9.2.3, one ~3.5 TB `local`, `storebox` (Hetzner CIFS) for backups.
- **CT 110 `jurisearch`**: Debian 13, **PostgreSQL 18.4 + pgvector 0.8.3 + pg_search 0.24.1**, the
  corpus on a 1 TB mountpoint, reachable at `192.168.0.110` on `vmbr1` (bridge-only).
- The jurisearch **service is not deployed** (it doesn't exist yet) — this is the DB server only.

## Notes / follow-ups
- Optional after load: `ALTER EXTENSION vector UPDATE;` to sync the catalog from 0.8.0 → 0.8.3.
- Remove the staging bind-mount when done: `pct set 110 --delete mp1 && pct reboot 110 && rm -rf /root/jurisearch-staging`.
- No scheduled `vzdump` backup job exists yet for CT 110 → storebox (recommended).
