# Make JuriSearch simple to deploy - implementation plan

Date: 2026-06-29
Scope: plan only. This document turns the work/09 site-server + thin-client result into an
operator-friendly deployment product. It does not redesign the query protocol, storage model, sync
semantics, or trusted-LAN security decision from work/09.

The current software shape is strong enough: `jurisearch serve-site`, `jurisearch-syncd run`, the
local bge-m3 endpoint, the system PostgreSQL roles, and `jurisearch-client` already define the
runtime. The weak point is the operator surface: the person deploying it still has to know too many
flags, files, paths, service ordering rules, and readiness preconditions.

This plan makes deployment boring: one config file, one admin surface, idempotent commands, generated
units/env files, explicit preflight failures, and a smoke test that only passes when a real site can
answer a real client.

---

## Target operator experience

### Site host

The happy path on host S should be:

```sh
sudo jurisearchctl site init --config /etc/jurisearch/site.toml
sudo jurisearchctl site provision-db --config /etc/jurisearch/site.toml
sudo jurisearchctl site doctor --config /etc/jurisearch/site.toml
sudo jurisearchctl site install --config /etc/jurisearch/site.toml --no-start
sudo jurisearchctl site bootstrap-trust --config /etc/jurisearch/site.toml
sudo jurisearchctl site catch-up --config /etc/jurisearch/site.toml --wait
sudo jurisearchctl site readiness --config /etc/jurisearch/site.toml
sudo jurisearchctl embed doctor --config /etc/jurisearch/site.toml
sudo systemctl enable --now jurisearch-bge-m3 jurisearch-syncd
sudo systemctl enable --now jurisearch-site
jurisearchctl site smoke --config /etc/jurisearch/site.toml --fetch-id '<known-id>'
```

`site provision-db` owns database creation, extensions, roles, and migrations; `site install` owns
derived files and systemd lifecycle. If a privileged prerequisite is missing, the responsible command
must stop with the exact command or SQL file to run next, not a generic "see docs" failure. The query
service must not be started for clients until `site readiness` exits zero against an active,
readiness-stamped corpus and, for embedder-configured sites, `embed doctor` exits zero.

### Thin client host

The happy path on host C should be:

```sh
jurisearch-client configure --server tcp://<site-host>:8099
jurisearch-client status
jurisearch-client search '{"query":"responsabilite","mode":"bm25","kind":"decision"}'
```

The client remains thin. It stores only the site URL in an XDG config file, while `--server`,
`--local`, and `JURISEARCH_SITE_URL` continue to work.

### Local-only demo mode

There should also be a single-host demo path for proving the product on a workstation or VM:

```sh
jurisearchctl demo up --config ./demo-site.toml
SITE_URL="$(jurisearchctl demo url --config ./demo-site.toml)"
jurisearch-client --server "$SITE_URL" status
jurisearchctl demo smoke --config ./demo-site.toml --fetch-id '<demo-id>'
jurisearchctl demo down --config ./demo-site.toml
```

This is not a fake in-memory mode. It starts the same binaries and exercises the same site protocol;
the only shortcut is that producer, site, and client are collapsed onto one host. The demo command must
bind a socket owned by the invoking user, or print the exact `unix://`/`tcp://` URL to use; it must not
rely on `jurisearch-client --local` unless it binds exactly the `$XDG_RUNTIME_DIR/jurisearch-site.sock`
path that `--local` resolves. `demo up` must apply a small fixture corpus or a configured package root
so `demo smoke` can run real status/fetch/search legs; a status-only demo is not sufficient product
proof. The bundled fixture must expose a documented known id for the `--fetch-id <demo-id>` smoke leg.
If the demo enables hybrid search, it must also start a loopback bge-m3 endpoint and require the
model/tokenizer assets to be present or fetched via `jurisearchctl embed fetch-assets`; otherwise the
hybrid leg is skipped only with an explicit recorded reason.

---

## Current bear infrastructure snapshot

Captured on 2026-06-29. This is the current bootstrap environment for implementation and testing, not
the hardened product contract. Hardening, credential rotation, least-privilege PostgreSQL roles, and
non-root service users are future work.

### Access

- Proxmox host: `bear`
- Proxmox Web UI: `https://bear.tail0cb6c3.ts.net:8006`
- SSH: `root@bear.tail0cb6c3.ts.net`
- Current bootstrap password for `root`: `20Sense20`
- Current bootstrap PostgreSQL superuser credential: `postgres / postgres`
- Network assumption: tailnet/private-LAN only for this bootstrap stage.

### Bear host

