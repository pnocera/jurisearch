# WAL-RUS PostgreSQL Backup Analysis

Date: 2026-07-01

## Executive Recommendation

WAL-RUS is a good candidate for JuriSearch PostgreSQL backups, but it should be adopted as an operator-managed PostgreSQL backup layer, not as application storage code in this repo.

Use it first for the external PostgreSQL deployments introduced by the site/producer architecture. Do not wire it into `ManagedPostgres` yet. The embedded/local `ManagedPostgres` path owns a private data directory and runtime config from inside the Rust process; backup policy there is a different product decision. The deployed site and producer databases are already external/operator-owned, with systemd units depending on `postgresql.service`, so WAL archival and base backup belong beside PostgreSQL on the database host.

The safe rollout is:

1. Pilot WAL-RUS on a disposable clone using full base backups plus continuous WAL archiving.
2. Prove restore and point-in-time recovery before enabling retention deletion.
3. Add production backup runbooks and monitoring.
4. Only then consider rendering helper files from `jurisearchctl site render`.

## Source Summary

ClickHouse introduced WAL-RUS as an open-source Rust implementation of PostgreSQL backup and WAL archival tooling. The stated goal is predictable memory use in constrained environments while preserving WAL-G compatibility. The blog reports daemonized WAL archival, streaming-oriented pipelines, bounded worker pools, and compatibility with `WALG_*` configuration variables.

The upstream repository describes WAL-RUS as a Rust port of WAL-G for PostgreSQL, tuned for no-overcommit hosts with streaming I/O and no full-segment buffering. It currently lists these storage backends: `file://`, `s3://`, and `gs://`; compression methods: `none`, `zstd`, `brotli`, `lz4`, `lzma`, and `gzip`; and PostgreSQL commands including `wal-push`, `wal-fetch`, `wal-verify`, `wal-restore`, `wal-receive`, `backup-push`, `backup-fetch`, `backup-list`, `backup-show`, `delete`, `copy`, `daemon`, and `daemon-client`.

PostgreSQL's own PITR model still applies: enable WAL archiving with `wal_level = replica` or higher, `archive_mode = on`, and either `archive_command` or `archive_library`; take base backups; retain all WAL needed from the base backup start; configure `restore_command` during recovery; and monitor archive lag because repeated archive failures can fill `pg_wal`.

## Repo Fit

### External Site PostgreSQL

This is the best first target.

Relevant local context:

- `crates/jurisearch-deploy/src/config.rs` defines `[database]` as a connection and role topology for a shared site PostgreSQL: host, port, database name, admin/bootstrap user, writer user, read user, and owner role.
- `crates/jurisearch-deploy/src/ops/provision.rs` provisions the database, migrations, and read/write/owner roles through the admin/bootstrap connection.
- `deploy/systemd/jurisearch-site.service` and `deploy/systemd/jurisearch-syncd.service` both order after `postgresql.service`, which confirms PostgreSQL is external infrastructure in the deployment shape.

Backup ownership should therefore be at the PostgreSQL cluster level. The app roles `jurisearch_read` and `jurisearch_write` must not own backups. Introduce a separate operator backup identity, for example `jurisearch_backup`, or use the existing PostgreSQL administrative identity during an initial pilot. The final role should be least-privilege and validated against the exact WAL-RUS command path used (`backup-push` over replication protocol vs local `PGDATA` backup path).

### External Producer PostgreSQL

This is also a good fit. `crates/jurisearch-producer/src/config.rs` explicitly says the producer database is external and never a self-managed `ManagedPostgres`. Producer state is more expensive to reconstruct than the read-only site path because it includes ingest/update state, source archive accounting, and package publication state. It should use the same backup standard.

### Embedded `ManagedPostgres`

Do not prioritize WAL-RUS here.

