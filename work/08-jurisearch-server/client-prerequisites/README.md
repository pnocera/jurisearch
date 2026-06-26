# Client prerequisites — persistent PostgreSQL 18 server on this workstation

This provisions **this machine** as a jurisearch package-distribution **client** per the design
(`../2026-06-26-central-ingest-package-distribution-design.md` §7.1 and the prerequisites doc E2/S1/S2):
a **persistent system PostgreSQL 18 service** with `pgvector` + `pg_search`, mirroring the bear
producer.

## Why not the CLI's embedded Postgres (pgembed)

The design runs a **long-running local service + the CLI concurrently over one persistent database**
(advisory-lock coordination, short critical sections). The current CLI's `ManagedPostgres` (pgembed)
spins up an **ephemeral single-owner cluster per index dir** and cannot host that. So the client needs
a real, always-on server — like bear.

> **The client software that uses this server (syncd + an external-DSN read path) does not exist yet.**
> This is the **environment** prerequisite, provisioned ahead of the package-distribution P-phase code.
> The current `jurisearch` CLI still uses pgembed and ignores this server until that code lands.

## `setup-client-postgres.sh`

Mirrors bear's `infra/bear-storage/{build-pg-search.sh,switch-to-pg18.sh}`, adapted to Fedora (dnf,
`postgresql-setup`, systemd) and to the **build-as-user / install-as-root** split (bear ran as root in
a container).

**Run it as your normal user — NOT `sudo`:**

```bash
./setup-client-postgres.sh
```

It builds the extensions as you (your Rust toolchain, the `~/Work/paradedb` fork, `~/.pgrx`) and
`sudo`s only the privileged steps; you enter your sudo password once (kept alive through the long
build).

What it does: non-destructive **preflight** (cargo-pgrx, pgvector tag, repo PG18 candidate) → removes
`pgadmin4-server` + `postgresql-contrib`, **swaps the PGDG `libpq5` for Fedora `libpq`** (see below),
and **upgrades the PostgreSQL stack to the repo's `18.3-7`** + installs `server-devel`/build deps
(`dnf install --best`, fails closed) → **purges `/var/lib/pgsql/data` and re-`initdb`s** (the clean
cluster) → builds `pg_search` (~20–40 min) and `pgvector` → sets `shared_preload_libraries='pg_search'`,
conservative tuning, **`postgres` password = `postgres`**, and scram password auth over loopback TCP →
creates the `jurisearch` database with both extensions → verifies.

### The pgadmin4 / libpq trade-off

This host had the **PGDG** `libpq5` installed (it owns `/usr/lib64/libpq.so*` and `pgadmin4-server`
hard-requires it), which **file-conflicts** with Fedora's `postgresql-private-devel` (needed to build
server extensions). PGDG does not package a full PostgreSQL *server* for Fedora — only that client lib —
so the host install can only proceed by dropping `pgadmin4-server` and replacing PGDG `libpq5` with
Fedora's `libpq` (which still provides `libpq.so.5` for `psql`, gdal, etc.). This was a chosen
trade-off; `pgadmin4` can be reinstalled later against Fedora `libpq`, or use another admin client
(`psql`, DBeaver, …). The alternative that keeps pgadmin4 is a **podman container** (isolated from the
host libpq), which also mirrors bear's containerized model.

### "Start clean" — why a fresh **cluster**, not a full package wipe

Fedora's repo currently ships only `postgresql-*-18.3-7`, the PG subpackages version-lock together, and
`postgresql-contrib-18.3-7` is **broken** (a Python 3.15 dependency conflict). A literal
remove-then-reinstall would drag in that broken contrib *after* the cluster is already gone and could
leave the machine with **no PostgreSQL**. So the script removes the unused `postgresql-contrib` (the
version-lock blocker), upgrades the server in place, and gets its clean slate by **purging + re-initdb'ing
the cluster** — the same end state (a brand-new PG18 cluster) without the brick risk. Nothing
destructive to the cluster happens until the package step has succeeded.

Resulting DSN: `host=127.0.0.1 port=5432 user=postgres password=postgres dbname=jurisearch`
(admin: `sudo -u postgres psql -d jurisearch`, local peer, no password).

Tunables (env): `PARADEDB_DIR`, `PGVECTOR_REF` (default `v0.8.3`), `JURISEARCH_PG_SUPERUSER_PASSWORD`
(default `postgres`), `JURISEARCH_APP_DB` (default `jurisearch`).

## Version parity vs bear

| | this client | bear (producer) |
|---|---|---|
| PostgreSQL | 18.3 (Fedora distro, `-7` rebuild) | 18.4 |
| pgvector | 0.8.3 (built here) | 0.8.3 |
| pg_search | 0.24.1 (built here) | 0.24.1 |

Same **major** 18; extension ABI is stable within a major and both extensions are built against the
exact running server, so a 18.3-vs-18.4 minor delta is design-acceptable (S3: the logical package path
is major-version-flexible). If exact 18.4 is later required, install PG 18.4 from PGDG instead of the
distro package and re-run.