- Proxmox VE: `9.2.3`
- Tailscale address: `100.102.96.111`
- Private guest bridge: `vmbr1`, host/gateway `192.168.0.3/24`, MTU `1400`
- Public bridge: `wan0`, host public address `142.132.159.90/26`
- Storebox mount on bear: `/mnt/pve/storebox` (`//u465033.your-storagebox.de/backup`, about 10 TB)
- Active APT sources are `trixie`/`trixie-security` plus Proxmox `trixie pve-no-subscription` and
  Tailscale `trixie`; stale `bookworm` repo files were moved under
  `/root/apt-sources-disabled-20260629-105227`.

### Current LXC layout

| VMID | Hostname | IP | Role | Notes |
|---|---|---|---|---|
| `101` | `nats1` | `192.168.0.101` | NATS/supporting service | Existing CT, on `vmbr1`. |
| `107` | `postgresql` | `192.168.0.107` | Older PostgreSQL CT | Debian 12 / PostgreSQL 17 helper CT; not the target JuriSearch PG18 database. |
| `110` | `jurisearch` | `192.168.0.110` | JuriSearch PostgreSQL database host | PostgreSQL 18, 48 cores, 192 GB RAM, 1 TB `/var/lib/postgresql` mount. |
| `111` | `jurisearch-update` | `192.168.0.111` | Update-server / producer orchestration host | Lightweight CT: 2 cores, 4 GB RAM, 1 GB swap, 32 GB rootfs. |

### Database guest: CT 110 `jurisearch`

- PostgreSQL cluster: `18/main`, database `jurisearch`
- Current bootstrap DSN from CT 111:
  `host=192.168.0.110 port=5432 dbname=jurisearch user=postgres password=postgres sslmode=disable`
- PostgreSQL listens on `127.0.0.1`, `::1`, and `192.168.0.110`.
- `pg_hba.conf` currently allows CT 111 only for remote private-LAN access:
  `host all all 192.168.0.111/32 scram-sha-256`
- Config backup before enabling private-LAN access:
  `/root/pg18-network-backup-20260629-090440` inside CT 110.

### Update-server guest: CT 111 `jurisearch-update`

CT 111 is the intended host for producer/update orchestration. It should download official legal-source
archives to Storebox and drive ingest/publish work against the JuriSearch PostgreSQL database on CT
110. It should not store large legal downloads, corpus packages, vector indexes, database data, model
weights, or tokenizer files on its 32 GB root disk.

- SSH: `root@192.168.0.111` via bear/private LAN
- Current bootstrap password for `root`: `20Sense20`
- LXC mode: privileged. The first unprivileged attempt could read the CIFS-backed Storebox bind mount
  but could not write to it as root inside the CT.
- Storebox host path: `/mnt/pve/storebox/jurisearch-update`
- Storebox path inside CT: `/srv/jurisearch/storebox`
- Storebox capacity visible inside CT: about 10 TB
- Storebox subdirectories:
  - `/srv/jurisearch/storebox/archives`
  - `/srv/jurisearch/storebox/packages`
  - `/srv/jurisearch/storebox/manifests`
  - `/srv/jurisearch/storebox/tmp`
  - `/srv/jurisearch/storebox/state`
  - `/srv/jurisearch/storebox/logs`
- Convenience symlinks:
  - `/srv/jurisearch/archives -> /srv/jurisearch/storebox/archives`
  - `/srv/jurisearch/packages -> /srv/jurisearch/storebox/packages`
  - `/srv/jurisearch/manifests -> /srv/jurisearch/storebox/manifests`
- Bootstrap env file: `/etc/jurisearch/producer-paths.env`
- In-CT note: `/root/README-jurisearch-update-server.txt`
- Installed baseline tools: `openssh-server`, `curl`, `wget`, `rsync`, `jq`, `zstd`, `xz-utils`,
  `unzip`, `gnupg`, `postgresql-client`, `iproute2`, `iputils-ping`, `netcat-openbsd`.
- Validated:
  - Storebox writes from inside CT 111 work.
  - `apt -s full-upgrade` reports no pending changes.
  - DILA download host `https://echanges.dila.gouv.fr/` is reachable.
  - CT 111 can connect to CT 110 PostgreSQL with `sslmode=disable`.

The current `/etc/jurisearch/producer-paths.env` shape is:

```sh
JURISEARCH_ARCHIVES_DIR=/srv/jurisearch/storebox/archives
JURISEARCH_PACKAGES_DIR=/srv/jurisearch/storebox/packages
JURISEARCH_MANIFESTS_DIR=/srv/jurisearch/storebox/manifests
JURISEARCH_DOWNLOAD_TMP_DIR=/srv/jurisearch/storebox/tmp
JURISEARCH_PRODUCER_STATE_DIR=/var/lib/jurisearch-producer
JURISEARCH_PRODUCER_LOG_DIR=/var/log/jurisearch-producer
JURISEARCH_POSTGRES_HOST=192.168.0.110
JURISEARCH_POSTGRES_PORT=5432
```

---

## Non-goals

- No Kubernetes, Helm, Docker Compose, or cloud service shape in this phase.
- No internet-exposed service and no new client authentication. The work/09 trusted LAN / Tailscale
  assumption remains the product boundary.