`crates/jurisearch-storage/src/runtime.rs` creates private PostgreSQL clusters under the index directory for durable local indexes and under a temp dir for tests. It writes runtime PostgreSQL config itself, chooses free loopback ports, and runs migrations after startup. WAL-RUS could technically back up the durable data directory, but there is no stable operator surface yet for configuring `archive_command`, storage credentials, retention, and restore. For now, local indexes are better treated as rebuildable unless product requirements say otherwise.

## Proposed Architecture

Run WAL-RUS next to the PostgreSQL server, not inside `jurisearch-site`, `jurisearch-syncd`, or `jurisearch-producer`.

Recommended deployment shape:

- Install a pinned WAL-RUS binary or exact git SHA on the PostgreSQL host. Do not track `main` implicitly.
- Store WAL-RUS environment in a root/postgres-readable `0600` file such as `/etc/jurisearch/walrus.env`.
- Use a unique object-storage prefix per PostgreSQL cluster, environment, and deployment, for example `s3://jurisearch-backups/prod/site/<cluster-id>`.
- Enable WAL archiving in PostgreSQL:
  - `wal_level = replica`
  - `archive_mode = on`
  - `archive_command = '<small wrapper> "%p" "%f"'`
  - optional `archive_timeout = '5min'` for low-write deployments where RPO matters more than WAL storage volume.
- The wrapper should load `/etc/jurisearch/walrus.env` and invoke WAL-RUS `wal-push` or `daemon-client` after confirming the exact CLI from `walrus --help`.
- Run a `walrus daemon` systemd unit if the pilot confirms lower overhead and stable behavior; otherwise start with direct `wal-push` from `archive_command`.
- Run a base-backup systemd timer, initially nightly full backups.
- Keep incremental/delta backup mode disabled until full-backup restore drills pass. WAL-RUS has PG17 WAL-summary support, but the compatibility doc notes delta selection differences from WAL-G that deserve a separate validation pass.

## Pilot Plan

1. Build or install WAL-RUS at a pinned version.
2. Stand up a disposable PostgreSQL instance matching production major version and required JuriSearch extension binaries (`pg_search`, `vector`).
3. Provision a JuriSearch database with `jurisearchctl site provision-db` or producer equivalent.
4. Configure WAL-RUS to a non-production storage prefix using `file://` first if local validation is enough, then the real S3/GCS backend.
5. Enable `archive_mode` and `archive_command`; force a WAL switch with `pg_switch_wal`; confirm `pg_stat_archiver` success and WAL-RUS object layout.
6. Run a full `backup-push`.
7. Mutate the DB with a known marker row or table, then run another WAL switch.
8. Restore to a clean host/data directory using `backup-fetch` plus `restore_command`.
9. Prove point-in-time recovery to before and after the marker mutation.
10. Run `backup-list`, `backup-show`, and `wal-verify` as operational checks.
11. Only after successful restore drills, test `delete` in dry-run mode; keep deletion disabled until retention policy is approved.

Acceptance criteria:

- A full restore starts PostgreSQL successfully with the JuriSearch extensions installed.
- `jurisearchctl site doctor` or equivalent readiness checks pass after restore.
- PITR can recover to a selected timestamp before an intentional destructive transaction.
- Archive failure causes a visible alert before `pg_wal` disk pressure becomes dangerous.
- Credentials, encryption keys, and storage prefixes are not exposed in process listings, units, or rendered app env files.

## Operational Policy

Initial defaults:

- RPO: 5 minutes, controlled by WAL archival health plus `archive_timeout`.
- RTO: measure during pilot; do not promise until restore timings exist.
- Base backup cadence: nightly full backup.
- Retention: keep at least 7 daily full backups and all WAL required to restore them; keep monthly snapshots if storage cost allows.
- Deletion: dry-run only until two independent restore drills pass.
- Encryption: prefer WAL-RUS libsodium encryption or bucket-level encryption plus strict bucket IAM. Avoid OpenPGP because WAL-RUS intentionally does not support `WALG_PGP_*`.
- Monitoring:
  - `pg_stat_archiver.failed_count`, `last_failed_wal`, `last_failed_time`
  - age of last successful archived WAL
  - `pg_wal` filesystem usage
  - object-store write failures and latency
  - age of last successful base backup
  - restore drill freshness