- No HTTP/gRPC transport. The versioned JSONL site protocol remains the deployment protocol.
- No attempt to bundle PostgreSQL, `pgvector`, `pg_search`, or `llama-server` into the Rust release.
  The deploy tool may install/check them per supported OS later, but the first version treats them as
  host prerequisites. Release artifacts also do not contain database contents, corpus package data,
  vector indexes, model weights, or tokenizer files; those large/runtime assets are provisioned or
  fetched separately and referenced by config/manifests.
- No hidden external embedding API fallback. The default site deployment uses a local bge-m3 endpoint.
  This is a confidentiality boundary: site query text may contain privileged client/matter details and
  must not leave the customer network. Producer-side document embedding is different; it happens
  upstream over public legal-source text and is covered by the producer automation plan. In these
  plans, "server-side embeddings" means that producer/update-ingest path, not the customer-facing site
  query path.

---

## Current deployment pain

1. **No single source of truth.** The same topology is repeated across `/etc/jurisearch/site.env`,
   `/etc/jurisearch/syncd.env`, `/etc/jurisearch/bge-m3.env`, service files, acceptance scripts, and
   client env vars.
2. **Manual systemd install.** The checked-in units say "copy this unit" and "create the env file";
   they do not render from a validated config, set permissions, or account for path directives that
   cannot expand env vars.
3. **Database provisioning is not a product command.** Operators need a database, extensions, roles,
   grants, migrations, and read-role visibility before the services are useful.
4. **The bge-m3 prerequisite is underspecified.** The site service needs a local endpoint plus matching
   tokenizer/model/fingerprint settings, but there is no deploy-time probe that proves the endpoint is
   compatible before systemd starts looping.
5. **Trust and subscription bootstrap are disconnected from service install.** `jurisearch-syncd`
   exposes `trust`, `subscribe`, `update`, and `run`, but deployment has no guided sequence around
   anchors, license tokens, package roots, and the first catch-up.
6. **Acceptance can be skipped too easily.** The work/09 single-host script correctly refuses to
   fabricate data legs, but the product still lacks a command that says "this site is deployable now"
   and fails if it is not.
7. **Thin client setup is env-var-first.** `JURISEARCH_SITE_URL` works, but a user should not need shell
   profile editing to point a thin client at the site.

---

## Product decisions

### Add `jurisearchctl`

Introduce a small admin binary, `jurisearchctl`, rather than overloading the query CLI. It owns deploy
orchestration and intentionally depends on operational crates and templates. The shipped artifacts
become:

- `jurisearch`: local CLI plus `serve-site`;
- `jurisearch-syncd`: package consumer and daemon;
- `jurisearch-client`: thin query client;
- `jurisearch-package`: producer/package tooling;
- `jurisearchctl`: deployment, provisioning, config rendering, doctor, smoke, and demo commands.

`jurisearchctl` may initially live in `crates/jurisearch-deploy` with a `[[bin]]` named
`jurisearchctl`.

### One config file

Define `/etc/jurisearch/site.toml` as the operator-owned source of truth. Generated files under
`/run` or `/etc/jurisearch/generated` are derived artifacts and may be overwritten.

Minimum shape:

```toml
[system]
service_user = "jurisearch"
service_group = "jurisearch"
install_dir = "/usr/local/bin"
config_dir = "/etc/jurisearch"
runtime_dir = "/run/jurisearch"
state_dir = "/var/lib/jurisearch"

[site]
bind = "tcp://100.100.20.30:8099"
workers = 8
allow_lan = true

[database]
host = "127.0.0.1"
port = 5432
name = "jurisearch"
admin_user = "postgres"
admin_database = "postgres"
admin_password_file = "/etc/jurisearch/secrets/postgres-admin-password"
writer_user = "jurisearch_write"
read_user = "jurisearch_read"
owner_role = "jurisearch_owner"

[sync]
source_root = "/srv/jurisearch/packages"
corpora = ["core"]
interval_secs = 30

[[trust.anchor]]
purpose = "package"
key_id = "producer-k1"
key_epoch = 1
public_key_hex = "<hex>"
algorithm = "ed25519"

[[trust.anchor]]
purpose = "license"
key_id = "license-k1"
key_epoch = 1
public_key_hex = "<hex>"
algorithm = "ed25519"

[license]
token_json = "/etc/jurisearch/license-token.json"

[embedder]
provider = "openai_compatible"
base_url = "http://127.0.0.1:8081"
model_name = "bge-m3"
dimension = 1024
normalize = true
pooling = "cls"
llama_server = "/usr/local/bin/llama-server"
model_path = "/srv/jurisearch/models/bge-m3-Q8_0.gguf"
tokenizer_json = "/srv/jurisearch/models/bge-m3-tokenizer.json"
port = 8081
```

Secrets must not be printed in normal output. If passwords are supported, they should live in a
separate `0600` file or systemd credential, not inline in world-readable TOML.
The admin/bootstrap connection uses peer/ident, `.pgpass`, a systemd credential, or the optional
`database.admin_password_file`; the password itself is never stored inline in `site.toml`.

Trust anchors and license tokens are operator inputs received from the producer/license issuer. A
configured license token requires a `license`-purpose trust anchor before it can be verified and
installed. Package key rotation is represented by adding another `[[trust.anchor]]` entry; replacing an
existing anchor is an explicit operator action.

### Generated runtime files

`jurisearchctl site render` writes:

- `/etc/jurisearch/generated/site.env`;
- `/etc/jurisearch/generated/syncd.env`;
- `/etc/jurisearch/generated/bge-m3.env`;
- `/etc/systemd/system/jurisearch-site.service`;
- `/etc/systemd/system/jurisearch-syncd.service`;
- `/etc/systemd/system/jurisearch-bge-m3.service`.

The generated systemd units must contain absolute paths for directives such as `ReadOnlyPaths`; they
must not rely on environment expansion where systemd does not support it.

### Embedding placement

The site-local embedder is for **query-time embedding only**. The thin client does not embed; it sends a
versioned site request to `jurisearch-site`, and `jurisearch-site` embeds the user's query through the
local loopback bge-m3 endpoint. This is non-negotiable for confidentiality: customer query text may be
privileged and must not be sent to OpenRouter or any other external embedding provider.

Document/chunk embeddings are produced upstream by the producer pipeline and shipped inside signed
packages. `jurisearch-syncd` applies those packages to the site database; it does not call OpenRouter
or any other embedding API during catch-up. The producer/update-ingest service may use fast external
embedding for public legal-source documents because those inputs are not customer-confidential queries.

Rendering translates `site.bind` into the existing runtime flags: `tcp://host:port` becomes
`serve-site --tcp host:port`, while `unix:///absolute/path` becomes `serve-site --socket
/absolute/path`. The generated env files also split the embedder config into the two existing env
families: `JURISEARCH_EMBED_*` for `serve-site` and `JURISEARCH_BGE_M3_*` for the local
`llama-server` unit. `JURISEARCH_EMBED_POOLING` is rendered for `serve-site`, and the generated
bge-m3 unit renders the same value into the `llama-server --pooling` flag. The effective storage
fingerprint is derived by the embedder from model, dimension, and normalization only; pooling configures
the endpoint but is not part of the storage-fingerprint comparison. In this phase `pooling` is fixed to
`cls`; non-`cls` configurations are rejected by explicit deploy validation rather than inferred from the
storage fingerprint.

---

## Phase 1 - Config schema, rendering, and validation

- **Goal.** Establish one validated deployment config and generate the files current units expect.
- **Builds on.** Existing `deploy/systemd/*.service`, `ServeSiteArgs`, `jurisearch-syncd` shared-server
  flags, and bge-m3 env requirements.
- **Deliverables.**
  - `crates/jurisearch-deploy` with a strict `SiteConfig` parser.
  - `jurisearchctl site init --config <path>` to create parent directories and write a commented
    template if the file does not already exist.
  - `jurisearchctl site config-example` to print a complete commented TOML template.
  - `jurisearchctl site validate --config <path>` for structural checks only.
  - `jurisearchctl site render --config <path> --output-root <dir>` for dry-run rendering.
  - Golden tests for the rendered env files and systemd units.
- **Validation rules.**
  - `site.bind` must be `tcp://host:port` or `unix:///absolute/path`.
  - Non-loopback TCP requires `allow_lan = true`; wildcard binds require a second explicit flag.
  - `system.*_dir`, `sync.source_root`, model path, tokenizer path, and binary paths must be
    absolute.
  - `sync.corpora` must be non-empty and must correspond to corpora the configured producer actually
    publishes. For the current v1 producer this means `core` only; a missing producer manifest for any
    configured corpus is a distinct doctor/catch-up failure, not a generic network failure.
  - `database.admin_user` and `database.admin_database` must be present because external PostgreSQL
    provisioning needs a bootstrap connection before the target DB/roles necessarily exist.
  - Any configured `database.*_password_file` path must be absolute, owned by root or the configured
    service user, and not world-readable.
  - Every `[[trust.anchor]]` must have purpose `package` or `license`; at least one `package` anchor is
    required, and a configured `[license]` requires at least one `license` anchor.
  - Embedder provider, base URL, served model name, dimension, normalization, pooling, model path, and
    tokenizer path must be internally consistent enough to render both env families deterministically.
    `embedder.base_url` is always required to resolve to loopback (`localhost`, `127.0.0.0/8`, or
    `::1`) for site deployments. If `base_url` and `port` are both set, they must name the same
    loopback port. A site config pointing query embeddings at OpenRouter or any other non-loopback
    provider is rejected before any file is written.
  - `embedder.pooling` must be `cls` in this phase; supporting any other pooling mode requires package
    metadata that can be checked independently of `storage_embedding_fingerprint()`.
  - `database.read_user`, `writer_user`, and `owner_role` must be distinct unless an explicit
    `unsafe_single_role = true` test-only flag is set.
  - Generated env files are `0600`; generated units are `0644`.