## Risks And Constraints

- Maturity risk: WAL-RUS is new compared with WAL-G and pgBackRest. Treat it as a candidate that needs restore drills, not as proven infrastructure because the blog is compelling.
- Documentation is still sparse. The README delegates command details to `walrus --help`, so production scripts must be based on the actual binary help for the pinned version.
- Backend constraints: upstream lists `file://`, `s3://`, and `gs://`; if a deployment needs Azure Blob, Swift, SSH, or another backend, WAL-RUS is not currently the right tool.
- AWS auth constraints: the design doc says static keys and IMDS are implemented, while shared profiles and STS web identity are not. That matters for Kubernetes/EKS-style deployments.
- Metrics constraints: the compatibility doc says WAL-G StatsD settings are not implemented. Monitoring must use PostgreSQL, logs, wrapper exit statuses, object-store checks, and WAL-RUS commands.
- Encryption constraints: OpenPGP env vars are a hard startup error. Migration from an OpenPGP WAL-G bucket would need re-encryption to libsodium or a new prefix.
- Delta backup constraints: WAL-RUS delta backup selection trusts WAL/summary-derived maps rather than WAL-G's default full page scan. Use full backups first.
- Restore environment risk: PostgreSQL WAL/base backups restore cluster files, not OS packages. The restore host must have the same PostgreSQL major version and compatible `pg_search`/`vector` extension binaries.
- Config-file risk: PostgreSQL documentation notes WAL archiving does not restore manually edited config files such as `postgresql.conf`, `pg_hba.conf`, and `pg_ident.conf`. Back those up separately.
- Archive outage risk: if archiving fails long enough, PostgreSQL can fill `pg_wal` and shut down. This needs alerting before production.
- Prefix collision risk: PostgreSQL recommends archive commands refuse unsafe overwrites. Use unique prefixes and verify WAL-RUS behavior for pre-existing WAL files during the pilot.

## Integration Work Worth Doing Later

After the pilot, add repo support around the operator workflow rather than embedding backup execution in the app:

- A `work/13-backups/runbook.md` with exact commands for the pinned WAL-RUS version.
- Example systemd units for `walrus-daemon.service` and `jurisearch-postgres-backup.timer`.
- A `site doctor` advisory check that reports archive status from PostgreSQL when the admin connection is available.
- A rendered sample backup env file template that contains variable names but no secrets.
- A restore checklist in release documentation, including extension package prerequisites.

Avoid adding WAL-RUS settings to `site.toml` until the operational contract is stable. `site.toml` currently owns app/database role topology, not PostgreSQL cluster administration.

## Open Questions

- Which storage backend is the real target: S3, GCS, or local/file/NAS?
- Are site and producer databases separate clusters or separate databases in one cluster?
- What PostgreSQL major versions must be supported in production?
- What are the required RPO/RTO and retention periods?
- Who owns encryption key custody and restore authority?
- Will deployments run PostgreSQL on the same host as JuriSearch services, or as a managed/external service where `archive_command` is not operator-controlled?

## Sources

- ClickHouse blog, "Why we rewrote WAL-G for Postgres backups in Rust: Meet WAL-RUS", 2026-06-25: https://clickhouse.com/blog/walrus-postgres-backups-in-rust
- WAL-RUS README: https://github.com/ClickHouse/wal-rus
- WAL-RUS design notes: https://raw.githubusercontent.com/ClickHouse/wal-rus/main/docs/DESIGN.md
- WAL-RUS WAL-G compatibility notes: https://raw.githubusercontent.com/ClickHouse/wal-rus/main/docs/WALG_COMPAT.md
- PostgreSQL 18 documentation, continuous archiving and PITR: https://www.postgresql.org/docs/current/continuous-archiving.html