- **Invariants under test.**
  - The same TOML renders all three env files and all three units.
  - Rendering is deterministic.
  - Golden rendering pins both bind translations, the full `JURISEARCH_EMBED_*` /
    `JURISEARCH_BGE_M3_*` blocks, and the generated `llama-server --pooling` flag.
  - Secrets are redacted from logs and `Debug` output.
  - Invalid network exposure fails before any file is written.
- **Done when.** An operator can run `jurisearchctl site config-example > site.toml`, edit it, and
  render deployable files into a temp directory without touching `/etc`.

---

## Phase 2 - Host doctor and prerequisite probes

- **Goal.** Give operators one command that explains exactly what is missing before install.
- **Builds on.** Phase 1 config parser; current serve-site and syncd flag surfaces.
- **Deliverables.**
  - `jurisearchctl site doctor --config <path>`.
  - Machine-readable `--json` output for CI/runbooks.
  - Separate checks for OS tools, PostgreSQL, extensions, roles, package source, trust/license,
    embedder, ports/sockets, and service files.
- **Checks.**
  - Required binaries exist: `jurisearch`, `jurisearch-syncd`, `jurisearch-client`, `llama-server`.
  - PostgreSQL is reachable as the intended admin/bootstrap, owner, writer, and read identities.
  - `pgvector` and `pg_search` are installed in the target database.
  - Read role cannot write; the admin/bootstrap identity can run migrations; the writer role can perform
    sync/apply work and activation visibility grants.
  - Package source contains `<root>/<corpus>/manifest.json` for every configured corpus.
  - Package trust anchors are present; when a license token is configured, a license-purpose anchor is
    present and the token input is parseable.
  - bge-m3 endpoint can be started or reached on loopback and returns vectors of the configured
    dimension.
  - Site bind address is free and permitted by config.
  - The active corpus/readiness state is either ready or reported as "not yet caught up" with the
    exact sync command to run.
- **Invariants under test.**
  - Doctor does not mutate the database or filesystem except for optional temp files.
  - Every failed check has a stable code, a human message, and a suggested next command.
  - A missing DB and a stale readiness stamp are distinct failures.
  - Pre-bootstrap doctor can exit zero with advisory "not yet bootstrapped" statuses for absent trust
    rows and active corpora when config/package inputs are valid; post-bootstrap readiness and smoke
    remain the hard serving gates.
  - Any doctor-started embedder endpoint is stopped before exit, or skipped when the managed unit is
    already active, so the subsequent systemd service can bind the configured port.
- **Done when.** Running doctor on a fresh machine produces a short ordered list of missing
  prerequisites; running it after DB provisioning but before trust/catch-up exits zero only when the
  remaining items are advisory bootstrap statuses; running it on a fully bootstrapped site exits zero.

---

## Phase 3 - Idempotent database provisioning

- **Goal.** Turn "create a correct site PostgreSQL" into an explicit product command.
- **Builds on.** work/09 shared-server roles and activation visibility requirements. Running
  migrations against an operator-owned external PostgreSQL is new capability in this phase; today the
  migration runner is tied to `ManagedPostgres`.
- **Deliverables.**
  - `jurisearchctl site provision-db --config <path>`.
  - `--dry-run-sql <file>` mode for operators who need a DBA to review/apply SQL.
  - A connection-based migration applier that runs the static storage migrations against an external
    PostgreSQL using the configured admin/bootstrap identity, independent of `ManagedPostgres`.
  - Migration and role/grant checks that can be rerun safely.
- **Responsibilities.**
  - Create the database if requested and privileges allow it.
  - Create or validate roles: owner, writer, read.
  - Install/validate extensions: `pgvector`, `pg_search`; if extension creation needs superuser or
    platform-specific privileges, emit the exact DBA command and stop.
  - Run storage migrations as the admin/bootstrap identity, not as the read role and not implicitly as
    the sync writer.
  - Apply baseline grants on control schemas/views.
  - Verify the activation-time grant path by creating a temporary generation-like schema and checking
    read-role visibility, then cleaning it up.
- **Invariants under test.**
  - Rerunning provision-db is safe.
  - Read role cannot `INSERT`, `UPDATE`, `DELETE`, `CREATE`, or `ALTER`.
  - Writer role can perform the operations syncd needs, including activation visibility grants, but
    does not need superuser or migration ownership after provisioning.
  - A failed provisioning step leaves a clear checkpoint and can be retried.
- **Done when.** A blank supported PostgreSQL instance can be converted into a ready JuriSearch site DB
  with one command or one generated SQL script.

---

## Phase 4 - Service installation and lifecycle

- **Goal.** Install the three services from config without hand-copying units or env files.
- **Builds on.** Phase 1 rendering; existing systemd units.
- **Deliverables.**
  - `jurisearchctl site install --config <path>`.
  - `jurisearchctl site uninstall --config <path>` that disables/removes generated services only after
    confirmation.
  - `jurisearchctl site restart`, `stop`, `logs`, and `status` wrappers over systemd/journalctl.
- **Install behavior.**
  - Assume `site provision-db` has already succeeded; if the DB is not provisioned, stop with the
    `jurisearchctl site provision-db --config <path>` command rather than trying to migrate implicitly.
  - Create `/etc/jurisearch` and generated subdirectories with correct ownership and permissions.
  - Create the configured service user/group when absent, unless `--no-user-management` is set.
  - Render env files and units.
  - Run `systemctl daemon-reload`.
  - Enable services in dependency order.
  - With `--no-start`, start nothing. Without it, starting prerequisites is allowed, but starting
    `jurisearch-site` must be refused until `site readiness` exits zero and, for embedder-configured
    sites, `embed doctor` exits zero, unless the operator passes an explicit force flag.
  - Refuse to overwrite locally modified generated files unless `--force` or the previous render hash
    matches.
- **Invariants under test.**
  - Generated units use absolute paths and do not depend on unsupported env expansion.
  - A local-only UDS deployment and a LAN TCP deployment render different, correct units.
  - `site install --dry-run` shows all writes without performing them.
  - Reinstalling after a config change updates only derived files.
  - `site uninstall` never drops the operator database, corpus data, package source, model files, or
    operator-owned `site.toml`; it only disables/removes generated units/env files after confirmation.
- **Done when.** The manual instructions in `deploy/systemd/*.service` can be replaced by
  `jurisearchctl site install`.

---

## Phase 5 - Trust, subscription, first catch-up, and readiness

- **Goal.** Make "the site has a corpus and can answer" a guided deploy step, not a hidden runbook.
- **Builds on.** Existing `jurisearch-syncd trust`, `subscribe`, `update`, `run`, and `status`.
- **Deliverables.**
  - `jurisearchctl site bootstrap-trust --config <path>`.
  - `jurisearchctl site catch-up --config <path> [--wait]`.
  - `jurisearchctl site readiness --config <path>`.
- **Behavior.**
  - Install package and license-purpose trust anchors from config if absent.
  - Install license token if configured and absent.
  - Run one-shot `jurisearch-syncd update` for each corpus before enabling the daemon.
  - With `--wait`, fetch and verify the producer manifest for each corpus, then poll/run catch-up until
    the local cursor sequence equals the verified manifest head sequence, or until a timeout expires.
  - Check `query_readiness` signature and report shape using the same active topology semantics as the
    read gate.
- **Invariants under test.**
  - No trust anchor is silently replaced; key rotation requires an explicit command.
  - A failed license/token/package verification leaves the cursor unchanged.
  - Catch-up cannot be reported green if no active corpus exists or if the local cursor is behind the
    verified producer head.
  - Active-but-unstamped, stale, and malformed readiness are separate failures.
- **Done when.** A configured site can install trust, apply at least one corpus, and prove readiness
  before `jurisearch-site` is started for clients.

---

## Phase 6 - bge-m3 model endpoint management

- **Goal.** Make the local embedding prerequisite testable and optionally installable.
- **Builds on.** Existing `jurisearch-bge-m3.service` and `PreparedQueryEmbedder::from_env`.
- **Deliverables.**
  - `jurisearchctl embed doctor --config <path>`.
  - `jurisearchctl embed render-service --config <path>`.
  - Optional `jurisearchctl embed fetch-assets --config <path> --manifest <asset-manifest.toml>` for
    model/tokenizer downloads with pinned hashes.
- **Checks.**
  - `llama-server` exists and is executable.
  - Model and tokenizer paths exist and match configured hashes when hashes are supplied.
  - Endpoint binds loopback only.
  - Embeddings route returns the expected dimension.
  - If an active corpus exists, the effective fingerprint computed by the embedder's
    `storage_embedding_fingerprint()` matches the `corpus_state.embedding_fingerprint` package value;
    before catch-up, report "no active corpus to compare" without treating it as a model-endpoint
    failure.
  - Pooling is fixed to `cls` and rendered into both client-side embedder config and the generated
    bge-m3 `--pooling` flag; because the storage fingerprint does not encode pooling, pooling is a
    deploy-time validation rule, not a fingerprint guard.
- **Invariants under test.**
  - `serve-site` startup errors for tokenizer/model/endpoint mismatch can be reproduced by
    `jurisearchctl embed doctor` before systemd is involved.
  - Optional asset fetching never runs implicitly during query service startup.
  - Any temporary endpoint started by `embed doctor` is stopped before the command exits.
- **Done when.** Operators can distinguish "model endpoint is wrong" from "database/site service is
  wrong" with one command.

---

## Phase 7 - Thin client configuration and distribution

- **Goal.** Make the client a real installable artifact with persistent server selection.
- **Builds on.** `jurisearch-client` endpoint parsing and dependency-cone tests.
- **Deliverables.**
  - `jurisearch-client configure --server <url>` writes `$XDG_CONFIG_HOME/jurisearch/client.toml`.
  - Parser changes from today's flat positional command to local subcommands plus forwarded site
    operations; `configure` and `doctor` are reserved client-local verbs.
  - Endpoint resolution keeps `--server` and `--local` mutually exclusive, preserves the existing
    `JURISEARCH_SITE_URL` fallback, and adds the client config file strictly below the env var.
  - `jurisearch-client doctor` validates URL parsing, reachability, protocol version, and `status`.
  - Release bundle for client-only install.
- **Invariants under test.**
  - The thin client still has no dependency on storage/embed/ingest/cli/postgres/tokenizers.
  - Existing `--server`, `--local`, and `JURISEARCH_SITE_URL` behavior stays compatible.
  - Bad config file permissions or malformed URLs produce clear exit-2 diagnostics.
- **Done when.** A user can install one binary, run one configure command, and then use
  `jurisearch-client status/search/fetch` without shell env setup.

---

## Phase 8 - Smoke tests and acceptance

- **Goal.** Replace prerequisite-skipping runbook gaps with explicit pass/fail product checks.
- **Builds on.** Phases 1-7.
- **Deliverables.**
  - `jurisearchctl site smoke --config <path> --fetch-id <id>`.
  - `work/10-next-plans/scripts/deploy-single-host-acceptance.sh` as the operated acceptance harness.
  - Updated two-host runbook that uses `jurisearchctl` commands instead of manual unit/env setup.
- **Smoke legs.**
  - Validate config.
  - Confirm services are active using positive readiness signals: expected systemd state, a
    `serve-site` bind/listening signal, and green `site readiness`. Do not rely on absence of journal
    error strings.
  - Confirm `jurisearch-syncd status --json` reports active corpora.
  - Confirm site `status` through `jurisearch-client`.
  - Fetch a known document id through the thin client.
  - Run a lexical search through the thin client.
  - If embedder is configured, run a hybrid search; fingerprint mismatch is a failed deployment, not a
    silent skip.
  - Negative checks: bad URL, unreachable port, disallowed public bind, read-role write attempt.
- **Invariants under test.**
  - Smoke exits zero only after real data legs succeed.
  - Missing fetch id is a failure when data prerequisites are present.
  - A skipped leg is only allowed under an explicit `--allow-skip <reason>` flag for development.
  - Every negative check asserts the expected diagnostic substring, not merely non-zero exit.
- **Done when.** A two-host deployment can produce a compact acceptance record from product commands,
  and CI still covers config rendering plus non-root dry-run behavior.

---

## Phase 9 - Release packaging and upgrade/rollback

- **Goal.** Ship the deploy experience as repeatable artifacts, not source-tree instructions.
- **Builds on.** All prior phases.
- **Deliverables.**
  - A repository-root release script, `./dist.sh`, that can be run from the repository root and writes
    all local release outputs under the repository-local `./dist/` directory. In the current checkout,
    that means `/home/pierre/Work/jurisearch/dist/`; it must never write to the filesystem root
    directory `/dist`. It may delegate to `cargo xtask dist --out ./dist`, but the operator-facing
    contract is the root script.
  - `./dist/manifest.toml` describing version, git commit, target triples, bundle filenames, checksums,
    binary versions, and intentionally external prerequisites.
  - Distinct deployment bundles:
    - `./dist/update-server/`: producer/update-ingest assets for the package origin, including
      `jurisearch-producer`, the package/ingest binaries required by
      `02-auto-update-server-crons.md` Phase 2 / open decision #1, producer systemd service/timer
      templates, example `producer.toml`, checksums, and an archive such as
      `jurisearch-update-server-<version>-<target>.tar.zst`. Under the recommended shell-out path, this
      intentionally includes the heavy `jurisearch` binary plus `jurisearch-package`; the thin-cone
      invariant applies to `./dist/cli/`, not to the update-server role.
    - `./dist/site-server/`: customer site assets, including `jurisearch`, `jurisearch-syncd`,
      `jurisearchctl`, site/bge-m3/syncd systemd templates, example `site.toml`, checksums, and an
      archive such as `jurisearch-site-server-<version>-<target>.tar.zst`.
    - `./dist/cli/`: thin-client assets, including `jurisearch-client`, shell completions/manpage if
      generated, checksums, and an archive such as `jurisearch-cli-<version>-<target>.tar.zst`.
  - No release bundle contains database contents, corpus packages, vector indexes, model weights, or
    tokenizer files. If model/tokenizer fetch automation is shipped, the dist output contains only a
    signed/checksummed asset manifest and fetch command wiring, not the huge assets themselves.
  - Optional Debian package metadata later, preserving the same three role boundaries:
    update-server, site-server, and cli.
  - `jurisearchctl site upgrade --config <path> --bundle <tarball>`.
  - `jurisearchctl site rollback --config <path> --to <version>`.
  - `jurisearchctl backup pre-upgrade --config <path>`.
- **Upgrade behavior.**
  - Stop `jurisearch-site` before replacing binaries.
  - Keep `jurisearch-syncd` policy explicit: either stop during binary replacement or prove daemon
    compatibility.
  - Run DB migration checks before starting the new services.
  - Preserve generated config and operator TOML.
  - Record installed version, render hash, and binary checksums.
- **Invariants under test.**
  - Running `./dist.sh` from the repository root creates a fresh repository-local `./dist/` with
    update-server, site-server, cli, and top-level manifest/checksum outputs.
  - Each bundle is installable independently and contains only the assets for its deployment role.
  - The update-server bundle includes every binary needed by the selected producer orchestration
    strategy, including shell-out dependencies if `02-auto-update-server-crons.md` Phase 2 / open
    decision #1 chooses that path.
  - The site-server bundle can render/install services without the source tree.
  - The cli bundle has no storage, embedder, ingest, PostgreSQL, or producer dependencies.
  - The release builder fails if a huge/runtime asset is accidentally included in a bundle.
  - Upgrade refuses to proceed if the new binary cannot parse the existing config.
  - Rollback does not mutate corpus data.
  - Generated files can be recreated from config after upgrade.
- **Done when.** Operators can install from a release artifact and upgrade without consulting the
  source tree, and a reviewer can inspect `./dist/manifest.toml` to see exactly which assets are bundled
  versus provisioned externally.

---

## Test matrix

| Area | Required tests |
|---|---|
| Config parser | valid minimal config, complete config, bad URL, non-absolute paths, unsafe role reuse, missing license anchor, external site embedder URL rejected |
| Rendering | golden env files, golden units, bind translation, full embed env blocks including normalize/pooling, absolute `ReadOnlyPaths`, redacted secrets |
| Doctor | missing binary, missing DB, missing extension, stale readiness, occupied port, bad model path |
| DB provisioning | external-PG migration runner, idempotence, read role cannot write, activation visibility postcondition |
| Trust/catch-up | anchor install idempotence, package vs license anchors, bad token, missing manifest, missing configured corpus manifest, behind-head cursor, no active corpus is not green |
| bge-m3 | endpoint dimension mismatch, tokenizer missing, non-loopback endpoint refusal |
| Client config | resolution order, malformed file, legacy env behavior, dependency cone |
| Smoke | real status/fetch/bm25/hybrid search, no silent skips, diagnostic substring checks for negatives |
| Release dist | repository-root `./dist.sh`, clean repository-local `./dist/`, distinct update-server/site-server/cli bundles, manifest/checksums, no DB/model/tokenizer/corpus payloads |

---

## Documentation updates

- Replace the manual install comments in `deploy/systemd/*.service` with "generated by
  `jurisearchctl site install`; templates live here for reference."
- Add `work/10-next-plans/02-deploy-runbook.md` after implementation starts, containing:
  - single-host demo;
  - production site host;
  - thin client install;
  - logs and troubleshooting;
  - upgrade/rollback;
  - backup/restore;
  - firewall/Tailscale notes.
- Update `work/09-jurisearch-cli/05-two-host-acceptance.md` to call the new product commands.
- Add `./dist/README.md` generation documenting the three bundles, excluded large assets, required host
  prerequisites, and the install command for each role.

---

## Sequencing summary

1. Config schema and deterministic rendering.
2. Doctor checks with stable failure codes.
3. Idempotent DB provisioning.
4. Service install/lifecycle wrapper.
5. Trust/license/catch-up/readiness bootstrap.
6. Embedder doctor and optional asset fetch.
7. Thin client persistent config.
8. Smoke tests and operated acceptance.
9. Release packaging plus upgrade/rollback.

The first useful milestone is phases 1-4: an operator can render, validate, and install the same
services that already exist today. The second useful milestone is phases 5-8: a fresh site can prove it
has data and can answer a thin client. Packaging and upgrade hardening come after the deploy path is
correct and repeatable. `./dist.sh` can emit `site-server` and `cli` bundles from this plan alone, but
the complete `update-server` bundle is gated on `02-auto-update-server-crons.md` Phase 2-3 because that
plan creates `jurisearch-producer` and the producer service/timer templates.
